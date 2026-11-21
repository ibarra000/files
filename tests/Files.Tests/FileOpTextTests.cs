using Files.Services;
using Files.UI;

namespace Files.Tests;

public sealed class FileOpTextTests
{
    private static FileResult Done(string source, string? target) => new(source, target, FileOutcome.Done);

    [Fact]
    public void A_finished_copy_names_the_copy_and_how_to_undo_it()
    {
        var batch = new FileBatch(FileOpKind.Copy, [Done(@"C:\v\report.pdf", @"C:\v\report (1).pdf")]);

        Assert.Equal("Copied report (1).pdf · Ctrl+Z to undo", FileOpText.Done(batch));
        Assert.Equal("Undo copy", FileOpText.UndoLabel(batch));
    }

    [Fact]
    public void Problems_are_counted_after_what_was_done()
    {
        var batch = new FileBatch(FileOpKind.Move,
        [
            Done(@"C:\v\a.pdf", @"C:\w\a.pdf"),
            Done(@"C:\v\b.pdf", @"C:\w\b.pdf"),
            new FileResult(@"C:\v\c.pdf", null, FileOutcome.Locked),
            new FileResult(@"C:\v\d.pdf", null, FileOutcome.Changed),
        ]);

        Assert.Equal("Moved 2 items · 1 open in another program · 1 changed since copied · Ctrl+Z to undo", FileOpText.Done(batch));
    }

    [Fact]
    public void One_file_that_failed_says_why_in_a_sentence()
    {
        var batch = new FileBatch(FileOpKind.Delete, [new FileResult(@"C:\v\report.pdf", null, FileOutcome.Locked)]);

        Assert.Equal("report.pdf is open in another program", FileOpText.Done(batch));
    }

    [Fact]
    public void A_delete_for_good_offers_no_undo()
    {
        var batch = new FileBatch(FileOpKind.Delete, [Done(@"\\server\share\report.pdf", null)], Permanent: true);

        Assert.Equal("Deleted report.pdf", FileOpText.Done(batch));
    }

    [Fact]
    public void Progress_counts_up()
    {
        Assert.Equal("Moving 3 of 12…", FileOpText.Progress("Moving", new FileProgress(3, 12)));
    }
}
