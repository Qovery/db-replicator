use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::io::{BufReader, Error, ErrorKind, Read};
use std::process::{Command, Stdio};

use dump_parser::mysql::Keyword::NoKeyword;
use dump_parser::mysql::{
    get_column_names_from_insert_into_query, get_column_names_from_create_query,
    get_column_values_from_insert_into_query, get_single_quoted_string_value_at_position, 
    get_tokens_from_query_str, match_keyword_at_position, Keyword, Token,
};
use dump_parser::utils::{list_sql_queries_from_dump_reader, ListQueryResult};

use crate::connector::Connector;
use crate::source::{Explain, Source};
use crate::transformer::Transformer;
use crate::types::{Column, InsertIntoQuery, OriginalQuery, Query};
use crate::utils::{binary_exists, table, wait_for_command};

use super::SourceOptions;

#[derive(Debug, PartialEq)]
enum RowType {
    InsertInto { table_name: String },
    CreateTable { table_name: String },
    Others,
}

pub struct Mysql<'a> {
    host: &'a str,
    port: u16,
    database: &'a str,
    username: &'a str,
    password: &'a str,
}

impl<'a> Mysql<'a> {
    pub fn new(
        host: &'a str,
        port: u16,
        database: &'a str,
        username: &'a str,
        password: &'a str,
    ) -> Self {
        Self {
            host,
            port,
            database,
            username,
            password,
        }
    }
}

impl<'a> Connector for Mysql<'a> {
    fn init(&mut self) -> Result<(), Error> {
        let _ = binary_exists("mysqldump")?;

        Ok(())
    }
}

