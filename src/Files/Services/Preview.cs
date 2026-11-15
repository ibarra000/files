using Files.Core;

namespace Files.Services;

/// <summary>What the preview pane shows for one file or folder; <see cref="Note"/> says why something is missing.</summary>
/// <param name="Thumbnail">A picture of the file (a PDF's first page, a drawing's saved preview) as PNG, BMP or JPEG bytes.</param>
public sealed record PreviewInfo(
    string Name,
    string Folder,
    string Kind,
    long? Size,
    DateTime? Modified,
    IReadOnlyList<string>? Lines,
    IReadOnlyList<string>? Entries,
    bool IsImage,
    string? Note,
    byte[]? Thumbnail = null);

/// <summary>
/// Reads the details of the one selected file: its size, date, and first lines
/// or entries. The index holds names only, so this touches the disk or share,
/// and is only ever done for the selection, off the UI thread.
/// </summary>
public static class Preview
{
    public const int MaxLines = 40;
    public const int MaxLineLength = 200;
    public const int MaxEntries = 15;
    private const long MaxTextBytes = 1024 * 1024;

    /// <summary>How wide a PDF page is rendered: twice the pane's picture, so it stays sharp on a high-DPI screen.</summary>
    public const int ThumbnailWidth = 720;

    /// <summary>Larger PDFs are not opened: Windows reads more of a big file to find its first page.</summary>
    private const long MaxPdfBytes = 256L * 1024 * 1024;

    /// <summary>Larger DXF, plot and STEP files are not read: they are read whole to be drawn.</summary>
    private const long MaxDrawnBytes = 64L * 1024 * 1024;

    /// <summary>Shown when a drawing has no preview this app can draw; Windows' thumbnail is tried next.</summary>
    public const string NoDrawingPreview = "No preview: open it in its CAD program to see it";

    /// <summary>
    /// Shown for a Creo file saved without a preview (older versions saved none). Creo's own thumbnail
    /// handler draws only that same picture, so Windows' thumbnail is not tried.
    /// </summary>
    public const string NoCreoPreview = "No preview saved in this file: open it in Creo to see it";
    private const int SniffBytes = 4096;
    private const string TabAsSpaces = "    ";

    private static readonly IReadOnlySet<string> TextTypes = FileKinds.ExtensionsOf(FileKind.Code)
        .Concat(["txt", "md", "csv", "log", "ini", "cfg", "yml", "yaml"])
        .ToHashSet(StringComparer.OrdinalIgnoreCase);

    /// <summary>Creo's parts, assemblies, drawings and formats, which hold a saved preview.</summary>
    private static readonly IReadOnlySet<string> CreoTypes = new HashSet<string>(["prt", "asm", "drw", "frm"], StringComparer.OrdinalIgnoreCase);

    private static readonly IReadOnlySet<string> ImageTypes = new HashSet<string>(["png", "jpg", "jpeg", "gif", "bmp"], StringComparer.OrdinalIgnoreCase);

    /// <summary>Reads the details, and renders a PDF's first page, off the calling thread.</summary>
    public static async Task<PreviewInfo> LoadAsync(string path, CancellationToken cancel)
    {
        PreviewInfo info = await Task.Run(() => Load(path, cancel), cancel);
        if (info.Note == NoDrawingPreview)
        {
            // Drawn by the CAD program's own handler, when one is installed.
            byte[]? picture = await ShellThumbnail.ReadAsync(path, ThumbnailWidth, cancel);
            return picture is null ? info : info with { Thumbnail = picture, Note = null };
        }
        bool isPdf = info.Note is null && info.Size is not null && Path.GetExtension(path).Equals(".pdf", StringComparison.OrdinalIgnoreCase);
        if (!isPdf)
        {
            return info;
        }
        if (info.Size > MaxPdfBytes)
        {
            return info with { Note = "Too large to preview" };
        }
        var page = await Task.Run(() => PdfThumbnail.RenderAsync(path, ThumbnailWidth, cancel), cancel);
        return page is var (png, pages)
            ? info with { Thumbnail = png, Note = $"Page 1 of {pages:N0}" }
            : info with { Note = "Could not show this PDF" };
    }

