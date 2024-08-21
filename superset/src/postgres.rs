use crate::dedup::does_line_exist_and_set;
use crate::postgres::SupersetStrategy::RandomPercent;
use crate::{
    utils, PassthroughTableSuperSet, Progress, Superset, SupersetOptions, SupersetTable, SupersetTableRelation,
};
use dump_parser::postgres::{
    get_column_names_from_insert_into_query, get_column_values_str_from_insert_into_query,
    get_tokens_from_query_str, get_word_value_at_position, match_keyword_at_position,
    trim_pre_whitespaces, Keyword, Token,
};
use dump_parser::utils::{list_sql_queries_from_dump_reader, ListQueryResult};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Error, ErrorKind, Read};
use std::ops::Index;
use std::path::Path;

type Database = String;
type Table = String;

#[derive(Debug)]
struct ForeignKey {
    from_database: String,
    from_table: String,
    from_property: String,
    to_database: String,
    to_table: String,
    to_property: String,
}

struct TableStats {
    database: String,
    table: String,
    columns: Vec<String>,
    total_rows: usize,
    first_insert_into_row_index: usize,
    last_insert_into_row_index: usize,
}

pub enum SupersetStrategy<'a> {
    RandomPercent {
        database: &'a str,
        table: &'a str,
        percent: u8,
    },
}

impl<'a> SupersetStrategy<'a> {
    pub fn random(database: &'a str, table: &'a str, percent: u8) -> Self {
        RandomPercent {
            database,
            table,
            percent,
        }
    }
}

pub struct PostgresSuperset<'a> {
    superset_table_by_database_and_table_name: HashMap<(Database, Table), SupersetTable>,
    dump: &'a Path,
    superset_strategy: SupersetStrategy<'a>,
    superset_options: SupersetOptions<'a>,
}

impl<'a> PostgresSuperset<'a> {
    pub fn new(
        dump: &'a Path,
        superset_strategy: SupersetStrategy<'a>,
        superset_options: SupersetOptions<'a>,
    ) -> Result<Self, Error> {
        Ok(PostgresSuperset {
            superset_table_by_database_and_table_name: get_superset_table_by_database_and_table_name(
                BufReader::new(File::open(dump).unwrap()),
            )?,
            dump,
            superset_strategy,
            superset_options,
        })
    }

    fn dump_reader(&self) -> BufReader<File> {
        BufReader::new(File::open(self.dump).unwrap())
    }

    fn reference_rows(
        &self,
        table_stats: &HashMap<(Database, Table), TableStats>,
    ) -> Result<Vec<String>, Error> {
        match self.superset_strategy {
            SupersetStrategy::RandomPercent {
                database,
                table,
                percent,
            } => Ok(list_percent_of_insert_into_rows(
                percent,
                table_stats
                    .get(&(database.to_string(), table.to_string()))
                    .unwrap(),
                self.dump_reader(),
            )?),
        }
    }

    fn visits<F: FnMut(String)>(
        &self,
        row: String,
        table_stats: &HashMap<(Database, Table), TableStats>,
        data: &mut F,
    ) -> Result<(), Error> {
        data(format!("{}\n", row));

        // tokenize `INSERT INTO ...` row
        let row_tokens = get_tokens_from_query_str(row.as_str());

        // find the database and table names from this row
        let (row_database, row_table) =
            get_insert_into_database_and_table_name(&row_tokens).unwrap();

        if self.superset_options.passthrough_tables.is_empty()
            || !self
                .superset_options
                .passthrough_tables
                .contains(&PassthroughTableSuperSet::new(
                    row_database.as_str(),
                    row_table.as_str(),
                ))
        {
            // only insert if the row is not from passthrough tables list
            // otherwise we'll have duplicated rows
            data(format!("{}\n", row));
        }

        // find the superset table from this row
        let row_superset_table = self
            .superset_table_by_database_and_table_name
            .get(&(row_database.to_string(), row_table.to_string()))
            .unwrap();

        let row_column_names = get_column_names_from_insert_into_query(&row_tokens);
        let row_column_values = get_column_values_str_from_insert_into_query(&row_tokens);

        for row_relation in &row_superset_table.relations {
            let column = row_relation.from_property.as_str();
            // find the value from the current row for the relation column
            let column_idx = row_column_names.iter().position(|x| *x == column).unwrap(); // FIXME unwrap
            let value = row_column_values.get(column_idx).unwrap();

            let database_and_table_tuple =
                (row_relation.database.clone(), row_relation.table.clone());

            // find the table stats for this row
            let row_relation_table_stats = table_stats.get(&database_and_table_tuple).unwrap();

            // TODO break acyclic graph
            let row_clb = |row: &str| match self.visits(row.to_string(), table_stats, data) {
                Ok(_) => {}
                Err(err) => {
                    panic!("{}", err);
                }
            };

            let _ = filter_insert_into_rows(
                row_relation.to_property.as_str(),
                value.as_str(),
                self.dump_reader(),
                row_relation_table_stats,
                row_clb,
            )?;
        }

        Ok(())
    }
}