impl<'a> Explain for Mysql<'a> {
    fn schema(&self) -> Result<(), Error> {
        let s_port = self.port.to_string();
        let password = &format!("-p{}", self.password);

        let dump_args = vec![
            "-h",
            self.host,
            "-P",
            s_port.as_str(),
            "-u",
            self.username,
            password,
            "--no-data", // do not write any table row information
            "--quick", // reads out large tables in a way that doesn't require having enough RAM to fit the full table in memory
            "--hex-blob",
            self.database,
        ];

        let mut process = Command::new("mysqldump")
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

impl<'a> Source for Mysql<'a> {
    fn read<F: FnMut(OriginalQuery, Query)>(
        &self,
        options: SourceOptions,
        query_callback: F,
    ) -> Result<(), Error> {
        let s_port = self.port.to_string();
        let password = &format!("-p{}", self.password);

        let mut dump_args = vec![
            "-h",
            self.host,
            "-P",
            s_port.as_str(),
            "-u",
            self.username,
            password,
            "--add-drop-database", // add DROP DATABASE statement before each CREATE DATABASE statement
            "--add-drop-table",    // add DROP TABLE statement before each CREATE TABLE statement
            "--skip-extended-insert", // have a row by INSERT INTO statement
            "--complete-insert",   // have column names in INSERT INTO rows
            "--single-transaction", // https://dev.mysql.com/doc/refman/8.0/en/mysqldump.html#option_mysqldump_single-transaction
            "--quick", // reads out large tables in a way that doesn't require having enough RAM to fit the full table in memory
            "--hex-blob",
            self.database,
        ];

        let ignore_tables_args: Vec<String> = options
            .skip_config
            .iter()
            .map(|cfg| format!("--ignore-table={}.{}", cfg.database, cfg.table))
            .collect();
        let mut ignore_tables_args: Vec<&str> =
            ignore_tables_args.iter().map(String::as_str).collect();

        dump_args.append(&mut ignore_tables_args);

        let mut only_tables_args: Vec<&str> = options
            .only_tables
            .iter()
            .map(|cfg| String::as_str(&cfg.table))
            .collect();

        dump_args.append(&mut only_tables_args);

        let mut process = Command::new("mysqldump")
            .args(dump_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Other, "Could not capture standard output."))?;

        let reader = BufReader::new(stdout);

        read_and_transform(reader, options, query_callback);

        wait_for_command(&mut process)
    }
}

pub fn read_and_transform<R: Read, F: FnMut(OriginalQuery, Query)>(
    reader: BufReader<R>,
    options: SourceOptions,
    mut query_callback: F,
) {
    // create a map variable with Transformer by column_name
    let mut transformer_by_db_and_table_and_column_name: HashMap<String, &Box<dyn Transformer>> =
        HashMap::with_capacity(options.transformers.len());

    for transformer in options.transformers {
        let _ = transformer_by_db_and_table_and_column_name
            .insert(transformer.table_and_column_name(), transformer);
    }

    match list_sql_queries_from_dump_reader(reader, |query| {
        let tokens = get_tokens_from_query_str(query);

        match get_row_type(&tokens) {
            RowType::InsertInto { table_name } => {
                let (original_columns, columns) = transform_columns(
                    table_name.as_str(),
                    &tokens,
                    &transformer_by_db_and_table_and_column_name,
                );

                query_callback(
                    to_query(
                        None,
                        InsertIntoQuery {
                            table_name: table_name.to_string(),
                            columns: original_columns,
                        },
                    ),
                    to_query(
                        None,
                        InsertIntoQuery {
                            table_name: table_name.to_string(),
                            columns,
                        },
                    ),
                )
            }
            RowType::CreateTable { table_name: _ } => {
                no_change_query_callback(query_callback.borrow_mut(), query);
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
            RowType::CreateTable { table_name } => {
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
    table_name: &str,
    tokens: &Vec<Token>,
    transformer_by_db_and_table_and_column_name: &HashMap<String, &Box<dyn Transformer>>,
) -> (Vec<Column>, Vec<Column>) {
    // find database name by filtering out all queries starting with
    // INSERT INTO `<table>` (...)
    // INSERT       -> position 0
    // INTO         -> position 2
    // <table>      -> position 4
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

        let table_and_column_name = format!("{}.{}", table_name, *column_name);

        let column =
            match transformer_by_db_and_table_and_column_name.get(table_and_column_name.as_str()) {
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

fn get_row_type(tokens: &Vec<Token>) -> RowType {
    let mut row_type = RowType::Others;

    if is_insert_into_statement(&tokens) {
        if let Some(table_name) = get_single_quoted_string_value_at_position(&tokens, 4) {
            row_type = RowType::InsertInto {
                table_name: table_name.to_string(),
            };
        }
    }

    if is_create_table_statement(&tokens) {
        if let Some(table_name) = get_single_quoted_string_value_at_position(&tokens, 4) {
            row_type = RowType::CreateTable {
                table_name: table_name.to_string(),
            };
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
                values.push(format!("'{}'", value));
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
        Some(_database) => panic!("database should not be present in a MySQL dump"),
        None => "INSERT INTO".to_string(),
    };

    let query_string = format!(
        "{} `{}` ({}) VALUES ({});",
        query_prefix,
        query.table_name.as_str(),
        column_names
            .iter()
            .map(|column_name| format!("`{}`", column_name))
            .collect::<Vec<String>>()
            .join(", "),
        values.join(", "),
    );

    Query(query_string.into_bytes())
}

#[cfg(test)]
mod tests {
    use crate::connector::Connector;
    use crate::source::mysql::{is_create_table_statement, is_insert_into_statement, RowType};
    use crate::source::SourceOptions;
    use crate::transformer::{transient::TransientTransformer, Transformer};
    use crate::Source;
    use dump_parser::mysql::Tokenizer;

    use super::{get_row_type, Mysql};

    fn get_mysql() -> Mysql<'static> {
        Mysql::new("127.0.0.1", 3306, "world", "root", "password")
    }

    fn get_invalid_mysql() -> Mysql<'static> {
        Mysql::new("127.0.0.1", 3306, "world", "root", "wrong_password")
    }

    #[test]
    fn connect() {
        let mut p = get_mysql();
        assert!(p.init().is_ok());

        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };

        assert!(p.read(source_options, |_original_query, _query| {}).is_ok());

        let p = get_invalid_mysql();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            only_tables: &vec![],
            chunk_size: &None,
        };
        assert!(p
            .read(source_options, |_original_query, _query| {})
            .is_err());
    }

    #[test]
    fn list_rows() {
        let p = get_mysql();
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
    fn test_is_insert_into_statement() {
        let q = "INSERT INTO `customers` (`first_name`, `is_valid`) VALUES ('Romaric', true);";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();
        assert_eq!(is_insert_into_statement(&tokens), true);

        let q = "CREATE TABLE `city` (
    `ID` int NOT NULL AUTO_INCREMENT,
    `Name` char(35) NOT NULL DEFAULT '',
    `CountryCode` char(3) NOT NULL DEFAULT '',
    `District` char(20) NOT NULL DEFAULT '',
    `Population` int NOT NULL DEFAULT '0',
    PRIMARY KEY (`ID`),
    KEY `CountryCode` (`CountryCode`),
CONSTRAINT `city_ibfk_1` FOREIGN KEY (`CountryCode`) REFERENCES `country` (`Code`)
) ENGINE=InnoDB AUTO_INCREMENT=4080 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();
        assert_eq!(is_insert_into_statement(&tokens), false);
    }

    #[test]
    fn test_is_create_table_statement() {
        let q = "CREATE TABLE `city` (
    `ID` int NOT NULL AUTO_INCREMENT,
    `Name` char(35) NOT NULL DEFAULT '',
    `CountryCode` char(3) NOT NULL DEFAULT '',
    `District` char(20) NOT NULL DEFAULT '',
    `Population` int NOT NULL DEFAULT '0',
    PRIMARY KEY (`ID`),
    KEY `CountryCode` (`CountryCode`),
CONSTRAINT `city_ibfk_1` FOREIGN KEY (`CountryCode`) REFERENCES `country` (`Code`)
) ENGINE=InnoDB AUTO_INCREMENT=4080 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();
        assert_eq!(is_create_table_statement(&tokens), true);

        let q = "INSERT INTO `customers` (`first_name`, `is_valid`) VALUES ('Romaric', true);";
        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();
        assert_eq!(is_create_table_statement(&tokens), false);
    }

    #[test]
    fn test_get_row_type() {
        let q = "INSERT INTO `customers` (`first_name`, `is_valid`) VALUES ('Romaric', true);";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();

        let expected_row_type = RowType::InsertInto {
            table_name: "customers".to_string(),
        };
        assert_eq!(get_row_type(&tokens), expected_row_type);

        let q = "CREATE TABLE `city` (
    `ID` int NOT NULL AUTO_INCREMENT,
    `Name` char(35) NOT NULL DEFAULT '',
    `CountryCode` char(3) NOT NULL DEFAULT '',
    `District` char(20) NOT NULL DEFAULT '',
    `Population` int NOT NULL DEFAULT '0',
    PRIMARY KEY (`ID`),
    KEY `CountryCode` (`CountryCode`),
CONSTRAINT `city_ibfk_1` FOREIGN KEY (`CountryCode`) REFERENCES `country` (`Code`)
) ENGINE=InnoDB AUTO_INCREMENT=4080 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();

        let expected_row_type = RowType::CreateTable {
            table_name: "city".to_string(),
        };
        assert_eq!(get_row_type(&tokens), expected_row_type);
    }

    #[test]
    fn test_create_table_without_comma_at_the_end_of_the_last_property() {
        let q = "CREATE TABLE `test` (
 `id` int(10) unsigned NOT NULL AUTO_INCREMENT,
 `withDefault` tinyint(1) NOT NULL DEFAULT '0',
) ENGINE=InnoDB DEFAULT CHARSET=latin1;";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();
        assert_eq!(is_create_table_statement(&tokens), true);

        let q = "CREATE TABLE `test` (
 `id` int(10) unsigned NOT NULL AUTO_INCREMENT,
 `withDefault` tinyint(1) NOT NULL DEFAULT '0'
) ENGINE=InnoDB DEFAULT CHARSET=latin1;";

        let mut tokenizer = Tokenizer::new(q);
        let tokens = tokenizer.tokenize().unwrap();
        assert_eq!(is_create_table_statement(&tokens), true);
    }
}
