using Files.Core;

namespace Files.Tests;

public sealed class LiveSearchTests
{
    private static readonly IReadOnlySet<string> NothingHidden = new HashSet<string>();

    private static async Task<(LiveSearch Live, SearchResult Result)> Run(string line, IReadOnlySet<string>? hidden, params (string Name, string Root)[] volumes)
    {
        var live = new LiveSearch(volumes, Query.Parse(line), hidden ?? NothingHidden, skipHidden: true);
        await live.RunAsync(CancellationToken.None);
        return (live, live.Snapshot());
    }

    [Fact]
    public async Task A_file_no_index_knows_about_is_found()
    {
        using var temp = new TempFolder();
        string added = temp.File(@"jobs\1234\plan-new.pdf");
        temp.File(@"jobs\1234\other.txt");

        (LiveSearch live, SearchResult result) = await Run("plan", null, ("v", temp.Path));

        Hit hit = Assert.Single(result.Hits);
        Assert.Equal(added, hit.Path);
        Assert.Equal("v", hit.Volume);
        Assert.Equal(0, hit.MatchStart);
        Assert.Equal(3, live.FoldersRead);
    }

    [Fact]
    public async Task A_matching_folder_is_found_live_too()
    {
        using var temp = new TempFolder();
        temp.File(@"jobs\plans\notes.txt");
        Directory.CreateDirectory(Path.Combine(temp.Path, "empty plans"));

        (_, SearchResult result) = await Run("plans", null, ("v", temp.Path));

        Assert.Equal(["plans", "empty plans"], result.Folders.Select(hit => hit.Name));
    }

    [Fact]
    public async Task Ext_filters_and_hidden_extensions_apply()
    {
        using var temp = new TempFolder();
        temp.File("a.pdf");
        temp.File("a.dwg");
        temp.File("a.tmp");

        (_, SearchResult hiddenTmp) = await Run("a", new HashSet<string> { "tmp" }, ("v", temp.Path));
        (_, SearchResult onlyPdf) = await Run("a ext:pdf", null, ("v", temp.Path));

        Assert.Equal(["a.dwg", "a.pdf"], hiddenTmp.Hits.Select(hit => hit.Name));
        Assert.Equal(["a.pdf"], onlyPdf.Hits.Select(hit => hit.Name));
    }

    [Fact]
    public async Task Files_in_a_matching_folder_come_after_direct_matches()
    {
        using var temp = new TempFolder();
        temp.File("budget.xlsx");
        temp.File(@"budget 2024\notes.txt");
        temp.File(@"elsewhere\notes.txt");

        (_, SearchResult result) = await Run("budget", null, ("v", temp.Path));

        Assert.Equal(["budget.xlsx", "notes.txt"], result.Hits.Select(hit => hit.Name));
        Assert.True(result.Hits[1].Inherited);
        Assert.Equal(Path.Combine(temp.Path, "budget 2024"), result.Hits[1].Folder);
    }

    [Fact]
    public async Task A_volume_that_cannot_be_read_is_reported_and_the_others_still_answer()
    {
        using var temp = new TempFolder();
        temp.File("plan.pdf");

        (LiveSearch live, SearchResult result) = await Run("plan", null,
            ("gone", @"C:\no-such-folder-for-files-tests"),
            ("here", temp.Path));

        Assert.Equal("here", Assert.Single(result.Hits).Volume);
        Assert.StartsWith("gone could not be read", Assert.Single(live.Problems));
    }

    [Fact]
    public async Task A_cancelled_search_stops()
    {
        using var temp = new TempFolder();
        temp.File(@"a\plan.pdf");
        using var cancel = new CancellationTokenSource();
        cancel.Cancel();
        var live = new LiveSearch([("v", temp.Path)], Query.Parse("plan"), NothingHidden, skipHidden: true);

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => live.RunAsync(cancel.Token));
    }

    [Fact]
    public async Task Past_the_cap_matches_are_counted_but_not_kept()
    {
        using var temp = new TempFolder();
        for (int i = 0; i < 30; i++)
        {
            temp.File($@"f{i % 3}\plan{i}.pdf");
        }
        var live = new LiveSearch([("v", temp.Path)], Query.Parse("plan"), NothingHidden, skipHidden: true, maxKept: 10);

        await live.RunAsync(CancellationToken.None);
        SearchResult result = live.Snapshot();

        Assert.True(live.IsCapped);
        Assert.Equal(10, result.Hits.Count);
        Assert.Equal(30, result.Matched);
    }

    [Fact]
    public void A_failing_filter_ends_the_walk_with_its_error_instead_of_the_process()
    {
        using var temp = new TempFolder();
        temp.File(@"a\x.txt");
        temp.File(@"b\y.txt");

        var error = Assert.Throws<InvalidOperationException>(() => Walker.Walk(
            temp.Path,
            skipHidden: true,
            CancellationToken.None,
            (relative, files) => relative.Length > 0 ? throw new InvalidOperationException("boom") : files,
            (_, _) => { }));

        Assert.Equal("boom", error.Message);
    }

    [Fact]
    public void A_snapshot_before_anything_is_read_is_empty()
    {
        var live = new LiveSearch([("v", @"C:\v")], Query.Parse("plan"), NothingHidden, skipHidden: true);
        Assert.Empty(live.Snapshot().Hits);
    }

    [Fact]
    public void The_walker_stores_only_what_it_is_told_to_keep()
    {
        using var temp = new TempFolder();
        temp.File("keep.txt");
        temp.File(@"a\drop.txt");
        temp.File(@"b\keep-too.txt");
        var seen = new List<string>();

        WalkResult walk = Walker.Walk(
            temp.Path,
            skipHidden: true,
            CancellationToken.None,
            (_, files) => files.Where(name => name.StartsWith("keep", StringComparison.Ordinal)).ToList(),
            (relative, files) =>
            {
                lock (seen)
                {
                    seen.AddRange(files.Select(file => Path.Join(relative, file)));
                }
            });

        Assert.Equal(2, walk.Index.FileCount);
        Assert.Equal(2, walk.Folders);
        Assert.Equal(new[] { "keep.txt", @"b\keep-too.txt" }.Order(), seen.Order());
    }
}
