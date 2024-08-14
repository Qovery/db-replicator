use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Replibyte is a tool to seed your databases with your production data while keeping sensitive data safe, just pass `-h`
#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
#[clap(propagate_version = true)]
pub struct CLI {
    /// Replibyte configuration file
    #[clap(short, long, parse(from_os_str), value_name = "configuration file")]
    pub config: PathBuf,
    #[clap(subcommand)]
    pub sub_commands: SubCommand,
    /// disable telemetry
    #[clap(short, long)]
    pub no_telemetry: bool,
}

/// sub commands
#[derive(Subcommand, Debug)]
pub enum SubCommand {
    /// all dump commands
    #[clap(subcommand)]
    Dump(DumpCommand),
    /// all source commands
    #[clap(subcommand)]
    Source(SourceCommand),
    /// all transformer commands
    #[clap(subcommand)]
    Transformer(TransformerCommand),
}

/// all dump commands
#[derive(Subcommand, Debug)]
pub enum DumpCommand {
    /// list available dumps
    List,
    /// launch dump -- use `-h` to show all the options
    Create(DumpCreateArgs),
    /// all restore commands
    #[clap(subcommand)]
    Restore(RestoreCommand),
    /// delete a dump from the defined datastore
    Delete(DumpDeleteArgs),
}

/// all transformer commands
#[derive(Subcommand, Debug)]
pub enum TransformerCommand {
    /// list available transformers
    List,
}

/// all restore commands
#[derive(Subcommand, Debug)]
pub enum RestoreCommand {
    /// Restore dump inside a local Docker container
    Local(RestoreLocalArgs),
    /// Restore dump inside the configured destination
    Remote(RestoreArgs),
}

/// all restore commands
#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// restore dump -- set `latest` or `<dump name>` - use `dump list` command to list all dumps available
    #[clap(short, long, value_name = "[latest | dump name]")]
    pub value: String,
    /// stream output on stdout
    #[clap(short, long)]
    pub output: bool,
}

/// restore dump in a local Docker container
#[derive(Args, Debug)]
pub struct RestoreLocalArgs {
    /// restore dump -- set `latest` or `<dump name>` - use `dump list` command to list all dumps available
    #[clap(short, long, value_name = "[latest | dump name]")]
    pub value: String,
    /// stream output on stdout
    #[clap(short, long)]
    pub output: bool,
    /// Docker image tag for the container to spawn
    #[clap(short, long)]
    pub tag: Option<String>,
    /// Docker container port to map on the host
    #[clap(short, long)]
    pub port: Option<u16>,
    /// Remove the Docker container on Ctrl-c
    #[clap(short, long)]
    pub remove: bool,
    /// Docker image type
    #[clap(short, long, value_name = "[postgresql | mysql | mongodb]")]
    pub image: Option<String>,
}

/// all dump run commands
#[derive(Args, Debug)]
pub struct DumpCreateArgs {
    #[clap(name = "source_type", short, long, value_name = "[postgresql | mysql | mongodb]", possible_values = &["postgresql", "mysql", "mongodb"], requires = "input")]
    /// database source type to import
    pub source_type: Option<String>,
    /// import dump from stdin
    #[clap(name = "input", short, long, requires = "source_type")]
    pub input: bool,
    #[clap(short, long, parse(from_os_str), value_name = "dump file")]
    /// dump file
    pub file: Option<PathBuf>,
    /// dump name
    #[clap(short, long)]
    pub name: Option<String>,
    /// page chunk size, default to 100 MB
    #[clap(short, long)]
    pub chunk_size: Option<usize>,
}

#[derive(Args, Debug)]
#[clap(group = clap::ArgGroup::new("delete-mode").multiple(false))]
pub struct DumpDeleteArgs {
    /// Name of the dump to delete
    #[clap(group = "delete-mode")]
    pub dump: Option<String>,
    /// Remove all dumps older than the specified number of days. Example: `14d` for deleting dumps older than 14 days
    #[clap(long, group = "delete-mode")]
    pub older_than: Option<String>,
    /// Keep only the last N dumps
    #[clap(long, group = "delete-mode")]
    pub keep_last: Option<usize>,
}

/// all source commands
#[derive(Subcommand, Debug)]
pub enum SourceCommand {
    /// Show the database schema. When used with MongoDB, the schema will be probabilistic and returned as a JSON document
    Schema,
}
