using System.Runtime.CompilerServices;
using System.Text;

namespace Files.Core;

public enum MatchMode
{
    /// <summary>The term appears anywhere in the name.</summary>
    Contains,

    /// <summary><c>term*</c>: the name starts with the term.</summary>
    Prefix,

    /// <summary><c>*term</c>: the name, without its extension, ends with the term.</summary>
    Suffix,

    /// <summary><c>*</c> or <c>?</c> inside the term: a wildcard pattern the whole name must fit.</summary>
    Pattern,
}

/// <summary>One of the phrases a search finds, the parts of the line between <c>|</c>.</summary>
/// <param name="RawPattern">A pattern as parsed, where quoted <c>* ?</c> are still characters no name has, so they never act as wildcards.</param>
public sealed record Alternative(string Term, MatchMode Mode, string? RawPattern = null)
{
    private readonly string _pattern = Names.Fold(RawPattern ?? Term);

    /// <summary>What the name arenas are scanned for: the term lowercased, or a pattern's longest piece without wildcards.</summary>
    public string Needle { get; } = Names.Fold(Mode == MatchMode.Pattern ? LongestLiteral(RawPattern ?? Term) : Term);

    /// <summary>Where this phrase matches <paramref name="lowerName"/>, or -1.</summary>
    [MethodImpl(MethodImplOptions.AggressiveInlining)]
    public int Match(ReadOnlySpan<char> lowerName) => MatchAt(lowerName, lowerName.IndexOf(Needle, StringComparison.Ordinal));

    /// <summary>The same, given <paramref name="firstHit"/>, the leftmost place the needle appears (or -1).</summary>
    public int MatchAt(ReadOnlySpan<char> lowerName, int firstHit)
    {
        switch (Mode)
        {
            case MatchMode.Prefix:
                return firstHit == 0 ? 0 : -1;
            case MatchMode.Suffix:
                int dot = lowerName.LastIndexOf('.');
                ReadOnlySpan<char> stem = dot < 0 ? lowerName : lowerName[..dot];
                return stem.EndsWith(Needle, StringComparison.Ordinal) ? stem.Length - Needle.Length : -1;
            case MatchMode.Pattern:
                return firstHit >= 0 && Fits(lowerName, _pattern) ? firstHit : -1;
            default:
                return firstHit;
        }
    }

    private static string LongestLiteral(string pattern) =>
        pattern.Split('*', '?').MaxBy(piece => piece.Length) ?? "";

    /// <summary>Whether all of <paramref name="name"/> fits <paramref name="pattern"/>, where <c>*</c> is any run of characters and <c>?</c> any one.</summary>
    private static bool Fits(ReadOnlySpan<char> name, ReadOnlySpan<char> pattern)
    {
        int n = 0, p = 0, star = -1, resume = 0;
        while (n < name.Length)
        {
            if (p < pattern.Length && (pattern[p] == '?' || pattern[p] == name[n]))
            {
                n++;
                p++;
            }
            else if (p < pattern.Length && pattern[p] == '*')
            {
                star = p++;
                resume = n;
            }
            else if (star >= 0)
            {
                p = star + 1;
                n = ++resume;
            }
            else
            {
                return false;
            }
        }
        while (p < pattern.Length && pattern[p] == '*')
        {
            p++;
        }
        return p == pattern.Length;
    }
}

/// <summary>
/// A parsed search line: the words, which are one phrase as typed, and the operators.
/// <c>a|b</c> finds either, <c>!term</c> leaves out names holding it, <c>ext:pdf;dwg</c> and
/// <c>ext:!exe</c> choose types, <c>path:x</c> keeps files whose folder holds x, a star at either
/// end means starts or ends with, and <c>*</c> or <c>?</c> inside a word make a wildcard pattern.
/// Quotes keep spaces and make operators plain text. Windows forbids <c>* ? | : "</c> in names,
/// so the syntax never clashes with a real name. Parsing never throws: an operator that is not a
/// valid one is searched as written, and problems are reported in <see cref="Problem"/>.
/// </summary>
public sealed record Query(IReadOnlyList<Alternative> Alternatives, IReadOnlySet<string> Extensions, string? Problem)
{
    private const int MaxExtensionLength = 8;
    private static readonly char[] BadExtensionChars = ['"', '*', '?', '<', '>', '|', ':', '\\', '/', '.', '!'];

    // Quoted * ? | stand in as characters no name has, so they stay plain text through parsing.
    private const string Operators = "*?|";
    private const string QuotedOperators = "\uE001\uE002\uE003";

    // Match runs once per candidate name: an array loops without an enumerator.
    private readonly Alternative[] _alternatives = [.. Alternatives];

