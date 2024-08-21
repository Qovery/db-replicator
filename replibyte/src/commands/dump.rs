use std::fs::File;
use std::io::{stdin, BufReader, Error, ErrorKind, Read};
use std::sync::mpsc;
use std::time::Duration;

use timeago::Formatter;

use crate::cli::{DumpCreateArgs, DumpDeleteArgs};
use crate::cli::{RestoreArgs, RestoreLocalArgs};
use crate::config::{Config, ConnectionUri};
use crate::datastore::Datastore;
use crate::datastore::ReadOptions;
use crate::destination::generic_stdout::GenericStdout;
use crate::destination::mongodb_docker::{MongoDBDocker, DEFAULT_MONGO_CONTAINER_PORT};
use crate::destination::mysql_docker::{
    MysqlDocker, DEFAULT_MYSQL_CONTAINER_PORT, DEFAULT_MYSQL_IMAGE_TAG,
};
use crate::destination::postgres_docker::{
    PostgresDocker, DEFAULT_POSTGRES_CONTAINER_PORT, DEFAULT_POSTGRES_DB,
    DEFAULT_POSTGRES_IMAGE_TAG, DEFAULT_POSTGRES_PASSWORD, DEFAULT_POSTGRES_USER,
};
use crate::source::mongodb::MongoDB;
use crate::source::mongodb_stdin::MongoDBStdin;
use crate::source::mysql::Mysql;
use crate::source::mysql_stdin::MysqlStdin;
use crate::source::postgres::Postgres;
use crate::source::postgres_stdin::PostgresStdin;
use crate::source::SourceOptions;
use crate::tasks::full_dump::FullDumpTask;
use crate::tasks::full_restore::FullRestoreTask;
use crate::tasks::Task;
use crate::utils::{epoch_millis, table, to_human_readable_unit};
use crate::{destination, CLI};
use clap::CommandFactory;

/// List all dumps
pub fn list(datastore: &mut Box<dyn Datastore>) -> Result<(), Error> {
    let mut index_file = datastore.index_file()?;

    if index_file.dumps.is_empty() {
        println!("<empty> no dumps available\n");
        return Ok(());
    }

    index_file.dumps.sort_by(|a, b| a.cmp(b).reverse());

    let mut table = table();
    table.set_titles(row!["name", "size", "when", "compressed", "encrypted"]);
    let formatter = Formatter::new();
    let now = epoch_millis();

    for dump in index_file.dumps {
        table.add_row(row![
            dump.directory_name.as_str(),
            to_human_readable_unit(dump.size),
            formatter.convert(Duration::from_millis((now - dump.created_at) as u64)),
            dump.compressed,
            dump.encrypted,
        ]);
    }

    let _ = table.printstd();

    Ok(())
}