    public static PreviewInfo Load(string path, CancellationToken cancel)
    {
        string trimmed = path.TrimEnd('\\');
        string name = Path.GetFileName(trimmed) is { Length: > 0 } own ? own : path;
        string folder = Path.GetDirectoryName(trimmed) ?? "";
        var empty = new PreviewInfo(name, folder, "File", null, null, null, null, false, null);
        try
        {
            if (Directory.Exists(path))
            {
                return ForFolder(empty, new DirectoryInfo(path), cancel);
            }
            var file = new FileInfo(path);
            if (!file.Exists)
            {
                return empty with { Note = "Not found" };
            }
            return ForFile(empty with { Kind = KindLabel(name), Size = file.Length, Modified = file.LastWriteTime }, file, cancel);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or System.Security.SecurityException
            or NotSupportedException or ArgumentException)
        {
            return empty with { Note = $"Could not read it: {error.Message}" };
        }
    }

    public static string FormatSize(long bytes)
    {
        string[] units = ["KB", "MB", "GB", "TB"];
        if (bytes < 1024)
        {
            return $"{bytes} B";
        }
        double size = bytes;
        int unit = -1;
        while (size >= 1024 && unit < units.Length - 1)
        {
            size /= 1024;
            unit++;
        }
        return $"{size:N1} {units[unit]}";
    }

    private static PreviewInfo ForFolder(PreviewInfo empty, DirectoryInfo folder, CancellationToken cancel)
    {
        // Folders first, as Explorer lists them; only the first few are read.
        List<string> entries = folder.EnumerateDirectories().Take(MaxEntries).Select(d => d.Name + "\\")
            .Concat(folder.EnumerateFiles().Take(MaxEntries).Select(f => f.Name))
            .Take(MaxEntries)
            .ToList();
        cancel.ThrowIfCancellationRequested();
        return empty with
        {
            Kind = "Folder",
            Modified = folder.LastWriteTime,
            Entries = entries,
            Note = entries.Count == 0 ? "Empty folder" : null,
        };
    }

    private static PreviewInfo ForFile(PreviewInfo details, FileInfo file, CancellationToken cancel)
    {
        string extension = FileKinds.ExtensionOf(file.Name) ?? "";
        if (ImageTypes.Contains(extension))
        {
            return details with { IsImage = true };
        }
        if (extension.Equals("pdf", StringComparison.OrdinalIgnoreCase))
        {
            // Rendered by LoadAsync, which Windows' PDF engine needs.
            return details;
        }
        if (FileKinds.Of(file.Name) == FileKind.Drawing)
        {
            return ForDrawing(details, file, extension.ToLowerInvariant(), cancel);
        }
        if (IsCreoAssembly(file, extension))
        {
            // "asm" alone is assembly source; this one is Creo's.
            return ForDrawing(details with { Kind = "Drawing" }, file, "asm", cancel);
        }
        if (!TextTypes.Contains(extension))
        {
            return details with { Note = "No preview for this type" };
        }
        if (file.Length > MaxTextBytes)
        {
            return details with { Note = "Too large to preview" };
        }
        IReadOnlyList<string>? lines = ReadLines(file, cancel);
        return lines is null ? details with { Note = "Binary file: no text preview" } : details with { Lines = lines };
    }

