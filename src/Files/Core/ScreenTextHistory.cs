using System.Text.Json;

namespace Files.Core;

/// <summary>One line read from the screen, and how sure the engine was (0 to 1), when it says.</summary>
public sealed record ScreenTextLine(string Text, float? Confidence)
{
    /// <summary>Below this the engine is likely to have misread a character or two.</summary>
    public const float SureEnough = 0.8f;

    public bool IsUnsure => Confidence < SureEnough;
}

/// <summary>A read of the screen: when, from which window, and the lines read.</summary>
public sealed record ScreenTextEntry(DateTime TimeUtc, string Source, IReadOnlyList<ScreenTextLine> Lines)
{
    public string Text => string.Join(Environment.NewLine, Lines.Select(line => line.Text));
}

/// <summary>
/// The last reads of the screen, newest first, kept in <c>screen-text.json</c> so they can be
/// searched again or copied later. Nothing is recorded while history is paused.
/// </summary>
public static class ScreenTextHistory
{
    public const int MaxEntries = 50;

    private static readonly JsonSerializerOptions Json = new() { WriteIndented = true };

    /// <summary>Returns the history with <paramref name="entry"/> first; the same text read again moves up instead of repeating.</summary>
    public static IReadOnlyList<ScreenTextEntry> Record(IReadOnlyList<ScreenTextEntry> history, ScreenTextEntry entry)
    {
        if (entry.Text.Trim().Length == 0)
        {
            return history;
        }
        return history
            .Where(old => !old.Text.Equals(entry.Text, StringComparison.OrdinalIgnoreCase))
            .Prepend(entry)
            .Take(MaxEntries)
            .ToList();
    }

    /// <summary>The reads whose text or window holds <paramref name="text"/>, ignoring case; all of them for blank text.</summary>
    public static IReadOnlyList<ScreenTextEntry> Filter(IReadOnlyList<ScreenTextEntry> history, string text)
    {
        string wanted = text.Trim();
        if (wanted.Length == 0)
        {
            return history;
        }
        return history
            .Where(entry => entry.Text.Contains(wanted, StringComparison.OrdinalIgnoreCase)
                || entry.Source.Contains(wanted, StringComparison.OrdinalIgnoreCase))
            .ToList();
    }

    /// <summary>The saved history, or none when the file is missing or cannot be read as one.</summary>
    public static IReadOnlyList<ScreenTextEntry> Load(string file)
    {
        if (!File.Exists(file))
        {
            return [];
        }
        try
        {
            List<ScreenTextEntry>? entries = JsonSerializer.Deserialize<List<ScreenTextEntry>>(File.ReadAllText(file), Json);
            // A hand-edited file can hold nulls where values belong; those entries are dropped.
            return (entries ?? [])
                .Where(entry => entry?.Source is not null && entry.Lines is { Count: > 0 } && entry.Lines.All(line => line?.Text is not null))
                .Take(MaxEntries)
                .ToList();
        }
        catch (JsonException)
        {
            return [];
        }
    }

    public static void Save(string file, IReadOnlyList<ScreenTextEntry> history) =>
        AtomicFile.WriteAllText(file, JsonSerializer.Serialize(history, Json));
}
