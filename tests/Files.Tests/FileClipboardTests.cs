using System.Windows;
using Files.UI;

namespace Files.Tests;

public sealed class FileClipboardTests
{
    [Fact]
    public void Copied_files_paste_as_files_in_explorer_and_as_the_path_in_a_box()
    {
        DataObject data = FileClipboard.Make([@"C:\v\report.pdf"], cut: false);

        Assert.Equal([@"C:\v\report.pdf"], data.GetFileDropList().Cast<string>());
        Assert.Equal(@"C:\v\report.pdf", data.GetData(DataFormats.UnicodeText));
        Assert.Equal([5, 0, 0, 0], ((MemoryStream)data.GetData(FileClipboard.DropEffectFormat)!).ToArray());
        Assert.False(FileClipboard.IsCut(data));
    }

    [Fact]
    public void Cut_files_ask_to_be_moved()
    {
        DataObject data = FileClipboard.Make([@"C:\v\report.pdf", @"C:\v\plan.pdf"], cut: true);

        Assert.Equal([2, 0, 0, 0], ((MemoryStream)data.GetData(FileClipboard.DropEffectFormat)!).ToArray());
        Assert.True(FileClipboard.IsCut(data));
        Assert.Equal([@"C:\v\report.pdf", @"C:\v\plan.pdf"], FileClipboard.Files(data));
    }

    [Fact]
    public void Files_from_elsewhere_without_a_drop_effect_are_copied()
    {
        var data = new DataObject(DataFormats.FileDrop, new[] { @"C:\v\report.pdf" });

        Assert.False(FileClipboard.IsCut(data));
        Assert.Equal([@"C:\v\report.pdf"], FileClipboard.Files(data));
    }
}
