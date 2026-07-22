use crate::cli::Cli;
use clap::Parser;

mod loop_config;
mod profile;
mod provider;

fn cli_from(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).unwrap()
}
