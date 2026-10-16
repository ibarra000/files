//! Opening the selected file. Every launch reports success or failure, and
//! configured viewers are probed once at startup.
//!
//! # Destinations
//!
//! - [`ViewerKind::App`]: the selected file, passed to a program named in a
//!   `[[viewer]]` table. None is built in; see [`crate::config::viewers`].
//! - [`ViewerKind::Auto`]: the selected file, opened by whatever the system has
//!   registered for its extension.
//! - [`ViewerKind::Pdf`]: the search input names a document whose pages are
//!   separate files. They are re-collected ([`crate::search::pages`]), merged
//!   ([`pdf`]) and opened ([`launch`]). The page set is rebuilt from the
//!   snapshot because the rows on screen are capped at `MAX_RESULTS` and ranked
//!   by match position, so they can be truncated and out of order.
//!
//! The work runs on the [`worker`] thread (one for the life of the process, as
//! `app::actors` requires) and reports back through `app::event::OpenMsg`.

pub mod launch;
pub mod pdf;
pub mod worker;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{ViewerApp, ViewerKind, Viewers};
use crate::search::pages::{self, PageGroup};

#[cfg(windows)]
pub use launch::shell_open;
pub use launch::{LaunchError, program_on_path};

/// Everything an open needs.
///
/// Carries the viewer rather than reading shared state, so `Cmd` can derive
/// `Eq` and tests can assert exactly what a keypress does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// The selected row.
    pub path: Arc<str>,
    /// The search input, from which the page group is rebuilt.
    pub query: String,
    pub viewer: ViewerKind,
    /// The list `viewer` indexes, as it was when the key was pressed.
    ///
    /// The worker's settings are from startup and may differ after the config
    /// file is edited; indexing the wrong list opens the wrong program.
    pub viewers: Arc<Viewers>,
}

/// Why an open failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    FileMissing,
    /// No file in the page group was usable. Carries the first reason.
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
    /// Number of pages handed over; 1 for a single file.
    pub pages: usize,
    /// Pages that were left out.
    pub skipped: Vec<pdf::Skipped>,
    /// The document has more than `MAX_PAGES` pages and is cut short.
    /// Surfaced so the user is told the document is incomplete.
    pub truncated: bool,
}

/// Dependencies of an open, passed in so the logic is testable without a
/// share, a viewer or a terminal.
pub struct OpenContext<'a> {
    /// The listing the search resolves to. `None` means the share did not
    /// answer, which degrades to opening the selected file, never to failing.
    pub snapshot: Option<&'a crate::index::snapshot::Snapshot>,
    pub cache_dir: Option<&'a Path>,
    pub pdf_viewer: Option<&'a Path>,
    /// Whether an assembled document is marked read-only.
    pub read_only: bool,
}

/// Where an open goes once the mode and the file are known.
///
/// Separate from [`ViewerKind`] because `Auto` hands the file over without
/// naming a program, which neither `Pdf` nor a viewer does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Every page of the document, merged.
    Document,
    /// One file, passed to the configured viewer at this index.
    App(usize),
    /// One file, opened by the system's registered handler.
    Shell,
}

/// Returns the route for a mode and a file name.
///
/// Pure and filesystem-free. Used by both [`open`] and the worker (to decide
/// whether a listing is needed), so the two cannot disagree.
pub fn route_of(viewer: ViewerKind, path: &str, viewers: &Viewers) -> Route {
    match viewer.within(viewers) {
        // Every file goes to the chosen viewer.
        ViewerKind::App(i) => Route::App(i),
        // Other types cannot be converted to PDF in-process, so a type claimed by
        // a configured viewer goes to that viewer. Otherwise the document
        // route, which opens a non-document file with the system handler.
        ViewerKind::Pdf => {
            let claimed = Path::new(path)
                .extension()
                .and_then(|e| viewers.claiming(&e.to_string_lossy()));
            claimed.map_or(Route::Document, Route::App)
        }
        ViewerKind::Auto => Route::Shell,
    }
}

/// Returns which configured viewers cannot be found, indexed like `viewers`.
///
/// Looks the programs up without running them, for the startup warning.
pub fn probe_viewers(viewers: &Viewers) -> Box<[bool]> {
    viewers
        .all()
        .iter()
        .map(|v| !launch::program_on_path(&v.program))
        .collect()
}

/// Opens what `request` asked for.
pub fn open(request: &OpenRequest, cx: &OpenContext<'_>) -> Result<Opened, OpenError> {
    match route_of(request.viewer, &request.path, &request.viewers) {
        // `route_of` only names a viewer the list has.
        Route::App(i) => match request.viewers.get(i) {
            Some(app) => open_one_with(app, &request.path),
            None => open_one_with_shell(&request.path, cx),
        },
        Route::Document => open_as_document(request, cx),
        Route::Shell => open_one_with_shell(&request.path, cx),
    }
}

