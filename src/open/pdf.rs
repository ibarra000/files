//! Merges the separate page PDFs of one document into one cached file.
//!
//! # Inherited page attributes
//!
//! `/MediaBox`, `/Resources`, `/CropBox` and `/Rotate` may be inherited from an
//! ancestor `/Pages` node, so reparenting a page can silently change its paper
//! size, crop or content. [`resolve_inherited`] copies them onto each page
//! before it is moved (pinned by
//! `a_page_inheriting_its_mediabox_keeps_it_after_reparenting`).
//!
//! # Content-addressed output
//!
//! The file is named by a hash of its inputs, so a document is written once and
//! never overwritten. This matters on Windows, where renaming over a file a
//! viewer holds open fails; `index::persist` uses the same approach.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use lopdf::{Document, Object, ObjectId, dictionary};

use crate::search::pages::Page;
use crate::util::winpath;

/// Keys a page may inherit from an ancestor `/Pages` node: the complete list
/// from the PDF specification.
const INHERITABLE: [&[u8]; 4] = [b"MediaBox", b"Resources", b"CropBox", b"Rotate"];

/// Maximum `/Parent` chain depth before it is treated as a cycle, so a
/// malformed file cannot hang the open thread.
const MAX_PARENT_DEPTH: usize = 32;

/// Why one page could not be used. Shown to the user next to the file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Deleted since the last index scan, which can be minutes old.
    Gone,
    Unreadable(String),
    NotPdf,
    Malformed,
    Encrypted,
    NoPages,
    /// Some of its pages had unreadable dictionaries and were left out.
    PagesUnreadable {
        lost: usize,
    },
    /// Read back from a previous merge's note, where name and reason are
    /// already one rendered line.
    Recorded,
}

impl SkipReason {
    pub fn detail(&self) -> String {
        match self {
            Self::Gone => "no longer exists".into(),
            Self::Unreadable(e) => e.clone(),
            Self::NotPdf => "not a PDF".into(),
            Self::Malformed => "could not be read".into(),
            Self::Encrypted => "password protected".into(),
            Self::NoPages => "has no pages".into(),
            Self::PagesUnreadable { lost } => {
                format!("{lost} of its pages could not be read")
            }
            // Already a rendered "name (reason)" line; `describe` returns it
            // as is.
            Self::Recorded => String::new(),
        }
    }
}

/// A page that did not make it into the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub name: Arc<str>,
    pub reason: SkipReason,
}

impl Skipped {
    pub fn describe(&self) -> String {
        match self.reason {
            SkipReason::Recorded => self.name.to_string(),
            _ => format!("{} ({})", self.name, self.reason.detail()),
        }
    }
}

/// Why no document could be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembleError {
    /// Every page was skipped. Carries them so the report can say why.
    NothingUsable(Vec<Skipped>),
    /// The cache directory could not be created or written.
    Io(String),
}

impl AssembleError {
    pub fn detail(&self) -> String {
        match self {
            Self::NothingUsable(skipped) => match skipped.first() {
                Some(first) if skipped.len() == 1 => first.describe(),
                Some(first) => format!("{} and {} more", first.describe(), skipped.len() - 1),
                None => "nothing to open".into(),
            },
            Self::Io(e) => e.clone(),
        }
    }
}

/// A document ready to hand to a viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// The merged file, or the original page for a group of one.
    pub path: Arc<str>,
    pub pages: usize,
    /// Members that were left out, in the order they appeared.
    pub skipped: Vec<Skipped>,
    /// True when `path` names a file this function produced.
    pub merged: bool,
    /// True when the merged file was already on disk from a previous open.
    pub reused: bool,
}

