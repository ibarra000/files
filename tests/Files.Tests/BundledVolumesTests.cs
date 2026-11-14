using Files.Data;
using Files.Services;

namespace Files.Tests;

public sealed class BundledVolumesTests
{
    private static Indexer NewIndexer(TempFolder temp) =>
        new(Catalog.Open(Path.Combine(temp.Path, "catalog.db")), Path.Combine(temp.Path, "volumes"), () => new Settings { RescanHours = 0 });

    private static string Json(params (string Name, string Folder, bool LiveOnly)[] volumes) =>
        System.Text.Json.JsonSerializer.Serialize(volumes.Select(v => new { name = v.Name, folder = v.Folder, liveOnly = v.LiveOnly }));

    [Fact]
    public void Bundled_volumes_are_added_once_and_a_removed_one_stays_removed()
    {
        using var temp = new TempFolder();
        string drawings = Directory.CreateDirectory(Path.Combine(temp.Path, "drawings")).FullName;
        string archive = Directory.CreateDirectory(Path.Combine(temp.Path, "archive")).FullName;
        string bundle = temp.File("volumes.json", Json(("Drawings", drawings, false), ("Archive", archive, true)));
        string added = Path.Combine(temp.Path, "bundled.txt");
        using Indexer indexer = NewIndexer(temp);

        Assert.Empty(BundledVolumes.Add(bundle, added, indexer));
        Assert.Equal(["Archive", "Drawings"], indexer.Volumes.Select(state => state.Volume.Name));
        Assert.True(indexer.Volumes.Single(state => state.Volume.Name == "Archive").Volume.LiveOnly);

        indexer.Remove(indexer.Volumes.Single(state => state.Volume.Name == "Drawings").Volume.Id);
        Assert.Empty(BundledVolumes.Add(bundle, added, indexer));

        Assert.Equal("Archive", Assert.Single(indexer.Volumes).Volume.Name);
    }

    [Fact]
    public void A_volume_new_in_a_later_installer_is_added_and_one_already_there_is_kept()
    {
        using var temp = new TempFolder();
        string mine = Directory.CreateDirectory(Path.Combine(temp.Path, "mine")).FullName;
        string later = Directory.CreateDirectory(Path.Combine(temp.Path, "later")).FullName;
        string added = Path.Combine(temp.Path, "bundled.txt");
        using Indexer indexer = NewIndexer(temp);
        Assert.Null(indexer.Add("My name", mine));

        string bundle = temp.File("volumes.json", Json(("Bundled name", mine, false), ("Later", later, false)));
        Assert.Empty(BundledVolumes.Add(bundle, added, indexer));

        Assert.Equal(["Later", "My name"], indexer.Volumes.Select(state => state.Volume.Name));
    }

    [Fact]
    public void No_file_adds_nothing_and_a_bad_file_or_entry_says_so()
    {
        using var temp = new TempFolder();
        string added = Path.Combine(temp.Path, "bundled.txt");
        using Indexer indexer = NewIndexer(temp);

        Assert.Empty(BundledVolumes.Add(Path.Combine(temp.Path, "none.json"), added, indexer));
        Assert.StartsWith("The bundled volumes could not be read", Assert.Single(BundledVolumes.Add(temp.File("bad.json", "{ not json"), added, indexer)));

        string bundle = temp.File("volumes.json", "[{ \"name\": \"\", \"folder\": \"x\" }, { \"name\": \"ok\", \"folder\": \"" + temp.Path.Replace("\\", "\\\\") + "\" }]");
        string problem = Assert.Single(BundledVolumes.Add(bundle, added, indexer));

        Assert.StartsWith("Bundled volume 1: ", problem);
        Assert.Equal("ok", Assert.Single(indexer.Volumes).Volume.Name);
    }
}
