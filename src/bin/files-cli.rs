//! Console binary for the text modes: `--doctor`, `--check-config`, `--bench`,
//! `--help`, `--version`, plus the update helper.
//!
//! A separate executable is used because a windowed program that calls
//! `AttachConsole` does not make the shell wait: `cmd` and PowerShell decide
//! that from the PE subsystem bit, so output would land after the prompt.
//!
//! Every mode calls the same library functions as the window (`doctor::doctor`,
//! `cli::parse`), so the two cannot drift.

use std::io::{self, Write};
use std::sync::Arc;

use files::app;
use files::cli::{self, Mode};
use files::config::Settings;
use files::config::file as configfile;
use files::doctor;
use files::index::enumerate::DirSource;

fn main() -> io::Result<()> {
    let args = match cli::parse_env() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("files-cli: {err}");
            eprintln!("try `files-cli --help`");
            std::process::exit(2);
        }
    };

    // Handled before the configuration is resolved, so a broken config file
    // cannot block help, version, or an update.
    match args.mode {
        Mode::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Mode::Version => {
            println!("files {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // Runs from a staging folder to upgrade the installation; the user's
        // configuration is irrelevant here.
        Mode::ApplyUpdate {
            ref msi,
            wait_pid,
            ref relaunch,
        } => {
            return match files::update::apply::run_helper(msi, wait_pid, relaunch.as_deref()) {
                Ok(()) => Ok(()),
                Err(detail) => {
                    eprintln!("files-cli: {detail}");
                    std::process::exit(1);
                }
            };
        }
        _ => {}
    }

    let settings = match args.settings() {
        Ok(s) => s,
        Err(errors) => {
            eprint!("{}", configfile::report(&errors));
            std::process::exit(2);
        }
    };

    match args.mode {
        // Answered above, before the configuration was read.
        Mode::Help | Mode::Version | Mode::ApplyUpdate { .. } => Ok(()),
        Mode::CheckConfig { ref query } => {
            let mut out = io::stdout().lock();
            doctor::check_config(&settings, query.as_deref(), &mut out);
            out.flush()
        }
        Mode::Doctor => {
            let source = source_for(&args, &settings);
            let mut out = io::stdout().lock();
            // `None`: this process holds no hotkey, so probing the chord by
            // registering it is safe. The running app passes its listener's
            // result instead; see `hotkey::Probe`.
            doctor::doctor(&settings, source, None, &mut out);
            out.flush()
        }
        Mode::Bench {
            ref query,
            allow_write,
            walk,
        } => {
            let source = source_for(&args, &settings);
            let mut out = io::stdout().lock();
            match walk {
                // A tree walk measures share size rather than per-directory
                // read speed, and is slow enough that it runs on its own.
                Some(w) => doctor::bench_walk(&settings, source, w, &mut out),
                None => doctor::bench(&settings, source, query.as_deref(), allow_write, &mut out),
            }
            out.flush()
        }
        // Windows belong to the `files` binary; a console caller expects text.
        Mode::Gui | Mode::Settings { .. } => {
            eprintln!("files-cli prints reports only; run `files` for the search panel.");
            eprintln!("try `files-cli --help`");
            std::process::exit(2);
        }
    }
}

fn source_for(args: &cli::Args, settings: &Settings) -> Arc<dyn DirSource> {
    if args.demo {
        app::actors::fake_source_for_demo()
    } else {
        app::actors::default_source(settings)
    }
}
