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
        (FileKind.Drawing, Set("dwg", "dxf", "dgn", "rvt", "skp", "step", "stp", "iges", "igs", "plt", "hpgl",
            "drw", "prt", "frm", "slddrw", "sldprt", "sldasm", "idw", "ipt", "iam", "ipn",
            "catpart", "catproduct", "catdrawing", "x_t", "jt", "stl")),
        (FileKind.Sheet, Set("xls", "xlsx", "xlsm", "csv", "ods")),
        (FileKind.Archive, Set("zip", "7z", "rar", "tar", "gz")),
        (FileKind.Video, Set("mp4", "mov", "avi", "mkv", "wmv")),
        (FileKind.Audio, Set("mp3", "wav", "flac", "m4a", "ogg")),
        (FileKind.Code, Set("cs", "js", "ts", "py", "json", "xml", "html", "css", "sql", "ps1", "c", "cpp", "h", "asm", "java", "go", "rs")),
    ];

    /// <summary>Creo's file types: Creo numbers each save (part.prt.3), so the type is the extension before the number.</summary>
    private static readonly HashSet<string> NumberedTypes = Set("drw", "prt", "asm", "frm");

    /// <summary>
    /// A name's extension, without the dot, or null when it has none (".gitignore" has none: the dot
    /// starts its name). A numbered Creo save gives its type: "prt" for part.prt.3.
    /// </summary>
    public static string? ExtensionOf(string name)
    {
        int dot = name.LastIndexOf('.');
        if (dot <= 0 || dot == name.Length - 1)
        {
            return null;
        }
        string extension = name[(dot + 1)..];
        if (extension.All(char.IsAsciiDigit) && name.LastIndexOf('.', dot - 1) is int before and > 0
            && NumberedTypes.Contains(name[(before + 1)..dot]))
        {
            return name[(before + 1)..dot];
        }
        return extension;
    }

    /// <summary>Whether a name is a numbered Creo save, whose extension (a number) is not its type.</summary>
    public static bool IsNumbered(string name) =>
        ExtensionOf(name) is string type && !name.EndsWith("." + type, StringComparison.OrdinalIgnoreCase);

    public static FileKind Of(string name)
    {
        if (ExtensionOf(name) is not string extension)
        {
            return FileKind.Other;
        }
        if (IsNumbered(name))
        {
            // A Creo assembly too, though "asm" alone is assembly source.
            return FileKind.Drawing;
        }
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
