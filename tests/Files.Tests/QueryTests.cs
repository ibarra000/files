using Files.Core;

namespace Files.Tests;

public sealed class QueryTests
{
    private static readonly HashSet<string> Documents = ["pdf", "docx"];

    [Fact]
    public void Narrowing_a_search_without_types_keeps_only_those_types()
    {
        Query narrowed = Query.Parse("plan").Within(Documents);
        Assert.Equal(Documents, narrowed.Extensions);
        Assert.Equal("plan", narrowed.Term);
        Assert.Equal("plan", narrowed.Needle);
    }

    [Fact]
    public void Narrowing_keeps_the_typed_types_that_are_also_allowed()
    {
        Query narrowed = Query.Parse("plan ext:pdf,dwg").Within(Documents);
        Assert.Equal(new HashSet<string> { "pdf" }, narrowed.Extensions);
    }

    [Fact]
    public void Narrowing_to_types_that_were_not_typed_matches_nothing()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["plan.dwg", "plan.pdf"]));
        Query narrowed = Query.Parse("plan ext:dwg").Within(Documents);
        SearchResult result = Searcher.Search([("v", index)], narrowed, new HashSet<string>(), CancellationToken.None);
        Assert.Empty(result.Hits);
    }

    [Fact]
    public void Inner_spaces_collapse_and_the_ends_are_trimmed()
    {
        Query query = Query.Parse("  annual   report  ");
        Assert.Equal("annual report", query.Term);
        Assert.True(query.IsValid);
    }

    [Theory]
    [InlineData("abc", MatchMode.Contains, "abc")]
    [InlineData("abc*", MatchMode.Prefix, "abc")]
    [InlineData("*abc", MatchMode.Suffix, "abc")]
    [InlineData("*abc*", MatchMode.Contains, "abc")]
    public void Stars_at_the_ends_choose_the_match_mode(string line, MatchMode mode, string term)
    {
        Query query = Query.Parse(line);
        Assert.Equal(mode, query.Mode);
        Assert.Equal(term, query.Term);
    }

    [Theory]
    [InlineData("a*b")]
    [InlineData("**abc")]
    public void A_star_anywhere_else_is_a_problem(string line)
    {
        Assert.Equal("* is only allowed at the start or end", Query.Parse(line).Problem);
    }

    [Fact]
    public void Ext_filters_are_lowercased_without_dots_and_removed_from_the_term()
    {
        Query query = Query.Parse("plan ext:.PDF,dwg");
        Assert.Equal("plan", query.Term);
        Assert.Equal(new HashSet<string> { "pdf", "dwg" }, query.Extensions);
    }

    [Fact]
    public void An_invalid_extension_is_a_problem()
    {
        Assert.Equal("not a valid file extension", Query.Parse("plan ext:p*f").Problem);
    }

    [Fact]
    public void A_line_with_only_a_filter_has_nothing_to_search_for()
    {
        Assert.Equal("nothing to search for", Query.Parse("ext:pdf").Problem);
    }

    [Fact]
    public void Suffix_mode_compares_against_the_name_without_its_extension()
    {
        Query query = Query.Parse("*0704");
        Assert.Equal(5, query.MatchAt("ab-c-0704.pdf", 5));
        Assert.Equal(-1, query.MatchAt("ab-c-0704x.pdf", 5));
    }
}
