//! Picks the mark at the head of a result row from the file's extension.
//!
//! The shell icon (`SHGetFileInfo`) costs a COM round trip per path, too slow
//! for network shares while typing. The set is kept to six broad categories;
//! unknown extensions get [`Icon::File`].

use crate::gui::theme::Icon;

/// Returns the mark for a path, by its extension.
pub fn of(path: &str) -> Icon {
    match extension(path).as_str() {
        "pdf" => Icon::FilePdf,

        // CAD drawings and exchange formats.
        "dwg" | "dxf" | "dgn" | "plt" | "step" | "stp" | "iges" | "igs" | "sldprt" | "sldasm"
        | "ipt" | "iam" | "catpart" | "catproduct" => Icon::FileDrawing,

        // Images, including TIFF scans of drawings.
        "tif" | "tiff" | "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" => {
            Icon::FileImage
        }

        // Documents, spreadsheets, presentations and mail.
        "doc" | "docx" | "rtf" | "odt" | "txt" | "md" | "csv" | "xls" | "xlsx" | "xlsm" | "ppt"
        | "pptx" | "msg" | "eml" => Icon::FileDocument,

        "zip" | "7z" | "rar" | "tar" | "gz" | "cab" => Icon::FileArchive,

        _ => Icon::File,
    }
}

/// Returns the lowercased extension without its dot.
///
/// Only the final path component is examined, so a dot in a folder name is
/// ignored.
fn extension(path: &str) -> String {
    let name = match path.rfind(['\\', '/']) {
        Some(cut) => &path[cut + 1..],
        None => path,
    };
    // Last dot wins (`11-D-0704.rev-B.pdf`). A leading dot is a dotfile name,
    // not an extension.
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

    #[test]
    fn an_unknown_extension_gets_the_plain_mark() {
        for path in [r"R:\a\b.qqq", r"R:\a\b", "", "b.", r"R:\a\.hidden"] {
            assert_eq!(of(path), Icon::File, "{path:?}");
        }
    }

    #[test]
    fn the_case_of_the_extension_does_not_matter() {
        for name in ["a.PDF", "a.Pdf", "a.pdf"] {
            assert_eq!(of(name), Icon::FilePdf, "{name}");
        }
    }

    #[test]
    fn a_dot_in_a_folder_name_is_not_the_files_extension() {
        assert_eq!(of(r"R:\jobs\11-D-0704.pdf\GA"), Icon::File);
        assert_eq!(of(r"R:\jobs\11-D-0704.zip\GA.dwg"), Icon::FileDrawing);
    }

    #[test]
    fn the_last_dot_is_the_one_that_counts() {
        assert_eq!(of("11-D-0704.rev-B.pdf"), Icon::FilePdf);
    }

    /// Non-ASCII names must not panic.
    #[test]
    fn a_name_with_wide_characters_is_safe() {
        assert_eq!(of("R:\\Zeichnungen\\Prüfung\\日本語.pdf"), Icon::FilePdf);
        assert_eq!(of("日本語"), Icon::File);
    }
}
