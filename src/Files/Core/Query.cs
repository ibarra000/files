namespace Files.Core;

public enum MatchMode
{
    /// <summary>The term appears anywhere in the name.</summary>
    Contains,

    /// <summary><c>term*</c>: the name starts with the term.</summary>
    Prefix,

    /// <summary><c>*term</c>: the name, without its extension, ends with the term.</summary>
    Suffix,
}

/// <summary>
/// A parsed search line. Only <c>*</c> at either end and <c>ext:</c> tokens are
/// syntax; Windows forbids both <c>*</c> and <c>:</c> in file names, so they
/// can never clash with a real name. Parsing never throws: problems are
/// reported in <see cref="Problem"/>.
/// </summary>
public sealed record Query(string Term, MatchMode Mode, IReadOnlySet<string> Extensions, string? Problem)
{
    private const int MaxExtensionLength = 8;
    private static readonly char[] BadExtensionChars = ['"', '*', '?', '<', '>', '|', ':', '\\', '/', '.'];

    /// <summary>The term lowercased the way the name arenas are.</summary>
    public string Needle { get; } = Names.Fold(Term);

    public bool IsValid => Problem is null;

    public static Query Parse(string line)
    {
        var words = new List<string>();
        var extensions = new HashSet<string>(StringComparer.Ordinal);
        string? problem = null;

        foreach (string token in line.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries))
        {
            if (!token.StartsWith("ext:", StringComparison.OrdinalIgnoreCase))
            {
                words.Add(token);
                continue;
            }
            foreach (string piece in token[4..].Split(','))
            {
                string ext = piece.Trim().TrimStart('.');
                if (ext.Length == 0)
                {
                    continue;
                }
                if (!IsValidExtension(ext))
                {
                    problem = "not a valid file extension";
                    continue;
                }
                extensions.Add(ext.ToLowerInvariant());
            }
        }

        string term = string.Join(' ', words);
        MatchMode mode = MatchMode.Contains;
        bool leading = term.StartsWith('*');
        if (leading)
        {
            term = term[1..];
        }
        bool trailing = term.EndsWith('*');
        if (trailing)
        {
            term = term[..^1];
        }
        if (leading != trailing)
        {
            mode = leading ? MatchMode.Suffix : MatchMode.Prefix;
        }

        if (term.Contains('\0'))
        {
            problem = "contains a character that cannot be searched";
        }
        if (term.Contains('*'))
        {
            problem = "* is only allowed at the start or end";
        }
        if (term.Length == 0)
        {
            problem ??= "nothing to search for";
        }
        return new Query(term, mode, extensions, problem);
    }

    private static bool IsValidExtension(string ext) =>
        ext.Length <= MaxExtensionLength
        && ext.All(char.IsAscii)
        && ext.IndexOfAny(BadExtensionChars) < 0;

    /// <summary>The lowercased last extension of a name, or "" when it has none.</summary>
    public static string ExtensionOf(ReadOnlySpan<char> lowerName)
    {
        int dot = lowerName.LastIndexOf('.');
        return dot < 0 ? "" : lowerName[(dot + 1)..].ToString();
    }

    /// <summary>
    /// Where the term matches <paramref name="lowerName"/> under this query's
    /// mode, or -1. <paramref name="firstHit"/> is the leftmost substring hit.
    /// </summary>
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
            default:
                return firstHit;
        }
    }
}
