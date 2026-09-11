//! Entry point: argument dispatch, terminal lifecycle, panic safety.
//!
//! Deliberately thin. Everything real lives in the library so it can be
//! reached from `tests/` and `benches/` - a binary-only crate cannot be, and
//! on a machine without the network drives the tests are most of the
//! verification there is.

use std::io::{self, Write};
use std::sync::Arc;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

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
            eprintln!("files: {err}");
            eprintln!("try `files --help`");
            std::process::exit(2);
        }
    };

    // Configuration is resolved before anything else runs. A bad file stops
    // the program rather than silently falling back: searching the wrong
    // share looks exactly like a job having no files.
    let settings = match args.settings() {
        Ok(s) => s,
        Err(errors) => {
            eprint!("{}", configfile::report(&errors));
            std::process::exit(2);
        }
    };

    match args.mode {
        Mode::Help => {
            print!("{}", cli::HELP);
            Ok(())
        }
        Mode::Version => {
            println!("files {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Mode::CheckConfig { ref query } => {
            let mut out = io::stdout().lock();
            doctor::check_config(&settings, query.as_deref(), &mut out);
            out.flush()
        }
        Mode::Doctor => {
            let source = source_for(&args, &settings);
            let mut out = io::stdout().lock();
            doctor::doctor(&settings, source, &mut out);
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
        Mode::Tui => run_tui(args, settings),
    }
}

fn source_for(args: &cli::Args, settings: &Settings) -> Arc<dyn DirSource> {
    if args.demo {
        app::actors::fake_source_for_demo()
    } else {
        app::actors::default_source(settings)
    }
}

fn run_tui(args: cli::Args, settings: Settings) -> io::Result<()> {
    // Warm the SMB session before anything else. First contact with a mapped
    // drive can cost seconds of session setup and DFS resolution, and doing
    // it here overlaps that with terminal initialisation and the user's first
    // keystrokes.
    let source = source_for(&args, &settings);
    prewarm(&settings, Arc::clone(&source));

    let volume_serial = volume_serial(&settings, args.demo);

    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture is what makes click-to-place-a-caret and drag-to-select
    // possible. It costs the terminal's own drag-to-select, which is why it
    // was doing nothing but harm before: it was enabled and every mouse event
    // was thrown away. Holding Shift while dragging still reaches the
    // terminal's selection, over the whole window.
    //
    // Bracketed paste turns a paste into one event instead of a burst of
    // keystrokes, which is what lets it replace a selection and land at the
    // caret as a single edit.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = app::run(&mut terminal, settings, source, volume_serial);

    // Restored before anything else is waited on, so a wedged network thread
    // can never keep the user out of their shell.
    restore_terminal();

    if let Err(err) = &result {
        eprintln!("files: {err}");
    }
    result
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

/// The volume serial of the flat root, used to validate the persisted index.
///
/// Without it, a drive letter remapped to a different share would serve the
/// previous mapping's file list.
fn volume_serial(settings: &Settings, demo: bool) -> Option<u32> {
    if demo {
        return None;
    }
    #[cfg(windows)]
    {
        files::index::volume::volume_serial(&settings.custpro_path)
    }
    #[cfg(not(windows))]
    {
        let _ = settings;
        None
    }
}

/// Leaves raw mode and the alternate screen, ignoring errors.
///
/// Safe to call twice, which matters because both the normal exit path and
/// the panic hook call it.
fn restore_terminal() {
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = disable_raw_mode();
}

/// Restores the terminal before a panic message is printed.
///
/// Without this, any panic leaves the user in raw mode inside the alternate
/// screen - the message is invisible, the shell is unusable, and the window
/// has to be closed. The previous implementation had exactly that failure
/// mode.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}