/// Opens one file with the system's registered handler.
///
/// Checks existence first so a stale index reports the missing file itself
/// instead of leaving it to a shell dialog.
fn open_one_with_shell(path: &Arc<str>, cx: &OpenContext<'_>) -> Result<Opened, OpenError> {
    if !Path::new(path.as_ref()).exists() {
        return Err(OpenError::FileMissing);
    }
    launch::open_associated(path, cx.pdf_viewer).map_err(OpenError::Launch)?;
    Ok(Opened {
        path: Arc::clone(path),
        pages: 1,
        skipped: Vec::new(),
        truncated: false,
    })
}

/// Sets or clears the read-only attribute on an assembled document.
///
/// Applies only to files inside the cache; `pdf::assemble` returns the
/// original share path for a single page, and the share is never written.
/// Cached documents are named by content hash, so an annotation saved into one
/// would be served on every later open. Clearing is needed too, so turning the
/// option off frees documents marked by an earlier run. Failures are logged
/// and the open proceeds.
fn guard_cached(path: &str, cx: &OpenContext<'_>) {
    let Some(cache_dir) = cx.cache_dir else {
        return;
    };
    let path = Path::new(path);
    if !path.starts_with(cache_dir) {
        return;
    }
    if let Err(e) = pdf::set_read_only(path, cx.read_only) {
        log::warn!(
            "could not change the read-only attribute on {}: {e}",
            path.display()
        );
    }
}

fn open_one_with(app: &ViewerApp, path: &Arc<str>) -> Result<Opened, OpenError> {
    // Checked first so a stale index produces a clear message rather than a
    // confusing viewer error.
    if !Path::new(path.as_ref()).exists() {
        return Err(OpenError::FileMissing);
    }
    launch::open_with(&app.program, path).map_err(OpenError::Launch)?;
    Ok(Opened {
        path: Arc::clone(path),
        pages: 1,
        skipped: Vec::new(),
        truncated: false,
    })
}

fn open_as_document(request: &OpenRequest, cx: &OpenContext<'_>) -> Result<Opened, OpenError> {
    let group = collect_group(request, cx);

    // Falls back to the selected file alone when the share did not answer,
    // the group is empty, or the selected row is not part of the group (for
    // example `AB-C-0704 revision notes.pdf`).
    let (assembled, truncated) = match group {
        Some(group) if group.contains_path(&request.path) => {
            let Some(cache_dir) = cx.cache_dir else {
                // No cache to write the merge to; open the file alone.
                return open_one_as_pdf(&request.path, cx);
            };
            let truncated = group.capped;
            let assembled = pdf::assemble(&group.pages, cache_dir)
                .map_err(|e| OpenError::NothingUsable(e.detail()))?;
            (assembled, truncated)
        }
        _ => return open_one_as_pdf(&request.path, cx),
    };

    guard_cached(&assembled.path, cx);
    launch::open_associated(&assembled.path, cx.pdf_viewer).map_err(OpenError::Launch)?;
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
    launch::open_associated(path, cx.pdf_viewer).map_err(OpenError::Launch)?;
    Ok(Opened {
        path: Arc::clone(path),
        pages: 1,
        skipped: Vec::new(),
        truncated: false,
    })
}

