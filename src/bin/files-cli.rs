//! The console half.
//!
//! `files.exe` is a windowed program: it has no console, so anything it printed
//! would go nowhere. Everything that answers in *text* lives here instead -
//! `--doctor`, `--check-config`, `--bench`, `--help`, `--version`.
//!
//! # Why two executables rather than one that attaches a console
//!
//! A windowed program can call `AttachConsole(ATTACH_PARENT_PROCESS)` and print
//! to whatever launched it, and that nearly works. What it does not do is make
//! the shell *wait*: `cmd` and PowerShell decide whether to hold the prompt
//! from the subsystem bit in the PE header, so the prompt comes back
//! immediately and the output arrives on top of it. For a program whose console
//! side exists to be read during a support call, output that lands after the
//! prompt has returned is output nobody can follow.
//!
//! Two binaries cost a few hundred kilobytes and are what every Windows program
//! with both faces does.
//!
//! # The same code, not a second copy of it
//!
//! Every mode below is the same function the window calls: `doctor::doctor` is
//! what the Diagnostics window renders, and `cli::parse` is the same parser.
//! Nothing here can drift from what the program actually does, because there is
//! nothing here to drift.

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

    // Before the configuration is resolved. A file that will not load must not
    // stop somebody finding out which version they have, or reading the help
    // that explains how to check the file.
    match args.mode {
        Mode::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Mode::Version => {
            println!("files {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // Also before the configuration, and for a sharper reason than the
        // two above: this copy is running from a staging folder to upgrade an
        // installation, so the user's configuration has nothing to do with
        // it - and a file that would not load must not be what stops an
        // update the user already agreed to.
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
            // `None`: this process holds no hotkey, so registering the chord
            // to see whether it is free is both safe and the honest answer.
            // The running program passes what its own listener reported -
            // see `hotkey::Probe`.
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
                // A tree walk answers a different question from the strategy
                // shootout - how big the share is, not how fast one directory
                // reads - and takes long enough that running both would bury
                // it.
                Some(w) => doctor::bench_walk(&settings, source, w, &mut out),
                None => doctor::bench(&settings, source, query.as_deref(), allow_write, &mut out),
            }
            out.flush()
        }
        // The search panel is a window, and a window is the other executable's
        // job. Said plainly rather than by opening one: somebody who typed
        // `files-cli` at a prompt is expecting text back, and a panel appearing
        // over their terminal is a surprise rather than an answer.
        Mode::Gui => {
            eprintln!("files-cli prints reports; the search panel is `files`.");
            eprintln!("try `files-cli --doctor`, or run `files` to search.");
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
