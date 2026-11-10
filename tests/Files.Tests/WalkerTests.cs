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
    public void A_cancelled_walk_stops()
    {
        using var temp = new TempFolder();
        temp.File(@"a\x.txt");
        using var cancel = new CancellationTokenSource();
        cancel.Cancel();
        Assert.ThrowsAny<OperationCanceledException>(() => Walker.Walk(temp.Path, skipHidden: true, cancel.Token));
    }
}
