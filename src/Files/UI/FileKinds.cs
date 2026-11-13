namespace Files.UI;

/// <summary>
/// The icon for a file, chosen from its extension alone so no file or share is
/// touched. Glyphs are from Segoe Fluent Icons.
/// </summary>
public static class FileKinds
{
    public const string Document = "\uE8A5";
    public const string Folder = "\uE8B7";

    private static readonly (string Glyph, HashSet<string> Extensions)[] Kinds =
    [
        ("\uEA90", Set("pdf")), // PDF
        ("\uE8B9", Set("png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "heic", "svg")), // Picture
        ("\uEB3C", Set("dwg", "dxf", "dgn", "rvt", "skp", "step", "stp", "iges", "igs")), // Design
        ("\uE80A", Set("xls", "xlsx", "xlsm", "csv", "ods")), // ViewAll
        ("\uF012", Set("zip", "7z", "rar", "tar", "gz")), // ZipFolder
        ("\uE714", Set("mp4", "mov", "avi", "mkv", "wmv")), // Video
        ("\uE8D6", Set("mp3", "wav", "flac", "m4a", "ogg")), // Audio
        ("\uE943", Set("cs", "js", "ts", "py", "json", "xml", "html", "css", "sql", "ps1", "c", "cpp", "h", "asm", "java", "go", "rs")), // Code
    ];

    public static string Glyph(string name)
    {
        // ".gitignore" has no extension: the dot starts its name.
        int dot = name.LastIndexOf('.');
        if (dot <= 0)
        {
            return Document;
        }
        string extension = name[(dot + 1)..];
        foreach ((string glyph, HashSet<string> extensions) in Kinds)
        {
            if (extensions.Contains(extension))
            {
                return glyph;
            }
        }
        return Document;
    }

    private static HashSet<string> Set(params string[] extensions) => new(extensions, StringComparer.OrdinalIgnoreCase);
}
