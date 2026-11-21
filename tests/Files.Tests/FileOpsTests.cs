using Files.Services;

namespace Files.Tests;

public sealed class FileOpsTests
{
    private static FileSnapshot Snap(string path) => FileSnapshot.Take(path)!;

    private static string Folder(TempFolder temp, string relative) => Directory.CreateDirectory(Path.Combine(temp.Path, relative)).FullName;

    [Fact]
    public void A_free_name_counts_up_past_the_names_taken()
    {
        using var temp = new TempFolder();
        temp.File("report.pdf");
        temp.File("report (1).pdf");
        Directory.CreateDirectory(Path.Combine(temp.Path, "2024"));

        Assert.Equal("report (2).pdf", FileOps.FreeName(temp.Path, "report.pdf"));
        Assert.Equal("2024 (1)", FileOps.FreeName(temp.Path, "2024"));
        Assert.Equal("plan.pdf", FileOps.FreeName(temp.Path, "plan.pdf"));
    }

    [Fact]
    public void A_copy_never_overwrites_a_file_of_the_same_name()
    {
        using var temp = new TempFolder();
        string source = temp.File(@"a\report.pdf", "new");
        string existing = temp.File(@"b\report.pdf", "old");

        FileBatch batch = FileOps.Copy([Snap(source)], Folder(temp, "b"));

        FileResult result = Assert.Single(batch.Results);
        Assert.Equal(FileOutcome.Done, result.Outcome);
        Assert.Equal(Path.Combine(temp.Path, "b", "report (1).pdf"), result.Target);
        Assert.Equal("old", File.ReadAllText(existing));
        Assert.Equal("new", File.ReadAllText(result.Target!));
        Assert.True(File.Exists(source));
    }

    [Fact]
    public void A_copy_into_its_own_folder_gets_a_numbered_name()
    {
        using var temp = new TempFolder();
        string source = temp.File("report.pdf", "x");

        FileBatch batch = FileOps.Copy([Snap(source)], temp.Path);

        Assert.Equal(Path.Combine(temp.Path, "report (1).pdf"), Assert.Single(batch.Results).Target);
    }

    [Fact]
    public void A_folder_is_copied_with_everything_in_it()
    {
        using var temp = new TempFolder();
        temp.File(@"a\2024\plan.pdf", "p");
        temp.File(@"a\2024\sub\notes.txt", "n");

        FileBatch batch = FileOps.Copy([Snap(Path.Combine(temp.Path, "a", "2024"))], Folder(temp, "b"));

        Assert.Equal(FileOutcome.Done, Assert.Single(batch.Results).Outcome);
        Assert.Equal("n", File.ReadAllText(Path.Combine(temp.Path, "b", "2024", "sub", "notes.txt")));
        Assert.True(File.Exists(Path.Combine(temp.Path, "a", "2024", "plan.pdf")));
    }

    [Fact]
    public void A_file_changed_since_it_was_copied_is_skipped()
    {
        using var temp = new TempFolder();
        string source = temp.File("report.pdf", "first");
        FileSnapshot taken = Snap(source);
        File.WriteAllText(source, "changed since");

        FileBatch batch = FileOps.Copy([taken], Folder(temp, "b"));

        Assert.Equal(FileOutcome.Changed, Assert.Single(batch.Results).Outcome);
        Assert.False(File.Exists(Path.Combine(temp.Path, "b", "report.pdf")));
    }

    [Fact]
    public void Nothing_is_pasted_into_a_folder_that_is_gone()
    {
        using var temp = new TempFolder();
        string source = temp.File("report.pdf", "x");

        FileBatch batch = FileOps.Copy([Snap(source)], Path.Combine(temp.Path, "gone"));

        Assert.Equal(FileOutcome.NotFound, Assert.Single(batch.Results).Outcome);
        Assert.False(Directory.Exists(Path.Combine(temp.Path, "gone")));
    }

    [Fact]
    public void A_missing_source_is_reported_not_found()
    {
        using var temp = new TempFolder();
        string source = temp.File("report.pdf", "x");
        FileSnapshot taken = Snap(source);
        File.Delete(source);

        FileBatch batch = FileOps.Move([taken], Folder(temp, "b"));

        Assert.Equal(FileOutcome.NotFound, Assert.Single(batch.Results).Outcome);
    }

    [Fact]
    public void A_locked_file_fails_while_the_rest_are_moved()
    {
        using var temp = new TempFolder();
        string locked = temp.File(@"a\locked.pdf", "l");
        string free = temp.File(@"a\free.pdf", "f");
        string target = Path.Combine(temp.Path, "b");
        Directory.CreateDirectory(target);
        FileBatch batch;
        using (new FileStream(locked, FileMode.Open, FileAccess.Read, FileShare.None))
        {
            batch = FileOps.Move([Snap(locked), Snap(free)], target);
        }

        Assert.Equal([FileOutcome.Locked, FileOutcome.Done], batch.Results.Select(r => r.Outcome));
        Assert.True(File.Exists(locked));
        Assert.True(File.Exists(Path.Combine(target, "free.pdf")));
        Assert.False(File.Exists(free));
    }

