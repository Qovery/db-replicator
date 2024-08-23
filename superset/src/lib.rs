use std::collections::HashSet;
use std::io::Error;
use std::fmt;

mod dedup;
pub mod postgres;
mod utils;

pub type Bytes = Vec<u8>;

pub trait Superset {
    fn read<F: FnMut(String), P: FnMut(Progress)>(&self, data: F, progress: P)
        -> Result<(), Error>;
}

pub struct Progress {
    // total data rows
    pub total_rows: usize,
    // total rows to processed
    pub total_rows_to_process: usize,
    // rows processed
    pub processed_rows: usize,
    // last row processed exec time
    pub last_process_time: u128,
}

impl Progress {
    pub fn percent(&self) -> u8 {
        ((self.processed_rows as f64 / self.total_rows_to_process as f64) * 100.0) as u8
    }
}

#[derive(Debug, Hash, Eq, PartialEq)]
pub struct PassthroughTableSuperSet<'a> {
    pub database: &'a str,
    pub table: &'a str,
}

impl<'a> PassthroughTableSuperSet<'a> {
    pub fn new<S: Into<&'a str>>(database: S, table: S) -> Self {
        PassthroughTableSuperSet {
            database: database.into(),
            table: table.into(),
        }
    }
}

pub struct SupersetOptions<'a> {
    pub passthrough_tables: &'a HashSet<PassthroughTableSuperSet<'a>>,
}

impl<'a> SupersetOptions<'a> {
    pub fn new(passthrough_tables: &'a HashSet<PassthroughTableSuperSet<'a>>) -> Self {
        SupersetOptions { passthrough_tables }
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct SupersetTable {
    pub database: String,
    pub table: String,
    pub relations: Vec<SupersetTableRelation>,
}

impl SupersetTable {
    pub fn new<S: Into<String>>(
        database: S,
        table: S,
        relations: Vec<SupersetTableRelation>,
    ) -> Self {
        SupersetTable {
            database: database.into(),
            table: table.into(),
            relations,
        }
    }

    pub fn related_tables(&self) -> HashSet<&str> {
        self.relations
            .iter()
            .map(|r| r.table.as_str())
            .collect::<HashSet<_>>()
    }

    pub fn find_related_superset_tables<'a>(
        &self,
        superset_tables: &'a Vec<&SupersetTable>,
    ) -> Vec<&'a SupersetTable> {
        if superset_tables.is_empty() {
            return Vec::new();
        }

        let related_tables = self.related_tables();

        superset_tables
            .iter()
            .filter(|superset_table| related_tables.contains(superset_table.table.as_str()))
            .map(|superset_table| *superset_table)
            .collect::<Vec<_>>()
    }
}

/// Representing a query where...
/// database -> is the targeted database
/// table -> is the targeted table
/// from_property is the parent table property referencing the target table `to_property`
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct SupersetTableRelation {
    pub database: String,
    pub table: String,
    pub from_property: String,
    pub to_property: String,
}

impl SupersetTableRelation {
    pub fn new<S: Into<String>>(database: S, table: S, from_property: S, to_property: S) -> Self {
        SupersetTableRelation {
            database: database.into(),
            table: table.into(),
            from_property: from_property.into(),
            to_property: to_property.into(),
        }
    }
}
