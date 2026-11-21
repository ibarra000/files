using Files.Data;
using Files.Services;

namespace Files.Tests;

public sealed class IndexerTests
{
    private static readonly TimeSpan Patience = TimeSpan.FromSeconds(20);

    private static async Task<VolumeState> WaitFor(Indexer indexer, Func<VolumeState, bool> condition)
    {
        DateTime giveUp = DateTime.UtcNow + Patience;
        while (DateTime.UtcNow < giveUp)
        {
            VolumeState? state = indexer.Volumes.FirstOrDefault(condition);
            if (state is not null && !indexer.IsScanning)
            {
                return state;
            }
            await Task.Delay(25);
        }
        throw new TimeoutException("the indexer did not get there");
    }

    [Fact]
    public async Task An_added_folder_is_read_saved_and_loaded_again_at_the_next_start()
    {
        using var temp = new TempFolder();
        string share = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;
        File.WriteAllText(Path.Combine(share, "report.pdf"), "");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        string shards = Path.Combine(temp.Path, "volumes");

        using (var indexer = new Indexer(catalog, shards, () => new Settings { RescanHours = 0 }))
        {
            indexer.Start();
            Assert.Null(indexer.Add("share", share));
            VolumeState read = await WaitFor(indexer, s => s.Index is not null);
            Assert.Equal(1, read.Index!.FileCount);
            Assert.Equal(VolumeStatus.Online, read.Volume.Status);
            Assert.NotNull(read.Volume.LastScannedAt);
            Assert.Equal("That folder is already a volume.", indexer.Add("again", share));
        }

        using (var restarted = new Indexer(catalog, shards, () => new Settings { RescanHours = 0 }))
        {
            restarted.Start();
            VolumeState loaded = await WaitFor(restarted, s => s.Index is not null);
            Assert.Equal(1, loaded.Index!.FileCount);

            restarted.Remove(loaded.Volume.Id);
            Assert.Empty(restarted.Volumes);
            Assert.Empty(catalog.All());
            Assert.False(File.Exists(Shard.PathFor(shards, loaded.Volume.Uid)));
        }
    }

    [Fact]
    public async Task What_the_panel_changed_shows_until_the_volume_is_read_again()
    {
        using var temp = new TempFolder();
        string share = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;
        string report = Path.Combine(share, "report.pdf");
        File.WriteAllText(report, "");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        using var indexer = new Indexer(catalog, Path.Combine(temp.Path, "volumes"), () => new Settings { RescanHours = 0 });
        indexer.Start();
        Assert.Null(indexer.Add("share", share));
        VolumeState read = await WaitFor(indexer, s => s.Index is not null);

        indexer.Note(removed: [report, Path.Combine(temp.Path, "elsewhere.pdf")], added: []);
        Assert.True(indexer.NoteFor(read.Volume.Id).Hides(report));

        File.Delete(report);
        indexer.Rescan(read.Volume.Id);
        await WaitFor(indexer, s => s.Index is { FileCount: 0 });
        Assert.True(indexer.NoteFor(read.Volume.Id).IsEmpty);
    }

    [Fact]
    public async Task A_removed_volume_drops_what_the_panel_noted_for_it()
    {
        using var temp = new TempFolder();
        string share = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;
        string report = Path.Combine(share, "report.pdf");
        File.WriteAllText(report, "");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        using var indexer = new Indexer(catalog, Path.Combine(temp.Path, "volumes"), () => new Settings { RescanHours = 0 });
        indexer.Start();
        Assert.Null(indexer.Add("share", share));
        VolumeState read = await WaitFor(indexer, s => s.Index is not null);
        indexer.Note(removed: [report], added: []);

        Assert.Null(indexer.Remove(read.Volume.Id));

        Assert.True(indexer.NoteFor(read.Volume.Id).IsEmpty);
    }

    [Fact]
    public async Task A_held_indexer_reads_nothing_until_the_hold_is_released()
    {
        using var temp = new TempFolder();
        string share = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;
        File.WriteAllText(Path.Combine(share, "report.pdf"), "");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        using var indexer = new Indexer(catalog, Path.Combine(temp.Path, "volumes"), () => new Settings { RescanHours = 0 });
        indexer.Start();
        IDisposable hold = indexer.HoldScans();

        Assert.Null(indexer.Add("share", share));
        await Task.Delay(500);
        Assert.Null(Assert.Single(indexer.Volumes).Index);

        // Released once, however often it is disposed.
        hold.Dispose();
        hold.Dispose();
        VolumeState read = await WaitFor(indexer, s => s.Index is not null);
        Assert.Equal(1, read.Index!.FileCount);
    }

