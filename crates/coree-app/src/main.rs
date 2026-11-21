// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! Production CLI & native desktop studio entrypoint for `coree`.

use clap::Parser;
use coree_app::app::run_desktop_gui_with_options;
use coree_app::cli::{execute_cli, Cli, CliOutcome};
use coree_app::ipc::try_handoff_to_existing;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute_cli(&cli) {
        Ok(CliOutcome::Printed(out)) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Ok(CliOutcome::LaunchGui(files)) => {
            if !cli.new_window && try_handoff_to_existing(&files) {
                return ExitCode::SUCCESS;
            }
            match run_desktop_gui_with_options(files, cli.new_window) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("coree GUI error: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(err) => {
            eprintln!("coree error: {err}");
            ExitCode::FAILURE
        }
    }
}
