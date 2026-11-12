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
        Assert.Equal(0, walk.Unreadable);
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
}
