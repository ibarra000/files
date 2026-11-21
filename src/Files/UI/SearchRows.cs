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

    /// <param name="filter">Files leaves folders out; Folders lists every folder found and no files.</param>
    /// <param name="stale">By volume name, the mark for results from a volume that cannot be reached (<see cref="StatusText.Stale"/>).</param>
    public static List<ResultRow> Build(SearchResult result, bool showVolume, SearchFilter filter = SearchFilter.All,
        IReadOnlyDictionary<string, string>? stale = null)
    {
        var rows = new List<ResultRow>();
        IReadOnlyList<Hit> allFiles = filter == SearchFilter.Folders ? [] : result.Hits;
        IReadOnlyList<Hit> allFolders = filter == SearchFilter.Files ? [] : result.Folders;
        int foldersShown = filter == SearchFilter.Folders ? allFolders.Count : FoldersShown;
        Hit? best = allFiles.FirstOrDefault() ?? allFolders.FirstOrDefault();
        if (best is null)
        {
            return rows;
        }
        // The best match is not listed again in its group.
        IEnumerable<Hit> files = best.IsFolder ? allFiles : allFiles.Skip(1);
        IEnumerable<Hit> folders = (best.IsFolder ? allFolders.Skip(1) : allFolders).Take(foldersShown);
        rows.Add(ResultRow.ForHeader("Best match"));
        rows.Add(Row(best, showVolume, stale));
        AddGroup(rows, "Folders", folders, showVolume, stale);
        AddGroup(rows, "Files", files, showVolume, stale);
        return rows;
    }

    private static ResultRow Row(Hit hit, bool showVolume, IReadOnlyDictionary<string, string>? stale) =>
        ResultRow.ForHit(hit, showVolume, stale?.GetValueOrDefault(hit.Volume));

    private static void AddGroup(List<ResultRow> rows, string title, IEnumerable<Hit> hits, bool showVolume,
        IReadOnlyDictionary<string, string>? stale)
    {
        List<ResultRow> group = hits.Select(hit => Row(hit, showVolume, stale)).ToList();
        if (group.Count == 0)
        {
            return;
        }
        rows.Add(ResultRow.ForHeader(title));
        rows.AddRange(group);
    }
}
