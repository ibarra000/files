using Files.Core;

namespace Files.Services;

/// <summary>
/// A Files search in kensaku's query language (issue #97), so content results keep to the same search: the words,
/// one extension as <c>ext:</c>, and a chosen volume as <c>in:"root"</c>. kensaku joins repeated <c>ext:</c> filters
/// with AND, so several extensions (typed, or from a type filter) are not sent; matches are kept to
/// <see cref="Extensions"/> here instead.
/// </summary>
public sealed record KensakuQuery(string Text, IReadOnlySet<string> Extensions)
{
    /// <summary>The query for <paramref name="query"/> within <paramref name="scopeRoot"/>, or null when no file can match it.</summary>
    public static KensakuQuery? For(Query query, string? scopeRoot)
    {
        // A type filter with none of the typed extensions: Files matches nothing, so kensaku is not asked.
        if (query.Extensions.Any(ext => ext.Contains('*')))
        {
            return null;
        }
        // *term and term* choose how a name matches; inside a file the words are just words.
        var parts = query.Term
            .Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries)
            .Select(Word)
            .ToList();
        if (query.Extensions.Count == 1)
        {
            parts.Add("ext:" + query.Extensions.First());
        }
        if (scopeRoot is not null)
        {
            parts.Add($"in:{Quoted(scopeRoot)}");
        }
        return new KensakuQuery(string.Join(' ', parts), query.Extensions);
    }

    /// <summary>Whether a match kensaku found is one this search keeps.</summary>
    public bool Keeps(string path) =>
        Extensions.Count == 0 || Extensions.Contains(Query.ExtensionOf(Path.GetFileName(path).ToLowerInvariant()));

    /// <summary>A word kensaku would read as an operator (<c>-word</c>, <c>key:value</c>, a quote) is quoted, so it is searched as typed.</summary>
    private static string Word(string word) =>
        word.StartsWith('-') || word.Contains(':') || word.Contains('"') ? Quoted(word) : word;

    /// <summary>Inside kensaku's quotes only <c>\"</c> and <c>\\</c> are escapes.</summary>
    private static string Quoted(string text) => "\"" + text.Replace("\\", "\\\\").Replace("\"", "\\\"") + "\"";
}