impl<'a> Superset for PostgresSuperset<'a> {
    /// Return every superset rows
    /// Algorithm used:
    /// 1. find the reference table and take the X rows from this table with the appropriate SupersetStrategy
    /// 2. iterate over each row and their relations (0 to N relations)
    /// 3. for each rows from each relations, filter on the id from the parent related row id. (equivalent `SELECT * FROM table_1 INNER JOIN ... WHERE table_1.id = 'xxx';`
    /// 4. do it recursively for table_1.relations[*].relations[*]... but the algo stops when reaching the end or reach a cyclic ref.
    ///
    /// Notes:
    /// a. the algo must visits all the tables, even the one that has no relations.
    fn read<F: FnMut(String), P: FnMut(Progress)>(
        &self,
        mut data: F,
        mut progress: P,
    ) -> Result<(), Error> {
        let temp_dir = tempfile::tempdir()?;

        let _ = read(
            self,
            |line| {
                if line.contains("INSERT INTO") {
                    // Dedup INSERT INTO queries
                    // check if the line has not already been sent
                    match does_line_exist_and_set(
                        temp_dir.path(),
                        &get_insert_into_md5_hash(line.as_str()),
                        line.as_str(),
                    ) {
                        Ok(does_line_exist) => {
                            if !does_line_exist {
                                data(line);
                            }
                        }
                        Err(err) => {
                            panic!("{}", err);
                        }
                    }
                } else {
                    data(line);
                }
            },
            progress,
        )?;

        Ok(())
    }
}

fn read<F: FnMut(String), P: FnMut(Progress)>(
    postgres_superset: &PostgresSuperset,
    mut data: F,
    mut progress: P,
) -> Result<(), Error> {
    let table_stats = table_stats_by_database_and_table_name(postgres_superset.dump_reader())?;
    let rows = postgres_superset.reference_rows(&table_stats)?;

    // send schema header
    let table_stats_values = table_stats.values().collect::<Vec<_>>();
    let _ = dump_header(
        postgres_superset.dump_reader(),
        last_header_row_idx(&table_stats_values),
        |row| {
            data(row.to_string());
        },
    )?;

    let total_rows = table_stats_values
        .iter()
        .fold(0usize, |acc, y| acc + y.total_rows);

    let total_rows_to_process = rows.len();
    let mut processed_rows = 0usize;

    progress(Progress {
        total_rows,
        total_rows_to_process,
        processed_rows,
        last_process_time: 0,
    });

    // send INSERT INTO rows
    for row in rows {
        let start_time = utils::epoch_millis();
        let _ = postgres_superset.visits(row, &table_stats, &mut data)?;

        processed_rows += 1;

        progress(Progress {
            total_rows,
            total_rows_to_process,
            processed_rows,
            last_process_time: utils::epoch_millis() - start_time,
        });
    }

    for passthrough_table in postgres_superset.superset_options.passthrough_tables {
        // copy all rows from passthrough tables
        for table_stats in &table_stats_values {
            if table_stats.database.as_str() == passthrough_table.database
                && table_stats.table.as_str() == passthrough_table.table
            {
                let _ = list_insert_into_rows(postgres_superset.dump_reader(), table_stats, |row| {
                    data(row.to_string());
                })?;
            }
        }
    }

    // send schema footer
    let _ = dump_footer(
        postgres_superset.dump_reader(),
        first_footer_row_idx(&table_stats_values),
        |row| {
            data(row.to_string());
        },
    )?;

    Ok(())
}

