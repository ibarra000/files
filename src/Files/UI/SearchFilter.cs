using Files.Core;

namespace Files.UI;

/// <summary>The chips under the search box, in their order (Ctrl+1 to Ctrl+5).</summary>
public enum SearchFilter
{
    All,
    Files,
    Folders,
    Code,
    Documents,
}

/// <summary>What each chip does to a search before it runs.</summary>
public static class SearchFilters
{
    private static readonly IReadOnlySet<string> CodeTypes = FileKinds.ExtensionsOf(FileKind.Code);
    private static readonly IReadOnlySet<string> DocumentTypes = FileKinds.ExtensionsOf(FileKind.Pdf, FileKind.Document, FileKind.Sheet);

    /// <summary>The search kept to the chip's types; All, Files and Folders leave it as typed and act on the results.</summary>
    public static Query Narrow(Query query, SearchFilter filter) => filter switch
    {
        SearchFilter.Code => query.Within(CodeTypes),
        SearchFilter.Documents => query.Within(DocumentTypes),
        _ => query,
    };
}
