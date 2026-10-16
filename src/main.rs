//! Entry point for the windowed `files` binary.
//!
//! Kept thin so the logic lives in the library, where `tests/` and `benches/`
//! can reach it.
//!
//! `windows_subsystem = "windows"` prevents a console window from flashing up
//! on launch, but also means this binary cannot print. Text modes live in
//! `files-cli`; fatal errors here are shown in a message box.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io;

use std::sync::Arc;

use files::app;
use files::cli::{self, Mode};
use files::config::Settings;
use files::config::file as configfile;
use files::index::enumerate::DirSource;

fn main() -> io::Result<()> {
    // Must run before any window opens: `eframe` and `winit` report through
    // the `log` facade, which discards everything until a logger is installed.
    // See `files::log`.
    files::log::install();

    let args = match cli::parse_env() {
        Ok(args) => args,
        Err(err) => {
            tell(&format!("{err}\n\nTry `files-cli --help`."));
            std::process::exit(2);
        }
    };

    // A bad configuration file stops the program instead of falling back to
    // defaults: searching the wrong share looks exactly like a search with no results.
    let settings = match args.settings() {
        Ok(s) => s,
        Err(errors) => {
            // Same text as `files-cli --check-config`, which can be copied.
            tell(&format!(
                "{}\nRun `files-cli --check-config` to see this at a prompt.",
                configfile::report(&errors)
            ));
            std::process::exit(2);
        }
    };

    match args.mode {
        Mode::Gui => run_gui(args, settings),
        // Separate window in its own process. See `gui::settings::app`.
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
        // Text modes need a console; point the user at `files-cli`.
        _ => {
            tell(
                "files is a windowed application and cannot print output.\n\n\
                 Use files-cli for --doctor, --check-config, --bench, --help and \
                 --version.",
            );
            std::process::exit(2);
        }
    }
}

/// Shows a fatal error in a message box, since this binary has no console.
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

/// Runs the search panel window.
fn run_gui(args: cli::Args, settings: Settings) -> io::Result<()> {
    let source = source_for(&args, &settings);
    // Create app directories before any worker thread tries to write to them.
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

/// Touches every enabled root off-thread so SMB sessions are established early.
fn prewarm(settings: &Settings, source: Arc<dyn DirSource>) {
    let roots: Vec<_> = settings.routes.enabled().map(|m| m.path.clone()).collect();
    let _ = std::thread::Builder::new()
        .name("files-prewarm".into())
        .spawn(move || {
            for root in roots {
                source.prewarm(&root);
            }
        });
}