    /// <summary>
    /// A drawing's picture, where this app can make one: the preview a DWG or Creo file saves, or the lines
    /// of a DXF, a plot or a STEP model. Otherwise the note asks LoadAsync to try Windows' thumbnail.
    /// </summary>
    private static PreviewInfo ForDrawing(PreviewInfo details, FileInfo file, string extension, CancellationToken cancel)
    {
        try
        {
            byte[]? saved = extension switch
            {
                "dwg" => DwgThumbnail.Read(file.FullName),
                _ when CreoTypes.Contains(extension) => CreoThumbnail.Read(file.FullName, cancel),
                _ => null,
            };
            if (saved is not null)
            {
                return details with { Thumbnail = saved };
            }
            if (CreoTypes.Contains(extension))
            {
                // Another program's .prt or .drw (Siemens NX's, say) may still have a Windows thumbnail.
                return details with { Note = CreoThumbnail.IsCreoFile(file.FullName) ? NoCreoPreview : NoDrawingPreview };
            }
            if (extension is not ("dxf" or "plt" or "hpgl" or "step" or "stp"))
            {
                return details with { Note = NoDrawingPreview };
            }
            if (file.Length > MaxDrawnBytes)
            {
                return details with { Note = "Too large to preview" };
            }
            if (extension is "step" or "stp")
            {
                return StepPreview.Render(file.FullName, ThumbnailWidth, cancel) switch
                {
                    { TooLarge: true } => details with { Note = "Too large to preview" },
                    { Png: byte[] model } => details with { Thumbnail = model },
                    _ => details with { Note = NoDrawingPreview },
                };
            }
            (LineWork? work, byte[]? fallback) = extension == "dxf"
                ? DxfPreview.Read(file.FullName, cancel) is DxfContent dxf ? (dxf.Work, dxf.Saved) : (null, null)
                : (PltPreview.Read(file.FullName, cancel), null);
            if (work is { TooLarge: true })
            {
                return details with { Note = "Too large to preview" };
            }
            // Drawn lines are sharper than a saved preview, which is kept for a drawing of text or solids only.
            byte[]? picture = (work is null ? null : LineDrawing.Render(work, ThumbnailWidth)) ?? fallback;
            return picture is null ? details with { Note = NoDrawingPreview } : details with { Thumbnail = picture };
        }
        catch (Exception error) when (error is not (OperationCanceledException or IOException or UnauthorizedAccessException))
        {
            // The file is untrusted and these readers are this app's own: whatever a damaged or hostile file makes
            // them throw (or GDI+ while drawing) leaves only this preview without a picture. Windows' thumbnail is tried next.
            return details with { Note = NoDrawingPreview };
        }
    }

    /// <summary>Whether a file named *.asm is a Creo assembly saved without a number, which starts with "#UGC:".</summary>
    private static bool IsCreoAssembly(FileInfo file, string extension) =>
        extension.Equals("asm", StringComparison.OrdinalIgnoreCase) && CreoThumbnail.IsCreoFile(file.FullName);

    /// <summary>The first lines of a text file, or null when it holds a NUL byte (so it is not text).</summary>
    private static IReadOnlyList<string>? ReadLines(FileInfo file, CancellationToken cancel)
    {
        using FileStream stream = file.OpenRead();
        var head = new byte[SniffBytes];
        int read = stream.Read(head, 0, head.Length);
        if (Array.IndexOf(head, (byte)0, 0, read) >= 0)
        {
            return null;
        }
        stream.Position = 0;
        using var reader = new StreamReader(stream, detectEncodingFromByteOrderMarks: true);
        var lines = new List<string>();
        while (lines.Count < MaxLines && reader.ReadLine() is string line)
        {
            cancel.ThrowIfCancellationRequested();
            string shown = line.Replace("\t", TabAsSpaces);
            lines.Add(shown.Length > MaxLineLength ? shown[..MaxLineLength] : shown);
        }
        return lines;
    }

    private static string KindLabel(string name) => FileKinds.Of(name) switch
    {
        FileKind.Pdf => "PDF document",
        FileKind.Document => "Document",
        FileKind.Image => "Image",
        FileKind.Drawing => "Drawing",
        FileKind.Sheet => "Spreadsheet",
        FileKind.Archive => "Archive",
        FileKind.Video => "Video",
        FileKind.Audio => "Audio",
        FileKind.Code => "Source code",
        _ => Path.GetExtension(name) is { Length: > 1 } ext ? $"{ext[1..].ToUpperInvariant()} file" : "File",
    };
}