/// Builds one PDF from `pages`, writing it under `cache_dir`.
///
/// `pages` must already be in merge order (see [`crate::search::pages`]).
pub fn assemble(pages: &[Page], cache_dir: &Path) -> Result<Assembled, AssembleError> {
    if pages.is_empty() {
        return Err(AssembleError::NothingUsable(Vec::new()));
    }

    // A single page is opened in place after validation; no copy is written.
    if pages.len() == 1 {
        let page = &pages[0];
        return match probe(&page.path) {
            Some(reason) => Err(AssembleError::NothingUsable(vec![Skipped {
                name: Arc::clone(&page.name),
                reason,
            }])),
            None => Ok(Assembled {
                path: Arc::clone(&page.path),
                pages: 1,
                skipped: Vec::new(),
                merged: false,
                reused: false,
            }),
        };
    }

    let dir = cache_dir.join("pdf");
    let key = format!("{:016x}", cache_key(pages));
    let out = dir.join(format!("{key}.pdf"));
    let note = dir.join(format!("{key}.txt"));

    // A cache hit does no I/O beyond the note, so repeated opens need no
    // in-flight guard. The note records which pages the original merge
    // skipped, so a hit reports the same page count and skips.
    if out.is_file()
        && let Some(skipped) = read_note(&note)
    {
        return Ok(Assembled {
            path: Arc::from(out.to_string_lossy().as_ref()),
            pages: pages.len() - skipped.len(),
            skipped,
            merged: true,
            reused: true,
        });
    }

    let (doc, count, skipped) = merge(pages);
    if count == 0 {
        return Err(AssembleError::NothingUsable(skipped));
    }

    std::fs::create_dir_all(&dir).map_err(|e| AssembleError::Io(e.to_string()))?;
    write_atomically(doc, &out).map_err(|e| AssembleError::Io(e.to_string()))?;
    // Written after the document so a hit implies the document exists. A
    // failure only costs a re-merge next time.
    let _ = write_note(&note, &skipped);

    Ok(Assembled {
        path: Arc::from(out.to_string_lossy().as_ref()),
        pages: count,
        skipped,
        merged: true,
        reused: false,
    })
}

/// Records what a merge left out, one described line per skipped page, beside
/// the document. An empty file means the document is complete.
fn write_note(path: &Path, skipped: &[Skipped]) -> std::io::Result<()> {
    let mut text = String::new();
    for s in skipped {
        text.push_str(&s.describe());
        text.push('\n');
    }
    std::fs::write(path, text)
}

/// Reads back a merge outcome. `None` (no readable note) forces a re-merge.
fn read_note(path: &Path) -> Option<Vec<Skipped>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Skipped {
                name: Arc::from(l),
                reason: SkipReason::Recorded,
            })
            .collect(),
    )
}

/// Reads every usable page into one document.
///
/// Returns the document, how many pages it holds, and what was left out.
fn merge(pages: &[Page]) -> (Document, usize, Vec<Skipped>) {
    let mut out = Document::with_version("1.5");
    let mut skipped = Vec::new();

    // Pages are reparented last: the new `/Pages` node cannot be allocated
    // until every source has been renumbered.
    let mut collected: Vec<(ObjectId, Object)> = Vec::new();
    let mut objects: Vec<(ObjectId, Object)> = Vec::new();
    let mut next_id = 1u32;

    for page in pages {
        let mut doc = match load(&page.path) {
            Ok(doc) => doc,
            Err(reason) => {
                skipped.push(Skipped {
                    name: Arc::clone(&page.name),
                    reason,
                });
                continue;
            }
        };

        // Object ids are per-document, so each source gets a fresh range.
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;

        let ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
        if ids.is_empty() {
            skipped.push(Skipped {
                name: Arc::clone(&page.name),
                reason: SkipReason::NoPages,
            });
            continue;
        }

        // Every page of the member, not just the first.
        let before = collected.len();
        for id in &ids {
            let Some(dict) = resolve_inherited(&doc, *id) else {
                continue;
            };
            collected.push((*id, Object::Dictionary(dict)));
        }

        // Report pages whose dictionaries could not be read.
        let taken = collected.len() - before;
        if taken < ids.len() {
            skipped.push(Skipped {
                name: Arc::clone(&page.name),
                reason: SkipReason::PagesUnreadable {
                    lost: ids.len() - taken,
                },
            });
        }
        objects.extend(std::mem::take(&mut doc.objects));
    }

    if collected.is_empty() {
        return (out, 0, skipped);
    }

    // Sources occupy `1..next_id`, so the new `/Pages` and `/Catalog` must be
    // allocated above that, or they overwrite source objects (see
    // `merging_does_not_overwrite_the_first_sources_objects`).
    out.max_id = next_id.saturating_sub(1);

    // The page tree is rebuilt, so the sources' `/Catalog`, `/Pages` and
    // outline objects are dropped.
    let pages_id = out.new_object_id();
    for (id, object) in objects {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" | b"Pages" | b"Outlines" | b"Outline" | b"Page" => {}
            _ => {
                out.objects.insert(id, object);
            }
        }
    }

    let count = collected.len();
    let kids: Vec<Object> = collected
        .iter()
        .map(|(id, _)| Object::Reference(*id))
        .collect();
    for (id, object) in collected {
        let mut dict = match object {
            Object::Dictionary(d) => d,
            _ => continue,
        };
        dict.set("Parent", pages_id);
        out.objects.insert(id, Object::Dictionary(dict));
    }

    out.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => count as u32,
            "Kids" => kids,
        }),
    );
    let catalog_id = out.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    out.trailer.set("Root", catalog_id);

    // Compacts ids from 1; lopdf also resets `max_id`.
    out.renumber_objects();

    (out, count, skipped)
}

