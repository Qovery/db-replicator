use std::borrow::BorrowMut;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::io::{BufReader, Error, ErrorKind, Read, Write};
use std::process::{Command, Stdio};

use log::info;

use dump_parser::postgres::Keyword::NoKeyword;
use dump_parser::postgres::{
    get_column_names_from_create_query, get_column_names_from_insert_into_query,
    get_column_values_from_insert_into_query, get_tokens_from_query_str,
    get_word_value_at_position, match_keyword_at_position, Keyword, Token,
};
use dump_parser::utils::{list_sql_queries_from_dump_reader, ListQueryResult};
use subset::postgres::{PostgresSubset, SubsetStrategy};
use subset::{PassthroughTable, Subset, SubsetOptions};

use crate::config::DatabaseSubsetConfigStrategy;
use crate::connector::Connector;
use crate::source::{Explain, Source};
use crate::transformer::Transformer;
use crate::types::{Column, InsertIntoQuery, OriginalQuery, Query};
use crate::utils::{binary_exists, table, wait_for_command};
use crate::DatabaseSubsetConfig;

use super::SourceOptions;

enum RowType {
    InsertInto {
        database_name: String,
        table_name: String,
    },
    CreateTable {
        database_name: String,
        table_name: String,
    },
    AlterTable {
        database_name: String,
        table_name: String,
    },
    Others,
}

pub struct Postgres<'a> {
    host: &'a str,
    port: u16,
    database: &'a str,
    username: &'a str,
    password: &'a str,
}

impl<'a> Postgres<'a> {
    pub fn new(
        host: &'a str,
        port: u16,
        database: &'a str,
        username: &'a str,
        password: &'a str,
    ) -> Self {
        Postgres {
            host,
            port,
            database,
            username,
            password,
        }
    }
}

impl<'a> Connector for Postgres<'a> {
    fn init(&mut self) -> Result<(), Error> {
        binary_exists("pg_dump")
    }
}

impl<'a> Explain for Postgres<'a> {
    fn schema(&self) -> Result<(), Error> {
        let s_port = self.port.to_string();

        let dump_args = vec![
            "-s", // dump only the schema definitions
            "--no-owner",
            "-h",
            self.host,
            "-p",
            s_port.as_str(),
            "-U",
            self.username,
        ];

        let mut process = Command::new("pg_dump")
            .env("PGPASSWORD", self.password)
            .args(dump_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Other, "Could not capture standard output."))?;

        let reader = BufReader::new(stdout);

        read_and_parse_schema(reader)?;

        wait_for_command(&mut process)
    }
}

impl<'a> Source for Postgres<'a> {
    fn read<F: FnMut(OriginalQuery, Query)>(
        &self,
        options: SourceOptions,
        query_callback: F,
    ) -> Result<(), Error> {
        let s_port = self.port.to_string();

        let mut dump_args = vec![
            "--column-inserts", // dump data as INSERT commands with column names
            "--no-owner",       // skip restoration of object ownership
            "-h",
            self.host,
            "-p",
            s_port.as_str(),
            "-U",
            self.username,
        ];

        let only_tables_args: Vec<String> = options
            .only_tables
            .iter()
            .map(|cfg| format!("--table={}.{}", cfg.database, cfg.table))
            .collect();
        let mut only_tables_args: Vec<&str> = only_tables_args.iter().map(String::as_str).collect();

        dump_args.append(&mut only_tables_args);

        dump_args.push(self.database);

        // TODO: as for mysql we can exclude tables directly here so we can remove the skip_tables_map checks
        let mut process = Command::new("pg_dump")
            .env("PGPASSWORD", self.password)
            .args(dump_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Other, "Could not capture standard output."))?;

        match &options.database_subset {
            None => {
                let reader = BufReader::new(stdout);
                read_and_transform(reader, options, query_callback);
            }
            Some(subset_config) => {
                let dump_reader = BufReader::new(stdout);
                let reader = subset(dump_reader, subset_config)?;
                read_and_transform(reader, options, query_callback);
            }
        };

        wait_for_command(&mut process)
    }
}