    [Fact]
    public void A_move_to_another_folder_leaves_nothing_behind()
    {
        using var temp = new TempFolder();
        string file = temp.File(@"a\report.pdf", "r");
        temp.File(@"a\2024\plan.pdf", "p");
        string target = Folder(temp, "b");

        FileBatch batch = FileOps.Move([Snap(file), Snap(Path.Combine(temp.Path, "a", "2024"))], target);

        Assert.All(batch.Results, result => Assert.Equal(FileOutcome.Done, result.Outcome));
        Assert.Equal("r", File.ReadAllText(Path.Combine(target, "report.pdf")));
        Assert.Equal("p", File.ReadAllText(Path.Combine(target, "2024", "plan.pdf")));
        Assert.Empty(Directory.EnumerateFileSystemEntries(Path.Combine(temp.Path, "a")));
    }

    [Fact]
    public void Moving_to_the_folder_it_is_in_changes_nothing()
    {
        using var temp = new TempFolder();
        string file = temp.File("report.pdf", "r");

        FileBatch batch = FileOps.Move([Snap(file)], temp.Path);

        Assert.Equal(FileOutcome.Exists, Assert.Single(batch.Results).Outcome);
        Assert.True(File.Exists(file));
        Assert.False(batch.CanUndo);
    }

    [Fact]
    public void A_folder_cannot_be_moved_into_itself()
    {
        using var temp = new TempFolder();
        temp.File(@"a\plan.pdf");
        string folder = Path.Combine(temp.Path, "a");

        FileBatch batch = FileOps.Move([Snap(folder)], Folder(temp, @"a\inside"));

        Assert.Equal(FileOutcome.Failed, Assert.Single(batch.Results).Outcome);
        Assert.True(File.Exists(Path.Combine(folder, "plan.pdf")));
    }

    [Fact]
    public void Progress_counts_each_item()
    {
        using var temp = new TempFolder();
        string a = temp.File("a.pdf");
        string b = temp.File("b.pdf");
        var seen = new List<FileProgress>();

        FileOps.Copy([Snap(a), Snap(b)], Folder(temp, "out"), new Collect(seen));

        Assert.Equal([new FileProgress(1, 2), new FileProgress(2, 2)], seen);
    }

    [Fact]
    public void A_rename_refuses_a_name_already_there()
    {
        using var temp = new TempFolder();
        string file = temp.File("report.pdf", "r");
        temp.File("plan.pdf", "p");

        FileBatch batch = FileOps.Rename(file, "plan.pdf");

        Assert.Equal(FileOutcome.Exists, Assert.Single(batch.Results).Outcome);
        Assert.Equal("p", File.ReadAllText(Path.Combine(temp.Path, "plan.pdf")));
        Assert.True(File.Exists(file));
    }

    [Fact]
    public void A_rename_may_change_only_the_case()
    {
        using var temp = new TempFolder();
        string file = temp.File("report.pdf", "r");

        FileBatch batch = FileOps.Rename(file, "Report.pdf");

        Assert.Equal(FileOutcome.Done, Assert.Single(batch.Results).Outcome);
        Assert.Equal("Report.pdf", Path.GetFileName(Directory.GetFiles(temp.Path).Single()));
    }

    [Theory]
    [InlineData("")]
    [InlineData("  ")]
    [InlineData("a:b.pdf")]
    [InlineData("plan.pdf.")]
    [InlineData("..")]
    public void A_name_windows_cannot_take_is_refused(string name)
    {
        Assert.NotNull(FileOps.NameProblem(name));
    }

    [Fact]
    public void Undoing_a_copy_removes_the_copy_only()
    {
        using var temp = new TempFolder();
        string source = temp.File(@"a\report.pdf", "r");
        FileBatch copy = FileOps.Copy([Snap(source)], Folder(temp, "b"));
        string copied = copy.Results[0].Target!;

        FileBatch undo = FileOps.Undo(copy, permanently: true);

        Assert.Equal(FileOutcome.Done, Assert.Single(undo.Results).Outcome);
        Assert.False(File.Exists(copied));
        Assert.True(File.Exists(source));
    }

    [Fact]
    public void A_copy_changed_since_is_not_removed_by_undo()
    {
        using var temp = new TempFolder();
        string source = temp.File(@"a\report.pdf", "r");
        FileBatch copy = FileOps.Copy([Snap(source)], Folder(temp, "b"));
        string copied = copy.Results[0].Target!;
        File.WriteAllText(copied, "edited after copying");

        FileBatch undo = FileOps.Undo(copy, permanently: true);

        Assert.Equal(FileOutcome.Changed, Assert.Single(undo.Results).Outcome);
        Assert.True(File.Exists(copied));
    }