// Create a new dump
pub fn run<F>(
    args: &DumpCreateArgs,
    mut datastore: Box<dyn Datastore>,
    config: Config,
    progress_callback: F,
) -> anyhow::Result<()>
where
    F: Fn(usize, usize) -> (),
{
    if let Some(encryption_key) = config.encryption_key()? {
        datastore.set_encryption_key(encryption_key)
    }

    match config.source {
        Some(source) => {
            // Configure datastore options (compression is enabled by default)
            datastore.set_compression(source.compression.unwrap_or(true));

            // Match the transformers from the config
            let transformers = match &source.transformers {
                Some(transformers) => transformers
                    .iter()
                    .flat_map(|transformer| {
                        transformer.columns.iter().map(|column| {
                            column.transformer.transformer(
                                transformer.database.as_str(),
                                transformer.table.as_str(),
                                column.name.as_str(),
                            )
                        })
                    })
                    .collect::<Vec<_>>(),
                None => vec![],
            };

            let empty_config = vec![];
            let skip_config = match &source.skip {
                Some(config) => config,
                None => &empty_config,
            };

            let empty_config = vec![];
            let only_tables_config = match &source.only_tables {
                Some(config) => config,
                None => &empty_config,
            };

            for only_table in only_tables_config {
                for skip in skip_config {
                    if only_table.database == skip.database && only_table.table == skip.table {
                        return Err(anyhow::Error::from(Error::new(
                            ErrorKind::Other,
                            format!(
                                "Table \"{}.{}\" cannot be both in \"only_table\" and in \"skip_table\" at the same time",
                                only_table.database,
                                only_table.table
                            )
                        )));
                    }
                }
            }

            let options = SourceOptions {
                transformers: &transformers,
                skip_config: &skip_config,
                database_subset: &source.database_subset,
                database_superset: &source.database_superset,
                only_tables: &only_tables_config,
            };

            match args.source_type.as_ref().map(|x| x.as_str()) {
                None => match source.connection_uri()? {
                    ConnectionUri::Postgres(host, port, username, password, database) => {
                        let postgres = Postgres::new(
                            host.as_str(),
                            port,
                            database.as_str(),
                            username.as_str(),
                            password.as_str(),
                        );

                        let task = FullDumpTask::new(postgres, datastore, options);
                        task.run(progress_callback)?
                    }
                    ConnectionUri::Mysql(host, port, username, password, database) => {
                        let mysql = Mysql::new(
                            host.as_str(),
                            port,
                            database.as_str(),
                            username.as_str(),
                            password.as_str(),
                        );

                        let task = FullDumpTask::new(mysql, datastore, options);
                        task.run(progress_callback)?
                    }
                    ConnectionUri::MongoDB(uri, database) => {
                        let mongodb = MongoDB::new(uri.as_str(), database.as_str());

                        let task = FullDumpTask::new(mongodb, datastore, options);
                        task.run(progress_callback)?
                    }
                },
                // some user use "postgres" and "postgresql" both are valid
                Some(v) if v == "postgres" || v == "postgresql" => {
                    if args.file.is_some() {
                        let dump_file = File::open(args.file.as_ref().unwrap())?;
                        let mut stdin = stdin(); // FIXME
                        let reader = BufReader::new(dump_file);
                        let _ = stdin.read_to_end(&mut reader.buffer().to_vec())?;
                    }

                    let postgres = PostgresStdin::default();
                    let task = FullDumpTask::new(postgres, datastore, options);
                    task.run(progress_callback)?
                }
                Some(v) if v == "mysql" => {
                    if args.file.is_some() {
                        let dump_file = File::open(args.file.as_ref().unwrap())?;
                        let mut stdin = stdin(); // FIXME
                        let reader = BufReader::new(dump_file);
                        let _ = stdin.read_to_end(&mut reader.buffer().to_vec())?;
                    }

                    let mysql = MysqlStdin::default();
                    let task = FullDumpTask::new(mysql, datastore, options);
                    task.run(progress_callback)?
                }
                Some(v) if v == "mongodb" => {
                    if args.file.is_some() {
                        let dump_file = File::open(args.file.as_ref().unwrap())?;
                        let mut stdin = stdin(); // FIXME
                        let reader = BufReader::new(dump_file);
                        let _ = stdin.read_to_end(&mut reader.buffer().to_vec())?;
                    }

                    let mongodb = MongoDBStdin::default();
                    let task = FullDumpTask::new(mongodb, datastore, options);
                    task.run(progress_callback)?
                }
                Some(v) => {
                    return Err(anyhow::Error::from(Error::new(
                        ErrorKind::Other,
                        format!("source type '{}' not recognized", v),
                    )));
                }
            }

            println!("Dump created successfully!");
            Ok(())
        }
        None => {
            return Err(anyhow::Error::from(Error::new(
                ErrorKind::Other,
                "missing <source> object in the configuration file",
            )));
        }
    }
}

pub fn delete(datastore: Box<dyn Datastore>, args: &DumpDeleteArgs) -> anyhow::Result<()> {
    let _ = datastore.delete(args)?;
    println!("Dump deleted!");
    Ok(())
}

