namespace Files.Core;

/// <summary>Recent searches, newest first, kept in a plain text file one per line.</summary>
public static class History
{
    public const int MaxEntries = 200;
    public const int MaxEntryLength = 512;

    /// <summary>
    /// Returns the list with <paramref name="entry"/> recorded at the front.
    /// A repeat moves to the front instead of duplicating, ignoring case. A
    /// search that extends the newest one replaces it, so typing AB-C on the
    /// way to AB-C-0704 leaves one entry; shortening a search keeps both.
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
        bool extendsNewest = entries.Count > 0
            && trimmed.Length > entries[0].Length
            && trimmed.StartsWith(entries[0], StringComparison.OrdinalIgnoreCase);
        IEnumerable<string> rest = extendsNewest ? entries.Skip(1) : entries;
        return rest
            .Where(e => !e.Equals(trimmed, StringComparison.OrdinalIgnoreCase))
            .Prepend(trimmed)
            .Take(MaxEntries)
            .ToList();
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
