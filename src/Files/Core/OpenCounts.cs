using System.Globalization;

namespace Files.Core;

/// <summary>How often a file was opened from the panel, and when last (UTC).</summary>
public sealed record OpenCount(int Count, DateTime LastUtc);

/// <summary>
/// The files opened from the panel with how often and when, which ranks them higher in searches.
/// Kept in its own file, one <c>count TAB ticks TAB path</c> line each, so the newest-first
/// <c>opened.txt</c> list (and older versions reading it) stays as it was.
/// </summary>
public static class OpenCounts
{
    /// <summary>The most files counted; the ones opened longest ago fall off first.</summary>
    public const int MaxEntries = 500;

    public static IReadOnlyDictionary<string, OpenCount> Empty { get; } = new Dictionary<string, OpenCount>(StringComparer.OrdinalIgnoreCase);

    /// <summary>Returns the counts with one more open of <paramref name="path"/> at <paramref name="nowUtc"/>, ignoring case.</summary>
    public static IReadOnlyDictionary<string, OpenCount> Record(IReadOnlyDictionary<string, OpenCount> counts, string path, DateTime nowUtc)
    {
        // Ignoring case, so a path opened with other casing adds to the same count (and keeps the first spelling).
        var updated = new Dictionary<string, OpenCount>(counts, StringComparer.OrdinalIgnoreCase);
        updated[path] = new OpenCount((updated.GetValueOrDefault(path)?.Count ?? 0) + 1, nowUtc);
        return Trim(updated);
    }

    /// <summary>Returns the counts without <paramref name="path"/>.</summary>
    public static IReadOnlyDictionary<string, OpenCount> Remove(IReadOnlyDictionary<string, OpenCount> counts, string path)
    {
        if (!counts.ContainsKey(path))
        {
            return counts;
        }
        var updated = new Dictionary<string, OpenCount>(counts, StringComparer.OrdinalIgnoreCase);
        updated.Remove(path);
        return updated;
    }

    public static IReadOnlyDictionary<string, OpenCount> Load(string file)
    {
        var counts = new Dictionary<string, OpenCount>(StringComparer.OrdinalIgnoreCase);
        if (!File.Exists(file))
        {
            return counts;
        }
        foreach (string line in File.ReadLines(file))
        {
            // A damaged line is skipped rather than losing the rest.
            string[] parts = line.Split('\t', 3);
            if (parts.Length == 3
                && int.TryParse(parts[0], NumberStyles.None, CultureInfo.InvariantCulture, out int count) && count > 0
                && long.TryParse(parts[1], NumberStyles.None, CultureInfo.InvariantCulture, out long ticks)
                && ticks <= DateTime.MaxValue.Ticks
                && parts[2].Trim().Length is > 0 and <= History.MaxEntryLength
                && parts[2].IndexOfAny(Path.GetInvalidPathChars()) < 0)
            {
                counts[parts[2].Trim()] = new OpenCount(count, new DateTime(ticks, DateTimeKind.Utc));
            }
        }
        return Trim(counts);
    }

    public static void Save(string file, IReadOnlyDictionary<string, OpenCount> counts)
    {
        IEnumerable<string> lines = counts
            .OrderByDescending(entry => entry.Value.LastUtc)
            .Select(entry => FormattableString.Invariant($"{entry.Value.Count}\t{entry.Value.LastUtc.Ticks}\t{entry.Key}"));
        AtomicFile.WriteAllText(file, string.Join(Environment.NewLine, lines));
    }

    private static Dictionary<string, OpenCount> Trim(Dictionary<string, OpenCount> counts)
    {
        if (counts.Count <= MaxEntries)
        {
            return counts;
        }
        return counts
            .OrderByDescending(entry => entry.Value.LastUtc)
            .Take(MaxEntries)
            .ToDictionary(entry => entry.Key, entry => entry.Value, StringComparer.OrdinalIgnoreCase);
    }
}
