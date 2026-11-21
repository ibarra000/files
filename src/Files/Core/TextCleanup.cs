using System.Text.RegularExpressions;

namespace Files.Core;

/// <summary>Text after the corrections, and why it may be unchanged.</summary>
public sealed record CleanedText(string Text, string? Note);

/// <summary>
/// The text corrections from Settings, applied to the text under the mouse before it is
/// searched. A line is a regular expression whose matches are taken out, such as
/// <c>_Page\d+</c> or a drawing stamp. A line <c>pattern => replacement</c> puts the
/// replacement in their place instead, such as <c>(?&lt;=\d)O(?=\d) => 0</c> for a zero
/// read as the letter O; <c>$1</c> and the like name the pattern's groups. Each match is
/// limited in time, so a pattern that backtracks badly can never hang the panel.
/// </summary>
public sealed class TextCleanup
{
    public static readonly TimeSpan MatchTimeout = TimeSpan.FromMilliseconds(100);

    public static TextCleanup None { get; } = new([]);

    private static readonly Regex Spaces = new(@"\s+", RegexOptions.CultureInvariant, MatchTimeout);

    // The first " =>" followed by a space or the end of the line parts a pattern from its replacement,
    // so a pattern can still hold "=>" written without spaces.
    private static readonly Regex Arrow = new(@"^(?<pattern>.*?) =>(?: (?<replacement>.*))?$", RegexOptions.CultureInvariant | RegexOptions.Singleline);

    private readonly IReadOnlyList<Correction> _corrections;

    private TextCleanup(IReadOnlyList<Correction> corrections)
    {
        _corrections = corrections;
    }

    private sealed record Correction(Regex Pattern, string Replacement);

    /// <summary>
    /// Compiles the lines, one per entry; blank entries are skipped. Returns null and
    /// a problem for each invalid pattern if any is invalid, so none are applied half.
    /// </summary>
    public static (TextCleanup? Cleanup, IReadOnlyList<string> Problems) Compile(IEnumerable<string> lines)
    {
        var compiled = new List<Correction>();
        var problems = new List<string>();
        int number = 0;
        foreach (string line in lines)
        {
            number++;
            if (line.Trim().Length == 0)
            {
                continue;
            }
            Match arrow = Arrow.Match(line);
            string pattern = arrow.Success ? arrow.Groups["pattern"].Value : line;
            string replacement = arrow.Success ? arrow.Groups["replacement"].Value : "";
            try
            {
                compiled.Add(new Correction(new Regex(pattern, RegexOptions.CultureInvariant | RegexOptions.IgnoreCase, MatchTimeout), replacement));
            }
            catch (ArgumentException error)
            {
                problems.Add($"Pattern {number} \"{pattern}\" is not valid: {error.Message}");
            }
        }
        return problems.Count > 0 ? (null, problems) : (new TextCleanup(compiled), problems);
    }

    /// <summary>
    /// Replaces every match of each pattern, in order (with nothing, for a removal), then tidies
    /// the spaces left behind. A pattern that takes too long leaves the text as it was read, with a note.
    /// </summary>
    public CleanedText Apply(string text)
    {
        string cleaned = text;
        try
        {
            foreach (Correction correction in _corrections)
            {
                cleaned = correction.Pattern.Replace(cleaned, correction.Replacement);
            }
            return cleaned == text ? new CleanedText(text, null) : new CleanedText(Spaces.Replace(cleaned, " ").Trim(), null);
        }
        catch (RegexMatchTimeoutException error)
        {
            return new CleanedText(text, $"The pattern \"{error.Pattern}\" took too long, so the text was searched as read");
        }
    }
}
