//! What the pane beside the list says about a file.
//!
//! Wording only, like everything else here: no colour, no rectangle, no widget.
//! [`crate::gui::preview`] decides where these lines go and how big they are.
//!
//! # What the pane is for, and what it is therefore allowed to say
//!
//! It exists for one moment: two rows whose names differ by a character, and
//! the question of which one to open. So the lines are ordered by how well they
//! settle that question - the size and the date first, because two pages of one
//! drawing set differ there and nowhere else; then the page set, because
//! "13 pages" confirms the code was the right one; then the folder, which is
//! the slowest thing to read and the least often needed.
//!
//! It is not a properties dialogue. Nothing here reports a permission bit, an
//! attribute, or a path split into its parts, because none of those distinguish
//! two rows of a result list.

use crate::preview::{Facts, Preview};
use crate::util::humanize;

/// One line of the pane, and how much weight it carries.
///
/// The emphasis is a *role*, not an appearance: `gui::preview` maps these onto
/// the palette, so this module can order the pane by importance without
/// choosing a colour - which is the split `view` exists to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// The file name. One per pane, at the top.
    Name,
    /// Size, date, share: the facts that separate two similar rows.
    Fact,
    /// A heading over the page list.
    Heading,
    /// One page of the document this file belongs to.
    Page,
    /// The folder, and anything else worth saying quietly.
    Quiet,
}

/// A line to draw, and its role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub line: Line,
    pub text: String,
}

fn row(line: Line, text: impl Into<String>) -> Row {
    Row {
        line,
        text: text.into(),
    }
}

/// What to draw when nothing is under the pointer.
///
/// A sentence rather than an empty pane, because an empty rectangle beside a
/// full list reads as something that failed to load. `None` once a file *is*
/// selected, so the caller does not have to decide which of the two it is
/// looking at.
pub fn idle() -> Vec<Row> {
    vec![row(Line::Quiet, "Point at a result to see what it is.")]
}

/// The pane for one file.
///
/// `wall` is passed in rather than read, for the reason every other age readout
/// in this program takes it: a clock read inside a renderer is a clock that
/// makes two frames of the same state differ, and the snapshot tests could not
/// pin anything that did it.
pub fn rows(preview: &Preview, wall: std::time::SystemTime) -> Vec<Row> {
    let mut out = vec![row(Line::Name, preview.name.as_ref())];

    if preview.facts.missing {
        // Said plainly and first. A file that has been deleted since the index
        // was built is the one case where every other line would be a lie, and
        // it is also the case somebody most needs told - Enter on this row is
        // about to fail.
        out.push(row(Line::Fact, "Not there any more"));
        out.push(row(
            Line::Quiet,
            "The drive has changed since this list was built.",
        ));
        push_folder(&mut out, &preview.facts);
        return out;
    }

    if let Some(facts) = size_and_age(&preview.facts, wall) {
        out.push(row(Line::Fact, facts));
    }
    if let Some(share) = &preview.facts.share {
        out.push(row(Line::Fact, format!("On {share}")));
    }

    if let Some(pages) = &preview.pages {
        out.push(row(Line::Heading, pages_heading(pages)));
        for name in &pages.named {
            out.push(row(Line::Page, name.as_ref()));
        }
        if pages.total > pages.named.len() {
            let rest = pages.total - pages.named.len();
            out.push(row(Line::Page, format!("and {rest} more")));
        }
    }

    push_folder(&mut out, &preview.facts);
    out
}

fn push_folder(out: &mut Vec<Row>, facts: &Facts) {
    if let Some(folder) = &facts.folder {
        out.push(row(Line::Quiet, folder.as_ref()));
    }
}

/// `2.3 MiB · changed 4h ago`, or whichever half is known.
///
/// Joined with ` · ` rather than written as two lines because they are read
/// together: "how big and how recent" is one judgement, and splitting it costs
/// a line the folder needs.
fn size_and_age(facts: &Facts, wall: std::time::SystemTime) -> Option<String> {
    let size = facts.bytes.map(humanize::bytes);
    let age = facts
        .modified
        .and_then(|at| wall.duration_since(at).ok())
        .map(|d| match humanize::age(d).as_str() {
            // `age` answers the first five seconds with a phrase rather than a
            // duration, and "changed just now ago" is what wrapping it
            // unconditionally produces. Worth the special case because a file
            // saved while somebody is looking for it is not a rare event on a
            // shared drawing drive - it is the busy afternoon this program
            // exists for.
            "just now" => "changed just now".to_string(),
            elapsed => format!("changed {elapsed} ago"),
        });

    match (size, age) {
        (Some(size), Some(age)) => Some(format!("{size} \u{b7} {age}")),
        (Some(size), None) => Some(size),
        (None, Some(age)) => Some(age),
        (None, None) => None,
    }
}

