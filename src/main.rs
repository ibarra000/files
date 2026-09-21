//! Entry point for the window.
//!
//! Deliberately thin. Everything real lives in the library so it can be reached
//! from `tests/` and `benches/` - a binary-only crate cannot be, and on a
//! machine without the network drives the tests are most of the verification
//! there is.
//!
//! This file used to own a terminal: raw mode, the alternate screen, mouse
//! capture, bracketed paste, and a panic hook whose only job was to put all
//! four back if the program fell over - because a panic in raw mode leaves
//! somebody with an invisible error message and an unusable shell. A window
//! needs none of it.
//!
//! # No console
//!
//! `windows_subsystem = "windows"` is what stops a black rectangle flashing up
//! behind the panel every time it is launched - from the Start menu, from a
//! shortcut, from the installer, or at sign-in. It also means this program can
//! print nothing at all, which is why every mode that answers in text lives in
//! `files-cli` instead. What is left here is the window and the two errors that
//! can stop it opening, and those are said in a message box, because a message
//! box is the only thing a windowed program can say anything with.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io;

use std::sync::Arc;

use files::app;
use files::cli::{self, Mode};
use files::config::Settings;
use files::config::file as configfile;
use files::index::enumerate::DirSource;

fn main() -> io::Result<()> {
    // First, and before anything opens a window. `eframe` and `winit`
    // report through the `log` facade, and with nothing installed they
    // report into nothing - which is how `lens` came to ship opaque while
    // the graphics stack said so on every run. This window is the one that
    // asks for per-pixel alpha now. See `files::log`.
    files::log::install();

    let args = match cli::parse_env() {
        Ok(args) => args,
        Err(err) => {
            tell(&format!("{err}\n\nTry `files-cli --help`."));
            std::process::exit(2);
        }
    };

    // Configuration is resolved before anything else runs. A bad file stops
    // the program rather than silently falling back: searching the wrong
    // share looks exactly like a job having no files.
    let settings = match args.settings() {
        Ok(s) => s,
        Err(errors) => {
            // The report is written for a terminal and reads perfectly well in
            // a message box, which is the only place this program can put it.
            // `files-cli --check-config` prints the same text, and the box says
            // so, because somebody who wants to copy it needs a console.
            tell(&format!(
                "{}\nRun `files-cli --check-config` to see this at a prompt.",
                configfile::report(&errors)
            ));
            std::process::exit(2);
        }
    };

    match args.mode {
        Mode::Gui => run_gui(args, settings),
        // Its own window and its own process. See `gui::settings::app`.
        Mode::Settings { page, .. } => {
            let choice = args.config.clone();
            if let Err(err) = files::gui::settings::app::run(settings, choice, page) {
                tell(&format!(
                    "The settings window could not be opened.

{err}"
                ));
                std::process::exit(1);
            }
            Ok(())
        }
        // Everything that answers in text. This program has no console to
        // answer with, so rather than printing into the void it says where the
        // answer lives - once, in the one way a windowed program can.
        _ => {
            tell(
                "files is the search panel, and has no console to print to.\n\n\
                 For --doctor, --check-config, --bench, --help and --version, \
                 run files-cli instead.",
            );
            std::process::exit(2);
        }
    }
}

/// Says something to somebody who has no console to be told in.
///
/// The only two things this is ever used for are a configuration file that
/// will not load and a window that will not open - both of which stop the
/// program, and both of which would otherwise be a process that starts and
/// vanishes with nothing on screen at all.
fn tell(message: &str) {
    files::notify::tell("files", message);
}

fn source_for(args: &cli::Args, settings: &Settings) -> Arc<dyn DirSource> {
    if args.demo {
        app::actors::fake_source_for_demo()
    } else {
        app::actors::default_source(settings)
    }
}

/// Runs the window.
///
/// Shorter than the terminal entry point it replaced by everything that was
/// about owning a terminal: no raw mode, no alternate screen, no mouse capture,
/// and no panic hook whose only job was to put all three back if the program
/// fell over.
fn run_gui(args: cli::Args, settings: Settings) -> io::Result<()> {
    let source = source_for(&args, &settings);
    // Before the workers, so the first thing that writes finds somewhere to
    // write to rather than discovering it is missing three threads away.
    for created in files::paths::ensure_app_dirs(&settings) {
        log::info!("created {}", created.display());
    }

    prewarm(&settings, Arc::clone(&source));

    if let Err(err) = files::gui::run(settings, args.config.clone(), source) {
        tell(&format!("The search panel could not be opened.\n\n{err}"));
        std::process::exit(1);
    }
    Ok(())
}

/// Touches both roots off-thread so the SMB session is established early.
fn prewarm(settings: &Settings, source: Arc<dyn DirSource>) {
    // Every configured mapping, not a fixed pair: adding a share should warm
    // it too.
    let roots: Vec<_> = settings.routes.enabled().map(|m| m.path.clone()).collect();
    let _ = std::thread::Builder::new()
        .name("files-prewarm".into())
        .spawn(move || {
            for root in roots {
                source.prewarm(&root);
            }
        });
}