/// Rebuilds the search input's page group, restricted to the selected file's folder.
///
/// With several mappings enabled, `Backend::snapshot_for` returns the first
/// matching target, which may not be the selected row's share. The folder
/// filter keeps pages from two shares out of one document.
fn collect_group(request: &OpenRequest, cx: &OpenContext<'_>) -> Option<PageGroup> {
    let snapshot = cx.snapshot?;
    let mut group = pages::collect(
        snapshot,
        &request.query,
        // Further typing must not cancel a requested open.
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
            viewers: Arc::new(sketch()),
        }
    }

    /// A single viewer that claims `tif`.
    fn sketch() -> Viewers {
        Viewers::new(vec![ViewerApp::new("sketch", "sketch.exe", &["tif"])])
    }

    /// Single-page documents open the share file directly, which must never be
    /// marked read-only.
    #[test]
    fn only_a_document_in_the_cache_is_ever_marked_read_only() {
        let cache = tempfile::tempdir().unwrap();
        let share = tempfile::tempdir().unwrap();

        let merged = cache.path().join("aaaa.pdf");
        std::fs::write(&merged, b"%PDF-1.5\n").unwrap();
        let on_the_share = share.path().join("AB-C-0704.pdf");
        std::fs::write(&on_the_share, b"%PDF-1.5\n").unwrap();

        let cx = OpenContext {
            snapshot: None,
            cache_dir: Some(cache.path()),
            pdf_viewer: None,
            read_only: true,
        };
        guard_cached(&merged.to_string_lossy(), &cx);
        guard_cached(&on_the_share.to_string_lossy(), &cx);

        let readonly = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().readonly();
        assert!(
            readonly(&merged),
            "the assembled document was not protected"
        );
        assert!(
            !readonly(&on_the_share),
            "an attribute was written to the share"
        );

        // Turning the option off clears the attribute again.
        let writable = OpenContext {
            read_only: false,
            ..cx
        };
        guard_cached(&merged.to_string_lossy(), &writable);
        assert!(
            !readonly(&merged),
            "turning it off did not give the file back"
        );
    }

    #[test]
    fn a_missing_file_is_reported_before_the_viewer_is_involved() {
        let cx = OpenContext {
            snapshot: None,
            cache_dir: None,
            pdf_viewer: None,
            read_only: true,
        };
        let err = open(
            &request(
                r"C:\definitely-not-here-4a91\nope.pdf",
                "nope",
                ViewerKind::App(0),
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
            read_only: true,
        };

        // sketch.exe is normally absent on a development machine.
        match open(
            &request(file.to_str().unwrap(), "real", ViewerKind::App(0)),
            &cx,
        ) {
            Err(OpenError::Launch(LaunchError::ViewerNotFound(name))) => {
                assert!(name.contains("sketch.exe"));
            }
            Err(other) => panic!("unexpected failure: {other:?}"),
            // The viewer is installed on this machine.
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
            OpenError::Launch(LaunchError::ViewerNotFound("sketch.exe".into()))
                .detail()
                .contains("sketch.exe")
        );
    }

    /// A selected row outside the page group is not treated as a member.
    #[test]
    fn a_selected_row_outside_the_group_is_not_part_of_a_document() {
        let snap = snapshot(
            r"R:\abc",
            &[
                "ab-c-0704.pdf",
                "ab-c-0704_Page1.pdf",
                "ab-c-0704 revision notes.pdf",
            ],
        );
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
            read_only: true,
        };

        let inside = request(r"R:\abc\ab-c-0704_Page1.pdf", "ab-c-0704", ViewerKind::Pdf);
        let outside = request(
            r"R:\abc\ab-c-0704 revision notes.pdf",
            "ab-c-0704",
            ViewerKind::Pdf,
        );

        let group = collect_group(&inside, &cx).expect("the search input has a document");
        assert_eq!(group.len(), 2);
        assert!(group.contains_path(&inside.path));
        assert!(!group.contains_path(&outside.path));
    }

    /// Pages from a different share than the selected row are excluded.
    #[test]
    fn a_group_never_reaches_outside_the_selected_files_folder() {
        let snap = snapshot(r"S:\archive", &["ab-c-0704.pdf", "ab-c-0704_Page1.pdf"]);
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
            read_only: true,
        };
        let picked = request(r"R:\abc\ab-c-0704.pdf", "ab-c-0704", ViewerKind::Pdf);

        assert!(
            collect_group(&picked, &cx).is_none(),
            "another share's pages must not be adopted"
        );
    }

    /// Without a listing, the open falls back to the selected file.
    #[test]
    fn no_listing_means_the_selected_file_is_still_opened() {
        let cx = OpenContext {
            snapshot: None,
            cache_dir: None,
            pdf_viewer: None,
            read_only: true,
        };
        let picked = request(
            r"C:\definitely-not-here-4a91\x.pdf",
            "ab-c-0704",
            ViewerKind::Pdf,
        );
        assert!(collect_group(&picked, &cx).is_none());

        // Fails on the missing file, not on the listing.
        assert_eq!(open(&picked, &cx).unwrap_err(), OpenError::FileMissing);
    }

    #[test]
    fn a_search_with_no_pages_at_all_has_no_group() {
        let snap = snapshot(r"R:\abc", &["something-else.pdf"]);
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
            read_only: true,
        };
        let picked = request(r"R:\abc\something-else.pdf", "ab-c-0704", ViewerKind::Pdf);
        assert!(collect_group(&picked, &cx).is_none());
    }

    /// Real files through listing, membership and merge to one document,
    /// stopping short of the launch.
    #[test]
    fn a_folder_of_pages_becomes_one_document_in_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // Out of order and with decoys.
        let mut names = vec![
            "ab-c-0704 revision notes.pdf".to_string(),
            "ab-c-0704_Page10.pdf".to_string(),
            "ab-c-0704.pdf".to_string(),
            "ab-c-0704_Page20.tif".to_string(),
        ];
        names.extend((1..=9).map(|i| format!("ab-c-0704_Page{i}.pdf")));
        names.extend((11..=18).map(|i| format!("ab-c-0704_Page{i}.pdf")));
        for name in &names {
            std::fs::write(dir.path().join(name), minimal_pdf()).unwrap();
        }

        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let snap = snapshot(&dir.path().to_string_lossy(), &refs);
        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: Some(cache.path()),
            pdf_viewer: None,
            read_only: true,
        };
        let picked = request(
            &dir.path().join("ab-c-0704.pdf").to_string_lossy(),
            "ab-c-0704",
            ViewerKind::Pdf,
        );

        let group = collect_group(&picked, &cx).expect("the search input has a document");
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

    /// A document is rebuilt from the search input, not from the rows on screen, so it
    /// can exceed `MAX_RESULTS`. No files are needed: `collect_group` reads
    /// only the snapshot.
    #[test]
    fn a_document_larger_than_the_result_cap_is_still_assembled_whole() {
        let pages = crate::config::MAX_RESULTS + 100;
        let mut names = vec!["ab-c-0704.pdf".to_string()];
        names.extend((1..=pages).map(|i| format!("ab-c-0704_Page{i}.pdf")));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let snap = snapshot("R:\\abc", &refs);

        let cx = OpenContext {
            snapshot: Some(&snap),
            cache_dir: None,
            pdf_viewer: None,
            read_only: true,
        };
        let picked = request("R:\\abc\\ab-c-0704.pdf", "ab-c-0704", ViewerKind::Pdf);

        let group = collect_group(&picked, &cx).expect("the search input has a document");
        assert_eq!(group.len(), pages + 1, "the bare file plus every page");
        assert!(
            group.len() > crate::config::MAX_RESULTS,
            "the fixture has to outgrow the cap or it proves nothing"
        );

        // A capped implementation would lose the tail.
        let order: Vec<Option<u32>> = group.pages.iter().map(|p| p.page).collect();
        assert_eq!(order.first(), Some(&None), "the bare file leads");
        assert_eq!(order.last(), Some(&Some(pages as u32)), "the last page");
    }

    /// Returns a minimal valid one-page PDF.
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

    #[test]
    fn route_of_sends_each_kind_of_file_to_the_right_place() {
        use ViewerKind::{App, Auto, Pdf};
        let viewers = sketch();
        let sketch = App(0);

        for (viewer, path, want) in [
            // Auto: every file goes to the system.
            (Auto, r"R:\abc\AB-C-0704.pdf", Route::Shell),
            (Auto, r"R:\abc\AB-C-0704.TIF", Route::Shell),
            (Auto, r"R:\abc\notes.docx", Route::Shell),
            (Auto, r"R:\abc\scan.tif", Route::Shell),
            (Auto, r"R:\abc\README", Route::Shell),
            // Pdf: documents are assembled; a claimed type goes to its viewer.
            (Pdf, r"R:\abc\AB-C-0704.pdf", Route::Document),
            (Pdf, r"R:\abc\AB-C-0704.tif", Route::App(0)),
            (Pdf, r"R:\abc\notes.docx", Route::Document),
            // App: every file goes to the viewer.
            (sketch, r"R:\abc\AB-C-0704.pdf", Route::App(0)),
            (sketch, r"R:\abc\AB-C-0704.tif", Route::App(0)),
            (sketch, r"R:\abc\notes.docx", Route::App(0)),
        ] {
            assert_eq!(route_of(viewer, path, &viewers), want, "{viewer:?} {path}");
        }
    }

    /// With no viewer claiming its type, PDF mode sends a file down the document
    /// route.
    #[test]
    fn with_no_viewers_an_unclaimed_file_takes_the_document_route() {
        let none = Viewers::default();
        assert_eq!(
            route_of(ViewerKind::Pdf, r"R:\abc\AB-C-0704.tif", &none),
            Route::Document
        );
    }

    /// An out-of-range viewer index routes to the shell.
    #[test]
    fn a_viewer_that_is_gone_goes_to_the_shell() {
        let none = Viewers::default();
        assert_eq!(
            route_of(ViewerKind::App(2), r"R:\abc\a.pdf", &none),
            Route::Shell
        );
        assert_eq!(
            route_of(ViewerKind::App(1), r"R:\abc\a.pdf", &sketch()),
            Route::Shell
        );
    }

    /// Extension matching is case-insensitive. Tested under `Pdf`, the only
    /// mode that reads the extension.
    #[test]
    fn the_extension_is_matched_however_it_is_spelled() {
        for spelling in ["tif", "TIF", "Tif", "tIf"] {
            let path = format!(r"R:\abc\AB-C-0704.{spelling}");
            assert_eq!(route_of(ViewerKind::Pdf, &path, &sketch()), Route::App(0));
        }
    }
}