/// Restore a dump in a local container
pub fn restore_local<F>(
    args: &RestoreLocalArgs,
    mut datastore: Box<dyn Datastore>,
    config: Config,
    progress_callback: F,
) -> anyhow::Result<()>
where
    F: Fn(usize, usize) -> (),
{
    if let Some(encryption_key) = config.encryption_key()? {
        datastore.set_encryption_key(encryption_key);
    }

    let options = match args.value.as_str() {
        "latest" => ReadOptions::Latest,
        v => ReadOptions::Dump {
            name: v.to_string(),
        },
    };

    if args.output {
        let mut generic_stdout = GenericStdout::new();
        let task = FullRestoreTask::new(&mut generic_stdout, datastore, options);
        let _ = task.run(|_, _| {})?; // do not display the progress bar
        return Ok(());
    }

    let image = match &args.image {
        Some(image) => image,
        None => {
            let mut cmd = CLI::command();
            cmd.error(
                clap::ErrorKind::MissingRequiredArgument,
                "you must use --output or --image [database_type] option",
            )
            .exit();
        }
    };

    if image.as_str() == "postgres" || image.as_str() == "postgresql" {
        let port = args.port.unwrap_or(DEFAULT_POSTGRES_CONTAINER_PORT);
        let tag = match &args.tag {
            Some(tag) => tag,
            None => DEFAULT_POSTGRES_IMAGE_TAG,
        };

        let mut postgres = PostgresDocker::new(tag.to_string(), port);
        let task = FullRestoreTask::new(&mut postgres, datastore, options);
        let _ = task.run(progress_callback)?;

        print_connection_string_and_wait(
            "To connect to your PostgreSQL instance, use the following connection string:",
            &format!(
                "postgres://{}:{}@localhost:{}/{}",
                DEFAULT_POSTGRES_USER, DEFAULT_POSTGRES_PASSWORD, port, DEFAULT_POSTGRES_DB
            ),
        );

        match postgres.container {
            Some(container) => {
                if args.remove {
                    match container.rm() {
                        Ok(_) => {
                            println!("Container removed!");
                            return Ok(());
                        }
                        Err(err) => return Err(anyhow::Error::from(err)),
                    }
                }

                match container.stop() {
                    Ok(_) => {
                        println!("container stopped!");
                        return Ok(());
                    }
                    Err(err) => return Err(anyhow::Error::from(err)),
                }
            }
            None => {
                return Err(anyhow::Error::from(Error::new(
                    ErrorKind::Other,
                    "command error: unable to retrieve container ID",
                )));
            }
        }
    }

    if image.as_str() == "mongodb" {
        let port = args.port.unwrap_or(DEFAULT_MONGO_CONTAINER_PORT);
        let tag = match &args.tag {
            Some(tag) => tag,
            None => crate::destination::mongodb_docker::DEFAULT_MONGO_IMAGE_TAG,
        };

        let mut mongodb = MongoDBDocker::new(tag.to_string(), port);
        let task = FullRestoreTask::new(&mut mongodb, datastore, options);
        let _ = task.run(progress_callback)?;

        print_connection_string_and_wait(
            "To connect to your MongoDB instance, use the following connection string:",
            &format!("mongodb://root:password@localhost:{}/root", port),
        );

        match mongodb.container {
            Some(container) => {
                if args.remove {
                    match container.rm() {
                        Ok(_) => {
                            println!("Container removed!");
                            return Ok(());
                        }
                        Err(err) => return Err(anyhow::Error::from(err)),
                    }
                }

                match container.stop() {
                    Ok(_) => {
                        println!("container stopped!");
                        return Ok(());
                    }
                    Err(err) => return Err(anyhow::Error::from(err)),
                }
            }
            None => {
                return Err(anyhow::Error::from(Error::new(
                    ErrorKind::Other,
                    "command error: unable to retrieve container ID",
                )));
            }
        }
    }

    if image.as_str() == "mysql" {
        let port = args.port.unwrap_or(DEFAULT_MYSQL_CONTAINER_PORT);
        let tag = match &args.tag {
            Some(tag) => tag,
            None => DEFAULT_MYSQL_IMAGE_TAG,
        };

        let mut mysql = MysqlDocker::new(tag.to_string(), port);
        let task = FullRestoreTask::new(&mut mysql, datastore, options);
        let _ = task.run(progress_callback)?;

        print_connection_string_and_wait(
            "To connect to your MySQL instance, use the following connection string:",
            &format!("mysql://root:password@127.0.0.1:{}/root", port),
        );

        match mysql.container {
            Some(container) => {
                if args.remove {
                    match container.rm() {
                        Ok(_) => {
                            println!("Container removed!");
                            return Ok(());
                        }
                        Err(err) => return Err(anyhow::Error::from(err)),
                    }
                }

                match container.stop() {
                    Ok(_) => {
                        println!("container stopped!");
                        return Ok(());
                    }
                    Err(err) => return Err(anyhow::Error::from(err)),
                }
            }
            None => {
                return Err(anyhow::Error::from(Error::new(
                    ErrorKind::Other,
                    "command error: unable to retrieve container ID",
                )));
            }
        }
    }

    Ok(())
}

