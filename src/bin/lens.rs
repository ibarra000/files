//! `lens` - hover-to-select OCR.
//!
//! Thin over the library, exactly as `main.rs` is: everything worth testing
//! lives in [`files::lens`], so integration tests and benchmarks can reach it.
//!
//! Recognition has not landed yet. What runs today is the overlay itself,
//! against the hardcoded boxes of [`files::lens::fixture`] - which is point 2 of
//! the specification rather than a shortcut: hit-testing, drag selection and
//! cursor swapping are the hard parts, and they are finished in isolation
//! before a single pixel is captured.

use files::lens::overlay::window::{self, Action};
use files::lens::px::Point;
use files::lens::{fixture, modifier};

fn main() -> std::process::ExitCode {
    // First, and before anything touches a window. `eframe` and `wgpu` report
    // through the `log` facade, and with nothing installed they report into
    // nothing - which is how the overlay shipped opaque while the graphics
    // stack was saying so on every run. See `files::lens::log`.
    files::lens::log::install();

    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("lens: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
lens - select text that is only pixels

USAGE:
    lens [OPTIONS]

OPTIONS:
    --fixture            Run against hardcoded text instead of the screen.
                         The only mode that works today.
    --modifier <CHORD>   Hold this to arm the overlay. Default: ctrl+shift.
                         Any of ctrl, alt, shift, win, joined by '+', or 'off'.
    --seconds <N>        Close on its own after N seconds. Worth using the
                         first time: the overlay is full-screen and always on
                         top, so a mistake in it has no window to close.
    -h, --help           Print this.

Hold the modifier, and the cursor becomes an I-beam over text. Drag to select.
Release, and the Copy and Search chips appear under the selection.
";

fn run() -> Result<(), String> {
    let mut chord = modifier::parse(modifier::DEFAULT).expect("the default chord parses");
    let mut fixture_mode = false;
    let mut quit_after = None;
    let mut diagnose = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture" => fixture_mode = true,
            "--diagnose" => {
                fixture_mode = true;
                diagnose = true;
            }
            "--modifier" => {
                let text = args.next().ok_or_else(|| {
                    "--modifier needs a chord, such as \"ctrl+shift\"".to_string()
                })?;
                chord = modifier::parse(&text).map_err(|e| e.detail())?;
            }
            "--seconds" => {
                let text = args
                    .next()
                    .ok_or_else(|| "--seconds needs a number".to_string())?;
                let n: u64 = text
                    .parse()
                    .map_err(|_| format!("--seconds wants a whole number, not {text:?}"))?;
                quit_after = Some(std::time::Duration::from_secs(n));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }

    if !fixture_mode {
        // Said plainly rather than by starting an overlay that would find
        // nothing to select. Capture is the next phase; promising it now would
        // look exactly like a broken recogniser.
        return Err(
            "capture has not landed yet, so there is nothing to read off the screen.\n      \
             Try `lens --fixture` to use the overlay against hardcoded text."
                .into(),
        );
    }

    // Two blocks of text some way apart, so a drag between them is reachable by
    // hand. Placed off the top-left corner rather than at it, because a
    // selection flush against the edge of the screen cannot be dragged past.
    let map = fixture::title_block(Point::new(200, 200));

    eprintln!(
        "lens: hold {} and drag over the text at the top left of the screen. \
         Escape or Alt+F4 to quit.",
        modifier::describe(chord)
    );

    window::run(map, chord, quit_after, diagnose, Box::new(on_action)).map_err(|e| e.to_string())
}

/// Points 12 and 13, as far as they go without an index behind them.
///
/// Copy is real and final - it is [`files::clipboard`], which the rest of the
/// program already uses. Search is not: wiring it to the share index is the
/// last phase of this work, and printing what *would* be searched is honest in
/// the meantime, where a silently ignored click would not be.
fn on_action(action: Action) {
    match action {
        Action::Copy(text) => {
            let chars = text.chars().count();
            // On a thread, because `OpenClipboard` fails while another process
            // holds the clipboard and the retry can block - which the frame
            // thread never may. The same arrangement `clipboard::copy_async`
            // makes, without needing an event channel to report into.
            let _ = std::thread::Builder::new()
                .name("lens-clipboard".into())
                .spawn(move || match files::clipboard::set_text(&text) {
                    Ok(()) => eprintln!("lens: copied {chars} characters"),
                    Err(why) => eprintln!("lens: could not copy - {}", why.detail()),
                });
        }
        Action::Search(text) => {
            eprintln!("lens: would search for {text:?} (not wired to the index yet)");
        }
    }
}
