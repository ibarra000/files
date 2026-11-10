using Files.Core;
using Files.Data;
using Microsoft.Data.Sqlite;

namespace Files.Tests;

public sealed class DataTests
{
    [Fact]
    public void A_new_catalog_is_migrated_and_marked_as_ours()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "catalog.db");
        Catalog.Open(path);
        Catalog.Open(path); // a second open must not run migration 1 again

        using var connection = new SqliteConnection($"Data Source={path}");
        connection.Open();
        Assert.Equal(1L, Scalar(connection, "PRAGMA user_version"));
        Assert.Equal(0x46494C45L, Scalar(connection, "PRAGMA application_id"));
    }

    [Fact]
    public void Volumes_are_added_with_a_normalized_root_and_listed()
    {
        using var temp = new TempFolder();
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        string folder = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;

        Volume volume = catalog.Add(" Share ", folder + @"\");

        Assert.Equal(folder, volume.Root);
        Assert.Equal("Share", volume.Name);
        Assert.Equal("local", volume.Type);
        Assert.Equal(VolumeStatus.Offline, volume.Status);
        Assert.Null(volume.LastScannedAt);
        Assert.Throws<SqliteException>(() => catalog.Add("again", folder));
    }

    [Fact]
    public void Status_errors_and_scan_times_round_trip()
    {
        using var temp = new TempFolder();
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        Volume volume = catalog.Add("v", temp.Path);

        catalog.SetStatus(volume.Id, VolumeStatus.Offline, VolumeError.PermissionDenied);
        catalog.MarkScanned(volume.Id);

        Volume stored = Assert.Single(catalog.All());
        Assert.Equal(VolumeError.PermissionDenied, stored.LastError);
        Assert.NotNull(stored.LastScannedAt);
        Assert.Equal(DateTimeKind.Utc, stored.LastScannedAt!.Value.Kind);
    }

    [Fact]
    public void A_volume_root_can_never_change()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "catalog.db");
        Catalog catalog = Catalog.Open(path);
        Volume volume = catalog.Add("v", temp.Path);

        using var connection = new SqliteConnection($"Data Source={path}");
        connection.Open();
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = "UPDATE volumes SET root = 'elsewhere' WHERE id = $id";
        command.Parameters.AddWithValue("$id", volume.Id);

        var error = Assert.Throws<SqliteException>(() => command.ExecuteNonQuery());
        Assert.Contains("immutable", error.Message);
    }

    [Fact]
    public void A_shard_loads_back_the_index_it_saved()
    {
        using var temp = new TempFolder();
        VolumeIndex index = SearchTests.Index(@"C:\v", ("", ["a.txt", "b.txt"]), ("empty", []), (@"sub\deep", ["c.pdf"]));
        Guid uid = Guid.NewGuid();
        string path = Shard.PathFor(temp.Path, uid);

        Shard.Write(path, uid, index);
        VolumeIndex loaded = Shard.Load(path, uid, @"C:\v")!;

        Assert.Equal(3, loaded.FileCount);
        Assert.Equal(["", "empty", @"sub\deep"], Enumerable.Range(0, loaded.Folders.Count).Select(i => loaded.Folders[i]));
        Assert.Equal(@"C:\v\sub\deep", loaded.FolderPath(loaded.FolderOf(2)));
        Assert.Null(Shard.Load(path, Guid.NewGuid(), @"C:\v"));
    }

    private static long Scalar(SqliteConnection connection, string sql)
    {
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = sql;
        return (long)command.ExecuteScalar()!;
    }
}
