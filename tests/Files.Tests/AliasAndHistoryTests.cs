using Files.Core;

namespace Files.Tests;

public sealed class AliasAndHistoryTests
{
    [Fact]
    public void An_alias_resolves_only_when_it_is_the_whole_line()
    {
        Alias[] aliases = [new("pdfs", "ext:pdf example")];
        Assert.Equal("ext:pdf example", Alias.Resolve(aliases, " PDFS ")?.Code);
        Assert.Null(Alias.Resolve(aliases, "pdfs report"));
    }

    [Theory]
    [InlineData("", "abc", "an alias needs a name")]
    [InlineData("two words", "abc", "an alias name cannot contain spaces")]
    [InlineData("EX", "abc", "that name is already an alias")]
    [InlineData("new", "", "an alias needs the search it expands to")]
    [InlineData("new", "a*b", "the expansion is not a valid search: * is only allowed at the start or end")]
    public void Invalid_aliases_say_why(string name, string code, string problem)
    {
        Alias[] existing = [new("ex", "AA-A-0000")];
        Assert.Equal(problem, new Alias(name, code).Problem(existing));
    }

    [Fact]
    public void A_repeated_search_moves_to_the_front_ignoring_case()
    {
        IReadOnlyList<string> history = History.Record(["beta", "alpha"], "ALPHA");
        Assert.Equal(["ALPHA", "beta"], history);
    }

    [Fact]
    public void A_search_that_extends_the_newest_replaces_it()
    {
        Assert.Equal(["AB-C-0704"], History.Record(["AB-C"], "AB-C-0704"));
        Assert.Equal(["AB", "AB-C"], History.Record(["AB-C"], "AB"));
    }

    [Fact]
    public void Blank_searches_are_not_recorded_and_the_list_is_capped()
    {
        IReadOnlyList<string> history = [];
        Assert.Same(history, History.Record(history, "   "));
        for (int i = 0; i < History.MaxEntries + 10; i++)
        {
            history = History.Record(history, $"s{i:D3}");
        }
        Assert.Equal(History.MaxEntries, history.Count);
    }

    [Fact]
    public void History_survives_a_save_and_load()
    {
        string path = Path.Combine(Path.GetTempPath(), $"files-history-{Guid.NewGuid():N}.txt");
        try
        {
            History.Save(path, ["one", "two"]);
            Assert.Equal(["one", "two"], History.Load(path));
        }
        finally
        {
            File.Delete(path);
        }
    }
}
