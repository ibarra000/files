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

    private static WalkResult Walk(string root, bool followLinks) =>
        Walker.Walk(root, skipHidden: true, CancellationToken.None, progress: null, Walker.CharLimit(), followLinks: followLinks);

    /// <summary>A directory junction, which needs no admin rights (a symbolic link does).</summary>
    private static void Junction(string link, string target)
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
