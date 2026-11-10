using System.Security.Cryptography;
using System.Text;
using PdfSharp.Pdf;
using PdfSharp.Pdf.IO;

namespace Files.Services;

/// <param name="Path">The merged document, or null when no page could be read.</param>
/// <param name="Skipped">File names of pages that were encrypted, broken or unreadable.</param>
public sealed record MergeResult(string? Path, IReadOnlyList<string> Skipped, int Total);

/// <summary>Joins a page set into one PDF in the local cache, reusing an earlier merge of the same files.</summary>
public static class PdfMerger
{
    public static readonly TimeSpan KeepFor = TimeSpan.FromDays(7);

    public static MergeResult Merge(IReadOnlyList<string> pages, string cacheFolder)
    {
        Directory.CreateDirectory(cacheFolder);
        string output = Path.Combine(cacheFolder, $"{CacheKey(pages)}.pdf");
        if (File.Exists(output))
        {
            return new MergeResult(output, [], pages.Count);
        }

        using var merged = new PdfDocument();
        var skipped = new List<string>();
        foreach (string page in pages)
        {
            try
            {
                using PdfDocument source = PdfReader.Open(page, PdfDocumentOpenMode.Import);
                foreach (PdfPage sourcePage in source.Pages)
                {
                    merged.AddPage(sourcePage);
                }
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException or PdfSharp.PdfSharpException or InvalidOperationException or InvalidCastException or ArgumentException)
            {
                skipped.Add(Path.GetFileName(page));
            }
        }
        if (merged.PageCount == 0)
        {
            return new MergeResult(null, skipped, pages.Count);
        }
        // A unique name, so two merges of the same pages at once cannot collide.
        string temporary = $"{output}.{Guid.NewGuid():N}.tmp";
        try
        {
            merged.Save(temporary);
            File.Move(temporary, output, overwrite: false);
        }
        catch (IOException) when (File.Exists(output))
        {
            // The other merge finished first, and its output is the same document.
            File.Delete(temporary);
            return new MergeResult(output, skipped, pages.Count);
        }
        // Read-only, so a viewer cannot save changes into the cache by mistake.
        File.SetAttributes(output, FileAttributes.ReadOnly);
        return new MergeResult(output, skipped, pages.Count);
    }

    /// <summary>Deletes merged documents older than <see cref="KeepFor"/>.</summary>
    public static void CleanUp(string cacheFolder)
    {
        if (!Directory.Exists(cacheFolder))
        {
            return;
        }
        foreach (string file in Directory.EnumerateFiles(cacheFolder, "*.pdf"))
        {
            try
            {
                if (DateTime.UtcNow - File.GetLastWriteTimeUtc(file) > KeepFor)
                {
                    File.SetAttributes(file, FileAttributes.Normal);
                    File.Delete(file);
                }
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                // Open in a viewer; try again next start.
            }
        }
    }

    /// <summary>Names a merge by its inputs, including size and time, so an edited page gives a new merge.</summary>
    private static string CacheKey(IReadOnlyList<string> pages)
    {
        var text = new StringBuilder();
        foreach (string page in pages)
        {
            var info = new FileInfo(page);
            text.Append(page.ToLowerInvariant()).Append('|')
                .Append(info.Exists ? info.Length : -1).Append('|')
                .Append(info.Exists ? info.LastWriteTimeUtc.Ticks : 0).Append('\n');
        }
        return Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(text.ToString())))[..32];
    }
}