/// `13 pages in this set`, and the honest version when the sweep was capped.
fn pages_heading(pages: &crate::preview::Pages) -> String {
    if pages.capped {
        // "At least" rather than a number that is quietly a floor. The cap
        // exists to stop a pathological set of names becoming an unbounded
        // sweep, and a document that hit it is one where the count on screen
        // would otherwise be wrong in a way nobody could detect.
        format!("At least {} pages in this set", pages.total)
    } else {
        format!("{} pages in this set", pages.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{Facts, Pages, Preview};
    use crate::view::style::{self, Slot};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    const WALL: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn preview(facts: Facts, pages: Option<Pages>) -> Preview {
        Preview {
            path: Arc::from(r"R:\11d\11-D-0704\11-D-0704_Page3.pdf"),
            name: Arc::from("11-D-0704_Page3.pdf"),
            facts,
            pages,
        }
    }

    fn text(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_name_is_the_first_thing_the_pane_says() {
        let rows = rows(&preview(Facts::default(), None), WALL);
        assert_eq!(rows[0].line, Line::Name);
        assert_eq!(rows[0].text, "11-D-0704_Page3.pdf");
    }

    #[test]
    fn the_size_and_the_date_are_read_as_one_fact() {
        let facts = Facts {
            bytes: Some(2_411_724),
            modified: Some(at(0)),
            ..Facts::default()
        };
        let shown = text(&rows(&preview(facts, None), at(3600 * 4)));
        assert!(shown.contains("2.3 MiB \u{b7} changed 4h ago"), "{shown}");
    }

    /// Either half alone is still worth a line. A share that answers with a
    /// size and no usable timestamp is an ordinary thing.
    #[test]
    fn half_a_fact_is_still_drawn() {
        let only_size = Facts {
            bytes: Some(1024),
            ..Facts::default()
        };
        assert!(text(&rows(&preview(only_size, None), WALL)).contains("1.0 KiB"));

        let only_age = Facts {
            modified: Some(at(0)),
            ..Facts::default()
        };
        let shown = text(&rows(&preview(only_age, None), at(60)));
        assert!(shown.contains("changed 1m ago"), "{shown}");
    }

    /// A file deleted since the index was built says so before anything else,
    /// because every other line would be about a file that is not there.
    #[test]
    fn a_missing_file_says_so_first_and_claims_nothing_else() {
        let facts = Facts {
            missing: true,
            folder: Some(r"R:\11d\11-D-0704".into()),
            ..Facts::default()
        };
        let rows = rows(&preview(facts, None), WALL);
        let shown = text(&rows);

        assert_eq!(rows[1].text, "Not there any more", "{shown}");

        // Checked by role rather than by substring: the explanatory line below
        // legitimately contains the word "changed", and an assertion that
        // cannot tell that from a `changed 4h ago` readout is one that would
        // start failing on a rewording which broke nothing.
        let facts: Vec<&str> = rows
            .iter()
            .filter(|r| r.line == Line::Fact)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            facts,
            ["Not there any more"],
            "a missing file reported facts about itself:\n{shown}"
        );
        assert!(shown.contains(r"R:\11d\11-D-0704"), "{shown}");
    }

    #[test]
    fn a_page_set_is_counted_and_the_first_few_are_named() {
        let pages = Pages {
            total: 13,
            named: vec![
                Arc::from("11-D-0704.pdf"),
                Arc::from("11-D-0704_Page1.pdf"),
                Arc::from("11-D-0704_Page2.pdf"),
                Arc::from("11-D-0704_Page3.pdf"),
            ],
            capped: false,
        };
        let shown = text(&rows(&preview(Facts::default(), Some(pages)), WALL));

        assert!(shown.contains("13 pages in this set"), "{shown}");
        assert!(shown.contains("11-D-0704_Page2.pdf"), "{shown}");
        assert!(shown.contains("and 9 more"), "{shown}");
    }

    /// A capped sweep must not present a floor as a count.
    #[test]
    fn a_capped_page_set_does_not_claim_an_exact_count() {
        let pages = Pages {
            total: 512,
            named: vec![Arc::from("11-D-0704.pdf")],
            capped: true,
        };
        let shown = text(&rows(&preview(Facts::default(), Some(pages)), WALL));
        assert!(shown.contains("At least 512 pages"), "{shown}");
    }

    /// A single file says nothing about pages at all. "1 page in this set" is a
    /// claim about a naming scheme it does not follow.
    #[test]
    fn a_file_that_stands_alone_says_nothing_about_pages() {
        let shown = text(&rows(&preview(Facts::default(), None), WALL));
        assert!(!shown.contains("page"), "{shown}");
        assert!(!shown.contains("set"), "{shown}");
    }

    #[test]
    fn an_empty_pane_explains_itself_rather_than_sitting_blank() {
        assert!(!idle().is_empty());
    }

    /// The house style, over every line this module can produce.
    #[test]
    fn every_line_keeps_the_house_style() {
        let mut lines: Vec<String> = idle().into_iter().map(|r| r.text).collect();

        let cases = [
            (Facts::default(), None),
            (
                Facts {
                    bytes: Some(0),
                    modified: Some(at(0)),
                    share: Some("jobs".into()),
                    folder: Some(r"R:\11d".into()),
                    missing: false,
                },
                Some(Pages {
                    total: 13,
                    named: vec![Arc::from("11-D-0704.pdf")],
                    capped: false,
                }),
            ),
            (
                Facts {
                    missing: true,
                    folder: Some(r"R:\11d".into()),
                    ..Facts::default()
                },
                None,
            ),
            (
                Facts::default(),
                Some(Pages {
                    total: 512,
                    named: Vec::new(),
                    capped: true,
                }),
            ),
        ];
        for (facts, pages) in cases {
            for r in rows(&preview(facts, pages), at(90_000)) {
                lines.push(r.text);
            }
        }

        // A file name is not prose - `11-D-0704_Page3.pdf` keeps its own
        // spelling, and so does a folder - so the corpus checked is the lines
        // this module *writes*, not the ones it passes through.
        let written: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|line| !line.contains('\\') && !line.ends_with(".pdf"))
            .collect();
        style::check_all("the preview pane", written, Slot::Body);
    }
}