    /// <summary>Types left out with <c>ext:!exe</c>.</summary>
    public IReadOnlySet<string> ExcludedExtensions { get; init; } = new HashSet<string>();

    /// <summary>Lowercased words a name must not hold (<c>!term</c>).</summary>
    public IReadOnlyList<string> Excluded { get; init; } = [];

    /// <summary>Lowercased text the folder path must hold (<c>path:x</c>), with <c>\</c> between folders.</summary>
    public IReadOnlyList<string> PathTerms { get; init; } = [];

    /// <summary>The phrase searched for, or the alternatives joined by " | ".</summary>
    public string Term => string.Join(" | ", Alternatives.Select(alternative => alternative.Term));

    /// <summary>The first alternative's mode.</summary>
    public MatchMode Mode => Alternatives.Count > 0 ? Alternatives[0].Mode : MatchMode.Contains;

    /// <summary>The first alternative's needle.</summary>
    public string Needle => Alternatives.Count > 0 ? Alternatives[0].Needle : "";

    /// <summary>One plain phrase and no operators: text that may also be a path typed in full.</summary>
    public bool IsPlain => Alternatives.Count == 1 && Mode == MatchMode.Contains && Excluded.Count == 0 && PathTerms.Count == 0;

    public bool IsValid => Problem is null;

    public static Query Parse(string line)
    {
        Query query = Parse(line, excludeWithBang: true);
        // "!readme" alone is a name that starts with "!", not a search that leaves out everything.
        return query.Alternatives.Count == 0 && query.Excluded.Count > 0 ? Parse(line, excludeWithBang: false) : query;
    }

    private static Query Parse(string line, bool excludeWithBang)
    {
        var words = new List<string>();
        var extensions = new HashSet<string>(StringComparer.Ordinal);
        var excludedExtensions = new HashSet<string>(StringComparer.Ordinal);
        var excluded = new List<string>();
        var pathTerms = new List<string>();

        foreach ((string text, bool quoted) in Tokens(line))
        {
            if (text.Length == 0)
            {
                continue;
            }
            bool isOperator = !quoted && (excludeWithBang || text[0] != '!')
                && Operator(text, extensions, excludedExtensions, excluded, pathTerms);
            if (!isOperator)
            {
                words.Add(text);
            }
        }

        var alternatives = string.Join(' ', words).Split('|')
            .Select(part => part.Trim())
            .Where(part => part.Length > 0)
            .Select(ParseAlternative)
            .OfType<Alternative>()
            .ToList();
        string? problem = null;
        if (line.Contains('\0'))
        {
            problem = "contains a character that cannot be searched";
        }
        else if (alternatives.Count == 0)
        {
            problem = "nothing to search for";
        }
        return new Query(alternatives, extensions, problem)
        {
            ExcludedExtensions = excludedExtensions,
            Excluded = excluded,
            PathTerms = pathTerms,
        };
    }

