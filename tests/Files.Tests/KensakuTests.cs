using System.Text.Json;
using Files.Core;
using Files.Data;
using Files.Services;

namespace Files.Tests;

public sealed class KensakuTests
{
    private static readonly KensakuQuery Anything = new("torque", new HashSet<string>());

    private static JsonElement Json(string text) => JsonDocument.Parse(text).RootElement.Clone();

    private static Volume Volume(string root, bool liveOnly = false) =>
        new(1, Guid.NewGuid(), root, "local", Path.GetFileName(root), VolumeStatus.Online, null, null, liveOnly);

    private static List<string> Texts(IEnumerable<KensakuCall> calls) => calls.Select(call => $"{call.Method} {call.Parameters.ToJsonString()}").ToList();

    [Fact]
    public void Content_matches_come_back_and_name_only_matches_and_folders_are_left_to_the_panel()
    {
        JsonElement answer = Json("""
            {"results":[
              {"path":"C:\\docs\\report.pdf","score":2.5,"snippet":"the  torque\nlimit is","match_type":"Content","is_dir":false},
              {"path":"C:\\docs\\torque.txt","score":2.0,"snippet":null,"match_type":"FileName","is_dir":false},
              {"path":"C:\\docs\\torque","score":1.5,"snippet":null,"match_type":"Semantic","is_dir":true},
              {"path":"C:\\docs\\scan.png","score":1.0,"snippet":null,"match_type":"ContentFolded","is_dir":false}
            ],"total":4}
            """);

        ContentResult result = Kensaku.Parse(answer, Anything);

        Assert.Null(result.Problem);
        Assert.Equal([@"C:\docs\report.pdf", @"C:\docs\scan.png"], result.Matches.Select(match => match.Path));
        Assert.Equal("the torque limit is", result.Matches[0].Snippet);
    }

    [Fact]
    public void An_answer_without_results_is_an_unsupported_version()
    {
        Assert.Equal("this kensaku version is not supported", Kensaku.Parse(Json("""{"hits":[]}"""), Anything).Problem);
    }

    [Fact]
    public void Several_extensions_are_kept_here_since_kensaku_would_want_all_of_them_at_once()
    {
        KensakuQuery query = KensakuQuery.For(Query.Parse("torque ext:pdf,dwg"), null)!;
        JsonElement answer = Json("""
            {"results":[
              {"path":"C:\\a.pdf","score":3,"match_type":"Content"},
              {"path":"C:\\b.txt","score":2,"match_type":"Content"},
              {"path":"C:\\c.DWG","score":1,"match_type":"Content"}
            ]}
            """);

        Assert.Equal("torque", query.Text);
        Assert.Equal([@"C:\a.pdf", @"C:\c.DWG"], Kensaku.Parse(answer, query).Matches.Select(match => match.Path));
    }

    [Theory]
    [InlineData("torque limit", null, "torque limit")]
    [InlineData("torque*", null, "torque")]
    [InlineData("torque ext:pdf", null, "torque ext:pdf")]
    [InlineData("-draft torque", null, "\"-draft\" torque")]
    [InlineData("report", @"C:\docs", @"report in:""C:\\docs""")]
    [InlineData("report", @"\\server\share", @"report in:""\\\\server\\share""")]
    public void A_search_is_put_in_kensakus_query_language(string line, string? scope, string text)
    {
        Assert.Equal(text, KensakuQuery.For(Query.Parse(line), scope)!.Text);
    }

    [Fact]
    public void A_type_filter_none_of_the_typed_extensions_fits_asks_kensaku_nothing()
    {
        Query query = Query.Parse("torque ext:txt").Within(new HashSet<string> { "pdf" });

        Assert.Null(KensakuQuery.For(query, null));
    }

    [Fact]
    public void Only_the_roots_of_volumes_read_in_the_background_are_watched()
    {
        KensakuWanted wanted = KensakuSetup.Wanted(
            [Volume(@"\\server\drawings"), Volume(@"\\server\archive", liveOnly: true), Volume(@"C:\docs"), Volume(@"c:\DOCS")], ocr: true);

        Assert.Equal([@"C:\docs", @"\\server\drawings"], wanted.WatchedDirs);
        Assert.True(wanted.Ocr);
        Assert.Equal(wanted, new KensakuWanted([@"c:\docs", @"\\server\drawings"], true));
    }

    [Fact]
    public void A_new_data_folder_is_set_up_then_given_the_ocr_setting()
    {
        var wanted = new KensakuWanted([@"C:\docs"], Ocr: false);

        Assert.Equal(
            ["""setup {"watched_dirs":["C:\\docs"]}""", """patch_config {"patch":{"ocr_enabled":false}}"""],
            Texts(KensakuSetup.Calls(setupComplete: false, applied: null, wanted)));
    }

