//! Opening what the user chose.
//!
//! The original implementation was
//! `let _ = Command::new("avwin.exe").arg(path).spawn();`, so a missing viewer
//! or a deleted file produced exactly nothing: the user pressed Enter and the
//! program appeared to ignore them. Every launch is reported either way now,
//! and `avwin.exe` is probed once at startup so its absence is learned before
//! it is needed rather than after.
//!
//! # Two viewers, one keypress
//!
//! [`ViewerKind::Avwin`] is that original behaviour: the one file under the
//! cursor, handed to one program.
//!
//! [`ViewerKind::Pdf`] treats the typed code as naming a *document*. The pages
//! of a drawing set are separate files on the share, so it re-collects them
//! ([`crate::search::pages`]), merges them ([`pdf`]) and opens the result
//! ([`launch`]). The page set is deliberately rebuilt from the snapshot rather
//! than taken from the rows on screen: those are capped at `MAX_RESULTS` and
//! ranked by match position, so a twenty-page set would arrive truncated and
//! out of order.
//!
//! # Why the work does not happen here
//!
//! Merging tens of files off an SMB share takes real time, so it runs on the
//! worker in [`worker`] - one thread for the life of the process, as
//! `app::actors` requires - and reports back through [`OpenMsg`]. Nothing in
//! this module blocks the UI thread.

pub mod launch;
pub mod pdf;
pub mod worker;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::ViewerKind;
use crate::search::pages::{self, PageGroup};

pub use launch::{AVWIN, LaunchError, avwin_available};

/// Everything an open needs, gathered where it is still testable.
///
/// Carries the viewer rather than reading it from shared state: `Cmd` derives
/// `Eq` precisely so a test can assert what a keypress will do, and a command
/// whose effect depended on a field some other thread might change is not that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// The row the user was on.
    pub path: Arc<str>,
    /// The typed code, which the page group is rebuilt from.
    pub query: String,
    pub viewer: ViewerKind,
}

/// Why an open failed outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    FileMissing,
    /// Nothing in the page group could be used. Carries the first reason, so
    /// the message says which file and why rather than only that it failed.
    NothingUsable(String),
    Launch(LaunchError),
}

impl OpenError {
    pub fn detail(&self) -> String {
        match self {
            Self::FileMissing => "the file no longer exists".into(),
            Self::NothingUsable(detail) => detail.clone(),
            Self::Launch(e) => e.detail(),
        }
    }
}

/// What an open produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub path: Arc<str>,
    /// How many pages were handed over. One for `Avwin`, so that path is not a
    /// special case anywhere downstream.
    pub pages: usize,
    /// Pages that were left out, if any.
    pub skipped: Vec<pdf::Skipped>,
    /// The code has more pages than `MAX_PAGES` and this document stops short
    /// of the end. Carried rather than dropped: a document that is missing its
    /// tail with nothing on screen to say so is the outcome this whole path
    /// exists to avoid.
    pub truncated: bool,
}

/// Everything the open needs from the rest of the program.
///
/// Passed in rather than reached for, so the whole decision is exercisable
/// without a share, a viewer or a terminal.
pub struct OpenContext<'a> {
    /// The listing the code resolves to, if one could be obtained. `None`
    /// means the share did not answer - which must degrade to opening the
    /// selected file, never to failing.
    pub snapshot: Option<&'a crate::index::snapshot::Snapshot>,
    pub cache_dir: Option<&'a Path>,
    pub pdf_viewer: Option<&'a Path>,
}

/// Opens what `request` asked for.
pub fn open(request: &OpenRequest, cx: &OpenContext<'_>) -> Result<Opened, OpenError> {
    match request.viewer {
        ViewerKind::Avwin => open_one_with_avwin(&request.path),
        ViewerKind::Pdf => open_as_document(request, cx),
    }
}

