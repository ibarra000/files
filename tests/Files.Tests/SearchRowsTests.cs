using Files.Core;
using Files.UI;

namespace Files.Tests;

public sealed class SearchRowsTests
{
    private static readonly VolumeIndex Index = new VolumeIndex.Builder(@"C:\v").Build();

    private static Hit File(string name) => new("v", Index, 0, name, @"C:\v", 0, 1, 0);

    private static Hit Folder(string name) => new("v", Index, 1, name, @"C:\v", 0, 1, 0) { IsFolder = true };

    private static List<string> Titles(List<ResultRow> rows) => rows.Select(row => row.IsHeader ? $"[{row.Before}]" : row.Before + row.Match + row.After).ToList();

    [Fact]
    public void Results_group_as_best_match_then_folders_then_files()
    {
        var result = new SearchResult([File("a.pdf"), File("b.pdf"), File("c.pdf")], 3) { Folders = [Folder("a")] };
        Assert.Equal(["[Best match]", "a.pdf", "[Folders]", "a", "[Files]", "b.pdf", "c.pdf"], Titles(SearchRows.Build(result, showVolume: false)));
    }

    [Fact]
    public void Only_the_first_few_folders_are_listed_among_files()
    {
        Hit[] folders = Enumerable.Range(0, 8).Select(i => Folder($"f{i}")).ToArray();
        var result = new SearchResult([File("a.pdf")], 1) { Folders = folders };
        List<ResultRow> rows = SearchRows.Build(result, showVolume: false);
        Assert.Equal(SearchRows.FoldersShown, rows.Count(row => row.Hit?.IsFolder == true));
    }

    [Fact]
    public void A_single_result_is_only_the_best_match()
    {
        var result = new SearchResult([File("a.pdf")], 1);
        Assert.Equal(["[Best match]", "a.pdf"], Titles(SearchRows.Build(result, showVolume: false)));
    }

    [Fact]
    public void With_no_files_the_best_folder_is_the_best_match()
    {
        var result = new SearchResult([], 0) { Folders = [Folder("a"), Folder("b")] };
        Assert.Equal(["[Best match]", "a", "[Folders]", "b"], Titles(SearchRows.Build(result, showVolume: false)));
    }

    [Fact]
    public void Nothing_found_lists_nothing()
    {
        Assert.Empty(SearchRows.Build(SearchResult.Empty, showVolume: false));
    }

    [Fact]
    public void The_files_filter_leaves_folders_out()
    {
        var result = new SearchResult([File("a.pdf"), File("b.pdf")], 2) { Folders = [Folder("a")] };
        Assert.Equal(["[Best match]", "a.pdf", "[Files]", "b.pdf"], Titles(SearchRows.Build(result, showVolume: false, SearchFilter.Files)));
    }

    [Fact]
    public void The_folders_filter_lists_every_folder_found_and_no_files()
    {
        Hit[] folders = Enumerable.Range(0, 8).Select(i => Folder($"f{i}")).ToArray();
        var result = new SearchResult([File("a.pdf")], 1) { Folders = folders };
        List<ResultRow> rows = SearchRows.Build(result, showVolume: false, SearchFilter.Folders);
        Assert.Equal(8, rows.Count(row => row.Hit?.IsFolder == true));
        Assert.DoesNotContain(rows, row => row.Hit is { IsFolder: false });
    }

    [Theory]
    [InlineData(SearchFilter.All, "plan")]
    [InlineData(SearchFilter.Files, "plan")]
    [InlineData(SearchFilter.Folders, "plan")]
    public void Filters_that_are_not_about_types_leave_the_search_as_typed(SearchFilter filter, string line)
    {
        Query query = Query.Parse(line);
        Assert.Same(query, SearchFilters.Narrow(query, filter));
    }

    [Fact]
    public void The_code_and_documents_filters_narrow_the_search_to_their_types()
    {
        Assert.Contains("cs", SearchFilters.Narrow(Query.Parse("plan"), SearchFilter.Code).Extensions);
        Assert.Contains("pdf", SearchFilters.Narrow(Query.Parse("plan"), SearchFilter.Documents).Extensions);
    }

    [Fact]
    public void A_folder_result_shows_a_folder_icon()
    {
        var result = new SearchResult([], 0) { Folders = [Folder("plans")] };
        Assert.Equal(FileIcons.Folder, SearchRows.Build(result, showVolume: false)[1].Icon);
    }
}