fn get_insert_into_md5_hash(query: &str) -> String {
    let tokens = get_tokens_from_query_str(query);
    let tokens = trim_pre_whitespaces(tokens);
    let database = get_word_value_at_position(&tokens, 4).unwrap();
    let table = get_word_value_at_position(&tokens, 6).unwrap();
    let key = format!("{}-{}", database, table);
    let digest = md5::compute(key.as_bytes());
    format!("{:x}", digest)
}

fn list_percent_of_insert_into_rows<R: Read>(
    percent: u8,
    table_stats: &TableStats,
    dump_reader: BufReader<R>,
) -> Result<Vec<String>, Error> {
    let mut insert_into_rows = vec![];

    if percent == 0 || table_stats.total_rows == 0 {
        return Ok(insert_into_rows);
    }

    let percent = if percent > 100 { 100 } else { percent };

    let total_rows_to_pick = table_stats.total_rows as f32 * percent as f32 / 100.0;
    let modulo = (table_stats.total_rows as f32 / total_rows_to_pick) as usize;

    let mut counter = 1usize;
    let _ = list_insert_into_rows(dump_reader, table_stats, |rows| {
        if counter % modulo == 0 {
            insert_into_rows.push(rows.to_string());
        }

        counter += 1;
    })?;

    Ok(insert_into_rows)
}

fn list_insert_into_rows<R: Read, F: FnMut(&str)>(
    dump_reader: BufReader<R>,
    table_stats: &TableStats,
    mut rows: F,
) -> Result<(), Error> {
    let mut query_idx = 0usize;
    let _ = list_sql_queries_from_dump_reader(dump_reader, |query| {
        let mut query_res = ListQueryResult::Continue;

        // optimization to avoid tokenizing unnecessary queries -- it's a 13x optim (benched)
        if query_idx >= table_stats.first_insert_into_row_index
            && query_idx <= table_stats.last_insert_into_row_index
        {
            let tokens = get_tokens_from_query_str(query);
            let tokens = trim_tokens(&tokens, Keyword::Insert);

            if match_keyword_at_position(Keyword::Insert, &tokens, 0)
                && match_keyword_at_position(Keyword::Into, &tokens, 2)
                && get_word_value_at_position(&tokens, 4) == Some(table_stats.database.as_str())
                && get_word_value_at_position(&tokens, 6) == Some(table_stats.table.as_str())
            {
                rows(query.as_ref());
            }
        }

        if query_idx > table_stats.last_insert_into_row_index {
            // early break to avoid parsing the dump while we have already parsed all the table rows
            query_res = ListQueryResult::Break;
        }

        query_idx += 1;
        query_res
    })?;

    Ok(())
}

fn filter_insert_into_rows<R: Read, F: FnMut(&str)>(
    column: &str,
    value: &str,
    dump_reader: BufReader<R>,
    table_stats: &TableStats,
    mut rows: F,
) -> Result<(), Error> {
    let column_idx = match table_stats
        .columns
        .iter()
        .position(|r| r.as_str() == column)
    {
        Some(idx) => idx,
        None => {
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "table {} does not contain column {}",
                    table_stats.table, column
                ),
            ));
        }
    };

    let mut query_idx = 0usize;
    let _ = list_sql_queries_from_dump_reader(dump_reader, |query| {
        let mut query_res = ListQueryResult::Continue;

        // optimization to avoid tokenizing unnecessary queries -- it's a 13x optim (benched)
        if query_idx >= table_stats.first_insert_into_row_index
            && query_idx <= table_stats.last_insert_into_row_index
        {
            let tokens = get_tokens_from_query_str(query);
            let tokens = trim_tokens(&tokens, Keyword::Insert);

            if match_keyword_at_position(Keyword::Insert, &tokens, 0)
                && match_keyword_at_position(Keyword::Into, &tokens, 2)
                && get_word_value_at_position(&tokens, 4) == Some(table_stats.database.as_str())
                && get_word_value_at_position(&tokens, 6) == Some(table_stats.table.as_str())
            {
                let column_values = get_column_values_str_from_insert_into_query(&tokens);

                if *column_values.index(column_idx) == value {
                    rows(query)
                }
            }
        }

        if query_idx > table_stats.last_insert_into_row_index {
            // early break to avoid parsing the dump while we have already parsed all the table rows
            query_res = ListQueryResult::Break;
        }

        query_idx += 1;
        query_res
    })?;

    Ok(())
}

