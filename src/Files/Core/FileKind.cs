namespace Files.Core;

/// <summary>What sort of file a name is, judged by its extension alone.</summary>
public enum FileKind
{
    Other,
    Document,
    Pdf,
    Image,
    Drawing,
    Sheet,
    Archive,
    Video,
    Audio,
    Code,
}

/// <summary>The extensions of each <see cref="FileKind"/>; no file or share is touched to tell them apart.</summary>
public static class FileKinds
{
    private static readonly (FileKind Kind, HashSet<string> Extensions)[] Kinds =
    [
        (FileKind.Pdf, Set("pdf")),
        (FileKind.Document, Set("doc", "docx", "odt", "rtf", "txt", "md", "ppt", "pptx")),
        (FileKind.Image, Set("png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "heic", "svg")),
        (FileKind.Drawing, Set("dwg", "dxf", "dgn", "rvt", "skp", "step", "stp", "iges", "igs")),
        (FileKind.Sheet, Set("xls", "xlsx", "xlsm", "csv", "ods")),
        (FileKind.Archive, Set("zip", "7z", "rar", "tar", "gz")),
        (FileKind.Video, Set("mp4", "mov", "avi", "mkv", "wmv")),
        (FileKind.Audio, Set("mp3", "wav", "flac", "m4a", "ogg")),
        (FileKind.Code, Set("cs", "js", "ts", "py", "json", "xml", "html", "css", "sql", "ps1", "c", "cpp", "h", "asm", "java", "go", "rs")),
    ];

    public static FileKind Of(string name)
    {
        // ".gitignore" has no extension: the dot starts its name.
        int dot = name.LastIndexOf('.');
        if (dot <= 0 || dot == name.Length - 1)
        {
            return FileKind.Other;
        }
        string extension = name[(dot + 1)..];
        foreach ((FileKind kind, HashSet<string> extensions) in Kinds)
        {
            if (extensions.Contains(extension))
            {
                return kind;
            }
        }
        return FileKind.Other;
    }

    /// <summary>The lowercase extensions of all of <paramref name="kinds"/>, for narrowing a search.</summary>
    public static IReadOnlySet<string> ExtensionsOf(params FileKind[] kinds) =>
        Kinds.Where(entry => kinds.Contains(entry.Kind))
            .SelectMany(entry => entry.Extensions)
            .ToHashSet(StringComparer.Ordinal);

    private static HashSet<string> Set(params string[] extensions) => new(extensions, StringComparer.OrdinalIgnoreCase);
}
