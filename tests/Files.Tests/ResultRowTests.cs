using Files.Core;
using Files.UI;

namespace Files.Tests;

public sealed class ResultRowTests
{
    private const string Root = @"\\server\share";

    private static Hit HitIn(string folder, string name)
    {
        VolumeIndex index = new VolumeIndex.Builder(Root).Build();
        return new Hit("drawings", index, 0, name, folder, 0, 2, 0);
    }

    [Fact]
    public void A_folder_is_shown_within_its_volume_as_a_breadcrumb()
    {
        Assert.Equal("2024 › site A", ResultRow.FolderText(@"\\server\share\2024\site A", Root));
    }

    [Fact]
    public void The_volume_root_is_compared_ignoring_case()
    {
        Assert.Equal("2024", ResultRow.FolderText(@"\\SERVER\Share\2024", Root));
    }

    [Fact]
    public void A_file_at_the_volume_root_shows_the_root()
    {
        Assert.Equal(Root, ResultRow.FolderText(Root, Root));
        Assert.Equal(@"C:\", ResultRow.FolderText(@"C:\", @"C:\"));
    }

    [Fact]
    public void A_folder_outside_the_root_is_shown_whole()
    {
        Assert.Equal(@"D:\other", ResultRow.FolderText(@"D:\other", Root));
        // A longer name that only starts like the root is another share.
        Assert.Equal(@"\\server\shared\2024", ResultRow.FolderText(@"\\server\shared\2024", Root));
    }

    [Fact]
    public void Roots_ending_in_a_backslash_work_the_same()
    {
        Assert.Equal("2024", ResultRow.FolderText(@"\\server\share\2024", Root + @"\"));
        Assert.Equal("drawings › 2024", ResultRow.FolderText(@"C:\drawings\2024", @"C:\"));
    }

    [Fact]
    public void The_volume_is_named_only_when_there_are_several()
    {
        Hit hit = HitIn(@"\\server\share\2024", "AB-C-0704.pdf");
        Assert.Equal("", ResultRow.ForHit(hit, showVolume: false).Badge);
        Assert.Equal("drawings", ResultRow.ForHit(hit, showVolume: true).Badge);
    }

    [Fact]
    public void A_result_shows_its_type_and_its_full_path_on_hover()
    {
        ResultRow row = ResultRow.ForHit(HitIn(@"\\server\share\2024", "AB-C-0704.pdf"), showVolume: false);
        Assert.Equal(FileKinds.Glyph("AB-C-0704.pdf"), row.Icon);
        Assert.Equal("2024", row.Detail);
        Assert.Equal(@"\\server\share\2024\AB-C-0704.pdf", row.ToolTipText);
    }
}
