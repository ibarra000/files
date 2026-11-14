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
        Catalog.Open(path); // a second open must not run the migrations again

        using var connection = new SqliteConnection($"Data Source={path}");
        connection.Open();
        Assert.Equal(2L, Scalar(connection, "PRAGMA user_version"));
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

    [Fact]
    public void A_catalog_from_before_live_only_volumes_keeps_its_volumes()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "catalog.db");
        using (var connection = new SqliteConnection($"Data Source={path}"))
        {
            connection.Open();
            using Stream script = typeof(Catalog).Assembly.GetManifestResourceStream("catalog-1.sql")!;
            Execute(connection, new StreamReader(script).ReadToEnd());
            Execute(connection, "PRAGMA user_version = 1");
            Execute(connection, $"INSERT INTO volumes (uid, root, vtype, name) VALUES (randomblob(16), '{temp.Path}', 'local', 'old')");
        }

        Volume volume = Assert.Single(Catalog.Open(path).All());

        Assert.Equal("old", volume.Name);
        Assert.False(volume.LiveOnly);
    }

    [Fact]
    public void A_rename_or_live_only_change_keeps_the_volume_and_its_uid()
    {
        using var temp = new TempFolder();
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        Volume volume = catalog.Add("v", temp.Path, liveOnly: true);
        Assert.True(volume.LiveOnly);

        catalog.Change(volume.Id, " renamed ", liveOnly: false);

        Volume stored = Assert.Single(catalog.All());
        Assert.Equal(volume with { Name = "renamed", LiveOnly = false }, stored);
    }

    [Fact]
    public void Moving_a_volume_makes_a_new_one_with_a_new_uid_and_id()
    {
        using var temp = new TempFolder();
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        string from = Directory.CreateDirectory(Path.Combine(temp.Path, "from")).FullName;
        string to = Directory.CreateDirectory(Path.Combine(temp.Path, "to")).FullName;
        string taken = Directory.CreateDirectory(Path.Combine(temp.Path, "taken")).FullName;
        Volume volume = catalog.Add("v", from, liveOnly: true);
        catalog.Add("other", taken);

        Volume moved = catalog.Move(volume.Id, to + @"\", " w ", liveOnly: true);

        Assert.Equal(to, moved.Root);
        Assert.Equal("w", moved.Name);
        Assert.True(moved.LiveOnly);
        Assert.NotEqual(volume.Uid, moved.Uid);
        // A running read of the old volume must never mistake the new one for it.
        Assert.NotEqual(volume.Id, moved.Id);
        Assert.DoesNotContain(catalog.All(), stored => stored.Id == volume.Id);
        // A folder that is already a volume is refused, and the volume stays where it was.
        Assert.Throws<SqliteException>(() => catalog.Move(moved.Id, taken, "w", liveOnly: true));
        Assert.Contains(catalog.All(), stored => stored.Id == moved.Id && stored.Root == to);
    }

    private static void Execute(SqliteConnection connection, string sql)
    {
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = sql;
        command.ExecuteNonQuery();
    }
}
