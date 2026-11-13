using Files.Core;
using Files.UI;

namespace Files.Tests;

public sealed class FavoritesTests
{
    private static Favorite File(string path) => new(FavoriteKind.File, path);

    private static Favorite Search(string text) => new(FavoriteKind.Search, text);

    [Fact]
    public void Toggle_adds_at_the_front_then_removes_ignoring_case()
    {
        IReadOnlyList<Favorite> favorites = [File(@"C:\a.pdf")];

        favorites = Favorites.Toggle(favorites, File(@"C:\b.pdf"));
        Assert.Equal([File(@"C:\b.pdf"), File(@"C:\a.pdf")], favorites);

        favorites = Favorites.Toggle(favorites, File(@"c:\A.PDF"));
        Assert.Equal([File(@"C:\b.pdf")], favorites);
    }

    [Fact]
    public void A_file_and_a_search_with_the_same_text_are_different_favorites()
    {
        IReadOnlyList<Favorite> favorites = [File("plan")];
        Assert.False(Favorites.Contains(favorites, Search("plan")));
    }

    [Fact]
    public void Favorites_survive_a_save_and_load_and_a_bad_file_gives_none()
    {
        using var temp = new TempFolder();
        string path = System.IO.Path.Combine(temp.Path, "favorites.json");
        Favorites.Save(path, [File(@"C:\a.pdf"), Search("ext:dwg plan"), new Favorite(FavoriteKind.Folder, @"C:\jobs")]);

        Assert.Equal([File(@"C:\a.pdf"), Search("ext:dwg plan"), new Favorite(FavoriteKind.Folder, @"C:\jobs")], Favorites.Load(path));

        System.IO.File.WriteAllText(path, "{ not json");
        Assert.Empty(Favorites.Load(path));
    }

    [Fact]
    public void Entries_without_a_target_or_with_an_unknown_kind_are_dropped()
    {
        using var temp = new TempFolder();
        string path = temp.File("favorites.json", """[ { "Kind": "File", "Target": "C:\\a.pdf" }, null, { "Kind": "Search" } ]""");
        Assert.Equal([File(@"C:\a.pdf")], Favorites.Load(path));

        System.IO.File.WriteAllText(path, """[ { "Kind": "Planet", "Target": "x" } ]""");
        Assert.Empty(Favorites.Load(path));
    }

    [Fact]
    public void Home_shows_five_favorites_then_see_all_then_recently_opened()
    {
        Favorite[] favorites = Enumerable.Range(0, 7).Select(i => File($@"C:\f{i}.pdf")).ToArray();

        List<ResultRow> rows = HomeRows.Build(favorites, [@"C:\opened.pdf"], new HashSet<string>(), showAll: false);

        Assert.Equal(
            [RowKind.Header, RowKind.Favorite, RowKind.Favorite, RowKind.Favorite, RowKind.Favorite, RowKind.Favorite,
             RowKind.SeeAll, RowKind.Header, RowKind.Opened],
            rows.Select(row => row.Kind));
        Assert.Equal("Favorites", rows[0].Before);
        Assert.Equal("Recent", rows[7].Before);
    }

    [Fact]
    public void See_all_lists_every_favorite_and_nothing_else()
    {
        Favorite[] favorites = Enumerable.Range(0, 7).Select(i => File($@"C:\f{i}.pdf")).ToArray();
        List<ResultRow> rows = HomeRows.Build(favorites, [@"C:\opened.pdf"], new HashSet<string>(), showAll: true);
        Assert.Equal(8, rows.Count);
        Assert.All(rows.Skip(1), row => Assert.Equal(RowKind.Favorite, row.Kind));
    }

    [Fact]
    public void With_no_favorites_the_section_is_hidden()
    {
        List<ResultRow> rows = HomeRows.Build([], [@"C:\opened.pdf"], new HashSet<string>(), showAll: false);
        Assert.Equal([RowKind.Header, RowKind.Opened], rows.Select(row => row.Kind));
    }

    [Fact]
    public void A_favorite_file_is_not_repeated_in_recently_opened()
    {
        List<ResultRow> rows = HomeRows.Build([File(@"C:\a.pdf")], [@"c:\A.pdf", @"C:\b.pdf"], new HashSet<string>(), showAll: false);
        Assert.Equal([@"C:\a.pdf", @"C:\b.pdf"], rows.Where(row => !row.IsHeader).Select(row => row.Path));
    }

    [Fact]
    public void Missing_files_are_marked_but_searches_never_are()
    {
        using var temp = new TempFolder();
        string here = temp.File("here.pdf");
        string gone = System.IO.Path.Combine(temp.Path, "gone.pdf");
        Favorite[] favorites = [File(here), File(gone), new Favorite(FavoriteKind.Folder, temp.Path), Search(gone)];

        IReadOnlySet<string> missing = HomeRows.FindMissing(favorites);
        List<ResultRow> rows = HomeRows.Build(favorites, [], missing, showAll: false);

        Assert.Equal([gone], missing);
        Assert.Equal([false, true, false, false], rows.Skip(1).Select(row => row.Missing));
        Assert.Equal("Not found", rows[2].Badge);
    }
}
