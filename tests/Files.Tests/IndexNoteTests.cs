using Files.Core;

namespace Files.Tests;

public sealed class IndexNoteTests
{
    private static readonly DateTime Earlier = new(2026, 1, 1, 9, 0, 0, DateTimeKind.Utc);
    private static readonly DateTime Later = Earlier.AddMinutes(5);

    private static VolumeIndex Volume()
    {
        var builder = new VolumeIndex.Builder(@"C:\v");
        builder.AddFolder("", ["report.pdf"]);
        builder.AddFolder("2024", ["plan.pdf", "report-2024.pdf"]);
        return builder.Build();
    }

    private static SearchResult Search(IndexNote note, string term)
    {
        var volumes = new List<(string, VolumeIndex)> { ("v", Volume()) };
        if (note.AddedIndex(@"C:\v") is VolumeIndex added)
        {
            volumes.Add(("v", added));
        }
        SearchResult found = Searcher.Search(volumes, Query.Parse(term), new HashSet<string>(), CancellationToken.None);
        return IndexNote.Apply(found, [note]);
    }

    private static List<string> Paths(SearchResult result) => result.Hits.Select(hit => hit.Path).Order().ToList();

    [Fact]
    public void Removed_files_leave_the_results()
    {
        IndexNote note = IndexNote.Empty.With(removed: [@"C:\v\2024\report-2024.pdf"], added: [], Earlier);

        Assert.Equal([@"C:\v\report.pdf"], Paths(Search(note, "report")));
    }

    [Fact]
    public void A_removed_folder_takes_its_files_with_it()
    {
        IndexNote note = IndexNote.Empty.With(removed: [@"C:\v\2024"], added: [], Earlier);

        SearchResult result = Search(note, "pdf");

        Assert.Equal([@"C:\v\report.pdf"], Paths(result));
        Assert.True(note.Hides(@"C:\V\2024\plan.pdf"));
        Assert.False(note.Hides(@"C:\v\2024-old\plan.pdf"));
    }

    [Fact]
    public void Added_files_that_match_join_the_results_once()
    {
        IndexNote note = IndexNote.Empty.With(removed: [@"C:\v\report.pdf"], added: [new(@"C:\v\2024\report (1).pdf", false), new(@"C:\v\2024\plan.pdf", false)], Earlier);

        SearchResult result = Search(note, "report");

        Assert.Equal([@"C:\v\2024\report (1).pdf", @"C:\v\2024\report-2024.pdf"], Paths(result));
        Assert.Single(Search(note, "plan").Hits);
    }

    [Fact]
    public void Putting_a_file_back_undoes_its_removal()
    {
        IndexNote note = IndexNote.Empty
            .With(removed: [@"C:\v\report.pdf"], added: [new(@"C:\v\2024\report.pdf", false)], Earlier)
            .With(removed: [@"C:\v\2024\report.pdf"], added: [new(@"C:\v\report.pdf", false)], Later);

        Assert.False(note.Hides(@"C:\v\report.pdf"));
        Assert.Equal([@"C:\v\2024\report-2024.pdf", @"C:\v\report.pdf"], Paths(Search(note, "report")));
    }

    [Fact]
    public void A_read_that_started_later_replaces_the_note()
    {
        IndexNote note = IndexNote.Empty
            .With(removed: [@"C:\v\report.pdf"], added: [], Earlier)
            .With(removed: [@"C:\v\2024\plan.pdf"], added: [], Later);

        IndexNote kept = note.Since(Earlier.AddMinutes(1));

        Assert.False(kept.Hides(@"C:\v\report.pdf"));
        Assert.True(kept.Hides(@"C:\v\2024\plan.pdf"));
        Assert.True(note.Since(Later.AddMinutes(1)).IsEmpty);
    }
}