/// return the last row index from dump header (with generated table stats)
fn last_header_row_idx(table_stats_values: &Vec<&TableStats>) -> usize {
    table_stats_values
        .iter()
        .filter(|ts| ts.first_insert_into_row_index > 0) // first_insert_into_row_index can be equals to 0 if there is no INSERT INTO...
        .min_by_key(|ts| ts.first_insert_into_row_index)
        .map(|ts| ts.first_insert_into_row_index)
        .unwrap()
        - 1 // FIXME catch this even if it should not happen
}

/// return the first row index from dump header (with generated table stats)
fn first_footer_row_idx(table_stats_values: &Vec<&TableStats>) -> usize {
    table_stats_values
        .iter()
        .max_by_key(|ts| ts.last_insert_into_row_index)
        .map(|ts| ts.last_insert_into_row_index)
        .unwrap()
        + 1 // FIXME catch this even if it should not happen
}

/// Get Postgres dump header - everything before the first `INSERT INTO ...` row
/// pg_dump export dump data in 3 phases: `CREATE TABLE ...`, `INSERT INTO ...`, and `ALTER TABLE ...`.
/// this function return all the `CREATE TABLE ...` rows.
fn dump_header<R: Read, F: FnMut(&str)>(
    dump_reader: BufReader<R>,
    last_header_row_idx: usize,
    mut rows: F,
) -> Result<(), Error> {
    let mut query_idx = 0usize;
    let _ = list_sql_queries_from_dump_reader(dump_reader, |query| {
        let mut query_res = ListQueryResult::Continue;

        if query_idx <= last_header_row_idx {
            rows(query)
        }

        if query_idx > last_header_row_idx {
            query_res = ListQueryResult::Break;
        }

        query_idx += 1;
        query_res
    })?;

    Ok(())
}

/// Get Postgres dump footer - everything after the last `INSERT INTO ...` row
/// pg_dump export dump data in 3 phases: `CREATE TABLE ...`, `INSERT INTO ...`, and `ALTER TABLE ...`.
/// this function return all the `ALTER TABLE ...` rows.
fn dump_footer<R: Read, F: FnMut(&str)>(
    dump_reader: BufReader<R>,
    first_footer_row_idx: usize,
    mut rows: F,
) -> Result<(), Error> {
    let mut query_idx = 0usize;
    let _ = list_sql_queries_from_dump_reader(dump_reader, |query| {
        if query_idx >= first_footer_row_idx {
            rows(query)
        }

        query_idx += 1;
        ListQueryResult::Continue
    })?;

    Ok(())
}

fn table_stats_by_database_and_table_name<R: Read>(
    dump_reader: BufReader<R>,
) -> Result<HashMap<(Database, Table), TableStats>, Error> {
    let mut table_stats_by_database_and_table_name =
        HashMap::<(Database, Table), TableStats>::new();

    let mut query_idx = 0usize;
    let _ = list_sql_queries_from_dump_reader(dump_reader, |query| {
        let tokens = get_tokens_from_query_str(query);

        let _ = match get_create_table_database_and_table_name(&tokens) {
            Some((database, table)) => {
                table_stats_by_database_and_table_name.insert(
                    (database.clone(), table.clone()),
                    TableStats {
                        database,
                        table,
                        columns: vec![],
                        total_rows: 0,
                        first_insert_into_row_index: 0,
                        last_insert_into_row_index: 0,
                    },
                );
            }
            None => {}
        };

        // remove potential whitespaces
        let tokens = trim_tokens(&tokens, Keyword::Insert);

        if match_keyword_at_position(Keyword::Insert, &tokens, 0)
            && match_keyword_at_position(Keyword::Into, &tokens, 2)
        {
            if let Some(database) = get_word_value_at_position(&tokens, 4) {
                if let Some(table) = get_word_value_at_position(&tokens, 6) {
                    match table_stats_by_database_and_table_name
                        .get_mut(&(database.to_string(), table.to_string()))
                    {
                        Some(table_stats) => {
                            if table_stats.total_rows == 0 {
                                // I assume that the INSERT INTO row has all the column set
                                let columns = get_column_names_from_insert_into_query(&tokens)
                                    .iter()
                                    .map(|name| name.to_string())
                                    .collect::<Vec<_>>();

                                table_stats.columns = columns;
                            }

                            if table_stats.first_insert_into_row_index == 0 {
                                table_stats.first_insert_into_row_index = query_idx;
                            }

                            table_stats.last_insert_into_row_index = query_idx;
                            table_stats.total_rows += 1;
                        }
                        None => {
                            // should not happen because INSERT INTO must come after CREATE TABLE
                            println!("Query: {}", query);
                            panic!("Unexpected: INSERT INTO happened before CREATE TABLE while creating table_stats structure")
                        }
                    }
                }
            }
        }

        query_idx += 1;
        ListQueryResult::Continue
    })?;

    Ok(table_stats_by_database_and_table_name)
}

