use std::io::{stdin, BufReader, Error};

use crate::connector::Connector;
use crate::source::mongodb::read_and_transform;
use crate::types::{OriginalQuery, Query};
use crate::Source;
use crate::SourceOptions;

pub struct MongoDBStdin {}

impl Default for MongoDBStdin {
    fn default() -> Self {
        Self {}
    }
}

impl Connector for MongoDBStdin {
    fn init(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Source for MongoDBStdin {
    fn read<F: FnMut(OriginalQuery, Query)>(
        &self,
        options: SourceOptions,
        query_callback: F,
    ) -> Result<(), Error> {
        let reader = BufReader::new(stdin());

        if let Some(_database_subset) = &options.database_subset {
            todo!("database subset not supported yet for MongoDB source")
        }

        if let Some(_database_superset) = &options.database_superset {
            todo!("database superset not supported yet for MongoDB source")
        }

        let _ = read_and_transform(reader, options, query_callback)?;
        Ok(())
    }
}
