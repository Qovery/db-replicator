use std::io::{Error, ErrorKind};
use std::sync::mpsc;
use std::thread;

use crate::datastore::Datastore;
use crate::source::SourceOptions;
use crate::tasks::{MaxBytes, Message, Task, TransferredBytes};
use crate::types::{to_bytes, Queries};
use crate::Source;

type DataMessage = (u16, Queries);

/// FullDumpTask is a wrapping struct to execute the synchronization between a *Source* and a *Datastore*
pub struct FullDumpTask<'a, S>
where
    S: Source,
{
    source: S,
    datastore: Box<dyn Datastore>,
    options: SourceOptions<'a>,
}

impl<'a, S> FullDumpTask<'a, S>
where
    S: Source,
{
    pub fn new(source: S, datastore: Box<dyn Datastore>, options: SourceOptions<'a>) -> Self {
        FullDumpTask {
            source,
            datastore,
            options,
        }
    }
}

impl<'a, S> Task for FullDumpTask<'a, S>
where
    S: Source,
{
    fn run<F: FnMut(TransferredBytes, MaxBytes)>(
        mut self,
        mut progress_callback: F,
    ) -> Result<(), Error> {
        // initialize the source
        let _ = self.source.init()?;

        let (tx, rx) = mpsc::sync_channel::<Message<DataMessage>>(1);
        let datastore = self.datastore;

        let join_handle = thread::spawn(move || -> Result<(), Error> {
            // managing Datastore (S3) upload here
            let datastore = datastore;

            loop {
                let result = match rx.recv() {
                    Ok(Message::Data((chunk_part, queries))) => Ok((chunk_part, queries)),
                    Ok(Message::EOF) => break,
                    Err(err) => Err(Error::new(ErrorKind::Other, format!("{}", err))),
                };

                if let Ok((chunk_part, queries)) = result {
                    let _ = match datastore.write(chunk_part, to_bytes(queries)) {
                        Ok(_) => {}
                        Err(err) => return Err(Error::new(ErrorKind::Other, format!("{}", err))),
                    };
                }
            }

            Ok(())
        });

        // buffer default of 100MB (unless specified) in memory to use and re-use to upload data into datastore
        let chunk_size = self.options.chunk_size.unwrap_or(100);
        let buffer_size = chunk_size * 1024 * 1024;
        let mut queries = vec![];
        let mut consumed_buffer_size = 0usize;
        let mut total_transferred_bytes = 0usize;
        let mut chunk_part = 0u16;

        // init progress
        progress_callback(
            total_transferred_bytes,
            buffer_size * (chunk_part as usize + 1),
        );

        let _ = self.source.read(self.options, |_original_query, query| {
            if consumed_buffer_size + query.data().len() > buffer_size {
                chunk_part += 1;
                consumed_buffer_size = 0;
                // TODO .clone() - look if we do not consume more mem

                let message = Message::Data((chunk_part, queries.clone()));

                let _ = tx.send(message); // FIXME catch SendError?
                let _ = queries.clear();
            }

            consumed_buffer_size += query.data().len();
            total_transferred_bytes += query.data().len();
            progress_callback(
                total_transferred_bytes,
                buffer_size * (chunk_part as usize + 1),
            );
            queries.push(query);
        })?;

        progress_callback(total_transferred_bytes, total_transferred_bytes);

        chunk_part += 1;
        let _ = tx.send(Message::Data((chunk_part, queries)));
        let _ = tx.send(Message::EOF);
        // wait for end of upload execution
        join_handle.join().unwrap()?;

        Ok(())
    }
}