    [Fact]
    public async Task A_volume_removed_while_it_is_read_leaves_no_file_list_behind()
    {
        using var temp = new TempFolder();
        string share = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;
        for (int folder = 0; folder < 40; folder++)
        {
            string sub = Directory.CreateDirectory(Path.Combine(share, $"f{folder}")).FullName;
            for (int file = 0; file < 50; file++)
            {
                File.WriteAllText(Path.Combine(sub, $"report-{file}.pdf"), "");
            }
        }
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        string shards = Path.Combine(temp.Path, "volumes");
        using var indexer = new Indexer(catalog, shards, () => new Settings { RescanHours = 0 });
        indexer.Start();

        Assert.Null(indexer.Add("share", share));
        Volume added = Assert.Single(catalog.All());
        Assert.Null(indexer.Remove(added.Id));

        DateTime giveUp = DateTime.UtcNow + Patience;
        while (indexer.IsScanning && DateTime.UtcNow < giveUp)
        {
            await Task.Delay(25);
        }
        Assert.False(indexer.IsScanning);
        Assert.Empty(indexer.Volumes);
        Assert.False(File.Exists(Shard.PathFor(shards, added.Uid)));
    }

    [Fact]
    public async Task An_unreachable_folder_is_marked_offline_with_the_reason()
    {
        using var temp = new TempFolder();
        string missing = Path.Combine(temp.Path, "gone");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        using var indexer = new Indexer(catalog, Path.Combine(temp.Path, "volumes"), () => new Settings());

        // Added before start-up loading runs: loading must not overwrite the scan's result.
        indexer.Add("gone", missing);
        indexer.Start();
        VolumeState state = await WaitFor(indexer, s => s.Problem is not null);

        Assert.Equal(VolumeStatus.Offline, state.Volume.Status);
        Assert.Equal(VolumeError.PathNotFound, state.Volume.LastError);
        Assert.Null(state.Index);
    }

    [Fact]
    public async Task A_live_only_volume_is_never_read_and_loses_its_saved_list()
    {
        using var temp = new TempFolder();
        string share = Directory.CreateDirectory(Path.Combine(temp.Path, "share")).FullName;
        File.WriteAllText(Path.Combine(share, "report.pdf"), "");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        string shards = Path.Combine(temp.Path, "volumes");
        using var indexer = new Indexer(catalog, shards, () => new Settings { RescanHours = 0 });
        indexer.Start();
        Assert.Null(indexer.Add("share", share));
        VolumeState read = await WaitFor(indexer, s => s.Index is not null);

        Assert.Null(indexer.Edit(read.Volume.Id, "share", share, liveOnly: true));
        indexer.RescanAll();

        VolumeState liveOnly = Assert.Single(indexer.Volumes);
        Assert.True(liveOnly.Volume.LiveOnly);
        Assert.Null(liveOnly.Index);
        Assert.False(indexer.IsScanning);
        Assert.False(File.Exists(Shard.PathFor(shards, read.Volume.Uid)));

        Assert.Null(indexer.Edit(read.Volume.Id, "share", share, liveOnly: false));
        VolumeState again = await WaitFor(indexer, s => s.Index is not null);
        Assert.Equal(1, again.Index!.FileCount);
    }

    [Fact]
    public void Editing_a_volume_removed_meanwhile_says_so()
    {
        using var temp = new TempFolder();
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        using var indexer = new Indexer(catalog, Path.Combine(temp.Path, "volumes"), () => new Settings { RescanHours = 0 });
        Assert.Null(indexer.Add("v", temp.Path, liveOnly: true));
        long id = Assert.Single(indexer.Volumes).Volume.Id;
        Assert.Null(indexer.Remove(id));

        Assert.Equal("That volume was removed meanwhile.", indexer.Edit(id, "v", temp.Path, liveOnly: false));
        Assert.Empty(catalog.All());
    }

    [Fact]
    public async Task Moving_a_volume_reads_the_new_folder_and_deletes_the_old_list()
    {
        using var temp = new TempFolder();
        string from = Directory.CreateDirectory(Path.Combine(temp.Path, "from")).FullName;
        string to = Directory.CreateDirectory(Path.Combine(temp.Path, "to")).FullName;
        File.WriteAllText(Path.Combine(from, "old.pdf"), "");
        File.WriteAllText(Path.Combine(to, "new.pdf"), "");
        File.WriteAllText(Path.Combine(to, "new.txt"), "");
        Catalog catalog = Catalog.Open(Path.Combine(temp.Path, "catalog.db"));
        string shards = Path.Combine(temp.Path, "volumes");
        using var indexer = new Indexer(catalog, shards, () => new Settings { RescanHours = 0 });
        indexer.Start();
        Assert.Null(indexer.Add("v", from));
        VolumeState first = await WaitFor(indexer, s => s.Index is not null);

        Assert.Null(indexer.Edit(first.Volume.Id, "renamed", to, liveOnly: false));

        VolumeState moved = await WaitFor(indexer, s => s.Index is { FileCount: 2 });
        Assert.Equal("renamed", moved.Volume.Name);
        Assert.Equal(to, moved.Volume.Root);
        Assert.Single(indexer.Volumes);
        Assert.False(File.Exists(Shard.PathFor(shards, first.Volume.Uid)));
        Assert.Null(indexer.Add("other", from));
        Assert.Equal("That folder is already a volume.", indexer.Edit(moved.Volume.Id, "renamed", from, liveOnly: false));
        Assert.Contains(indexer.Volumes, state => state.Volume.Id == moved.Volume.Id && state.Volume.Root == to);
    }
}
