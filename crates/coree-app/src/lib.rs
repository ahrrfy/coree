// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Clean-room native desktop studio GUI (`eframe 0.37` + `egui 0.37` +
//! `egui_dock 0.22` + `rfd 0.18`) and production headless CLI library for `coree`.

pub mod app;
pub mod cdr_bridge;
pub mod cli;
pub mod clipboard;
pub mod io_bridge;
pub mod ipc;
pub mod printing;
pub mod shortcuts;
pub mod state;
pub mod theme;
pub mod ui;