/// Reads and parses one page file.
fn load(path: &str) -> Result<Document, SkipReason> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => SkipReason::Gone,
        _ => SkipReason::Unreadable(e.to_string()),
    })?;
    parse(&bytes)
}

/// Parses PDF bytes, distinguishing "not a PDF" from a malformed one.
fn parse(bytes: &[u8]) -> Result<Document, SkipReason> {
    // Header check first so the report says "not a PDF" instead of a parser
    // error.
    if !bytes.starts_with(b"%PDF-") {
        return Err(SkipReason::NotPdf);
    }

    let doc = Document::load_mem(bytes).map_err(|_| SkipReason::Malformed)?;
    if doc.is_encrypted() {
        return Err(SkipReason::Encrypted);
    }
    Ok(doc)
}

/// Checks that a single page parses, for the group-of-one path, so a corrupt
/// file is reported here instead of by the viewer.
fn probe(path: &str) -> Option<SkipReason> {
    load(path).err()
}

/// Copies every inheritable attribute onto a page dictionary.
///
/// Takes the nearest ancestor's value for each missing key, as a renderer
/// would, and removes `/Parent`, so the page can be reparented unchanged.
fn resolve_inherited(doc: &Document, page: ObjectId) -> Option<lopdf::Dictionary> {
    let mut dict = doc.get_dictionary(page).ok()?.clone();

    let mut current = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    let mut depth = 0;
    while let Some(parent) = current {
        // Guards against `/Parent` cycles in malformed files.
        if depth >= MAX_PARENT_DEPTH {
            break;
        }
        depth += 1;

        let Ok(node) = doc.get_dictionary(parent) else {
            break;
        };
        for key in INHERITABLE {
            if !dict.has(key)
                && let Ok(value) = node.get(key)
            {
                dict.set(String::from_utf8_lossy(key).into_owned(), value.clone());
            }
        }
        current = node.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }

    dict.remove(b"Parent");
    Some(dict)
}

/// Returns the cache key for a set of pages.
///
/// FNV-1a over each page's path key (case- and separator-insensitive, via
/// [`winpath::path_key`]), size and modification time, so a replaced page
/// yields a new key. Not a security boundary.
fn cache_key(pages: &[Page]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    for page in pages {
        mix(winpath::path_key(Path::new(page.path.as_ref())));
        // Missing metadata hashes as zero.
        let meta = std::fs::metadata(page.path.as_ref()).ok();
        mix(meta.as_ref().map(|m| m.len()).unwrap_or(0));
        mix(meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0));
    }
    h
}

/// Writes the document to a unique temp file beside `out`, syncs it, then
/// renames it into place.
///
/// The sync prevents a power loss from leaving an empty file under a valid
/// cache name.
fn write_atomically(mut doc: Document, out: &Path) -> std::io::Result<()> {
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    write_bytes_atomically(&buf, out)
}

