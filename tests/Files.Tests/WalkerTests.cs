using System.Diagnostics;
using Files.Core;

namespace Files.Tests;

public sealed class WalkerTests
{
    [Fact]
    public void Every_file_under_the_root_is_found()
    {
        using var temp = new TempFolder();
        temp.File("top.txt");
        temp.File(@"a\b\deep.pdf");
        temp.File(@"a\middle.dwg");

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None);

        Assert.Equal(3, walk.Index.FileCount);
        Assert.Equal(3, walk.Folders);
        Assert.Equal(new WalkSkips(0, 0, 0), walk.Skipped);
        Hit hit = Assert.Single(walk.Index.Search("v", Query.Parse("deep"), new HashSet<string>(), CancellationToken.None).Hits);
        Assert.Equal(Path.Combine(temp.Path, @"a\b\deep.pdf"), hit.Path);
    }

    [Fact]
    public void Hidden_files_are_skipped_only_when_asked()
    {
        using var temp = new TempFolder();
        File.SetAttributes(temp.File("secret.txt"), FileAttributes.Hidden);
        temp.File("plain.txt");

        Assert.Equal(1, Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None).Index.FileCount);
        Assert.Equal(2, Walker.Walk(temp.Path, skipHidden: false, CancellationToken.None).Index.FileCount);
    }

    [Fact]
    public void A_missing_root_throws_so_the_old_index_is_kept()
    {
        Assert.Throws<DirectoryNotFoundException>(() =>
            Walker.Walk(@"C:\no-such-folder-for-files-tests", skipHidden: true, CancellationToken.None));
    }

    [Fact]
    public void A_walk_without_a_limit_hint_is_not_truncated()
    {
        using var temp = new TempFolder();
        for (int i = 0; i < 50; i++)
        {
            temp.File($@"f{i % 5}\file{i}.txt");
        }

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None);

        Assert.False(walk.Truncated);
        Assert.Equal(50, walk.Index.FileCount);
    }

    [Fact]
    public void A_walk_stops_when_the_names_no_longer_fit_and_keeps_whole_folders()
    {
        using var temp = new TempFolder();
        temp.File("root.txt");
        for (int folder = 0; folder < 10; folder++)
        {
            for (int i = 0; i < 4; i++)
            {
                temp.File($@"d{folder}\n{i}.txt");
            }
        }

        // "root.txt" plus room for two folders of four 6-char names.
        long limit = "root.txt".Length + 1 + 2 * 4 * ("n0.txt".Length + 1);
        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, progress: null, maxChars: limit);

        Assert.True(walk.Truncated);
        Assert.Equal(1 + 2 * 4, walk.Index.FileCount);
        Assert.Equal(3, walk.Folders);
    }

    [Fact]
    public void A_cancelled_walk_stops()
    {
        using var temp = new TempFolder();
        temp.File(@"a\x.txt");
        using var cancel = new CancellationTokenSource();
        cancel.Cancel();
        Assert.ThrowsAny<OperationCanceledException>(() => Walker.Walk(temp.Path, skipHidden: true, cancel.Token));
    }

    [Fact]
    public void A_link_on_a_local_volume_is_not_followed_but_counted()
    {
        using var temp = new TempFolder();
        using var elsewhere = new TempFolder();
        temp.File("own.txt");
        File.WriteAllText(Path.Combine(elsewhere.Path, "linked.txt"), "");
        Junction(Path.Combine(temp.Path, "link"), elsewhere.Path);

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None);

        Assert.Equal(1, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.Links);
    }

    [Fact]
    public void Followed_links_read_each_target_once()
    {
        using var temp = new TempFolder();
        using var elsewhere = new TempFolder();
        File.WriteAllText(Path.Combine(elsewhere.Path, "linked.txt"), "");
        Junction(Path.Combine(temp.Path, "first"), elsewhere.Path);
        Junction(Path.Combine(temp.Path, "second"), elsewhere.Path);

        WalkResult walk = Walk(temp.Path, followLinks: true);

        Assert.Equal(1, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.Links);
    }

    [Fact]
    public void A_link_back_to_a_parent_folder_ends()
    {
        using var temp = new TempFolder();
        temp.File(@"a\x.txt");
        Junction(Path.Combine(temp.Path, @"a\loop"), temp.Path);

        WalkResult walk = Walk(temp.Path, followLinks: true);

        // The root is read once more through the link, and the link inside it is not followed again.
        Assert.Equal(2, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.Links);
    }

    [Fact]
    public void Folders_past_the_depth_limit_are_counted()
    {
        using var temp = new TempFolder();
        string deepest = string.Join('\\', Enumerable.Repeat("d", Walker.MaxDepth + 1));
        temp.File(Path.Combine(deepest, "too-deep.txt"));

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None);

        Assert.Equal(0, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.TooDeep);
    }

    [Fact]
    public void Only_network_errors_count_towards_losing_the_share()
    {
        const int BadNetPath = unchecked((int)0x80070035);
        Assert.True(Walker.LooksLikeLostContact(new IOException("gone", BadNetPath)));
        Assert.False(Walker.LooksLikeLostContact(new PathTooLongException()));
        Assert.False(Walker.LooksLikeLostContact(new IOException("name too long", unchecked((int)0x800700CE))));
    }

    [Fact]
    public void Skips_are_described_in_words()
    {
        Assert.Null(new WalkSkips(0, 0, 0).Describe());
        Assert.Equal("3 folders could not be read · 1 link not followed · 2 folders too deep",
            new WalkSkips(3, 1, 2).Describe());
    }

    [Fact]
    public void A_live_walk_counts_links_and_never_reads_them()
    {
        using var temp = new TempFolder();
        using var elsewhere = new TempFolder();
        temp.File("own.txt");
        File.WriteAllText(Path.Combine(elsewhere.Path, "linked.txt"), "");
        Junction(Path.Combine(temp.Path, "link"), elsewhere.Path);
        var progress = new WalkProgress();

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (_, _) => { },
            new LiveWalkOptions(Walker.Workers, [], progress));

        Assert.Equal(1, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.Links);
        Assert.Equal(1, progress.Skipped.Links);
    }

    [Fact]
    public void A_cancelled_walk_returns_without_waiting_for_a_folder_still_being_read()
    {
        using var temp = new TempFolder();
        temp.File(@"slow\x.txt");
        using var cancel = new CancellationTokenSource();
        using var reading = new ManualResetEventSlim();
        using var release = new ManualResetEventSlim();
        try
        {
            Task<WalkResult> walk = Task.Run(() => Walker.Walk(temp.Path, skipHidden: true, cancel.Token, (relative, files) =>
            {
                if (relative == "slow")
                {
                    // Stands in for a listing that waits on a server that does not answer.
                    reading.Set();
                    release.Wait();
                }
                return files;
            }, (_, _) => { }));
            Assert.True(reading.Wait(TimeSpan.FromSeconds(10)));

            var clock = Stopwatch.StartNew();
            cancel.Cancel();

            Assert.ThrowsAny<OperationCanceledException>(() => walk.GetAwaiter().GetResult());
            Assert.True(clock.Elapsed < TimeSpan.FromSeconds(5), $"took {clock.Elapsed}");
        }
        finally
        {
            release.Set();
        }
    }

    [Fact]
    public void Folders_to_read_first_are_read_before_the_rest_and_only_once()
    {
        using var temp = new TempFolder();
        temp.File(@"a\one.txt");
        temp.File(@"b\two.txt");
        temp.File(@"c\deep\three.txt");
        var order = new List<string>();

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (relative, files) =>
        {
            lock (order)
            {
                order.Add(relative);
            }
            return files;
        }, (_, _) => { }, new LiveWalkOptions(1, [@"c\deep", "gone", @"..\outside"], null));

        Assert.Equal(["", @"c\deep"], order.Take(2));
        Assert.Single(order, relative => relative == @"c\deep");
        Assert.Equal(3, walk.Index.FileCount);
        // A folder the saved list knew that is gone is passed over, not counted as unreadable.
        Assert.Equal(WalkSkips.None, walk.Skipped);
    }

    [Fact]
    public void Progress_counts_every_folder_and_names_the_one_being_read()
    {
        using var temp = new TempFolder();
        temp.File(@"a\b\x.txt");
        temp.File(@"c\y.txt");
        var progress = new WalkProgress();

        Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (_, _) => { },
            new LiveWalkOptions(Walker.Workers, [], progress));

        Assert.Equal(4, progress.Folders);
        Assert.NotNull(progress.Reading);
    }

    [Fact]
    public void Each_folder_listed_is_traced_with_its_names_and_none_stay_in_flight()
    {
        using var temp = new TempFolder();
        temp.File(@"a\b\x.txt");
        temp.File(@"a\y.txt");
        var listed = new System.Collections.Concurrent.ConcurrentBag<FolderListed>();
        var progress = new WalkProgress { Listed = listed.Add };

        Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (_, _) => { },
            new LiveWalkOptions(Walker.Workers, ["gone"], progress));

        Dictionary<string, int> entries = listed.ToDictionary(folder => folder.Relative, folder => folder.Entries);
        Assert.Equal(new Dictionary<string, int> { [""] = 1, ["a"] = 2, [@"a\b"] = 1, ["gone"] = 0 }, entries);
        Assert.Empty(progress.InFlight);
    }

    [Fact]
    public void A_root_slow_to_list_does_not_hold_up_the_folders_read_first()
    {
        using var temp = new TempFolder();
        temp.File(@"jobs\1234\plan.pdf");
        temp.File("other.txt");
        using var firstRead = new ManualResetEventSlim();
        var options = new LiveWalkOptions(Walker.Workers, [@"jobs\1234"], null)
        {
            // The root lists only once the folder read first has been handed over, as a share with a huge root would.
            BeforeList = relative =>
            {
                if (relative.Length == 0)
                {
                    Assert.True(firstRead.Wait(TimeSpan.FromSeconds(10)), "the folder read first waited on the root");
                }
            },
        };

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (relative, _) =>
        {
            if (relative == @"jobs\1234")
            {
                firstRead.Set();
            }
        }, options);

        Assert.Equal(2, walk.Index.FileCount);
    }

    [Fact]
    public void A_root_that_cannot_be_listed_still_gives_what_the_folders_read_first_hold()
    {
        using var temp = new TempFolder();
        temp.File(@"jobs\1234\plan.pdf");
        temp.File("other.txt");
        var options = new LiveWalkOptions(Walker.Workers, [@"jobs\1234"], null)
        {
            BeforeList = relative =>
            {
                if (relative.Length == 0)
                {
                    throw new IOException("The network path was not found.", unchecked((int)0x80070035));
                }
            },
        };

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (_, _) => { }, options);

        Assert.Equal(1, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.Unreadable);
        Assert.IsType<IOException>(walk.RootError);
    }

    [Fact]
    public void A_root_that_cannot_be_listed_with_nothing_read_below_it_fails_the_walk()
    {
        using var temp = new TempFolder();
        temp.File(@"jobs\plan.pdf");
        var options = new LiveWalkOptions(Walker.Workers, [], null)
        {
            BeforeList = relative =>
            {
                if (relative.Length == 0)
                {
                    throw new IOException("The network path was not found.", unchecked((int)0x80070035));
                }
            },
        };

        Assert.Throws<IOException>(() => Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (_, _) => { }, options));
    }

    [Fact]
    public void A_large_folder_is_walked_while_it_lists_and_indexed_once_with_all_its_names()
    {
        using var temp = new TempFolder();
        foreach (string name in new[] { "a.txt", "b.txt", "c.txt", "d.txt", "e.txt" })
        {
            temp.File(name);
        }
        temp.File(@"sub1\x.txt");
        temp.File(@"sub2\y.txt");
        int rootBatches = 0;
        var options = new LiveWalkOptions(Walker.Workers, [], null) { StreamEvery = 2 };

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (relative, _) =>
        {
            if (relative.Length == 0)
            {
                Interlocked.Increment(ref rootBatches);
            }
        }, options);

        Assert.True(rootBatches > 1, $"the root came in {rootBatches} batch(es)");
        Assert.Equal(7, walk.Index.FileCount);
        Assert.Equal(3, walk.Folders);
    }

    [Fact]
    public void Batches_of_one_folder_are_merged_sorted_and_left_as_they_were()
    {
        IReadOnlyList<string> second = ["b", "a"];

        List<(string Relative, List<string> Files)> merged = Walker.Merge([("x", ["c"]), ("", ["z"]), ("x", second)]);

        Assert.Equal(["", "x"], merged.Select(folder => folder.Relative));
        Assert.Equal(["a", "b", "c"], merged[1].Files);
        Assert.Equal(["b", "a"], second);
    }

    [Fact]
    public void A_folder_being_listed_is_in_flight_with_how_long_it_has_waited_longest_first()
    {
        var progress = new WalkProgress();
        progress.Started("old");
        Thread.Sleep(30);
        progress.Started("new");
        progress.Started("old");

        Assert.Equal(["old", "new"], progress.InFlight.Select(folder => folder.Relative));
        Assert.True(progress.InFlight[0].Waited >= TimeSpan.FromMilliseconds(25));

        progress.Finished("old", 3, null);
        Assert.Equal("new", Assert.Single(progress.InFlight).Relative);
    }

    [Fact]
    public void A_first_folder_behind_a_link_is_never_read()
    {
        using var temp = new TempFolder();
        using var elsewhere = new TempFolder();
        temp.File("own.txt");
        File.WriteAllText(Path.Combine(elsewhere.Path, "linked.txt"), "");
        Directory.CreateDirectory(Path.Combine(elsewhere.Path, "inside"));
        File.WriteAllText(Path.Combine(elsewhere.Path, @"inside\deeper.txt"), "");
        Junction(Path.Combine(temp.Path, "link"), elsewhere.Path);

        WalkResult walk = Walker.Walk(temp.Path, skipHidden: true, CancellationToken.None, (_, files) => files, (_, _) => { },
            new LiveWalkOptions(Walker.Workers, ["link", @"link\inside"], null));

        Assert.Equal(1, walk.Index.FileCount);
        Assert.Equal(1, walk.Skipped.Links);
    }

    [Fact]
    public void A_folder_still_being_read_when_the_walk_is_cancelled_reports_nothing_after()
    {
        using var temp = new TempFolder();
        temp.File(@"slow\x.txt");
        using var cancel = new CancellationTokenSource();
        using var reading = new ManualResetEventSlim();
        using var release = new ManualResetEventSlim();
        var progress = new WalkProgress();
        int reported = 0;
        Task<WalkResult> walk = Task.Run(() => Walker.Walk(temp.Path, skipHidden: true, cancel.Token, (relative, files) =>
        {
            if (relative == "slow")
            {
                reading.Set();
                release.Wait();
            }
            return files;
        }, (relative, _) =>
        {
            if (relative == "slow")
            {
                Interlocked.Increment(ref reported);
            }
        }, new LiveWalkOptions(Walker.Workers, [], progress)));
        Assert.True(reading.Wait(TimeSpan.FromSeconds(10)));
        cancel.Cancel();
        Assert.ThrowsAny<OperationCanceledException>(() => walk.GetAwaiter().GetResult());
        int foldersAtCancel = progress.Folders;

        release.Set();
        Thread.Sleep(200);

        Assert.Equal(0, Volatile.Read(ref reported));
        Assert.Equal(foldersAtCancel, progress.Folders);
    }

    private static WalkResult Walk(string root, bool followLinks) =>
        Walker.Walk(root, skipHidden: true, CancellationToken.None, progress: null, Walker.CharLimit(), followLinks: followLinks);

    /// <summary>A directory junction, which needs no admin rights (a symbolic link does).</summary>
    internal static void Junction(string link, string target)
    {
        using Process mklink = Process.Start(new ProcessStartInfo("cmd.exe", $"/c mklink /J \"{link}\" \"{target}\"")
        {
            CreateNoWindow = true,
            UseShellExecute = false,
            RedirectStandardOutput = true,
        })!;
        mklink.WaitForExit();
        Assert.Equal(0, mklink.ExitCode);
    }
}