pub fn subset<R: Read>(
    mut dump_reader: BufReader<R>,
    subset_config: &DatabaseSubsetConfig,
) -> Result<BufReader<File>, Error> {
    let mut named_temp_file = tempfile::NamedTempFile::new()?;
    let mut temp_dump_file = named_temp_file.as_file_mut();
    let _ = io::copy(&mut dump_reader, &mut temp_dump_file)?;

    let strategy = match subset_config.strategy {
        DatabaseSubsetConfigStrategy::Random(opt) => SubsetStrategy::RandomPercent {
            database: subset_config.database.as_str(),
            table: subset_config.table.as_str(),
            percent: opt.percent,
        },
    };

    let empty_vec = Vec::new();
    let passthrough_tables = subset_config
        .passthrough_tables
        .as_ref()
        .unwrap_or(&empty_vec)
        .iter()
        .map(|table| PassthroughTable::new(subset_config.database.as_str(), table.as_str()))
        .collect::<HashSet<_>>();

    let subset_options = SubsetOptions::new(&passthrough_tables);
    let subset = PostgresSubset::new(named_temp_file.path(), strategy, subset_options)?;

    let named_subset_file = tempfile::NamedTempFile::new()?;
    let mut subset_file = named_subset_file.as_file();

    let _ = subset.read(
        |row| {
            match subset_file.write(format!("{}\n", row).as_bytes()) {
                Ok(_) => {}
                Err(err) => {
                    panic!("{}", err)
                }
            };
        },
        |progress| {
            info!("Database subset completion: {}%", progress.percent());
        },
    )?;

    Ok(BufReader::new(
        File::open(named_subset_file.path()).unwrap(),
    ))
}

/// consume reader and apply transformation on INSERT INTO queries if needed
pub fn read_and_transform<R: Read, F: FnMut(OriginalQuery, Query)>(
    reader: BufReader<R>,
    options: SourceOptions,
    mut query_callback: F,
) {
    // create a map variable with Transformer by column_name
    let mut transformer_by_db_and_table_and_column_name: HashMap<String, &Box<dyn Transformer>> =
        HashMap::with_capacity(options.transformers.len());

    for transformer in options.transformers {
        let _ = transformer_by_db_and_table_and_column_name.insert(
            transformer.database_and_quoted_table_and_column_name(),
            transformer,
        );
    }

    let mut skip_tables_map: HashMap<String, bool> =
        HashMap::with_capacity(options.skip_config.len());
    for skip in options.skip_config {
        let _ = skip_tables_map.insert(format!("{}.{}", skip.database, skip.table), true);
    }

    match list_sql_queries_from_dump_reader(reader, |query| {
        let tokens = get_tokens_from_query_str(query);

        match get_row_type(&tokens) {
            RowType::InsertInto {
                database_name,
                table_name,
            } => {
                if !skip_tables_map.contains_key(&format!("{}.{}", database_name, table_name)) {
                    let (original_columns, columns) = transform_columns(
                        database_name.as_str(),
                        table_name.as_str(),
                        &tokens,
                        &transformer_by_db_and_table_and_column_name,
                    );

                    query_callback(
                        to_query(
                            Some(database_name.as_str()),
                            InsertIntoQuery {
                                table_name: table_name.to_string(),
                                columns: original_columns,
                            },
                        ),
                        to_query(
                            Some(database_name.as_str()),
                            InsertIntoQuery {
                                table_name: table_name.to_string(),
                                columns,
                            },
                        ),
                    )
                }
            }
            RowType::CreateTable {
                database_name,
                table_name,
            } => {
                if !skip_tables_map.contains_key(&format!("{}.{}", database_name, table_name)) {
                    no_change_query_callback(query_callback.borrow_mut(), query);
                }
            }
            RowType::AlterTable {
                database_name,
                table_name,
            } => {
                if !skip_tables_map.contains_key(&format!("{}.{}", database_name, table_name)) {
                    no_change_query_callback(query_callback.borrow_mut(), query);
                }
            }
            RowType::Others => {
                // other rows than `INSERT INTO ...` and `CREATE TABLE ...`
                no_change_query_callback(query_callback.borrow_mut(), query);
            }
        }

        ListQueryResult::Continue
    }) {
        Ok(_) => {}
        Err(err) => panic!("{:?}", err),
    }
}