/// Writes raw bytes atomically, as [`write_atomically`] does.
fn write_bytes_atomically(bytes: &[u8], out: &Path) -> std::io::Result<()> {
    let dir = out.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        "{:x}-{:x}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let result = (|| -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();

    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    match std::fs::rename(&tmp, out) {
        Ok(()) => Ok(()),
        // The target already exists (another instance won, or it is
        // read-only). The name is the content, so the existing file is valid.
        Err(_) if out.is_file() => {
            let _ = std::fs::remove_file(&tmp);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Removes merged documents older than `max_age`.
///
/// A document cannot be deleted while a viewer holds it, so collection runs on
/// a later start. Best effort, like `index::persist::gc_orphans`.
pub fn gc(cache_dir: &Path, max_age: std::time::Duration) {
    let dir = cache_dir.join("pdf");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".pdf") && !name.ends_with(".tmp") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > max_age)
            .unwrap_or(false);
        if old {
            // Windows refuses to delete a read-only file, so always clear the
            // attribute first; an earlier run may have set it.
            let _ = set_read_only(&entry.path(), false);
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Sets or clears the read-only attribute on a file in the cache.
///
/// `Permissions::set_readonly` maps to `FILE_ATTRIBUTE_READONLY` on Windows.
/// Skips the write when the attribute already matches, avoiding a
/// `set_permissions` on a file a viewer may hold.
pub(crate) fn set_read_only(path: &Path, read_only: bool) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    if perms.readonly() == read_only {
        return Ok(());
    }
    perms.set_readonly(read_only);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Stream;
    use lopdf::content::{Content, Operation};

    /// Builds a one-page PDF carrying its own `MediaBox`.
    fn one_page(label: &str, width: i64) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![50.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(label)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), width.into(), 842.into()],
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

    /// A one-page PDF whose page has **no** `MediaBox` of its own and must
    /// inherit it from the `/Pages` node above it.
    fn inheriting_page(width: i64) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: vec![] }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                // Only here, never on the page.
                "MediaBox" => vec![0.into(), 0.into(), width.into(), 842.into()],
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

    fn page_at(dir: &Path, name: &str, bytes: &[u8]) -> Page {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        Page {
            path: Arc::from(path.to_string_lossy().as_ref()),
            name: Arc::from(name),
            page: None,
        }
    }

    fn media_widths(path: &str) -> Vec<i64> {
        let doc = Document::load(path).unwrap();
        doc.get_pages()
            .into_values()
            .map(|id| {
                let d = doc.get_dictionary(id).unwrap();
                let b = d.get(b"MediaBox").unwrap().as_array().unwrap();
                b[2].as_i64().unwrap()
            })
            .collect()
    }

    /// The new `/Pages` and `/Catalog` must not reuse ids from the first
    /// source. `one_page` puts its font at object 2, which a collision would
    /// replace.
    #[test]
    fn merging_does_not_overwrite_the_first_sources_objects() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", &one_page("two", 596)),
        ];

        let got = assemble(&pages, cache.path()).unwrap();
        let doc = Document::load(got.path.as_ref()).unwrap();

        for id in doc.get_pages().into_values() {
            let page = doc.get_dictionary(id).unwrap();
            let resources = page
                .get(b"Resources")
                .and_then(|r| match r {
                    Object::Reference(r) => doc.get_dictionary(*r),
                    other => other.as_dict(),
                })
                .expect("every page keeps its resources");
            let fonts = resources
                .get(b"Font")
                .and_then(|f| match f {
                    Object::Reference(r) => doc.get_dictionary(*r),
                    other => other.as_dict(),
                })
                .expect("the font dictionary must survive");
            let font = fonts
                .get(b"F1")
                .and_then(|f| match f {
                    Object::Reference(r) => doc.get_dictionary(*r),
                    other => other.as_dict(),
                })
                .expect("F1 must still resolve");
            assert_eq!(
                font.get(b"Type").unwrap().as_name().unwrap(),
                b"Font",
                "the font was overwritten by another object"
            );
        }
    }

    #[test]
    fn merging_preserves_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", &one_page("two", 596)),
            page_at(dir.path(), "c.pdf", &one_page("three", 597)),
        ];

        let got = assemble(&pages, cache.path()).unwrap();
        assert_eq!(got.pages, 3);
        assert!(got.merged && !got.reused);
        assert!(got.skipped.is_empty());
        // Distinct widths per source make this an order check.
        assert_eq!(media_widths(&got.path), [595, 596, 597]);
    }

    #[test]
    fn a_page_inheriting_its_mediabox_keeps_it_after_reparenting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &inheriting_page(1000)),
            page_at(dir.path(), "b.pdf", &one_page("plain", 595)),
        ];

        let got = assemble(&pages, cache.path()).unwrap();
        assert_eq!(got.pages, 2);
        assert_eq!(
            media_widths(&got.path),
            [1000, 595],
            "the inherited box must survive the move"
        );
    }

    #[test]
    fn a_non_pdf_member_is_skipped_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", b"this is not a pdf at all"),
            page_at(dir.path(), "c.pdf", &one_page("three", 597)),
        ];

        let got = assemble(&pages, cache.path()).unwrap();
        assert_eq!(got.pages, 2);
        assert_eq!(got.skipped.len(), 1);
        assert_eq!(got.skipped[0].reason, SkipReason::NotPdf);
        assert!(got.skipped[0].describe().contains("b.pdf"));
        assert!(got.skipped[0].describe().contains("not a PDF"));
    }

    #[test]
    fn a_truncated_member_is_skipped_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let whole = one_page("one", 595);
        let pages = vec![
            page_at(dir.path(), "a.pdf", &whole),
            page_at(dir.path(), "b.pdf", &whole[..whole.len() / 3]),
        ];

        let got = assemble(&pages, cache.path()).unwrap();
        assert_eq!(got.pages, 1);
        assert_eq!(got.skipped.len(), 1);
        assert_eq!(got.skipped[0].reason, SkipReason::Malformed);
    }

    #[test]
    fn a_member_that_vanished_since_the_scan_is_reported_as_gone() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", &one_page("two", 596)),
        ];
        std::fs::remove_file(pages[1].path.as_ref()).unwrap();
        pages[1].name = Arc::from("b.pdf");

        let got = assemble(&pages, cache.path()).unwrap();
        assert_eq!(got.pages, 1);
        assert_eq!(got.skipped[0].reason, SkipReason::Gone);
        assert!(got.skipped[0].describe().contains("no longer exists"));
    }

    #[test]
    fn a_single_page_group_is_opened_without_being_merged() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![page_at(dir.path(), "only.pdf", &one_page("one", 595))];

        let got = assemble(&pages, cache.path()).unwrap();
        assert!(!got.merged, "nothing should have been written");
        assert_eq!(got.pages, 1);
        assert_eq!(got.path, pages[0].path);
        assert!(!cache.path().join("pdf").exists());
    }

    #[test]
    fn a_single_unreadable_page_is_a_failure_rather_than_a_launch() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![page_at(dir.path(), "only.pdf", b"nope")];

        match assemble(&pages, cache.path()) {
            Err(AssembleError::NothingUsable(s)) => {
                assert_eq!(s.len(), 1);
                assert!(
                    AssembleError::NothingUsable(s)
                        .detail()
                        .contains("not a PDF")
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_group_where_nothing_survived_is_a_failure_not_a_launch() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", b"nope"),
            page_at(dir.path(), "b.pdf", b"also nope"),
        ];

        match assemble(&pages, cache.path()) {
            Err(AssembleError::NothingUsable(s)) => {
                assert_eq!(s.len(), 2);
                assert!(AssembleError::NothingUsable(s).detail().contains("1 more"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unchanged_group_reuses_the_merged_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", &one_page("two", 596)),
        ];

        let first = assemble(&pages, cache.path()).unwrap();
        assert!(!first.reused);
        let second = assemble(&pages, cache.path()).unwrap();
        assert!(second.reused, "the same document must not be merged twice");
        assert_eq!(first.path, second.path);
    }

    /// A cache hit reports the original merge's skips, since the key covers
    /// the requested pages, not the ones that were merged.
    #[test]
    fn a_reused_document_reports_the_pages_it_actually_has() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", b"not a pdf"),
            page_at(dir.path(), "c.pdf", &one_page("three", 597)),
        ];

        let first = assemble(&pages, cache.path()).unwrap();
        assert!(!first.reused);
        assert_eq!(first.pages, 2);
        assert_eq!(first.skipped.len(), 1);

        let second = assemble(&pages, cache.path()).unwrap();
        assert!(second.reused, "the same request must not be merged twice");
        assert_eq!(second.pages, 2, "a hit must not claim the missing page");
        assert_eq!(second.skipped.len(), 1);
        assert!(second.skipped[0].describe().contains("b.pdf"));
        assert!(second.skipped[0].describe().contains("not a PDF"));
    }

    #[test]
    fn a_cached_document_with_no_record_of_its_merge_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", &one_page("two", 596)),
        ];

        let first = assemble(&pages, cache.path()).unwrap();
        let note = std::path::Path::new(first.path.as_ref()).with_extension("txt");
        assert!(note.is_file(), "a merge must record its outcome");
        std::fs::remove_file(&note).unwrap();

        let second = assemble(&pages, cache.path()).unwrap();
        assert!(!second.reused, "an unexplained cache entry is not trusted");
        assert_eq!(second.pages, 2);
    }

    #[test]
    fn replacing_a_page_produces_a_different_document() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut pages = vec![
            page_at(dir.path(), "a.pdf", &one_page("one", 595)),
            page_at(dir.path(), "b.pdf", &one_page("two", 596)),
        ];
        let before = assemble(&pages, cache.path()).unwrap();

        // Different size, so the key changes even with a coarse clock.
        pages[1] = page_at(dir.path(), "b.pdf", &one_page("two but longer", 596));
        let after = assemble(&pages, cache.path()).unwrap();

        assert_ne!(before.path, after.path);
        assert!(!after.reused);
    }

    #[test]
    fn a_multi_page_member_contributes_all_of_its_pages() {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // Merge two singles into one file, then use that as a member.
        let staging = tempfile::tempdir().unwrap();
        let two = assemble(
            &[
                page_at(staging.path(), "x.pdf", &one_page("x", 100)),
                page_at(staging.path(), "y.pdf", &one_page("y", 200)),
            ],
            cache.path(),
        )
        .unwrap();
        let two_bytes = std::fs::read(two.path.as_ref()).unwrap();

        let pages = vec![
            page_at(dir.path(), "a.pdf", &two_bytes),
            page_at(dir.path(), "b.pdf", &one_page("z", 300)),
        ];
        let got = assemble(&pages, cache.path()).unwrap();
        assert_eq!(got.pages, 3, "the two-page member must contribute both");
        assert_eq!(media_widths(&got.path), [100, 200, 300]);
    }

    #[test]
    fn an_empty_group_is_refused() {
        let cache = tempfile::tempdir().unwrap();
        assert!(matches!(
            assemble(&[], cache.path()),
            Err(AssembleError::NothingUsable(_))
        ));
    }

    #[test]
    fn collection_removes_old_documents_and_leaves_fresh_ones() {
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("pdf");
        std::fs::create_dir_all(&dir).unwrap();
        let keep = dir.join("aaaa.pdf");
        std::fs::write(&keep, b"%PDF-1.5\n").unwrap();

        gc(cache.path(), std::time::Duration::from_secs(3600));
        assert!(keep.is_file(), "a document written just now must survive");

        gc(cache.path(), std::time::Duration::ZERO);
        assert!(!keep.is_file(), "an expired document must be collected");
    }

    /// Windows refuses `remove_file` on a read-only file, so `gc` must clear
    /// the attribute first.
    #[test]
    fn a_read_only_document_is_still_collected() {
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("pdf");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("bbbb.pdf");
        std::fs::write(&doc, b"%PDF-1.5\n").unwrap();
        set_read_only(&doc, true).unwrap();
        assert!(
            std::fs::metadata(&doc).unwrap().permissions().readonly(),
            "the fixture did not take the attribute, so this proves nothing"
        );

        gc(cache.path(), std::time::Duration::ZERO);

        assert!(!doc.is_file(), "a read-only document was never collected");
    }

    #[test]
    fn the_read_only_attribute_goes_on_and_comes_off_again() {
        let cache = tempfile::tempdir().unwrap();
        let doc = cache.path().join("cccc.pdf");
        std::fs::write(&doc, b"%PDF-1.5\n").unwrap();

        let readonly = || std::fs::metadata(&doc).unwrap().permissions().readonly();
        assert!(!readonly());

        set_read_only(&doc, true).unwrap();
        assert!(readonly(), "the document was not protected");

        // Twice: clearing an already-clear file must succeed.
        set_read_only(&doc, false).unwrap();
        set_read_only(&doc, false).unwrap();
        assert!(!readonly(), "turning the option off did not give it back");
        std::fs::write(&doc, b"%PDF-1.5\n%edited\n").unwrap();
    }

    #[test]
    fn every_skip_reason_carries_a_usable_detail() {
        for reason in [
            SkipReason::Gone,
            SkipReason::Unreadable("boom".into()),
            SkipReason::NotPdf,
            SkipReason::Malformed,
            SkipReason::Encrypted,
            SkipReason::NoPages,
        ] {
            assert!(!reason.detail().is_empty(), "{reason:?}");
        }
    }
}
