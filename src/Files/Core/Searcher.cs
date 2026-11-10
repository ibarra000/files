namespace Files.Core;

public sealed record SearchResult(IReadOnlyList<Hit> Hits, int Matched)
{
    public static SearchResult Empty { get; } = new([], 0);
}

/// <summary>Searches every loaded volume and merges the results into one ranked list.</summary>
public static class Searcher
{
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
        int matched = 0;
        foreach (var (name, index) in volumes)
        {
            cancel.ThrowIfCancellationRequested();
            var (volumeHits, volumeMatched) = index.Search(name, query, hiddenExtensions, cancel);
            hits.AddRange(volumeHits);
            matched += volumeMatched;
        }
        hits.Sort(Compare);
        if (hits.Count > VolumeIndex.MaxResults)
        {
            hits.RemoveRange(VolumeIndex.MaxResults, hits.Count - VolumeIndex.MaxResults);
        }
        return new SearchResult(hits, matched);
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
