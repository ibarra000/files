using System.ComponentModel;
using System.Diagnostics;
using Files.Core;

namespace Files.Services;

/// <summary>What to open a result with.</summary>
public abstract record OpenWith
{
    public sealed record Default : OpenWith;

    public sealed record CombinedPdf : OpenWith;

    public sealed record Program(Viewer Viewer) : OpenWith;

    /// <summary>The choice for Enter, from <see cref="Settings.EnterOpens"/>.</summary>
    public static OpenWith ForEnter(Settings settings)
    {
        if (settings.EnterOpens == Settings.PdfOpen)
        {
            return new CombinedPdf();
        }
        Viewer? viewer = settings.Viewers.FirstOrDefault(v => v.Name == settings.EnterOpens);
        return viewer is null ? new Default() : new Program(viewer);
    }
}

/// <summary>Opens files. Every method returns a message for the user, or null when all went well.</summary>
public sealed class Opener(Func<Settings> settings, string pdfCache)
{
    public string? Open(Hit hit, OpenWith with)
    {
        if (!File.Exists(hit.Path))
        {
            return "That file no longer exists";
        }
        return with switch
        {
            OpenWith.Program program => Launch(program.Viewer.Program, hit.Path),
            OpenWith.CombinedPdf => OpenCombined(hit),
            _ => OpenDefault(hit.Path),
        };
    }

    public static string? Reveal(string path)
    {
        // explorer.exe parses its own command line, so the path is quoted by hand.
        return Start(new ProcessStartInfo("explorer.exe", $"/select,\"{path}\""));
    }

    private string? OpenDefault(string path)
    {
        string pdfViewer = settings().PdfViewer;
        bool isPdf = path.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase);
        return isPdf && pdfViewer.Length > 0 ? Launch(pdfViewer, path) : Start(new ProcessStartInfo(path) { UseShellExecute = true });
    }

    private string? OpenCombined(Hit hit)
    {
        string extension = Query.ExtensionOf(hit.Name.ToLowerInvariant());
        Viewer? claimed = settings().Viewers.FirstOrDefault(v => v.Extensions.Contains(extension, StringComparer.OrdinalIgnoreCase));
        if (claimed is not null)
        {
            return Launch(claimed.Program, hit.Path);
        }
        IReadOnlyList<string> pages = PageSet.Collect(hit);
        if (pages.Count <= 1)
        {
            return OpenDefault(hit.Path);
        }
        MergeResult merged;
        try
        {
            merged = PdfMerger.Merge(pages, pdfCache);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            return $"The combined PDF could not be saved: {error.Message}";
        }
        if (merged.Path is null)
        {
            return $"None of the {pages.Count} pages could be read";
        }
        string? problem = OpenDefault(merged.Path);
        if (problem is null && merged.Skipped.Count > 0)
        {
            int opened = merged.Total - merged.Skipped.Count;
            return $"Opened {opened} of {merged.Total} pages · skipped {string.Join(", ", merged.Skipped)}";
        }
        return problem;
    }

    private static string? Launch(string program, string path)
    {
        // File names cannot contain a double quote, so quoting is enough.
        return Start(new ProcessStartInfo(program, $"\"{path}\"") { UseShellExecute = true });
    }

    private static string? Start(ProcessStartInfo info)
    {
        try
        {
            Process.Start(info)?.Dispose();
            return null;
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or FileNotFoundException)
        {
            return $"Could not open {Path.GetFileName(info.FileName)}: {error.Message}";
        }
    }
}