    [Fact]
    public void Undoing_a_move_puts_it_back()
    {
        using var temp = new TempFolder();
        string source = temp.File(@"a\report.pdf", "r");
        FileBatch move = FileOps.Move([Snap(source)], Folder(temp, "b"));

        FileBatch undo = FileOps.Undo(move);

        Assert.Equal(FileOutcome.Done, Assert.Single(undo.Results).Outcome);
        Assert.Equal("r", File.ReadAllText(source));
        Assert.False(File.Exists(Path.Combine(temp.Path, "b", "report.pdf")));
    }

    [Fact]
    public void Undoing_a_move_never_overwrites_what_took_its_place()
    {
        using var temp = new TempFolder();
        string source = temp.File(@"a\report.pdf", "moved");
        FileBatch move = FileOps.Move([Snap(source)], Folder(temp, "b"));
        File.WriteAllText(source, "a new one");

        FileBatch undo = FileOps.Undo(move);

        Assert.Equal(FileOutcome.Exists, Assert.Single(undo.Results).Outcome);
        Assert.Equal("a new one", File.ReadAllText(source));
        Assert.True(File.Exists(Path.Combine(temp.Path, "b", "report.pdf")));
    }

    [Fact]
    public void Undoing_a_rename_gives_the_old_name_back()
    {
        using var temp = new TempFolder();
        string file = temp.File("report.pdf", "r");
        FileBatch rename = FileOps.Rename(file, "plan.pdf");

        FileOps.Undo(rename);

        Assert.True(File.Exists(file));
        Assert.False(File.Exists(Path.Combine(temp.Path, "plan.pdf")));
    }

    [Fact]
    public void A_permanent_delete_removes_files_and_folders_and_cannot_be_undone()
    {
        using var temp = new TempFolder();
        string file = temp.File("report.pdf");
        temp.File(@"2024\plan.pdf");
        string folder = Path.Combine(temp.Path, "2024");

        FileBatch batch = FileOps.Delete([file, folder], permanently: true);

        Assert.All(batch.Results, result => Assert.Equal(FileOutcome.Done, result.Outcome));
        Assert.False(File.Exists(file));
        Assert.False(Directory.Exists(folder));
        Assert.False(batch.CanUndo);
    }

    [Fact]
    public void Deleting_what_is_gone_reports_not_found()
    {
        using var temp = new TempFolder();

        FileBatch batch = FileOps.Delete([Path.Combine(temp.Path, "gone.pdf")], permanently: true);

        Assert.Equal(FileOutcome.NotFound, Assert.Single(batch.Results).Outcome);
    }

    [Fact]
    public void A_network_path_is_never_sent_to_a_recycle_bin_it_does_not_have()
    {
        FileBatch batch = FileOps.Delete([@"\\server\share\report.pdf"], permanently: false);

        FileResult result = Assert.Single(batch.Results);
        Assert.Equal(FileOutcome.Failed, result.Outcome);
    }

    [Fact]
    public void A_deleted_file_goes_to_the_recycle_bin_and_comes_back()
    {
        if (!RecycleBin.IsAvailable)
        {
            // No Windows shell here (a server core or a locked-down runner): nothing to test against.
            return;
        }
        using var temp = new TempFolder();
        string file = temp.File($"recycled-{Guid.NewGuid():N}.txt", "keep me");

        FileBatch delete = FileOps.Delete([file], permanently: false);
        Assert.Equal(FileOutcome.Done, Assert.Single(delete.Results).Outcome);
        Assert.False(File.Exists(file));
        Assert.True(delete.CanUndo);

        FileBatch undo = FileOps.Undo(delete);

        Assert.Equal(FileOutcome.Done, Assert.Single(undo.Results).Outcome);
        Assert.Equal("keep me", File.ReadAllText(file));
    }

    [Fact]
    public void Sources_on_another_drive_or_share_are_on_another_volume()
    {
        Assert.True(FileOps.SameVolume(@"C:\a\report.pdf", @"c:\b"));
        Assert.False(FileOps.SameVolume(@"C:\a\report.pdf", @"D:\b"));
        Assert.False(FileOps.SameVolume(@"\\server\share\a.pdf", @"\\server\other\b"));
        Assert.True(FileOps.SameVolume(@"\\server\share\a.pdf", @"\\server\share\b"));
    }

    /// <summary>Records progress at once, on the calling thread, unlike Progress&lt;T&gt;.</summary>
    private sealed class Collect(List<FileProgress> seen) : IProgress<FileProgress>
    {
        public void Report(FileProgress value) => seen.Add(value);
    }
}