    /// <summary>Takes an <c>ext:</c>, <c>path:</c> or <c>!</c> token, or returns false to search it as written.</summary>
    private static bool Operator(string text, HashSet<string> extensions, HashSet<string> excludedExtensions, List<string> excluded, List<string> pathTerms)
    {
        if (text.StartsWith("ext:", StringComparison.OrdinalIgnoreCase))
        {
            var include = new List<string>();
            var exclude = new List<string>();
            foreach (string piece in text[4..].Split([',', ';'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
            {
                bool not = piece.StartsWith('!');
                string ext = piece.TrimStart('!').TrimStart('.');
                if (!IsValidExtension(ext))
                {
                    return false;
                }
                (not ? exclude : include).Add(ext.ToLowerInvariant());
            }
            if (include.Count + exclude.Count == 0)
            {
                return false;
            }
            extensions.UnionWith(include);
            excludedExtensions.UnionWith(exclude);
            return true;
        }
        if (text.StartsWith("path:", StringComparison.OrdinalIgnoreCase))
        {
            string path = Names.Fold(Restore(text[5..])).Replace('/', '\\').Trim('\\');
            if (path.Length == 0)
            {
                return false;
            }
            pathTerms.Add(path);
            return true;
        }
        if (text.Length > 1 && text[0] == '!')
        {
            excluded.Add(Names.Fold(Restore(text[1..])));
            return true;
        }
        return false;
    }

    /// <summary>The line split at spaces outside quotes; a token that starts with a quote is plain text.</summary>
    private static IEnumerable<(string Text, bool Quoted)> Tokens(string line)
    {
        var token = new StringBuilder();
        bool inQuotes = false, startsQuoted = false;
        foreach (char c in line)
        {
            if (c == '"')
            {
                startsQuoted |= token.Length == 0 && !inQuotes;
                inQuotes = !inQuotes;
            }
            else if (!inQuotes && char.IsWhiteSpace(c))
            {
                if (token.Length > 0 || startsQuoted)
                {
                    yield return (token.ToString(), startsQuoted);
                }
                token.Clear();
                startsQuoted = false;
            }
            else
            {
                int op = Operators.IndexOf(c);
                token.Append(inQuotes && op >= 0 ? QuotedOperators[op] : c);
            }
        }
        if (token.Length > 0)
        {
            yield return (token.ToString(), startsQuoted);
        }
    }

    /// <summary>One alternative, or null when it is only stars (<c>*</c>, <c>**</c>), which would list every file.</summary>
    private static Alternative? ParseAlternative(string text)
    {
        bool leading = text.StartsWith('*');
        string core = leading ? text[1..] : text;
        bool trailing = core.EndsWith('*');
        core = trailing ? core[..^1] : core;
        // "*.dwg" means names ending in .dwg, which the stem-only suffix mode would never find.
        bool pattern = core.IndexOfAny(['*', '?']) >= 0 || (leading && !trailing && core.Contains('.'));
        if (pattern)
        {
            return core.Trim('*').Length == 0 ? null : new Alternative(Restore(text), MatchMode.Pattern, text);
        }
        if (core.Length == 0)
        {
            return null;
        }
        MatchMode mode = leading == trailing ? MatchMode.Contains : leading ? MatchMode.Suffix : MatchMode.Prefix;
        return new Alternative(Restore(core), mode);
    }

    private static string Restore(string text)
    {
        for (int i = 0; i < Operators.Length; i++)
        {
            text = text.Replace(QuotedOperators[i], Operators[i]);
        }
        return text;
    }

    /// <summary>Where the best alternative matches <paramref name="lowerName"/> (the leftmost), or -1; <paramref name="length"/> is how much to highlight.</summary>
    public int Match(ReadOnlySpan<char> lowerName, out int length)
    {
        int best = -1;
        length = 0;
        foreach (Alternative alternative in _alternatives)
        {
            int at = alternative.Match(lowerName);
            if (at >= 0 && (best < 0 || at < best))
            {
                best = at;
                length = alternative.Needle.Length;
            }
        }
        return best;
    }

    public int Match(ReadOnlySpan<char> lowerName) => Match(lowerName, out _);

    /// <summary>Whether a matching name may be listed: its type is allowed and it holds none of the excluded words.</summary>
    public bool AdmitsName(ReadOnlySpan<char> lowerName) => AdmitsName(lowerName, ExtensionOf(lowerName));

    /// <summary>The same, given the name's lowercased extension (<see cref="ExtensionOf"/>).</summary>
    public bool AdmitsName(ReadOnlySpan<char> lowerName, string ext)
    {
        if (ExcludedExtensions.Contains(ext) || (Extensions.Count > 0 && !Extensions.Contains(ext)))
        {
            return false;
        }
        for (int i = 0; i < Excluded.Count; i++)
        {
            string word = Excluded[i];
            if (lowerName.Contains(word, StringComparison.Ordinal))
            {
                return false;
            }
        }
        return true;
    }

    /// <summary>Whether a lowercased folder path holds every <c>path:</c> term.</summary>
    public bool AdmitsPath(ReadOnlySpan<char> lowerFolder)
    {
        for (int i = 0; i < PathTerms.Count; i++)
        {
            string term = PathTerms[i];
            if (!lowerFolder.Contains(term, StringComparison.Ordinal))
            {
                return false;
            }
        }
        return true;
    }

    /// <summary>
    /// The same search kept to <paramref name="allowed"/> types (a filter such as Documents).
    /// Types typed with <c>ext:</c> stay only where they are allowed; when none are, nothing matches.
    /// </summary>
    public Query Within(IReadOnlySet<string> allowed)
    {
        IReadOnlySet<string> narrowed = Extensions.Count == 0
            ? allowed
            : Extensions.Where(allowed.Contains).ToHashSet(StringComparer.Ordinal);
        return this with { Extensions = narrowed.Count > 0 ? narrowed : NoExtension };
    }

    /// <summary>An extension no file can have (Windows forbids <c>*</c> in names), so a filter matches nothing.</summary>
    private static readonly IReadOnlySet<string> NoExtension = new HashSet<string> { "*" };

    private static bool IsValidExtension(string ext) =>
        ext.Length is > 0 and <= MaxExtensionLength
        && ext.All(char.IsAscii)
        && ext.IndexOfAny(BadExtensionChars) < 0;

    /// <summary>The lowercased last extension of a name, or "" when it has none.</summary>
    public static string ExtensionOf(ReadOnlySpan<char> lowerName)
    {
        int dot = lowerName.LastIndexOf('.');
        return dot < 0 ? "" : lowerName[(dot + 1)..].ToString();
    }
}
