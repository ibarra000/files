using System.Text.RegularExpressions;

namespace Files.Core;

/// <summary>
/// A document stored as one PDF per page: NAME.pdf, NAME_Page2.pdf,
/// NAME-p3.pdf, "NAME page 4.pdf" and so on, all in one folder.
/// </summary>
/// <remarks>
/// A page marker is an optional separator (_ - . or space), then p, pg or page
/// in any case, then an optional separator, then digits. The letter is
/// required: NAME-2.pdf is usually a different document, not page 2.
/// </remarks>
public static partial class PageSet
{
    public const int MaxPages = 512;

    [GeneratedRegex(@"^(?<base>.+?)[_\-. ]?p(?:age|g)?[_\- ]?(?<page>\d+)$", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant)]
    private static partial Regex Marked();

    /// <summary>The document name of a PDF, with any page marker removed, or null for a non-PDF.</summary>
    public static string? BaseOf(string fileName)
    {
        if (!fileName.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }
        string stem = fileName[..^4];
        Match marked = Marked().Match(stem);
        return marked.Success ? marked.Groups["base"].Value : stem;
    }

    /// <summary>
    /// Whether <paramref name="fileName"/> is a page of <paramref name="baseName"/>.
    /// Returns 0 for the unmarked file, the page number for a marked one, or null.
    /// </summary>
    public static int? PageOf(string baseName, string fileName)
    {
        if (!fileName.EndsWith(".pdf", StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }
        string stem = fileName[..^4];
        if (stem.Equals(baseName, StringComparison.OrdinalIgnoreCase))
        {
            return 0;
        }
        if (!stem.StartsWith(baseName, StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }
        Match marked = Marked().Match(stem);
        bool samePrefix = marked.Success && marked.Groups["base"].Value.Equals(baseName, StringComparison.OrdinalIgnoreCase);
        if (!samePrefix || !int.TryParse(marked.Groups["page"].Value, out int page))
        {
            return null;
        }
        return page;
    }

    /// <summary>
    /// The pages of the document <paramref name="hit"/> belongs to, from the
    /// same folder, in reading order: the unmarked file first, then by page
    /// number. Returns just the hit's own path when it is not part of a set.
    /// </summary>
    public static IReadOnlyList<string> Collect(Hit hit)
    {
        string? baseName = BaseOf(hit.Name);
        if (baseName is null)
        {
            return [hit.Path];
        }
        VolumeIndex index = hit.Index;
        int folder = index.FolderOf(hit.File);
        var pages = new List<(int Page, string Name)>();
        foreach (int file in index.FilesIn(folder))
        {
            string name = index.Files[file];
            if (PageOf(baseName, name) is int page)
            {
                pages.Add((page, name));
            }
        }
        return pages
            .OrderBy(p => p.Page)
            .ThenBy(p => p.Name.Length)
            .ThenBy(p => p.Name, StringComparer.OrdinalIgnoreCase)
            .Take(MaxPages)
            .Select(p => Path.Join(index.FolderPath(folder), p.Name))
            .ToList();
    }
}
