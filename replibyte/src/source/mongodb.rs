use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Error, ErrorKind, Read};
use std::process::{Command, Stdio};

use crate::connector::Connector;
use crate::source::{Explain, Source};
use crate::transformer::Transformer;
use crate::types::{Column, OriginalQuery, Query};
use crate::utils::{binary_exists, table, wait_for_command};
use crate::SourceOptions;

use bson::{Bson, Document};
use dump_parser::mongodb::Archive;
// use mongodb_schema_parser::SchemaParser;

pub struct MongoDB<'a> {
    uri: &'a str,
    database: &'a str,
}

impl<'a> MongoDB<'a> {
    pub fn new(uri: &'a str, database: &'a str) -> Self {
        MongoDB { uri, database }
    }
}

impl<'a> Connector for MongoDB<'a> {
    fn init(&mut self) -> Result<(), Error> {
        let _ = binary_exists("mongosh")?;
        let _ = binary_exists("mongodump")?;
        let _ = check_connection_status(self)?;

        Ok(())
    }
}

impl<'a> Explain for MongoDB<'a> {
    fn schema(&self) -> Result<(), Error> {
        let dump_args = vec![
            "--uri",
            self.uri,
            "--db",
            self.database,
            "--archive", // dump to stdin
        ];

        let mut process = Command::new("mongodump")
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

impl<'a> Source for MongoDB<'a> {
    fn read<F: FnMut(OriginalQuery, Query)>(
        &self,
        options: SourceOptions,
        query_callback: F,
    ) -> Result<(), Error> {
        if let Some(_database_subset) = &options.database_subset {
            todo!("database subset not supported yet for MongoDB source")
        }

        if let Some(_database_superset) = &options.database_superset {
            todo!("database superset not supported yet for MongoDB source")
        }

        let dump_args = vec![
            "--uri",
            self.uri,
            "--db",
            self.database,
            "--archive", // dump to stdin
        ];

        let mut process = Command::new("mongodump")
            .args(dump_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Other, "Could not capture standard output."))?;

        let reader = BufReader::new(stdout);

        read_and_transform(reader, options, query_callback)?;

        wait_for_command(&mut process)
    }
}

fn check_connection_status(db: &MongoDB) -> Result<(), Error> {
    let mut echo_process = Command::new("echo")
        .arg(r#"'db.runCommand("ping").ok'"#)
        .stdout(Stdio::piped())
        .spawn()?;

    let mut mongo_process = Command::new("mongosh")
        .args([db.uri, "--quiet"])
        .stdin(echo_process.stdout.take().unwrap())
        .stdout(Stdio::inherit())
        .spawn()?;

    let exit_status = mongo_process.wait()?;

    if !exit_status.success() {
        return Err(Error::new(
            ErrorKind::Other,
            format!("command error: {:?}", exit_status.to_string()),
        ));
    }

    Ok(())
}

pub fn recursively_transform_bson(
    key: String,
    bson: Bson,
    transformers: &HashMap<String, &Box<dyn Transformer + '_>>,
    wildcard_keys: &HashSet<String>,
) -> Bson {
    let mut column;
    match bson {
        Bson::String(value) => {
            column = Column::StringValue(key.clone(), value.clone());
            column = match transformers.get(key.as_str()) {
                Some(transformer) => transformer.transform(column), // apply transformation on the column
                None => column,
            };
            Bson::String((*column.string_value().unwrap()).to_string())
        }
        Bson::Double(value) => {
            column = Column::FloatNumberValue(key.clone(), value);
            column = match transformers.get(key.as_str()) {
                Some(transformer) => transformer.transform(column), // apply transformation on the column
                None => column,
            };
            Bson::Double(*column.float_number_value().unwrap())
        }
        Bson::Array(arr) => {
            let new_arr = arr
                .iter()
                .enumerate()
                .map(|(idx, bson)| {
                    let wildcard_key = format!("{}.$[]", key);
                    recursively_transform_bson(
                        if wildcard_keys.contains(&wildcard_key) {
                            wildcard_key
                        } else {
                            format!("{}.{}", key, idx)
                        },
                        bson.clone(),
                        transformers,
                        wildcard_keys,
                    )
                })
                .collect::<Vec<Bson>>();
            Bson::Array(new_arr)
        }
        Bson::Document(nested_doc) => Bson::Document(recursively_transform_document(
            key,
            nested_doc,
            transformers,
            wildcard_keys,
        )),
        Bson::Null => Bson::Null,
        Bson::Int32(value) => {
            column = Column::NumberValue(key.clone(), value as i128);
            column = match transformers.get(key.as_str()) {
                Some(transformer) => transformer.transform(column), // apply transformation on the column
                None => column,
            };
            Bson::Int32(column.number_value().map(|&n| n as i32).unwrap())
        }
        Bson::Int64(value) => {
            column = Column::NumberValue(key.clone(), value as i128);
            column = match transformers.get(key.as_str()) {
                Some(transformer) => transformer.transform(column), // apply transformation on the column
                None => column,
            };
            Bson::Int64(column.number_value().map(|&n| n as i64).unwrap())
        }
        // ALL OF THE NEXT TYPES ARE NOT TRANSFORMABLE (yet?)
        Bson::ObjectId(oid) => Bson::ObjectId(oid),
        Bson::Binary(bin) => Bson::Binary(bin),
        Bson::RegularExpression(regex) => Bson::RegularExpression(regex),
        Bson::Boolean(value) => Bson::Boolean(value),
        Bson::DateTime(value) => Bson::DateTime(value),
        Bson::Timestamp(value) => Bson::Timestamp(value),
        Bson::MinKey => Bson::MinKey,
        Bson::MaxKey => Bson::MaxKey,
        Bson::JavaScriptCode(jsc) => Bson::JavaScriptCode(jsc),
        Bson::JavaScriptCodeWithScope(jsc) => Bson::JavaScriptCodeWithScope(jsc),
        Bson::Symbol(symbol) => Bson::Symbol(symbol),
        Bson::Decimal128(decimal) => Bson::Decimal128(decimal),
        Bson::Undefined => Bson::Undefined,
        Bson::DbPointer(db_pointer) => Bson::DbPointer(db_pointer),
    }
}

pub fn recursively_transform_document(
    prefix: String,
    mut original_doc: Document,
    transformers: &HashMap<String, &Box<dyn Transformer + '_>>,
    wildcard_keys: &HashSet<String>,
) -> Document {
    for (key, bson) in original_doc.clone() {
        original_doc.insert(
            key.clone(),
            recursively_transform_bson(
                format!("{}.{}", prefix, key),
                bson,
                transformers,
                wildcard_keys,
            ),
        );
    }
    original_doc
}

pub(crate) fn find_all_keys_with_array_wildcard_op(
    transformers: &Vec<Box<dyn Transformer + '_>>,
) -> HashSet<String> {
    let mut wildcard_keys = HashSet::new();
    for transformer in transformers {
        let column_name = transformer.column_name();
        let delim = ".$[].";
        let mut iter = 0;
        while let Some(idx) = column_name[iter..].find(delim) {
            let offset = delim.len();
            iter += idx + offset;
            let key = column_name[..(iter - 1)].to_string();
            wildcard_keys.insert(format!("{}.{}", transformer.database_and_table_name(), key));
        }
        // try to find last delim
        let last_delim = ".$[]"; // no dot at the end
        if let Some(_) = column_name[iter..].find(last_delim) {
            let key = column_name.to_string();
            wildcard_keys.insert(format!("{}.{}", transformer.database_and_table_name(), key));
        }
    }
    wildcard_keys
}

/// consume reader and apply transformation on INSERT INTO queries if needed
pub fn read_and_transform<R: Read, F: FnMut(OriginalQuery, Query)>(
    reader: BufReader<R>,
    source_options: SourceOptions,
    mut query_callback: F,
) -> Result<(), Error> {
    let transformers = source_options.transformers;
    // create a set of wildcards to be used in the transformation
    let wildcard_keys = find_all_keys_with_array_wildcard_op(transformers);
    // create a map variable with Transformer by column_name
    let mut transformer_by_db_and_table_and_column_name: HashMap<String, &Box<dyn Transformer>> =
        HashMap::with_capacity(transformers.len());

    for transformer in transformers {
        let _ = transformer_by_db_and_table_and_column_name.insert(
            transformer.database_and_table_and_column_name(),
            transformer,
        );
    }
    // init archive from reader
    let mut archive = Archive::from_reader(reader)?;

    let original_query = Query(archive.clone().into_bytes()?);

    archive.alter_docs(|prefixed_collections| {
        for (prefix, collection) in prefixed_collections.to_owned() {
            let mut new_collection = vec![];
            for doc in collection {
                let new_doc = recursively_transform_document(
                    prefix.clone(), // prefix is <db_name>.<collection_name>
                    doc,
                    &transformer_by_db_and_table_and_column_name,
                    &wildcard_keys,
                );
                new_collection.push(new_doc);
            }
            prefixed_collections.insert(prefix, new_collection);
        }
    });

    let query = Query(archive.into_bytes()?);

    query_callback(original_query, query);
    Ok(())
}

pub fn read_and_parse_schema<R: Read>(reader: BufReader<R>) -> Result<(), Error> {
    let mut archive = Archive::from_reader(reader)?;

    archive.alter_docs(|prefixed_collections| {
        for (name, collection) in prefixed_collections.to_owned() {
            let mut table = table();

            table.set_titles(row![format!("Collection {}", name)]);

            // let mut schema_parser = SchemaParser::new();

            // for doc in collection {
            //     schema_parser.write_bson(doc).unwrap();
            // }

            // let schema = schema_parser.flush();

            // let json_data = serde_json::to_string_pretty(&schema).unwrap();

            // table.add_row(row![name]);
            // table.add_row(row![json_data]);

            // let _ = table.printstd();
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::source::SourceOptions;
    use crate::transformer::random::RandomTransformer;
    use crate::Source;
    use bson::{doc, Bson};
    use std::collections::{HashMap, HashSet};
    use std::vec;

    use crate::source::mongodb::{find_all_keys_with_array_wildcard_op, MongoDB};
    use crate::transformer::transient::TransientTransformer;
    use crate::transformer::Transformer;

    use super::recursively_transform_document;

    fn get_mongodb() -> MongoDB<'static> {
        MongoDB::new(
            "mongodb://root:password@localhost:27018/test?authSource=admin",
            "test",
        )
    }

    fn get_invalid_mongodb() -> MongoDB<'static> {
        MongoDB::new(
            "mongodb://root:wrongpassword@localhost:27018/test?authSource=admin",
            "test",
        )
    }

    #[test]
    fn connect() {
        let p = get_mongodb();

        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            database_superset: &None,
            only_tables: &vec![],
        };

        assert!(p.read(source_options, |_, _| {}).is_ok());

        let p = get_invalid_mongodb();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            database_superset: &None,
            only_tables: &vec![],
        };

        assert!(p.read(source_options, |_, _| {}).is_err());
    }

    #[test]
    fn list_rows() {
        let p = get_mongodb();
        let t1: Box<dyn Transformer> = Box::new(TransientTransformer::default());
        let transformers = vec![t1];
        let source_options = SourceOptions {
            transformers: &transformers,
            skip_config: &vec![],
            database_subset: &None,
            database_superset: &None,
            only_tables: &vec![],
        };

        p.read(source_options, |original_query, query| {
            assert!(original_query.data().len() > 0);
            assert!(query.data().len() > 0);
        })
        .unwrap();
    }

    #[test]
    fn recursive_document_transform() {
        let database_name = "test";
        let table_name = "users";
        let columns = vec!["no_nest", "info.ext.number", "info_arr.0.a", "info_arr.1.b"];
        let doc = doc! {
            "no_nest": 5,
            "info": {
                "ext": {
                    "number": 123456789000 as i64
                }
            },
            "info_arr" : [
                { "a": "SomeString" },
                { "b": 3.5 }
            ]
        };
        // Create a vec of all transformers
        let transformers_vec = Vec::from_iter(columns.iter().map(|&c| {
            let t: Box<dyn Transformer> = Box::new(RandomTransformer::new(
                database_name,
                table_name,
                &c.to_string(),
            ));
            t
        }));
        // Create a HashMap with Transformer by db_name.table_name.column_name
        let transformers = HashMap::from_iter(
            transformers_vec
                .iter()
                .map(|t| t.database_and_table_and_column_name())
                .zip(transformers_vec.iter()),
        );
        // Recursively transform the document
        let transformed_doc = recursively_transform_document(
            "test.users".to_string(),
            doc,
            &transformers,
            &HashSet::new(),
        );

        // Assert transformed values are not equal to original values
        // no_nest
        assert_ne!(transformed_doc.get("no_nest").unwrap(), &Bson::Int32(5));
        // info.ext.number
        assert_ne!(
            transformed_doc
                .get_document("info")
                .unwrap()
                .get_document("ext")
                .unwrap()
                .get("number")
                .unwrap(),
            &Bson::Int64(1234567890)
        );

        let arr = transformed_doc.get_array("info_arr").unwrap();
        // info_arr.0.a
        let doc = arr[0].as_document().unwrap();
        assert_ne!(
            doc.get("a").unwrap(),
            &Bson::String("SomeString".to_string())
        );
        // info_arr.1.b
        let doc = arr[1].as_document().unwrap();
        assert_ne!(doc.get("b").unwrap(), &Bson::Double(3.5));
    }

    #[test]
    fn recursive_document_transform_with_wildcard_nested() {
        let database_name = "test";
        let table_name = "users";
        let column_name = "a.b.$[].c.0";
        let doc = doc! {
            "a": {
                "b" : [
                    {
                        "c" : [
                            1, // should be transformed
                            2  // shouldn't be transformed
                        ]
                    },
                    {
                        "c" : [
                            3, // should be transformed
                            4  // shouldn't be transformed
                        ]
                    }
                ]
            }
        };
        let t: Box<dyn Transformer> = Box::new(RandomTransformer::new(
            database_name,
            table_name,
            column_name.into(),
        ));
        let transformers_vec = vec![t];
        // create a set of wildcards to be used in the transformation
        let wildcard_keys = find_all_keys_with_array_wildcard_op(&transformers_vec);
        // create a map variable with Transformer by column_name
        let mut transformers: HashMap<String, &Box<dyn Transformer>> =
            HashMap::with_capacity(transformers_vec.len());

        for transformer in transformers_vec.iter() {
            let _ = transformers.insert(
                transformer.database_and_table_and_column_name(),
                transformer,
            );
        }
        // Recursively transform the document
        let transformed_doc = recursively_transform_document(
            "test.users".to_string(),
            doc,
            &transformers,
            &wildcard_keys,
        );

        // Assert transformed values are not equal to original values
        let arr = transformed_doc
            .get_document("a")
            .unwrap()
            .get_array("b")
            .unwrap();
        // 1, 2
        let mut doc = arr[0].as_document().unwrap();
        let mut inner_arr = doc.get_array("c").unwrap();
        assert_ne!(inner_arr[0], Bson::Int32(1));
        assert_eq!(inner_arr[1], Bson::Int32(2));
        // 3, 4
        doc = arr[1].as_document().unwrap();
        inner_arr = doc.get_array("c").unwrap();
        assert_ne!(inner_arr[0], Bson::Int32(3));
        assert_eq!(inner_arr[1], Bson::Int32(4));
    }
}
