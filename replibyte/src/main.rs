#[macro_use]
extern crate prettytable;

use std::fs::File;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread::sleep;
use std::time::Duration;
use std::{env, thread};

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use migration::{migrations, Migrator};
use utils::get_replibyte_version;

use crate::cli::{DumpCommand, RestoreCommand, SubCommand, TransformerCommand, CLI, SourceCommand};
use crate::config::{Config, DatabaseSubsetConfig, DatabaseSupersetConfig, DatastoreConfig};
use crate::datastore::local_disk::LocalDisk;
use crate::datastore::s3::S3;
use crate::datastore::Datastore;
use crate::source::{Source, SourceOptions};
use crate::tasks::{MaxBytes, TransferredBytes};
use crate::telemetry::{ClientOptions, TelemetryClient, TELEMETRY_TOKEN};
use crate::utils::epoch_millis;

mod cli;
mod commands;
mod config;
mod connector;
mod datastore;
mod destination;
mod migration;
mod runtime;
mod source;
mod tasks;
mod telemetry;
mod transformer;
mod types;
mod utils;

fn show_progress_bar(rx_pb: Receiver<(TransferredBytes, MaxBytes)>) {
    let pb = ProgressBar::new(0);
    pb.set_style(ProgressStyle::default_spinner());

    let mut style_is_progress_bar = false;
    let mut _max_bytes = 0usize;
    let mut last_transferred_bytes = 0usize;

    loop {
        let (transferred_bytes, max_bytes) = match rx_pb.try_recv() {
            Ok(msg) => msg,
            Err(_) => (last_transferred_bytes, _max_bytes),
        };

        if _max_bytes == 0 && style_is_progress_bar {
            // show spinner if there is no max_bytes indicated
            pb.set_style(ProgressStyle::default_spinner());
            style_is_progress_bar = false;
        } else if _max_bytes > 0 && !style_is_progress_bar {
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.green/blue}] {bytes}/{total_bytes} ({eta})")
                .progress_chars("#>-"));
            style_is_progress_bar = true;
        }

        if max_bytes != _max_bytes {
            pb.set_length(max_bytes as u64);
            _max_bytes = max_bytes;
        }

        last_transferred_bytes = transferred_bytes;
        pb.set_position(transferred_bytes as u64);

        sleep(Duration::from_micros(50));
    }
}

fn main() {
    let start_exec_time = utils::epoch_millis();

    env_logger::init();

    let env_args = env::args().collect::<Vec<String>>();
    let args = CLI::parse();

    let file = File::open(args.config).expect("missing config file");
    let config: Config = serde_yaml::from_reader(file).expect("bad config file format");

    let sub_commands: &SubCommand = &args.sub_commands;

    let telemetry_client = match args.no_telemetry {
        true => None,
        false => Some(TelemetryClient::new(ClientOptions::from(TELEMETRY_TOKEN))),
    };

    let telemetry_config = config.clone();

    if let Some(telemetry_client) = &telemetry_client {
        let _ = telemetry_client.capture_command(&telemetry_config, sub_commands, &env_args, None);
    }

    let mut exit_code = 0;
    if let Err(err) = run(config, &sub_commands) {
        eprintln!("{}", err);
        exit_code = 1;
    }

    if let Some(telemetry_client) = &telemetry_client {
        let _ = telemetry_client.capture_command(
            &telemetry_config,
            sub_commands,
            &env_args,
            Some(epoch_millis() - start_exec_time),
        );
    }
    if exit_code != 0 {
         std::process::exit(exit_code);
    }
}

fn run(config: Config, sub_commands: &SubCommand) -> anyhow::Result<()> {
    let mut datastore: Box<dyn Datastore> = match &config.datastore {
        DatastoreConfig::AWS(config) => Box::new(S3::aws(
            config.bucket()?,
            config.region()?,
            config.profile()?,
            config.credentials()?,
            config.endpoint()?,
        )?),
        DatastoreConfig::GCP(config) => Box::new(S3::gcp(
            config.bucket()?,
            config.region()?,
            config.access_key()?,
            config.secret()?,
            config.endpoint()?,
        )?),
        DatastoreConfig::LocalDisk(config) => Box::new(LocalDisk::new(config.dir()?)),
    };

    let migrator = Migrator::new(get_replibyte_version(), &datastore, migrations());
    let _ = migrator.migrate()?;

    let _ = datastore.init()?;

    let (tx_pb, rx_pb) = mpsc::sync_channel::<(TransferredBytes, MaxBytes)>(1000);

    match sub_commands {
        // skip progress when output = true
        SubCommand::Dump(dump_cmd) => match dump_cmd {
            DumpCommand::Restore(cmd) => match cmd {
                RestoreCommand::Local(args) => if args.output {},
                RestoreCommand::Remote(args) => if args.output {},
            },
            _ => {
                // let _ = thread::spawn(move || show_progress_bar(rx_pb));
            }
        },
        _ => {
            // let _ = thread::spawn(move || show_progress_bar(rx_pb));
        }
    };

    let progress_callback = |bytes: TransferredBytes, max_bytes: MaxBytes| {
        let _ = tx_pb.send((bytes, max_bytes));
    };

    match sub_commands {
        SubCommand::Dump(cmd) => match cmd {
            DumpCommand::List => {
                let _ = commands::dump::list(&mut datastore)?;
                Ok(())
            }
            DumpCommand::Create(args) => {
                if let Some(name) = &args.name {
                    datastore.set_dump_name(name.to_string());
                }
                commands::dump::run(args, datastore, config, progress_callback)
            }
            DumpCommand::Delete(args) => commands::dump::delete(datastore, args),
            DumpCommand::Restore(restore_cmd) => match restore_cmd {
                RestoreCommand::Local(args) => {
                    commands::dump::restore_local(args, datastore, config, progress_callback)
                }
                RestoreCommand::Remote(args) => {
                    commands::dump::restore_remote(args, datastore, config, progress_callback)
                }
            },
        },
        SubCommand::Source(cmd) => match cmd {
            SourceCommand::Schema => {
                commands::source::schema(config)
            }
        },
        SubCommand::Transformer(cmd) => match cmd {
            TransformerCommand::List => {
                let _ = commands::transformer::list();
                Ok(())
            }
        },
    }
}
