//! Job-code file search over large SMB shares.
//!
//! The library holds everything; `main.rs` is a thin binary over it, so
//! integration tests and benchmarks can reach the real logic. On a machine
//! without the network drives, those tests are most of the verification
//! there is.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod doctor;
pub mod history;
pub mod index;
pub mod open;
pub mod paths;
pub mod search;
pub mod ui;
pub mod util;
