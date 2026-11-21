// SPDX-License-Identifier: MPL-2.0
#![forbid(unsafe_code)]

//! Production CLI & native desktop studio entrypoint for `coree`.

use clap::Parser;
use coree_app::app::run_desktop_gui;
use coree_app::cli::{execute_cli, Cli, CliOutcome};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute_cli(&cli) {
        Ok(CliOutcome::Printed(out)) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Ok(CliOutcome::LaunchGui(files)) => match run_desktop_gui(files) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("coree GUI error: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("coree error: {err}");
            ExitCode::FAILURE
        }
    }
}
