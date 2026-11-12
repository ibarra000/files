using System.Diagnostics;
using Files.Core;
using Xunit.Abstractions;

namespace Files.Tests;

public sealed class SearchTests(ITestOutputHelper output)
{
    private static readonly IReadOnlySet<string> NothingHidden = new HashSet<string>();

    internal static VolumeIndex Index(string root, params (string Folder, string[] Files)[] folders)
    {
        var builder = new VolumeIndex.Builder(root);
        foreach (var (folder, files) in folders)
        {
            builder.AddFolder(folder, files);
        }
        return builder.Build();
    }

    private static List<string> Names(SearchResult result) => result.Hits.Select(hit => hit.Name).ToList();

    private static SearchResult Search(VolumeIndex index, string line, IReadOnlySet<string>? hidden = null) =>
        Searcher.Search([("v", index)], Query.Parse(line), hidden ?? NothingHidden, CancellationToken.None);

    [Fact]
    public void An_earlier_match_ranks_first_then_a_shorter_name()
    {
        VolumeIndex index = Index(@"C:\v", ("", ["xx-plan.pdf", "plan-long.pdf", "plan.pdf"]));
        Assert.Equal(["plan.pdf", "plan-long.pdf", "xx-plan.pdf"], Names(Search(index, "plan")));
    }

    [Fact]
    public void Matching_ignores_case_and_keeps_the_original_spelling()
    {
        VolumeIndex index = Index(@"C:\v", ("Docs", ["Annual REPORT.docx"]));
        Hit hit = Assert.Single(Search(index, "report").Hits);
        Assert.Equal("Annual REPORT.docx", hit.Name);
        Assert.Equal(@"C:\v\Docs\Annual REPORT.docx", hit.Path);
        Assert.Equal(7, hit.MatchStart);
    }

    [Fact]
    public void Files_in_a_matching_folder_follow_the_direct_matches_and_appear_once()
    {
        VolumeIndex index = Index(@"C:\v",
            ("", ["budget.xlsx"]),
            ("budget 2024", ["notes.txt", "budget-final.xlsx"]));
        SearchResult result = Search(index, "budget");
        Assert.Equal(["budget.xlsx", "budget-final.xlsx", "notes.txt"], Names(result));
        Assert.True(result.Hits[2].Inherited);
        Assert.Equal(3, result.Matched);
    }

    [Fact]
    public void A_matching_folder_adds_at_most_64_files()
    {
        string[] files = Enumerable.Range(0, 100).Select(i => $"file{i}.txt").ToArray();
        SearchResult result = Search(Index(@"C:\v", ("", []), ("match", files)), "match");
        Assert.Equal(64, result.Hits.Count);
        Assert.Equal(100, result.Matched);
    }

    [Fact]
    public void Hidden_extensions_and_ext_filters_apply_before_counting()
    {
        VolumeIndex index = Index(@"C:\v", ("", ["a.pdf", "a.dwg", "a.tmp"]));
        Assert.Equal(["a.dwg", "a.pdf"], Names(Search(index, "a", new HashSet<string> { "tmp" })));
        SearchResult filtered = Search(index, "a ext:pdf");
        Assert.Equal(["a.pdf"], Names(filtered));
        Assert.Equal(1, filtered.Matched);
    }

    [Fact]
    public void Prefix_mode_needs_the_name_to_start_with_the_term()
    {
        VolumeIndex index = Index(@"C:\v", ("", ["plan.pdf", "my plan.pdf"]));
        Assert.Equal(["plan.pdf"], Names(Search(index, "plan*")));
    }

    [Fact]
    public void A_match_never_spans_two_names()
    {
        VolumeIndex index = Index(@"C:\v", ("", ["ab", "cd"]));
        Assert.Empty(Search(index, "bc").Hits);
    }

    [Fact]
    public void Results_from_several_volumes_merge_into_one_ranking()
    {
        VolumeIndex first = Index(@"C:\a", ("", ["xplan.txt"]));
        VolumeIndex second = Index(@"D:\b", ("", ["plan.txt"]));
        SearchResult result = Searcher.Search([("a", first), ("b", second)], Query.Parse("plan"), NothingHidden, CancellationToken.None);
        Assert.Equal(["b", "a"], result.Hits.Select(hit => hit.Volume));
    }

    [Fact]
    public void Names_survive_the_arena_growing_many_times()
    {
        var builder = new Names.Builder();
        for (int i = 0; i < 20_000; i++)
        {
            builder.Add($"Name-{i}");
        }

        Names names = builder.Build();

        Assert.Equal(20_000, names.Count);
        Assert.Equal("Name-0", names[0]);
        Assert.Equal("Name-12345", names[12345]);
        Assert.Equal("name-19999", names.LowerSpan(19999).ToString());
    }

    [Fact]
    public void Top_k_keeps_the_lowest_keys_in_order()
    {
        var top = new TopK(3);
        foreach (ulong key in new ulong[] { 9, 1, 8, 2, 7, 3 })
        {
            top.Add(key);
        }
        Assert.Equal([1UL, 2UL, 3UL], top.Sorted());
    }

    [Fact]
    public void A_million_names_search_in_milliseconds()
    {
        var builder = new VolumeIndex.Builder(@"C:\v");
        for (int folder = 0; folder < 1000; folder++)
        {
            builder.AddFolder($"folder{folder}", Enumerable.Range(0, 1000).Select(i => $"AB-C-{folder:D4}_Page{i}.pdf"));
        }
        VolumeIndex index = builder.Build();
        Search(index, "0704_page1"); // warm up the thread pool

        var clock = Stopwatch.StartNew();
        SearchResult result = Search(index, "0704_page1");
        clock.Stop();

        output.WriteLine($"{index.FileCount:N0} names searched in {clock.Elapsed.TotalMilliseconds:F1} ms");
        Assert.Equal(111, result.Matched);
        Assert.True(clock.ElapsedMilliseconds < 500, $"took {clock.ElapsedMilliseconds} ms");
    }
}
