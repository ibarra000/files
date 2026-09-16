//! The help panel.
//!
//! Every key and every mouse gesture, in plain language, in one place.
//!
//! Held as rows rather than as drawn text, so the same words reach a
//! terminal and a window. It is also why the hint bar along the bottom can
//! afford to be short: nothing has to be undiscoverable for the line to fit.
//!
//! The mouse gets documented here for the first time. It has worked since the
//! program gained it and has never been mentioned anywhere on screen.
//!
//! The wording is aimed at somebody who does not use a terminal by choice.
//! "open it" rather than "invoke the configured viewer", and every row says
//! what happens rather than what the key is called.

use crate::config::ViewerKind;

/// A heading, or a key and what it does.
pub enum Row {
    Heading(&'static str),
    Entry(&'static str, &'static str),
    Blank,
}

pub fn rows(viewer: ViewerKind) -> Vec<Row> {
    let opens = match viewer {
        ViewerKind::Auto => "open it in whatever Windows opens that kind of file with",
        ViewerKind::Pdf => "open every page of that code as one document",
        ViewerKind::Avwin => "open the selected file in avwin",
    };
    vec![
        Row::Blank,
        Row::Heading("Typing a code"),
        Row::Entry("", "Just type. For example: 11-D-0704"),
        Row::Entry("Left Right", "move the cursor along the code"),
        Row::Entry("Home End", "jump to the start or the end of it"),
        Row::Entry("Ctrl+A", "select the whole code"),
        Row::Entry("Ctrl+V", "paste a code"),
        Row::Entry("Ctrl+C", "copy what you have selected"),
        Row::Entry("Ctrl+X", "cut what you have selected"),
        Row::Entry("Backspace", "delete the character before the cursor"),
        Row::Entry("Ctrl+W", "delete back to the previous dash"),
        Row::Entry("Ctrl+U", "delete the whole code"),
        Row::Entry("Esc", "clear what you have typed"),
        Row::Blank,
        Row::Heading("Narrowing it down"),
        Row::Entry("", "Matching is on any part of the name, and on the folder"),
        Row::Entry("", "it sits in. These narrow that:"),
        Row::Entry("0704*", "names that start with 0704"),
        Row::Entry("*0704", "names that end with 0704, before the file type"),
        Row::Entry("ext:pdf", "only PDFs. Use ext:dwg,pdf for more than one"),
        Row::Entry("F3", "step through start, end, and anywhere"),
        Row::Entry("F4", "step through the file types"),
        Row::Entry("", "F3 and F4 write into the box, so what you see is"),
        Row::Entry("", "always what will run."),
        Row::Blank,
        Row::Heading("Finding your file"),
        Row::Entry("Down", "step into the list of results"),
        Row::Entry("Up Down", "move through the list, scrolling it"),
        Row::Entry("PgUp PgDn", "move a screenful at a time"),
        Row::Entry("", "The bottom left of the box says which of the matches"),
        Row::Entry("", "you are looking at (13-24 of 300), so a long list is"),
        Row::Entry("", "something you can get to the end of."),
        Row::Entry("Esc", "go back to typing"),
        Row::Blank,
        Row::Heading("Opening it"),
        Row::Entry("Enter", opens),
        Row::Entry("F2", "switch between the ways of opening"),
        Row::Entry("", "The key itself, at the bottom right, names the one in"),
        Row::Entry("", "use."),
        Row::Blank,
        Row::Heading("Using the mouse"),
        Row::Entry("Click", "pick a result, or put the caret in the code"),
        Row::Entry("Double-click", "open that result"),
        Row::Entry("Drag", "select part of the code, to copy it"),
        Row::Entry("Shift+drag", "select anything on screen, as usual"),
        Row::Entry("Alt+drag", "move the box itself, from anywhere on it"),
        Row::Entry("", "The edges around the box move it too, without the"),
        Row::Entry("", "Alt. Wherever you leave it is where it comes back,"),
        Row::Entry("", "next time and next week. Settings has a button to"),
        Row::Entry("", "put that back to the middle."),
        Row::Blank,
        Row::Heading("Codes you used before"),
        Row::Entry("Up", "from an empty line, list them"),
        Row::Entry("Enter", "use the one you picked"),
        Row::Entry("Esc", "go back to what you were typing"),
        Row::Blank,
        Row::Heading("If something looks wrong"),
        Row::Entry("F5", "list the drives and how old each one is"),
        Row::Entry("Enter", "update the drive you picked"),
        Row::Entry("A", "update every drive"),
        Row::Entry("", "F5 is where the ages live: one line per drive, saying"),
        Row::Entry("", "how old each list is and which is worth updating."),
        Row::Entry("", "The line under the box stays empty while all is well,"),
        Row::Entry("", "and names the drive when it is not."),
        Row::Entry("", "Drives update themselves as files change, so this is"),
        Row::Entry("", "only needed when something looks missing."),
        Row::Blank,
        Row::Heading("Leaving"),
        Row::Entry("F1", "show or hide this list of keys"),
        Row::Entry("Ctrl+Q", "quit. Esc does not quit, and Ctrl+C copies"),
        Row::Blank,
    ]
}

// `title`, `footer`, `len` and `max_scroll` used to live here. They described
// an in-panel pane that this program scrolled itself, inside a border it drew
// itself, in a terminal. Help is an ordinary window now, with a real caption
// and a real scrollbar that the toolkit owns - so all four described something
// that no longer exists, and `max_scroll` still had a test pinning its
// arithmetic.

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel as plain text, which is what every assertion below is about.
    /// Reading the rows rather than a rendering keeps these tests true of the
    /// terminal and the window alike.
    fn text(viewer: ViewerKind) -> String {
        rows(viewer)
            .iter()
            .map(|row| match row {
                Row::Blank => String::new(),
                Row::Heading(text) => (*text).to_string(),
                Row::Entry(key, what) => format!("{key} {what}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The panel is the program's only documentation, so every key that does
    /// something has to be in it. A key that works and is written down nowhere
    /// may as well not exist.
    #[test]
    fn every_key_the_program_answers_is_written_down() {
        let t = text(ViewerKind::Pdf);
        for key in [
            "Ctrl+Q", "Ctrl+V", "Ctrl+W", "Esc", "Enter", "F2", "F5", "Up", "Down", "Left",
            "Right", "PgUp", "PgDn",
        ] {
            assert!(t.contains(key), "{key} is not in the help panel:\n{t}");
        }
    }

    /// Every heading and every description, held to the house style.
    ///
    /// The headings are the reason this is here. They were written in
    /// capitals (`TYPING A CODE`), which is styling smuggled into the text
    /// while the renderer was already drawing them bold, dim and spaced: two
    /// ways of saying "this is a heading", one of which the wording layer is
    /// not allowed to have an opinion about.
    #[test]
    fn every_row_keeps_the_house_style() {
        let mut lines = Vec::new();
        for viewer in ViewerKind::ALL {
            for entry in rows(viewer) {
                match entry {
                    Row::Blank => {}
                    Row::Heading(text) => lines.push(text),
                    Row::Entry(key, what) => {
                        lines.push(key);
                        lines.push(what);
                    }
                }
            }
        }
        crate::view::style::check_all(
            "the shortcuts window",
            lines.iter().copied(),
            crate::view::style::Slot::Body,
        );
    }

    /// A heading names a section; it does not shout one.
    #[test]
    fn no_heading_is_written_in_capitals() {
        for viewer in ViewerKind::ALL {
            for entry in rows(viewer) {
                if let Row::Heading(text) = entry {
                    assert_ne!(
                        text,
                        text.to_uppercase(),
                        "{text:?} is styling written into the words"
                    );
                    assert!(
                        crate::view::style::starts_capitalised(text),
                        "{text:?} does not start a sentence"
                    );
                }
            }
        }
    }

    /// The mouse works, and until this panel existed the program said so
    /// nowhere at all.
    #[test]
    fn the_mouse_is_documented() {
        let t = text(ViewerKind::Pdf);
        for gesture in ["Click", "Double-click", "Drag", "Shift+drag"] {
            assert!(t.contains(gesture), "{gesture} is undocumented:\n{t}");
        }
    }

    /// F2 changes what Enter does, so the panel has to describe the mode the
    /// program is actually in rather than a fixed sentence about one of them.
    #[test]
    fn the_panel_describes_the_viewer_that_is_active() {
        assert!(text(ViewerKind::Pdf).contains("one document"));
        assert!(text(ViewerKind::Avwin).contains("avwin"));
        assert!(!text(ViewerKind::Avwin).contains("one document"));
    }

    /// The two traps this program sets for anyone who has used another one.
    #[test]
    fn the_keys_that_do_not_do_what_you_expect_say_so() {
        let t = text(ViewerKind::Pdf);
        assert!(t.contains("Esc does not quit"), "{t}");
        assert!(t.contains("Ctrl+C copies"), "{t}");
    }

    /// The rows that were wrong rather than merely missing, which is worse:
    /// somebody who read them went looking for a thing that is not there.
    ///
    /// Left and Right moved between columns of a three-column grid, and PgUp
    /// and PgDn moved by a screenful of one. The grid went when the program
    /// became a panel with room for eight rows; the arrows became caret keys
    /// unconditionally, and the page keys became "the top" and "the bottom".
    #[test]
    fn the_panel_does_not_describe_the_grid_that_was_removed() {
        let t = text(ViewerKind::Pdf);
        assert!(
            !t.contains("column of results"),
            "Left and Right are caret keys now:
{t}"
        );
        assert!(
            !t.contains("a whole screen"),
            "the list is at most a screenful, so there is no screen to page:
{t}"
        );
    }

    /// The keys that were never written down at all. Each of them works, and
    /// a key that works and is documented nowhere may as well not.
    #[test]
    fn the_editing_keys_are_written_down_too() {
        let t = text(ViewerKind::Pdf);
        for key in ["Ctrl+A", "Ctrl+U", "Home", "End"] {
            assert!(
                t.contains(key),
                "{key} is not in the help panel:
{t}"
            );
        }
    }
}