pub fn read_and_parse_schema<R: Read>(reader: BufReader<R>) -> Result<(), Error> {
    match list_sql_queries_from_dump_reader(reader, |query| {
        let tokens = get_tokens_from_query_str(query.clone());
        match get_row_type(&tokens) {
            RowType::CreateTable {
                database_name,
                table_name,
            } => {
                let column_schema = get_column_names_from_create_query(&tokens);

                let mut table = table();
                table.set_titles(row!["Field"]);

                column_schema.iter().for_each(|column_name| {
                    table.add_row(row![column_name]);
                });

                println!(" Table {}", table_name);

                let _ = table.printstd();

                println!();
            }
            _ => {}
        }

        ListQueryResult::Continue
    }) {
        Ok(_) => Ok(()),
        Err(err) => panic!("{:?}", err),
    }
}

fn no_change_query_callback<F: FnMut(OriginalQuery, Query)>(query_callback: &mut F, query: &str) {
    query_callback(
        // there is no diff between the original and the modified one
        Query(query.as_bytes().to_vec()),
        Query(query.as_bytes().to_vec()),
    );
}

fn transform_columns(
    database_name: &str,
    table_name: &str,
    tokens: &Vec<Token>,
    transformer_by_db_and_table_and_column_name: &HashMap<String, &Box<dyn Transformer>>,
) -> (Vec<Column>, Vec<Column>) {
    // find database name by filtering out all queries starting with
    // INSERT INTO <database>.<table> (...)
    // INSERT       -> position 0
    // INTO         -> position 2
    // <table>      -> position 6
    // L Paren      -> position X?
    // R Paren      -> position X?
    let column_names = get_column_names_from_insert_into_query(&tokens);
    let column_values = get_column_values_from_insert_into_query(&tokens);
    assert_eq!(
        column_names.len(),
        column_values.len(),
        "Column names do not match values: got {} names and {} values",
        column_names.len(),
        column_values.len()
    );

    let mut original_columns = vec![];
    let mut columns = vec![];

    for (i, column_name) in column_names.iter().enumerate() {
        let value_token = column_values.get(i).unwrap();

        let column = match value_token {
            Token::Number(column_value, _) => {
                if column_value.contains(".") {
                    Column::FloatNumberValue(
                        column_name.to_string(),
                        column_value.parse::<f64>().unwrap(),
                    )
                } else {
                    Column::NumberValue(
                        column_name.to_string(),
                        column_value.parse::<i128>().unwrap(),
                    )
                }
            }
            Token::Char(column_value) => {
                Column::CharValue(column_name.to_string(), column_value.clone())
            }
            Token::SingleQuotedString(column_value) => {
                Column::StringValue(column_name.to_string(), column_value.clone())
            }
            Token::NationalStringLiteral(column_value) => {
                Column::StringValue(column_name.to_string(), column_value.clone())
            }
            Token::HexStringLiteral(column_value) => {
                Column::StringValue(column_name.to_string(), column_value.clone())
            }
            Token::Word(w)
                if (w.value == "true" || w.value == "false")
                    && w.quote_style == None
                    && w.keyword == NoKeyword =>
            {
                Column::BooleanValue(column_name.to_string(), w.value.parse::<bool>().unwrap())
            }
            _ => Column::None(column_name.to_string()),
        };

        // get the right transformer for the right column name
        let original_column = column.clone();

        let db_and_table_and_column_name =
            format!("{}.{}.{}", database_name, table_name, *column_name);
        let column = match transformer_by_db_and_table_and_column_name
            .get(db_and_table_and_column_name.as_str())
        {
            Some(transformer) => transformer.transform(column), // apply transformation on the column
            None => column,
        };

        original_columns.push(original_column);
        columns.push(column);
    }

    (original_columns, columns)
}