fn open_one_with_avwin(path: &Arc<str>) -> Result<Opened, OpenError> {
    // Checked first so a stale index produces a clear message rather than a
    // confusing viewer error.
    if !Path::new(path.as_ref()).exists() {
        return Err(OpenError::FileMissing);
    }
    launch::open_avwin(path).map_err(OpenError::Launch)?;
    Ok(Opened {
        path: Arc::clone(path),
        pages: 1,
        skipped: Vec::new(),
        truncated: false,
    })
}

fn open_as_document(request: &OpenRequest, cx: &OpenContext<'_>) -> Result<Opened, OpenError> {
    let group = collect_group(request, cx);

    // Three ways to end up opening the one file the cursor was on, and all
    // three are deliberate:
    //
    //  * the share did not answer, so there is no group to build;
    //  * the selected row is not part of this code's document - it might be
    //    `11-D-0704 revision notes.pdf` - and assembling the group would open
    //    something the user did not point at;
    //  * the group is empty, which means the same thing.
    let (assembled, truncated) = match group {
        Some(group) if group.contains_path(&request.path) => {
            let Some(cache_dir) = cx.cache_dir else {
                // Nowhere to write the merge, so degrade rather than refuse.
                return open_one_as_pdf(&request.path, cx);
            };
            let truncated = group.capped;
            let assembled = pdf::assemble(&group.pages, cache_dir)
                .map_err(|e| OpenError::NothingUsable(e.detail()))?;
            (assembled, truncated)
        }
        _ => return open_one_as_pdf(&request.path, cx),
    };

    launch::open_pdf(&assembled.path, cx.pdf_viewer).map_err(OpenError::Launch)?;
    Ok(Opened {
        path: assembled.path,
        pages: assembled.pages,
        skipped: assembled.skipped,
        truncated,
    })
}

/// Opens a single file through the PDF route, without merging anything.
fn open_one_as_pdf(path: &Arc<str>, cx: &OpenContext<'_>) -> Result<Opened, OpenError> {
    if !Path::new(path.as_ref()).exists() {
        return Err(OpenError::FileMissing);
    }
    launch::open_pdf(path, cx.pdf_viewer).map_err(OpenError::Launch)?;
    Ok(Opened {
        path: Arc::clone(path),
        pages: 1,
        skipped: Vec::new(),
        truncated: false,
    })
}

/// Rebuilds the code's page group, restricted to the selected file's folder.
///
/// The directory filter matters once more than one mapping is enabled: the
/// query is re-resolved on this thread and `Backend::snapshot_for` answers
/// with the *first* matching target, which need not be the share the selected
/// row came from. Without this, a code present in two shares could quietly
/// produce a document made of pages from both.
fn collect_group(request: &OpenRequest, cx: &OpenContext<'_>) -> Option<PageGroup> {
    let snapshot = cx.snapshot?;
    let mut group = pages::collect(
        snapshot,
        &request.query,
        // An open is not superseded by anything. The user carrying on typing
        // must not cancel the document they just asked for.
        &crate::util::cancel::CancelToken::never(),
    );
    if group.cancelled {
        return None;
    }

    if let Some(dir) = parent_of(&request.path) {
        group.pages.retain(|p| {
            parent_of(&p.path).is_some_and(|d| crate::util::winpath::same_dir(&d, &dir))
        });
    }
    (!group.is_empty()).then_some(group)
}

