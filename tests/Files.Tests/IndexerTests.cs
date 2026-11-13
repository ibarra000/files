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
}
