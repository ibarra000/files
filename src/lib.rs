//! Fast file-name search over large network shares.
//!
//! All logic lives in the library so integration tests and benchmarks can
//! reach it; the binaries are thin wrappers.

pub mod alias;
pub mod app;
pub mod autostart;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod doctor;
pub mod gui;
pub mod history;
pub mod hotkey;
pub mod index;
pub mod ipc;
pub mod log;
pub mod notify;
pub mod open;
pub mod paths;
pub mod placement;
pub mod search;
#[cfg(windows)]
pub mod single;
pub mod update;
pub mod util;
pub mod view;