fn trim_tokens(tokens: &Vec<Token>, keyword: Keyword) -> Vec<Token> {
    tokens
        .iter()
        .skip_while(|token| match *token {
            Token::Word(word) if word.keyword == keyword => false,
            _ => true,
        })
        .map(|token| token.clone()) // FIXME - do not clone token
        .collect::<Vec<_>>()
}

fn get_superset_table_by_database_and_table_name<R: Read>(
    dump_reader: BufReader<R>,
) -> Result<HashMap<(Database, Table), SupersetTable>, Error> {
    let mut superset_table_by_database_and_table_name =
        HashMap::<(Database, Table), SupersetTable>::new();

    list_sql_queries_from_dump_reader(dump_reader, |query| {
        let tokens = get_tokens_from_query_str(query);

        if let Some((database, table)) = get_create_table_database_and_table_name(&tokens) {
            // add table into index
            let _ = superset_table_by_database_and_table_name.insert(
                (database.clone(), table.clone()),
                SupersetTable::new(database, table, vec![]),
            );
        }

        if let Some(fk) = get_alter_table_foreign_key(&tokens) {
            let _ = match superset_table_by_database_and_table_name
                .get_mut(&(fk.from_database, fk.from_table))
            {
                Some(superset_table) => {
                    superset_table.relations.push(SupersetTableRelation::new(
                        fk.to_database,
                        fk.to_table,
                        fk.from_property,
                        fk.to_property,
                    ));
                }
                None => {} // FIXME
            };
        }

        ListQueryResult::Continue
    })?;

    Ok(superset_table_by_database_and_table_name)
}

fn get_create_table_database_and_table_name(tokens: &Vec<Token>) -> Option<(Database, Table)> {
    let tokens = trim_tokens(&tokens, Keyword::Create);

    if tokens.is_empty() {
        return None;
    }

    if match_keyword_at_position(Keyword::Create, &tokens, 0)
        && match_keyword_at_position(Keyword::Table, &tokens, 2)
    {
        if let Some(database) = get_word_value_at_position(&tokens, 4) {
            if let Some(table) = get_word_value_at_position(&tokens, 6) {
                return Some((database.to_string(), table.to_string()));
            }
        }
    }

    None
}

fn get_insert_into_database_and_table_name(tokens: &Vec<Token>) -> Option<(Database, Table)> {
    let tokens = trim_tokens(&tokens, Keyword::Insert);

    if tokens.is_empty() {
        return None;
    }

    if match_keyword_at_position(Keyword::Insert, &tokens, 0)
        && match_keyword_at_position(Keyword::Into, &tokens, 2)
    {
        if let Some(database) = get_word_value_at_position(&tokens, 4) {
            if let Some(table) = get_word_value_at_position(&tokens, 6) {
                return Some((database.to_string(), table.to_string()));
            }
        }
    }

    None
}