fn parent_of(path: &str) -> Option<PathBuf> {
    Path::new(path).parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;
    use crate::index::snapshot::Snapshot;
    use std::time::SystemTime;

    fn snapshot(prefix: &str, names: &[&str]) -> Snapshot {
        let mut b = SnapshotBuilder::with_capacity(prefix, names.len(), 32);
        for n in names {
            b.push_str(n);
        }
        b.finish(SystemTime::UNIX_EPOCH, 0, None)
    }

    fn request(path: &str, query: &str, viewer: ViewerKind) -> OpenRequest {
        OpenRequest {
            path: Arc::from(path),
            query: query.to_string(),
            viewer,
        }
    }

    #[test]
    fn a_missing_file_is_reported_before_the_viewer_is_involved() {
        let cx = OpenContext {
            snapshot: None,
            cache_dir: None,
            pdf_viewer: None,
        };
        let err = open(
            &request(
                r"C:\definitely-not-here-4a91\nope.pdf",
                "nope",
                ViewerKind::Avwin,
            ),
            &cx,
        )
        .unwrap_err();
        assert_eq!(err, OpenError::FileMissing);
        assert!(err.detail().contains("no longer exists"));
    }

    #[test]
    fn a_missing_viewer_produces_an_actionable_message() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("real.pdf");
        std::fs::write(&file, b"x").unwrap();
        let cx = OpenContext {
            snapshot: None,
            cache_dir: None,
            pdf_viewer: None,
        };

        // avwin.exe is not present on a development machine, so this
        // exercises the real path.
        match open(
            &request(file.to_str().unwrap(), "real", ViewerKind::Avwin),
            &cx,
        ) {
            Err(OpenError::Launch(LaunchError::ViewerNotFound(name))) => {
                assert!(name.contains("avwin.exe"));
            }
            Err(other) => panic!("unexpected failure: {other:?}"),
            // A machine that genuinely has the viewer installed.
            Ok(_) => {}
        }
    }

    #[test]
    fn every_error_carries_a_usable_detail() {
        assert!(!OpenError::FileMissing.detail().is_empty());
        assert_eq!(
            OpenError::NothingUsable("a.pdf (not a PDF)".into()).detail(),
            "a.pdf (not a PDF)"
        );
        assert!(
            OpenError::Launch(LaunchError::ViewerNotFound(AVWIN.into()))
                .detail()
                .contains(AVWIN)
        );
    }

    /// The row the cursor is on decides which document is opened, so a row
    /// that belongs to no document opens alone rather than dragging in a set
    /// the user was not pointing at.
    #[test]
    fn a_selected_row_outside_the_group_is_not_part_of_a_document() {
        let snap = snapshot(
            r"R:\11d",
            &[
                "11-d-0704.pdf",
                "11-d-0704_Page1.pdf",
                "11-d-0704 revision notes.pdf",
            ],
        );
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
        };

        let inside = request(r"R:\11d\11-d-0704_Page1.pdf", "11-d-0704", ViewerKind::Pdf);
        let outside = request(
            r"R:\11d\11-d-0704 revision notes.pdf",
            "11-d-0704",
            ViewerKind::Pdf,
        );

        let group = collect_group(&inside, &cx).expect("the code has a document");
        assert_eq!(group.len(), 2);
        assert!(group.contains_path(&inside.path));
        assert!(!group.contains_path(&outside.path));
    }

    /// Once two mappings are enabled the query can re-resolve to a different
    /// share than the row came from. Pages from two shares are never one
    /// document.
    #[test]
    fn a_group_never_reaches_outside_the_selected_files_folder() {
        let snap = snapshot(r"S:\archive", &["11-d-0704.pdf", "11-d-0704_Page1.pdf"]);
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
        };
        let picked = request(r"R:\11d\11-d-0704.pdf", "11-d-0704", ViewerKind::Pdf);

        assert!(
            collect_group(&picked, &cx).is_none(),
            "another share's pages must not be adopted"
        );
    }

    /// A cold job folder is one round trip, and the prefetcher usually has it
    /// warm - but "usually" is not a guarantee, and a slow share must not turn
    /// Enter into a failure.
    #[test]
    fn no_listing_means_the_selected_file_is_still_opened() {
        let cx = OpenContext {
            snapshot: None,
            cache_dir: None,
            pdf_viewer: None,
        };
        let picked = request(
            r"C:\definitely-not-here-4a91\x.pdf",
            "11-d-0704",
            ViewerKind::Pdf,
        );
        assert!(collect_group(&picked, &cx).is_none());

        // It still tries, and fails on the file rather than on the listing.
        assert_eq!(open(&picked, &cx).unwrap_err(), OpenError::FileMissing);
    }

    #[test]
    fn a_code_with_no_pages_at_all_has_no_group() {
        let snap = snapshot(r"R:\11d", &["something-else.pdf"]);
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
        };
        let picked = request(r"R:\11d\something-else.pdf", "11-d-0704", ViewerKind::Pdf);
        assert!(collect_group(&picked, &cx).is_none());
    }

    /// The seam nothing else covers: real files on disk, through the listing,
    /// the membership rule and the merge, to one document. Stops short of the
    /// launch, which would open a window.
    ///
    /// Deliberately more than fifteen pages - `MAX_RESULTS`, and the whole
    /// reason the group is rebuilt from the code rather than from the rows on
    /// screen.
    #[test]
    fn a_folder_of_pages_becomes_one_document_in_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // Named out of order and with decoys, exactly as a share would be.
        let mut names = vec![
            "11-d-0704 revision notes.pdf".to_string(),
            "11-d-0704_Page10.pdf".to_string(),
            "11-d-0704.pdf".to_string(),
            "11-d-0704_Page20.tif".to_string(),
        ];
        names.extend((1..=9).map(|i| format!("11-d-0704_Page{i}.pdf")));
        names.extend((11..=18).map(|i| format!("11-d-0704_Page{i}.pdf")));
        for name in &names {
            std::fs::write(dir.path().join(name), minimal_pdf()).unwrap();
        }

        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let snap = snapshot(&dir.path().to_string_lossy(), &refs);
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: Some(cache.path()),
            pdf_viewer: None,
        };
        let picked = request(
            &dir.path().join("11-d-0704.pdf").to_string_lossy(),
            "11-d-0704",
            ViewerKind::Pdf,
        );

        let group = collect_group(&picked, &cx).expect("the code has a document");
        assert_eq!(group.len(), 19, "the bare file plus pages 1..18");

        let order: Vec<Option<u32>> = group.pages.iter().map(|p| p.page).collect();
        let mut expected = vec![None];
        expected.extend((1..=18).map(Some));
        assert_eq!(order, expected, "bare first, then numerically");

        let doc = pdf::assemble(&group.pages, cache.path()).unwrap();
        assert_eq!(doc.pages, 19);
        assert!(doc.merged && doc.skipped.is_empty());
        assert!(std::path::Path::new(doc.path.as_ref()).is_file());
    }

    /// A document is rebuilt from the code, not from the rows on screen.
    ///
    /// This used to be a clause on the assembly test above, which compared a
    /// nineteen-page document against a fifteen-row display cap. The cap is
    /// three hundred now, so nineteen pages no longer tell the two
    /// implementations apart - every page would be on screen either way and a
    /// version that reused the visible rows would pass. Hence a document
    /// deliberately larger than the cap, and no files on disk, since
    /// `collect_group` reads the snapshot and never the filesystem.
    #[test]
    fn a_document_larger_than_the_result_cap_is_still_assembled_whole() {
        let pages = crate::config::MAX_RESULTS + 100;
        let mut names = vec!["11-d-0704.pdf".to_string()];
        names.extend((1..=pages).map(|i| format!("11-d-0704_Page{i}.pdf")));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let snap = snapshot("R:\\11d", &refs);

        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
        };
        let picked = request("R:\\11d\\11-d-0704.pdf", "11-d-0704", ViewerKind::Pdf);

        let group = collect_group(&picked, &cx).expect("the code has a document");
        assert_eq!(group.len(), pages + 1, "the bare file plus every page");
        assert!(
            group.len() > crate::config::MAX_RESULTS,
            "the fixture has to outgrow the cap or it proves nothing"
        );

        // The tail is what a capped implementation would have lost.
        let order: Vec<Option<u32>> = group.pages.iter().map(|p| p.page).collect();
        assert_eq!(order.first(), Some(&None), "the bare file leads");
        assert_eq!(order.last(), Some(&Some(pages as u32)), "the last page");
    }

    /// A one-page PDF, as terse as a valid one gets.
    fn minimal_pdf() -> Vec<u8> {
        use lopdf::{Document, Object, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog", "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }
}
