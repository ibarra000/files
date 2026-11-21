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

    /// <param name="opened">Files opened from the panel, with how often and when: they rank first among matches as good, and are found even outside the top results.</param>
    public static SearchResult Search(
        IEnumerable<(string Name, VolumeIndex Index)> volumes,
        Query query,
        IReadOnlySet<string> hiddenExtensions,
        CancellationToken cancel,
        IReadOnlyDictionary<string, OpenCount>? opened = null)
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
            AddOpened(hits, name, index, query, hiddenExtensions, opened);
            folders.AddRange(index.SearchFolders(name, query, MaxFolders, cancel));
        }
        return new SearchResult(Best(hits, VolumeIndex.MaxResults, opened), matched) { Folders = Best(folders, MaxFolders, opened) };
    }

    /// <summary>Opened files in this volume that match but did not make its top results; already counted in its matches.</summary>
    private static void AddOpened(List<Hit> hits, string volume, VolumeIndex index, Query query, IReadOnlySet<string> hidden,
        IReadOnlyDictionary<string, OpenCount>? opened)
    {
        if (opened is not { Count: > 0 })
        {
            return;
        }
        var listed = hits.Select(hit => hit.Path).ToHashSet(StringComparer.OrdinalIgnoreCase);
        foreach (string path in opened.Keys)
        {
            if (path.StartsWith(index.Root, StringComparison.OrdinalIgnoreCase) && !listed.Contains(path)
                && index.HitFor(volume, query, hidden, path) is Hit hit && listed.Add(hit.Path))
            {
                hits.Add(hit);
            }
        }
    }

    /// <summary>The merged ranking, cut to <paramref name="limit"/>.</summary>
    private static List<Hit> Best(List<Hit> hits, int limit, IReadOnlyDictionary<string, OpenCount>? opened)
    {
        // Each hit's opens are looked up once, not on every comparison.
        Dictionary<Hit, OpenCount?>? opens = null;
        if (opened is { Count: > 0 })
        {
            opens = new Dictionary<Hit, OpenCount?>(ReferenceEqualityComparer.Instance);
            foreach (Hit hit in hits)
            {
                opens[hit] = opened.GetValueOrDefault(hit.Path);
            }
        }
        hits.Sort((a, b) => Compare(a, b, opens));
        if (hits.Count > limit)
        {
            hits.RemoveRange(limit, hits.Count - limit);
        }
        return hits;
    }

    /// <summary>
    /// Orders by how well the name matched, then opened files first (more opens, then the latest),
    /// then by the rest of the rank key without its per-volume file ordinal, then by path, so the
    /// merged order does not depend on which volume came first and stays put as the search grows.
    /// </summary>
    private static int Compare(Hit a, Hit b, Dictionary<Hit, OpenCount?>? opens)
    {
        int byTier = RankKey.Tier(a.Key).CompareTo(RankKey.Tier(b.Key));
        if (byTier != 0)
        {
            return byTier;
        }
        if (opens is not null)
        {
            int byOpens = ByOpens(opens[a], opens[b]);
            if (byOpens != 0)
            {
                return byOpens;
            }
        }
        int byRank = (a.Key >> 32).CompareTo(b.Key >> 32);
        return byRank != 0 ? byRank : string.Compare(a.Path, b.Path, StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Opened before not opened, then more opens, then opened more recently.</summary>
    private static int ByOpens(OpenCount? a, OpenCount? b)
    {
        if (a is null || b is null)
        {
            return a is null == b is null ? 0 : a is null ? 1 : -1;
        }
        int byCount = b.Count.CompareTo(a.Count);
        return byCount != 0 ? byCount : b.LastUtc.CompareTo(a.LastUtc);
    }
}