fn is_insert_into_statement(tokens: &Vec<Token>) -> bool {
    match_keyword_at_position(Keyword::Insert, &tokens, 0)
        && match_keyword_at_position(Keyword::Into, &tokens, 2)
}

fn is_create_table_statement(tokens: &Vec<Token>) -> bool {
    match_keyword_at_position(Keyword::Create, &tokens, 0)
        && match_keyword_at_position(Keyword::Table, &tokens, 2)
}

fn is_alter_table_statement(tokens: &Vec<Token>) -> bool {
    match_keyword_at_position(Keyword::Alter, &tokens, 0)
        && match_keyword_at_position(Keyword::Table, &tokens, 2)
}

fn get_row_type(tokens: &Vec<Token>) -> RowType {
    let mut row_type = RowType::Others;

    if is_insert_into_statement(&tokens) {
        if let Some(database_name) = get_word_value_at_position(&tokens, 4) {
            if let Some(table_name) = get_word_value_at_position(&tokens, 6) {
                row_type = RowType::InsertInto {
                    database_name: database_name.to_string(),
                    table_name: table_name.to_string(),
                };
            }
        }
    }

    if is_create_table_statement(&tokens) {
        if let Some(database_name) = get_word_value_at_position(&tokens, 4) {
            if let Some(table_name) = get_word_value_at_position(&tokens, 6) {
                row_type = RowType::CreateTable {
                    database_name: database_name.to_string(),
                    table_name: table_name.to_string(),
                };
            }
        }
    }

    if is_alter_table_statement(&tokens) {
        let database_name_pos = if match_keyword_at_position(Keyword::Only, &tokens, 4) {
            6
        } else {
            4
        };

        let table_name_pos = if match_keyword_at_position(Keyword::Only, &tokens, 4) {
            8
        } else {
            6
        };

        if let Some(database_name) = get_word_value_at_position(&tokens, database_name_pos) {
            if let Some(table_name) = get_word_value_at_position(&tokens, table_name_pos) {
                row_type = RowType::AlterTable {
                    database_name: database_name.to_string(),
                    table_name: table_name.to_string(),
                };
            }
        }
    }

    row_type
}

