// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Synchronous Console-subsystem CLI & Windows launcher (`coree.com` / `coree-cli`).
//!
//! Keeping this Console-subsystem companion beside the `WINDOWS_GUI` `coree.exe` ensures
//! PowerShell, CMD, CI runners, and scripts always receive immediate synchronous stdout/stderr
//! output and an exact exit code (`$LASTEXITCODE`), while interactive invocations forward to
//! an existing window over IPC or spawn the sibling GUI executable cleanly.

use clap::Parser;
use coree_app::app::run_desktop_gui_with_options;
use coree_app::cli::{execute_cli, Cli, CliOutcome};
use coree_app::ipc::try_handoff_to_existing;
use std::io::Write;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute_cli(&cli) {
        Ok(CliOutcome::Printed(out)) => {
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{out}");
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Ok(CliOutcome::LaunchGui(files)) => {
            if !cli.new_window && try_handoff_to_existing(&files) {
                return ExitCode::SUCCESS;
            }
            if let Some(gui_exe) = find_sibling_gui_executable() {
                let raw_args: Vec<_> = std::env::args_os().skip(1).collect();
                match Command::new(&gui_exe).args(&raw_args).spawn() {
                    Ok(_) => return ExitCode::SUCCESS,
                    Err(err) => {
                        eprintln!("coree-cli failed to spawn GUI {}: {err}", gui_exe.display());
                    }
                }
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
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "coree error: {err}");
            let _ = stderr.flush();
            ExitCode::FAILURE
        }
    }
}

fn find_sibling_gui_executable() -> Option<std::path::PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let gui_name = if cfg!(windows) { "coree.exe" } else { "coree" };
    let candidate = dir.join(gui_name);
    if candidate.is_file() && candidate != current {
        Some(candidate)
    } else {
        None
    }
}
