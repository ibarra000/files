//! Which mark goes at the head of a result row.
//!
//! Ueli puts the application's own icon there, extracted from the executable.
//! A file has no icon of its own, and the shell's - `SHGetFileInfo`, one COM
//! round trip per path off a network share - is not something to do three
//! hundred times while somebody is typing. So the mark comes from the
//! extension, which is a string comparison.
//!
//! # Six marks, not thirty
//!
//! The temptation is one per file type, and it is worth resisting. An icon is
//! only useful if it is recognised without being read, and a vocabulary of
//! thirty is one nobody learns - at which point every row has a small grey
//! shape on it that carries nothing and costs the name twenty points.
//!
//! These six are the distinctions that mean something in a drawing office: is
//! this the PDF, the CAD file, a scan, a letter, a zip of the lot, or
//! something else entirely. Anything the list below has not heard of is
//! [`Icon::File`], which is the honest answer.

use crate::gui::theme::Icon;

/// The mark for a path, by its extension.
pub fn of(path: &str) -> Icon {
    match extension(path).as_str() {
        "pdf" => Icon::FilePdf,

        // What avwin opens, and what a job code is usually about. The
        // exchange formats are here too: somebody who has been sent a STEP
        // file is looking at the same kind of thing.
        "dwg" | "dxf" | "dgn" | "plt" | "step" | "stp" | "iges" | "igs" | "sldprt" | "sldasm"
        | "ipt" | "iam" | "catpart" | "catproduct" => Icon::FileDrawing,

        // A scan of a drawing is a TIFF more often than not, which is why
        // this is its own mark rather than part of the one above.
        "tif" | "tiff" | "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" => {
            Icon::FileImage
        }

        // Everything somebody wrote: letters, schedules, minutes, and the
        // spreadsheets that go with them. One mark, because the difference
        // between a `.doc` and an `.xls` is already in the name.
        "doc" | "docx" | "rtf" | "odt" | "txt" | "md" | "csv" | "xls" | "xlsx" | "xlsm" | "ppt"
        | "pptx" | "msg" | "eml" => Icon::FileDocument,

        "zip" | "7z" | "rar" | "tar" | "gz" | "cab" => Icon::FileArchive,

        _ => Icon::File,
    }
}

/// The extension, folded, without its dot.
///
/// Taken after the last separator rather than off the whole path, because a
/// folder called `11-D-0704.old` holding a file called `GA` would otherwise
/// give every file in it the extension `old`.
fn extension(path: &str) -> String {
    let name = match path.rfind(['\\', '/']) {
        Some(cut) => &path[cut + 1..],
        None => path,
    };
    // `rfind` rather than `split_once`, because `11-D-0704.rev-B.pdf` has two.
    // Position zero is a dotfile, which is a name rather than an extension.
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(dot) => name[dot + 1..].to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drawing_is_marked_as_one() {
        assert_eq!(of(r"R:\jobs\11-D-0704\GA.dwg"), Icon::FileDrawing);
        assert_eq!(of(r"R:\jobs\11-D-0704\GA.pdf"), Icon::FilePdf);
        assert_eq!(of(r"R:\jobs\11-D-0704\scan.TIF"), Icon::FileImage);
        assert_eq!(of(r"R:\jobs\11-D-0704\notes.docx"), Icon::FileDocument);
        assert_eq!(of(r"R:\jobs\11-D-0704\issue.zip"), Icon::FileArchive);
    }

    /// An extension nobody listed is a file, not a guess.
    #[test]
    fn an_unknown_extension_gets_the_plain_mark() {
        for path in [r"R:\a\b.qqq", r"R:\a\b", "", "b.", r"R:\a\.hidden"] {
            assert_eq!(of(path), Icon::File, "{path:?}");
        }
    }

    /// Case is not a fact about a file. Half the drawings on the share are
    /// `.PDF`.
    #[test]
    fn the_case_of_the_extension_does_not_matter() {
        for name in ["a.PDF", "a.Pdf", "a.pdf"] {
            assert_eq!(of(name), Icon::FilePdf, "{name}");
        }
    }

    /// A folder with a dot in it must not lend its suffix to the files inside
    /// it, which is what reading the whole path would do.
    #[test]
    fn a_dot_in_a_folder_name_is_not_the_files_extension() {
        assert_eq!(of(r"R:\jobs\11-D-0704.pdf\GA"), Icon::File);
        assert_eq!(of(r"R:\jobs\11-D-0704.zip\GA.dwg"), Icon::FileDrawing);
    }

    /// And the *last* dot wins, because a revision is written into the name.
    #[test]
    fn the_last_dot_is_the_one_that_counts() {
        assert_eq!(of("11-D-0704.rev-B.pdf"), Icon::FilePdf);
    }

    /// A name that is not all ASCII must not panic on the way to a mark.
    #[test]
    fn a_name_with_wide_characters_is_safe() {
        assert_eq!(of("R:\\Zeichnungen\\Prüfung\\日本語.pdf"), Icon::FilePdf);
        assert_eq!(of("日本語"), Icon::File);
    }
}