fn to_query(database: Option<&str>, query: InsertIntoQuery) -> Query {
    let mut column_names = Vec::with_capacity(query.columns.len());
    let mut values = Vec::with_capacity(query.columns.len());

    for column in query.columns {
        match column {
            Column::NumberValue(column_name, value) => {
                column_names.push(column_name);
                values.push(value.to_string());
            }
            Column::FloatNumberValue(column_name, value) => {
                column_names.push(column_name);
                values.push(value.to_string());
            }
            Column::StringValue(column_name, value) => {
                column_names.push(column_name);
                values.push(format!("'{}'", value.replace("'", "''")));
            }
            Column::CharValue(column_name, value) => {
                column_names.push(column_name);
                values.push(format!("'{}'", value));
            }
            Column::BooleanValue(column_name, value) => {
                column_names.push(column_name);
                values.push(value.to_string());
            }
            Column::None(column_name) => {
                column_names.push(column_name);
                values.push("NULL".to_string());
            }
        }
    }

    let query_prefix = match database {
        Some(database) => format!("INSERT INTO {}.", database),
        None => "INSERT INTO ".to_string(),
    };

    let query_string = format!(
        "{}{} ({}) VALUES ({});",
        query_prefix,
        query.table_name.as_str(),
        column_names.join(", "),
        values.join(", "),
    );

    Query(query_string.into_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::str;
    use std::vec;

    use crate::config::{
        DatabaseSubsetConfig, DatabaseSubsetConfigStrategy, DatabaseSubsetConfigStrategyRandom,
        SkipConfig,
    };
    use crate::source::postgres::{to_query, Postgres};
    use crate::source::SourceOptions;
    use crate::transformer::random::RandomTransformer;
    use crate::transformer::transient::TransientTransformer;
    use crate::transformer::Transformer;
    use crate::types::{Column, InsertIntoQuery};
    use crate::Source;

    fn get_postgres() -> Postgres<'static> {
        Postgres::new("localhost", 5432, "root", "root", "password")
    }

    fn get_invalid_postgres() -> Postgres<'static> {
        Postgres::new("localhost", 5432, "root", "root", "wrongpassword")
    }

    #[test]
    fn connect() {
        let p = get_postgres();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };

        assert!(p.read(source_options, |original_query, query| {}).is_ok());

        let p = get_invalid_postgres();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };

        assert!(p.read(source_options, |original_query, query| {}).is_err());
    }

    #[test]
    fn list_rows() {
        let p = get_postgres();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };

        let _ = p.read(source_options, |original_query, query| {
            assert!(original_query.data().len() > 0);
            assert!(query.data().len() > 0);
        });
    }

    #[test]
    fn test_to_row() {
        let query = to_query(
            None,
            InsertIntoQuery {
                table_name: "test".to_string(),
                columns: vec![Column::StringValue(
                    "first_name".to_string(),
                    "romaric".to_string(),
                )],
            },
        );

        assert_eq!(
            query.data(),
            b"INSERT INTO test (first_name) VALUES ('romaric');"
        );

        let query = to_query(
            None,
            InsertIntoQuery {
                table_name: "test".to_string(),
                columns: vec![Column::StringValue(
                    r#""firstName""#.to_string(),
                    "romaric".to_string(),
                )],
            },
        );
        assert_eq!(
            query.data(),
            b"INSERT INTO test (\"firstName\") VALUES ('romaric');"
        );

        let query = to_query(
            None,
            InsertIntoQuery {
                table_name: "test".to_string(),
                columns: vec![Column::BooleanValue("is_valid".to_string(), true)],
            },
        );

        assert_eq!(query.data(), b"INSERT INTO test (is_valid) VALUES (true);");

        let query = to_query(
            Some("public"),
            InsertIntoQuery {
                table_name: "test".to_string(),
                columns: vec![
                    Column::StringValue("first_name".to_string(), "romaric".to_string()),
                    Column::FloatNumberValue("height_in_meters".to_string(), 1.78),
                ],
            },
        );

        assert_eq!(
            query.data(),
            b"INSERT INTO public.test (first_name, height_in_meters) VALUES ('romaric', 1.78);"
        );

        let query = to_query(
            Some("public"),
            InsertIntoQuery {
                table_name: "test".to_string(),
                columns: vec![
                    Column::None("first_name".to_string()),
                    Column::FloatNumberValue("height_in_meters".to_string(), 1.78),
                ],
            },
        );

        assert_eq!(
            query.data(),
            b"INSERT INTO public.test (first_name, height_in_meters) VALUES (NULL, 1.78);"
        );

        let query = to_query(
            Some("public"),
            InsertIntoQuery {
                table_name: "test".to_string(),
                columns: vec![
                    Column::StringValue("first_name".to_string(), "romaric".to_string()),
                    Column::FloatNumberValue("height_in_meters".to_string(), 1.78),
                    Column::StringValue(
                        "description".to_string(),
                        "I'd like to say... I don't know.".to_string(),
                    ),
                ],
            },
        );

        assert_eq!(
            query.data(),
            b"INSERT INTO public.test (first_name, height_in_meters, description) \
            VALUES ('romaric', 1.78, 'I''d like to say... I don''t know.');"
        );
    }

    #[test]
    fn list_rows_and_hide_last_name() {
        let p = get_postgres();

        let database_name = "public";
        let table_name = "employees";
        let column_name_to_obfuscate = "last_name";

        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let t2: Box<dyn Transformer> = Box::new(RandomTransformer::new(
            database_name,
            table_name,
            column_name_to_obfuscate,
        ));

        let transformers = vec![t1, t2];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };

        let _ = p.read(source_options, |original_query, query| {
            assert!(query.data().len() > 0);
            assert!(query.data().len() > 0);

            let query_str = str::from_utf8(query.data()).unwrap();

            if query_str.contains(database_name)
                && query_str.contains(table_name)
                && query_str.starts_with("INSERT INTO")
            {
                assert_ne!(query.data(), original_query.data());
                // TODO to complete to better check the column change only
            } else {
                assert_eq!(query.data(), original_query.data());
            }
        });
    }

    #[test]
    fn skip_table() {
        let p = get_postgres();

        let database_name = "public";
        let table_name = "employees";

        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let skip_employees_table = SkipConfig {
            database: database_name.to_string(),
            table: table_name.to_string(),
        };

        let transformers = vec![t1];
        let skip_config = vec![skip_employees_table];

        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &skip_config,
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };

        let _ = p.read(source_options, |_original_query, query| {
            assert!(query.data().len() > 0);
            assert!(query.data().len() > 0);

            let query_str = str::from_utf8(query.data()).unwrap();
            let unexpected_insert_into = format!("INSERT INTO {}.{}", database_name, table_name);
            let unexpected_create_table = format!("CREATE TABLE {}.{}", database_name, table_name);
            let unexpected_alter_table = format!("ALTER TABLE {}.{}", database_name, table_name);
            let unexpected_alter_table_only =
                format!("ALTER TABLE ONLY {}.{}", database_name, table_name);

            if query_str.contains(unexpected_insert_into.as_str()) {
                panic!("unexpected insert into: {}", unexpected_insert_into);
            }

            if query_str.contains(unexpected_create_table.as_str()) {
                panic!("unexpected create table: {}", unexpected_create_table);
            }

            if query_str.contains(unexpected_alter_table.as_str()) {
                panic!("unexpected alter table: {}", unexpected_alter_table);
            }

            if query_str.contains(unexpected_alter_table_only.as_str()) {
                panic!(
                    "unexpected alter table only: {}",
                    unexpected_alter_table_only
                );
            }
        });
    }

    #[test]
    fn subset_options() {
        let p = get_postgres();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());

        let source_options = SourceOptions {
            transformers: &vec![t1],
            skip_config: &vec![],
            database_subset: &Some(DatabaseSubsetConfig {
                database: "public".to_string(),
                table: "orders".to_string(),
                strategy: DatabaseSubsetConfigStrategy::Random(
                    DatabaseSubsetConfigStrategyRandom { percent: 50 },
                ),
                passthrough_tables: None,
            }),
            only_tables: &vec![],
            chunk_size: &None,
        };

        let mut rows_percent_50 = vec![];
        let _ = p.read(source_options, |_original_query, query| {
            assert!(query.data().len() > 0);
            rows_percent_50.push(String::from_utf8_lossy(query.data().as_slice()).to_string());
        });

        let x = rows_percent_50
            .iter()
            .filter(|x| x.contains("INSERT INTO"))
            .map(|x| x.as_str())
            .collect::<HashSet<_>>();

        let y = rows_percent_50
            .iter()
            .filter(|x| x.contains("INSERT INTO"))
            .map(|x| x.as_str())
            .collect::<Vec<_>>();

        // check that there is no duplicated rows
        assert_eq!(x.len(), y.len());

        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());

        let source_options = SourceOptions {
            transformers: &vec![t1],
            skip_config: &vec![],
            database_subset: &Some(DatabaseSubsetConfig {
                database: "public".to_string(),
                table: "orders".to_string(),
                strategy: DatabaseSubsetConfigStrategy::Random(
                    DatabaseSubsetConfigStrategyRandom { percent: 30 },
                ),
                passthrough_tables: None,
            }),
            only_tables: &vec![],
            chunk_size: &None,
        };

        let mut rows_percent_30 = vec![];
        let _ = p.read(source_options, |_original_query, query| {
            assert!(query.data().len() > 0);
            rows_percent_30.push(String::from_utf8_lossy(query.data().as_slice()).to_string());
        });

        // check that there is no duplicated rows
        assert_eq!(
            rows_percent_30
                .iter()
                .filter(|x| x.contains("INSERT INTO"))
                .collect::<HashSet<_>>()
                .len(),
            rows_percent_30
                .iter()
                .filter(|x| x.contains("INSERT INTO"))
                .collect::<Vec<_>>()
                .len(),
        );

        assert!(rows_percent_30.len() < rows_percent_50.len());
    }
}
