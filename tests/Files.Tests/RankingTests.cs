using Files.Core;

namespace Files.Tests;

public sealed class RankingTests
{
    private static readonly IReadOnlySet<string> NothingHidden = new HashSet<string>();

    private static SearchResult Search(VolumeIndex index, string line, IReadOnlyDictionary<string, OpenCount>? opened = null) =>
        Searcher.Search([("v", index)], Query.Parse(line), NothingHidden, CancellationToken.None, opened);

    private static List<string> Names(SearchResult result) => result.Hits.Select(hit => hit.Name).ToList();

    [Fact]
    public void A_whole_name_ranks_first_then_its_start_then_a_word_start_then_anywhere()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["reinvoiced.pdf", "2026 invoice.pdf", "invoices 2026.pdf", "invoice.pdf"]));

        Assert.Equal(["invoice.pdf", "invoices 2026.pdf", "2026 invoice.pdf", "reinvoiced.pdf"], Names(Search(index, "invoice")));
    }

    [Fact]
    public void A_word_starts_after_a_lower_case_letter_before_a_capital_and_after_digits()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["xinvoice.cs", "MyInvoice.cs", "2026inv.pdf"]));

        SearchResult result = Search(index, "inv");

        // Both word starts rank above the match inside "xinvoice", the earlier one first.
        Assert.Equal(["MyInvoice.cs", "2026inv.pdf", "xinvoice.cs"], Names(result));
        Assert.Equal(2, result.Hits.Single(hit => hit.Name == "MyInvoice.cs").MatchStart);
    }

    [Fact]
    public void A_later_match_at_a_word_start_is_the_one_highlighted()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["reinvoice invoice.pdf"]));

        Hit hit = Assert.Single(Search(index, "invoice").Hits);

        Assert.Equal(10, hit.MatchStart);
        Assert.Equal(7, hit.MatchLength);
    }

    [Fact]
    public void Files_opened_from_the_panel_rank_first_within_their_kind_of_match()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["plan a.pdf", "plan b.pdf", "plan c.pdf", "plan.pdf"]));
        var opened = new Dictionary<string, OpenCount>(StringComparer.OrdinalIgnoreCase)
        {
            [@"C:\v\plan c.pdf"] = new(3, DateTime.UtcNow.AddDays(-1)),
            [@"C:\v\plan b.pdf"] = new(3, DateTime.UtcNow),
        };

        // A whole-name match still beats any opened file; among the prefix matches, more opens and then the latest come first.
        Assert.Equal(["plan.pdf", "plan b.pdf", "plan c.pdf", "plan a.pdf"], Names(Search(index, "plan", opened)));
    }

    [Fact]
    public void An_opened_file_is_found_even_when_it_would_not_make_the_top_results()
    {
        string[] many = Enumerable.Range(0, VolumeIndex.MaxResults + 100).Select(i => $"plan {i:D4}.pdf").ToArray();
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", many), ("deep", ["plan with a much longer name than the others.pdf"]));
        var opened = new Dictionary<string, OpenCount>(StringComparer.OrdinalIgnoreCase)
        {
            [@"C:\v\deep\plan with a much longer name than the others.pdf"] = new(1, DateTime.UtcNow),
        };

        Assert.Equal("plan with a much longer name than the others.pdf", Search(index, "plan", opened).Hits[0].Name);
    }

    [Fact]
    public void Rows_keep_their_order_as_the_search_grows()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["2026 invoice b.pdf", "2026 invoice a.pdf", "invoice.pdf"]));

        foreach (string typed in new[] { "in", "inv", "invo", "invoice" })
        {
            List<string> names = Names(Search(index, typed));
            Assert.True(names.IndexOf("2026 invoice a.pdf") < names.IndexOf("2026 invoice b.pdf"), typed);
            Assert.Equal("invoice.pdf", names[0]);
        }
    }

    [Fact]
    public void Open_counts_add_up_and_survive_a_restart()
    {
        using var temp = new TempFolder();
        string file = Path.Combine(temp.Path, "opened-counts.txt");
        DateTime now = new(2026, 11, 21, 9, 0, 0, DateTimeKind.Utc);

        IReadOnlyDictionary<string, OpenCount> counts = OpenCounts.Record(OpenCounts.Empty, @"C:\v\plan.pdf", now);
        counts = OpenCounts.Record(counts, @"c:\V\PLAN.pdf", now.AddHours(1));
        OpenCounts.Save(file, counts);

        OpenCount loaded = OpenCounts.Load(file)[@"C:\v\plan.pdf"];
        Assert.Equal(new OpenCount(2, now.AddHours(1)), loaded);
    }

    [Fact]
    public void Only_the_most_recently_opened_files_are_counted()
    {
        IReadOnlyDictionary<string, OpenCount> counts = OpenCounts.Empty;
        DateTime start = new(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);
        for (int i = 0; i < OpenCounts.MaxEntries + 5; i++)
        {
            counts = OpenCounts.Record(counts, $@"C:\v\{i}.pdf", start.AddMinutes(i));
        }

        Assert.Equal(OpenCounts.MaxEntries, counts.Count);
        Assert.False(counts.ContainsKey(@"C:\v\0.pdf"));
        Assert.True(counts.ContainsKey($@"C:\v\{OpenCounts.MaxEntries + 4}.pdf"));
    }

    [Fact]
    public void A_damaged_line_in_the_counts_file_is_skipped()
    {
        using var temp = new TempFolder();
        string file = Path.Combine(temp.Path, "opened-counts.txt");
        File.WriteAllLines(file, ["nonsense", "2\t638680000000000000\tC:\\v\\plan.pdf", "x\ty\tC:\\v\\other.pdf", "1\t638680000000000000\tC:\\v\\bad\u0001name.pdf"]);

        IReadOnlyDictionary<string, OpenCount> counts = OpenCounts.Load(file);

        Assert.Equal(2, Assert.Single(counts).Value.Count);
    }
}