    [Fact]
    public void A_daemon_set_up_before_gets_the_volumes_and_later_only_what_changed()
    {
        var docs = new KensakuWanted([@"C:\docs"], Ocr: false);
        var more = new KensakuWanted([@"C:\docs", @"D:\"], Ocr: false);

        Assert.Equal(
            ["""patch_config {"patch":{"watched_dirs":["C:\\docs"],"ocr_enabled":false}}"""],
            Texts(KensakuSetup.Calls(setupComplete: true, applied: null, docs)));
        Assert.Equal(
            ["""patch_config {"patch":{"watched_dirs":["C:\\docs","D:\\"]}}"""],
            Texts(KensakuSetup.Calls(setupComplete: true, applied: docs, more)));
        Assert.Empty(KensakuSetup.Calls(setupComplete: true, applied: docs, docs));
    }

    [Fact]
    public void Turning_ocr_on_downloads_its_pack_and_turning_it_off_only_says_so()
    {
        var off = new KensakuWanted([@"C:\docs"], Ocr: false);
        var on = off with { Ocr = true };

        Assert.Equal(
            ["""patch_config {"patch":{"ocr_enabled":true}}""", """pack_download {"pack":"ocr"}"""],
            Texts(KensakuSetup.Calls(setupComplete: true, applied: off, on)));
        Assert.Equal(
            ["""patch_config {"patch":{"ocr_enabled":false}}"""],
            Texts(KensakuSetup.Calls(setupComplete: true, applied: on, off)));
    }

    [Theory]
    [InlineData("""{"roots":["a","b","c"],"files":12400,"content_docs":3100}""", """{"is_indexing":false}""", "Reading 3 volumes · 12,400 files · 3,100 with text")]
    [InlineData("""{"roots":["a"],"files":5,"content_docs":1}""", """{"is_indexing":true}""", "Indexing 1 volume · 5 files so far · 1 with text")]
    [InlineData("null", "{}", "kensaku is running; its index is not open yet")]
    public void The_status_line_says_what_kensaku_reads(string state, string status, string line)
    {
        using var culture = new CultureScope("en-US");

        Assert.Equal(line, Kensaku.Describe(Json(status), Json(state)));
    }

    [Fact]
    public void Content_matches_come_after_the_name_results_without_repeating_them()
    {
        VolumeIndex index = new VolumeIndex.Builder(@"C:\docs").Build();
        var result = new SearchResult([new Hit("v", index, 0, "torque.txt", @"C:\docs", 0, 6, 0)], 1);
        ContentMatch[] contents =
        [
            new(@"C:\docs\torque.txt", 2, null, "Both"),
            new(@"C:\docs\report.pdf", 1, "the torque limit", "Content"),
        ];

        List<Files.UI.ResultRow> rows = Files.UI.SearchRows.Build(result, showVolume: false, contents: contents);

        Assert.Equal(["Best match", "torque.txt", "Contents", "report.pdf"], rows.Select(row => row.Before + row.Match + row.After));
        Assert.Equal("…the torque limit…", rows[^1].Detail);
        Assert.Equal(@"C:\docs\report.pdf", rows[^1].Path);
    }

    [Fact]
    public void Content_search_and_reading_images_survive_a_save_and_the_old_kensaku_keys_are_ignored()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "settings.json");
        new Settings { ContentSearch = true, ContentOcr = true }.Save(path);
        (Settings loaded, _) = Settings.Load(path);
        Assert.Equal((true, true), (loaded.ContentSearch, loaded.ContentOcr));

        string old = temp.File("old.json", """{ "ContentSearch": true, "KensakuCli": "kensaku-cli.exe", "KensakuData": "data", "KensakuPort": 9000 }""");
        (Settings migrated, string? problem) = Settings.Load(old);
        Assert.Null(problem);
        Assert.Equal((true, false), (migrated.ContentSearch, migrated.ContentOcr));
    }

    /// <summary>Sets the current culture for a test, so numbers are written the same on every PC.</summary>
    private sealed class CultureScope : IDisposable
    {
        private readonly System.Globalization.CultureInfo _previous = System.Globalization.CultureInfo.CurrentCulture;

        public CultureScope(string name) => System.Globalization.CultureInfo.CurrentCulture = new System.Globalization.CultureInfo(name);

        public void Dispose() => System.Globalization.CultureInfo.CurrentCulture = _previous;
    }
}
