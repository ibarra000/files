namespace Files.Core;

public sealed record SearchResult(IReadOnlyList<Hit> Hits, int Matched)
{
    public static SearchResult Empty { get; } = new([], 0);

    /// <summary>Folders whose own name matched, best first; not counted in <see cref="Matched"/>.</summary>
    public IReadOnlyList<Hit> Folders { get; init; } = [];
}

/// <summary>Searches every loaded volume and merges the results into one ranked list.</summary>
public static class Searcher
{
    /// <summary>The most folders a search returns; the panel shows a few unless asked for folders only.</summary>
    public const int MaxFolders = 50;

    public static SearchResult Search(
        IEnumerable<(string Name, VolumeIndex Index)> volumes,
        Query query,
        IReadOnlySet<string> hiddenExtensions,
        CancellationToken cancel)
    {
        if (!query.IsValid)
        {
            return SearchResult.Empty;
        }
        var hits = new List<Hit>();
        var folders = new List<Hit>();
        int matched = 0;
        foreach (var (name, index) in volumes)
        {
            cancel.ThrowIfCancellationRequested();
            var (volumeHits, volumeMatched) = index.Search(name, query, hiddenExtensions, cancel);
            hits.AddRange(volumeHits);
            matched += volumeMatched;
            folders.AddRange(index.SearchFolders(name, query, MaxFolders, cancel));
        }
        return new SearchResult(Best(hits, VolumeIndex.MaxResults), matched) { Folders = Best(folders, MaxFolders) };
    }

    /// <summary>The merged ranking, cut to <paramref name="limit"/>.</summary>
    private static List<Hit> Best(List<Hit> hits, int limit)
    {
        hits.Sort(Compare);
        if (hits.Count > limit)
        {
            hits.RemoveRange(limit, hits.Count - limit);
        }
        return hits;
    }

    /// <summary>
    /// Orders by the rank key without its per-volume file ordinal, then by
    /// path, so the merged order does not depend on which volume came first.
    /// </summary>
    private static int Compare(Hit a, Hit b)
    {
        int byRank = (a.Key >> 32).CompareTo(b.Key >> 32);
        return byRank != 0 ? byRank : string.Compare(a.Path, b.Path, StringComparison.OrdinalIgnoreCase);
    }
}