/// Restore a dump in the configured destination
pub fn restore_remote<F>(
    args: &RestoreArgs,
    mut datastore: Box<dyn Datastore>,
    config: Config,
    progress_callback: F,
) -> anyhow::Result<()>
where
    F: Fn(usize, usize) -> (),
{
    if let Some(encryption_key) = config.encryption_key()? {
        datastore.set_encryption_key(encryption_key);
    }

    let options = match args.value.as_str() {
        "latest" => ReadOptions::Latest,
        v => ReadOptions::Dump {
            name: v.to_string(),
        },
    };

    if args.output {
        let mut generic_stdout = GenericStdout::new();
        let task = FullRestoreTask::new(&mut generic_stdout, datastore, options);
        let _ = task.run(|_, _| {})?; // do not display the progress bar
        return Ok(());
    }

    match config.destination {
        Some(destination) => {
            match destination.connection_uri()? {
                ConnectionUri::Postgres(host, port, username, password, database) => {
                    let mut postgres = destination::postgres::Postgres::new(
                        host.as_str(),
                        port,
                        database.as_str(),
                        username.as_str(),
                        password.as_str(),
                        destination.wipe_database.unwrap_or(true),
                    );

                    let task = FullRestoreTask::new(&mut postgres, datastore, options);
                    task.run(progress_callback)?
                }
                ConnectionUri::Mysql(host, port, username, password, database) => {
                    let mut mysql = destination::mysql::Mysql::new(
                        host.as_str(),
                        port,
                        database.as_str(),
                        username.as_str(),
                        password.as_str(),
                    );
                    let task = FullRestoreTask::new(&mut mysql, datastore, options);
                    task.run(progress_callback)?;
                }
                ConnectionUri::MongoDB(uri, database) => {
                    let mut mongodb =
                        destination::mongodb::MongoDB::new(uri.as_str(), database.as_str());

                    let task = FullRestoreTask::new(&mut mongodb, datastore, options);
                    task.run(progress_callback)?
                }
            }

            println!("Restore successful!");
            Ok(())
        }
        None => {
            return Err(anyhow::Error::from(Error::new(
                ErrorKind::Other,
                "missing <destination> object in the configuration file",
            )));
        }
    }
}

fn wait_until_ctrlc(msg: &str) {
    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || tx.send(()).expect("cound not send signal on channel"))
        .expect("Error setting Ctrl-C handler");
    println!("{}", msg);
    rx.recv().expect("Could not receive from channel.");
}

fn print_connection_string_and_wait(msg: &str, connection_string: &str) {
    println!("{}", msg);
    println!("> {}", connection_string);
    wait_until_ctrlc("Waiting for Ctrl-C to stop the container");
}