fn get_alter_table_foreign_key(tokens: &Vec<Token>) -> Option<ForeignKey> {
    let tokens = trim_tokens(&tokens, Keyword::Alter);

    if tokens.is_empty() {
        return None;
    }

    if !match_keyword_at_position(Keyword::Alter, &tokens, 0)
        || !match_keyword_at_position(Keyword::Table, &tokens, 2)
    {
        return None;
    }

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

    let from_database_name = match get_word_value_at_position(&tokens, database_name_pos) {
        Some(database_name) => database_name,
        None => return None,
    };

    let from_table_name = match get_word_value_at_position(&tokens, table_name_pos) {
        Some(table_name) => table_name,
        None => return None,
    };

    let next_foreign_tokens = tokens
        .iter()
        .skip_while(|token| match token {
            Token::Word(word) if word.keyword == Keyword::Foreign => false,
            _ => true,
        })
        .map(|token| token.clone())
        .collect::<Vec<_>>();

    let from_property = match get_word_value_at_position(&next_foreign_tokens, 5) {
        Some(property) => property,
        None => return None,
    };

    let to_database_name = match get_word_value_at_position(&next_foreign_tokens, 10) {
        Some(database_name) => database_name,
        None => return None,
    };

    let to_table_name = match get_word_value_at_position(&next_foreign_tokens, 12) {
        Some(table_name) => table_name,
        None => return None,
    };

    let to_property = match get_word_value_at_position(&next_foreign_tokens, 14) {
        Some(property) => property,
        None => return None,
    };

    Some(ForeignKey {
        from_database: from_database_name.to_string(),
        from_table: from_table_name.to_string(),
        from_property: from_property.to_string(),
        to_database: to_database_name.to_string(),
        to_table: to_table_name.to_string(),
        to_property: to_property.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::postgres::{
        dump_footer, dump_header, filter_insert_into_rows, first_footer_row_idx,
        get_alter_table_foreign_key, get_create_table_database_and_table_name,
        get_superset_table_by_database_and_table_name, last_header_row_idx,
        list_percent_of_insert_into_rows, table_stats_by_database_and_table_name, PostgresSuperset,
        SupersetStrategy,
    };
    use crate::{PassthroughTableSuperSet, Superset, SupersetOptions};
    use dump_parser::postgres::Tokenizer;
    use std::collections::HashSet;
    use std::fs::File;
    use std::io::BufReader;
    use std::path::{Path, PathBuf};

    fn dump_path() -> PathBuf {
        Path::new("db")
            .join("postgres")
            .join("fulldump-with-inserts.sql")
    }

    fn dump_reader() -> BufReader<File> {
        BufReader::new(File::open(dump_path()).unwrap())
    }

    #[test]
    fn check_statements_with_tokens() {
        let q = "SELECT * FROM toto;";
        let tokens = Tokenizer::new(q).tokenize().unwrap();
        assert_eq!(get_create_table_database_and_table_name(&tokens), None);

        let q = r#"
CREATE TABLE public.order_details (
    order_id smallint NOT NULL,
    product_id smallint NOT NULL,
    unit_price real NOT NULL,
    quantity smallint NOT NULL,
    discount real NOT NULL
);"#;

        let tokens = Tokenizer::new(q).tokenize().unwrap();

        assert_eq!(
            get_create_table_database_and_table_name(&tokens),
            Some(("public".to_string(), "order_details".to_string()))
        );

        let q = r#"ALTER TABLE public.employees OWNER TO root;"#;
        let tokens = Tokenizer::new(q).tokenize().unwrap();
        assert!(get_alter_table_foreign_key(&tokens).is_none());

        let q = r#"
ALTER TABLE ONLY public.territories
    ADD CONSTRAINT fk_territories_region FOREIGN KEY (region_id) REFERENCES public.region(region_id);
"#;

        let tokens = Tokenizer::new(q).tokenize().unwrap();
        let fk = get_alter_table_foreign_key(&tokens).unwrap();
        assert_eq!(fk.from_database, "public".to_string());
        assert_eq!(fk.from_table, "territories".to_string());
        assert_eq!(fk.from_property, "region_id".to_string());
        assert_eq!(fk.to_database, "public".to_string());
        assert_eq!(fk.to_table, "region".to_string());
        assert_eq!(fk.to_property, "region_id".to_string());
    }

    #[test]
    fn check_superset_table() {
        let m = get_superset_table_by_database_and_table_name(dump_reader()).unwrap();
        assert!(m.len() > 0);

        let t = m
            .get(&("public".to_string(), "customer_demographics".to_string()))
            .unwrap();

        assert_eq!(t.database, "public".to_string());
        assert_eq!(t.table, "customer_demographics".to_string());
        assert_eq!(t.relations.len(), 0);

        let t = m
            .get(&("public".to_string(), "customer_customer_demo".to_string()))
            .unwrap();

        assert_eq!(t.database, "public".to_string());
        assert_eq!(t.table, "customer_customer_demo".to_string());
        assert_eq!(t.relations.len(), 2);
        assert_eq!(t.related_tables().len(), 2);

        let t = m
            .get(&("public".to_string(), "customers".to_string()))
            .unwrap();

        assert_eq!(t.database, "public".to_string());
        assert_eq!(t.table, "customers".to_string());
        assert_eq!(t.relations.len(), 0);
    }

    #[test]
    fn check_table_stats() {
        let table_stats = table_stats_by_database_and_table_name(dump_reader()).unwrap();
        assert!(table_stats.len() > 0);
        // TODO add more tests to check table.rows size
    }

    #[test]
    fn check_percent_of_rows() {
        let table_stats = table_stats_by_database_and_table_name(dump_reader()).unwrap();
        let first_table_stats = table_stats
            .get(&("public".to_string(), "order_details".to_string()))
            .unwrap();

        let rows = list_percent_of_insert_into_rows(5, first_table_stats, dump_reader()).unwrap();

        assert!(rows.len() < first_table_stats.total_rows)
    }

    #[test]
    fn check_filter_insert_into_rows() {
        let table_stats = table_stats_by_database_and_table_name(dump_reader()).unwrap();
        let first_table_stats = table_stats
            .get(&("public".to_string(), "order_details".to_string()))
            .unwrap();

        let mut found_rows = vec![];
        filter_insert_into_rows(
            "product_id",
            "11",
            dump_reader(),
            first_table_stats,
            |row| {
                found_rows.push(row.to_string());
            },
        )
        .unwrap();

        assert_eq!(found_rows.len(), 38)
    }

    #[test]
    fn check_header_dump() {
        let table_stats = table_stats_by_database_and_table_name(dump_reader()).unwrap();

        assert!(!table_stats.is_empty());

        let table_stats_values = table_stats.values().collect::<Vec<_>>();
        let idx = last_header_row_idx(&table_stats_values);

        assert!(idx > 0);

        let mut rows = vec![];
        let _ = dump_header(dump_reader(), idx, |row| {
            rows.push(row.to_string());
        })
        .unwrap();

        assert_eq!(rows.iter().filter(|x| x.contains("INSERT INTO")).count(), 0);
        assert!(!rows.is_empty());
    }

    #[test]
    fn check_footer_dump() {
        let table_stats = table_stats_by_database_and_table_name(dump_reader()).unwrap();

        assert!(!table_stats.is_empty());

        let table_stats_values = table_stats.values().collect::<Vec<_>>();
        let idx = first_footer_row_idx(&table_stats_values);

        assert!(idx > 0);

        let mut rows = vec![];
        let _ = dump_footer(dump_reader(), idx, |row| {
            rows.push(row.to_string());
        })
        .unwrap();

        assert_eq!(rows.iter().filter(|x| x.contains("INSERT INTO")).count(), 0);
        assert!(!rows.is_empty());
    }

    #[test]
    fn check_postgres_superset() {
        let path = dump_path();
        let mut s = HashSet::new();
        s.insert(PassthroughTableSuperSet::new("public", "us_states"));

        let postgres_superset = PostgresSuperset::new(
            path.as_path(),
            SupersetStrategy::random("public", "orders", 50),
            SupersetOptions::new(&s),
        )
        .unwrap();

        let mut rows = vec![];
        let mut total_rows = 0usize;
        let mut total_rows_to_process = 0usize;
        let mut total_rows_processed = 0usize;
        postgres_superset
            .read(
                |row| {
                    rows.push(row);
                },
                |progress| {
                    if total_rows == 0 {
                        total_rows = progress.total_rows;
                    }

                    if total_rows_to_process == 0 {
                        total_rows_to_process = progress.total_rows_to_process;
                    }

                    total_rows_processed = progress.processed_rows;

                    println!(
                        "database superset progression: {}% (last process time: {}ms)",
                        progress.percent(),
                        progress.last_process_time
                    );
                },
            )
            .unwrap();

        println!(
            "{}/{} total database rows",
            total_rows_processed, total_rows
        );
        println!(
            "{}/{} rows processed",
            total_rows_processed, total_rows_to_process
        );
        assert!(total_rows_processed < total_rows);
        assert_eq!(total_rows_processed, total_rows_to_process);
        assert_eq!(
            rows.iter()
                .filter(|x| x.contains("INSERT INTO public.us_states"))
                .count(),
            51
        );
    }
}
