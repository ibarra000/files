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
    public static List<ResultRow> Build(SearchResult result, bool showVolume, SearchFilter filter = SearchFilter.All)
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
