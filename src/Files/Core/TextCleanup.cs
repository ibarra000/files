using System.Text.RegularExpressions;

namespace Files.Core;

/// <summary>Text after the removal patterns, and why it may be unchanged.</summary>
public sealed record CleanedText(string Text, string? Note);

/// <summary>
/// The removal patterns from Settings: regular expressions whose matches are taken
/// out of the text under the mouse before it is searched, such as <c>_Page\d+</c>
/// or a drawing stamp. Each match is limited in time, so a pattern that backtracks
/// badly can never hang the panel.
/// </summary>
public sealed class TextCleanup
{
    public static readonly TimeSpan MatchTimeout = TimeSpan.FromMilliseconds(100);

    public static TextCleanup None { get; } = new([]);

    private static readonly Regex Spaces = new(@"\s+", RegexOptions.CultureInvariant, MatchTimeout);

    private readonly IReadOnlyList<Regex> _patterns;

    private TextCleanup(IReadOnlyList<Regex> patterns)
    {
        _patterns = patterns;
    }

    /// <summary>
    /// Compiles the patterns, one per entry; blank entries are skipped. Returns null and
    /// a problem for each invalid pattern if any is invalid, so none are applied half.
    /// </summary>
    public static (TextCleanup? Cleanup, IReadOnlyList<string> Problems) Compile(IEnumerable<string> patterns)
    {
        var compiled = new List<Regex>();
        var problems = new List<string>();
        int number = 0;
        foreach (string pattern in patterns)
        {
            number++;
            if (pattern.Trim().Length == 0)
            {
                continue;
            }
            try
            {
                compiled.Add(new Regex(pattern, RegexOptions.CultureInvariant | RegexOptions.IgnoreCase, MatchTimeout));
            }
            catch (ArgumentException error)
            {
                problems.Add($"Pattern {number} \"{pattern}\" is not valid: {error.Message}");
            }
        }
        return problems.Count > 0 ? (null, problems) : (new TextCleanup(compiled), problems);
    }

    /// <summary>
    /// Removes every match of each pattern, in order, then tidies the spaces left behind.
    /// A pattern that takes too long leaves the text as it was read, with a note.
    /// </summary>
    public CleanedText Apply(string text)
    {
        string cleaned = text;
        try
        {
            foreach (Regex pattern in _patterns)
            {
                cleaned = pattern.Replace(cleaned, "");
            }
            return cleaned == text ? new CleanedText(text, null) : new CleanedText(Spaces.Replace(cleaned, " ").Trim(), null);
        }
        catch (RegexMatchTimeoutException error)
        {
            return new CleanedText(text, $"The removal pattern \"{error.Pattern}\" took too long, so the text was searched as read");
        }
    }
}
