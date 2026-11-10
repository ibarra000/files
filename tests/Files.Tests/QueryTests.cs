using Files.Core;

namespace Files.Tests;

public sealed class QueryTests
{
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
