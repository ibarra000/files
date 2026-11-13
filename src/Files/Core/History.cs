namespace Files.Core;

/// <summary>
/// A newest-first list kept in a plain text file, one entry per line: the
/// searches that led to an opened file, and the files opened.
/// </summary>
public static class History
{
    public const int MaxEntries = 50;
    public const int MaxEntryLength = 512;

    /// <summary>
    /// Returns the list with <paramref name="entry"/> at the front. A repeat
    /// moves to the front instead of duplicating, ignoring case; the oldest
    /// entries fall off past <see cref="MaxEntries"/>.
    /// </summary>
    public static IReadOnlyList<string> Record(IReadOnlyList<string> entries, string entry)
    {
        string trimmed = entry.Trim();
        if (trimmed.Length == 0 || trimmed.Length > MaxEntryLength)
        {
            return entries;
        }
        if (entries.Count > 0 && entries[0] == trimmed)
        {
            return entries;
        }
        return entries
            .Where(e => !e.Equals(trimmed, StringComparison.OrdinalIgnoreCase))
            .Prepend(trimmed)
            .Take(MaxEntries)
            .ToList();
    }

    /// <summary>Returns the list without <paramref name="entry"/>, ignoring case; the same list when it is not there.</summary>
    public static IReadOnlyList<string> Remove(IReadOnlyList<string> entries, string entry)
    {
        if (!entries.Any(e => e.Equals(entry, StringComparison.OrdinalIgnoreCase)))
        {
            return entries;
        }
        return entries.Where(e => !e.Equals(entry, StringComparison.OrdinalIgnoreCase)).ToList();
    }

    public static IReadOnlyList<string> Load(string path)
    {
        if (!File.Exists(path))
        {
            return [];
        }
        return File.ReadLines(path)
            .Select(line => line.Trim())
            .Where(line => line.Length is > 0 and <= MaxEntryLength)
            .Take(MaxEntries)
            .ToList();
    }

    public static void Save(string path, IReadOnlyList<string> entries)
    {
        AtomicFile.WriteAllText(path, string.Join(Environment.NewLine, entries));
    }
}
