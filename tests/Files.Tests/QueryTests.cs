using Files.Core;

namespace Files.Tests;

public sealed class QueryTests
{
    private static readonly HashSet<string> Documents = ["pdf", "docx"];
    private static readonly HashSet<string> NothingHidden = [];

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
    [InlineData("a*b*", "a-to-b.pdf", "b-to-a.pdf")]
    [InlineData("**abc", "x.abc", "abcx.pdf")]
    [InlineData("AB-?-07*.pdf", "ab-c-0704_page1.pdf", "ab-cd-0704.pdf")]
    [InlineData("*.dwg", "plan.dwg", "plan.dwg.bak")]
    public void A_wildcard_inside_a_word_matches_the_whole_name(string line, string matching, string other)
    {
        Query query = Query.Parse(line);
        Assert.True(query.IsValid);
        Assert.True(query.Match(matching) >= 0, matching);
        Assert.True(query.Match(other) < 0, other);
    }

    [Fact]
    public void Ext_filters_are_lowercased_without_dots_and_removed_from_the_term()
    {
        Query query = Query.Parse("plan ext:.PDF,dwg");
        Assert.Equal("plan", query.Term);
        Assert.Equal(new HashSet<string> { "pdf", "dwg" }, query.Extensions);
    }

    [Theory]
    [InlineData("plan ext:")]
    [InlineData("plan ext:p<f")]
    [InlineData("plan ext:!")]
    [InlineData("plan ext:pdf;toolongtype")]
    public void A_malformed_ext_is_a_problem(string line)
    {
        Query query = Query.Parse(line);
        Assert.Equal("ext: needs a type, like ext:pdf", query.Problem);
        Assert.Empty(query.Extensions);
    }

    [Theory]
    [InlineData("plan path:")]
    [InlineData("plan path:\\/")]
    public void An_empty_path_is_a_problem(string line)
    {
        Assert.Equal("path: needs a folder name, like path:2026", Query.Parse(line).Problem);
    }

    [Theory]
    [InlineData("plan !")]
    [InlineData("!")]
    public void A_lone_bang_is_a_problem(string line)
    {
        Assert.Equal("! needs a word after it, like !draft", Query.Parse(line).Problem);
    }

    [Fact]
    public void An_exclamation_mark_alone_on_the_line_is_part_of_a_name()
    {
        Query query = Query.Parse("!readme");
        Assert.True(query.IsValid);
        Assert.Equal("!readme", query.Term);
        Assert.Empty(query.Excluded);
    }

    [Fact]
    public void An_exclamation_mark_beside_an_operator_still_excludes()
    {
        Query query = Query.Parse("ext:pdf !draft");
        Assert.Empty(query.Alternatives);
        Assert.Equal(["draft"], query.Excluded);
        Assert.Equal("nothing to search for", query.Problem);
    }

    [Fact]
    public void Two_exclusions_alone_have_nothing_to_search_for()
    {
        Query query = Query.Parse("!a !b");
        Assert.Equal(["a", "b"], query.Excluded);
        Assert.Equal("nothing to search for", query.Problem);
    }

    [Theory]
    [InlineData("*")]
    [InlineData("**")]
    [InlineData("*** ext:pdf")]
    [InlineData("\"\"")]
    public void Stars_or_quotes_alone_have_nothing_to_search_for(string line)
    {
        Assert.Equal("nothing to search for", Query.Parse(line).Problem);
    }

    [Fact]
    public void A_quoted_star_stays_a_character_next_to_a_real_wildcard()
    {
        Query query = Query.Parse("\"a*b\" c*");
        Assert.True(query.Match("axxb cd") < 0);
    }

    [Fact]
    public void Quoted_text_keeps_its_spaces_and_is_never_an_operator()
    {
        Assert.Equal("annual  report", Query.Parse("\"annual  report\"").Term);
        Query quoted = Query.Parse("\"ext:pdf\" \"!draft\" \"a|b\"");
        Assert.Empty(quoted.Extensions);
        Assert.Empty(quoted.Excluded);
        Assert.Single(quoted.Alternatives);
        Assert.True(quoted.Match("x !draft y") < 0);
    }

    [Fact]
    public void A_bar_searches_for_either_and_an_exclamation_mark_leaves_names_out()
    {
        Query query = Query.Parse("invoice | quote !draft");
        Assert.Equal(["invoice", "quote"], query.Alternatives.Select(alternative => alternative.Term));
        Assert.Equal(["draft"], query.Excluded);
        Assert.True(query.Match("2026 quote.pdf") >= 0);
        Assert.True(new NameFilter(query, NothingHidden).Admits("2026 quote.pdf"));
        Assert.False(new NameFilter(query, NothingHidden).Admits("quote draft.pdf"));
    }

    [Fact]
    public void Types_can_be_listed_with_semicolons_and_left_out_with_an_exclamation_mark()
    {
        Query query = Query.Parse("setup ext:pdf;XLSX ext:!exe");
        Assert.Equal(new HashSet<string> { "pdf", "xlsx" }, query.Extensions);
        Assert.Equal(new HashSet<string> { "exe" }, query.ExcludedExtensions);
        Assert.False(new NameFilter(query, NothingHidden).Admits("setup.exe"));
    }

    [Fact]
    public void A_path_term_matches_the_folder_with_either_slash()
    {
        Query query = Query.Parse("plan path:\"Client/2026\"");
        Assert.Equal("plan", query.Term);
        Assert.Equal([@"client\2026"], query.PathTerms);
        Assert.True(query.AdmitsPath(@"jobs\client\2026\line 3"));
        Assert.False(query.AdmitsPath(@"jobs\client\2025"));
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
        Assert.Equal(5, query.Match("ab-c-0704.pdf"));
        Assert.Equal(-1, query.Match("ab-c-0704x.pdf"));
    }
}
