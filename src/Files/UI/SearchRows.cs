using Files.Core;

namespace Files.UI;

/// <summary>
/// What the panel lists for a search: the best match, then a few folders, then
/// the other files, each group under a section label.
/// </summary>
public static class SearchRows
{
    /// <summary>Folders listed among the files; more would push the files out of sight.</summary>
    public const int FoldersShown = 5;

    public static List<ResultRow> Build(SearchResult result, bool showVolume)
    {
        var rows = new List<ResultRow>();
        IEnumerable<Hit> files = result.Hits;
        IEnumerable<Hit> folders = result.Folders.Take(FoldersShown);
        Hit? best = result.Hits.FirstOrDefault() ?? result.Folders.FirstOrDefault();
        if (best is null)
        {
            return rows;
        }
        // The best match is not listed again in its group.
        if (best.IsFolder)
        {
            folders = result.Folders.Skip(1).Take(FoldersShown);
        }
        else
        {
            files = result.Hits.Skip(1);
        }
        rows.Add(ResultRow.ForHeader("Best match"));
        rows.Add(ResultRow.ForHit(best, showVolume));
        AddGroup(rows, "Folders", folders, showVolume);
        AddGroup(rows, "Files", files, showVolume);
        return rows;
    }

    private static void AddGroup(List<ResultRow> rows, string title, IEnumerable<Hit> hits, bool showVolume)
    {
        List<ResultRow> group = hits.Select(hit => ResultRow.ForHit(hit, showVolume)).ToList();
        if (group.Count == 0)
        {
            return;
        }
        rows.Add(ResultRow.ForHeader(title));
        rows.AddRange(group);
    }
}
