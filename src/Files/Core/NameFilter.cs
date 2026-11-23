namespace Files.Core;

/// <summary>
/// Whether a matching name may be listed: its type is not hidden, is one the query allows, and the
/// name holds none of the query's excluded words. Built once per search, then asked once per
/// matching name, up to millions of times: the type is looked up as a span, and only when a type
/// rule applies.
/// </summary>
public sealed class NameFilter
{
    private readonly Query _query;
    private readonly HashSet<string>.AlternateLookup<ReadOnlySpan<char>> _hidden;
    private readonly HashSet<string>.AlternateLookup<ReadOnlySpan<char>> _types;
    private readonly HashSet<string>.AlternateLookup<ReadOnlySpan<char>> _excludedTypes;
    private readonly bool _byType;
    private readonly bool _byWord;

    public NameFilter(Query query, IReadOnlySet<string> hiddenExtensions)
    {
        _query = query;
        _hidden = Lookup(hiddenExtensions);
        _types = Lookup(query.Extensions);
        _excludedTypes = Lookup(query.ExcludedExtensions);
        _byType = hiddenExtensions.Count + query.Extensions.Count + query.ExcludedExtensions.Count > 0;
        _byWord = query.Excluded.Count > 0;
    }

    public bool Admits(ReadOnlySpan<char> lowerName)
    {
        if (_byType && !AdmitsType(Extension(lowerName)))
        {
            return false;
        }
        return !_byWord || !_query.HoldsExcludedWord(lowerName);
    }

    private bool AdmitsType(ReadOnlySpan<char> ext)
    {
        if (_hidden.Contains(ext) || _excludedTypes.Contains(ext))
        {
            return false;
        }
        return _types.Set.Count == 0 || _types.Contains(ext);
    }

    /// <summary>The last extension of a lowercased name, or empty when it has none.</summary>
    private static ReadOnlySpan<char> Extension(ReadOnlySpan<char> lowerName)
    {
        int dot = lowerName.LastIndexOf('.');
        return dot < 0 ? [] : lowerName[(dot + 1)..];
    }

    private static HashSet<string>.AlternateLookup<ReadOnlySpan<char>> Lookup(IReadOnlySet<string> extensions) =>
        new HashSet<string>(extensions, StringComparer.Ordinal).GetAlternateLookup<ReadOnlySpan<char>>();
}
