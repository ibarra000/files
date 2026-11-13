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
    public void The_volume_is_named_only_when_there_are_several()
    {
        Hit hit = HitIn(@"\\server\share\2024", "AB-C-0704.pdf");
        Assert.Equal("", ResultRow.ForHit(hit, showVolume: false).Badge);
        Assert.Equal("drawings", ResultRow.ForHit(hit, showVolume: true).Badge);
    }

    [Fact]
    public void A_result_shows_its_type_its_folder_and_its_full_path_on_hover()
    {
        ResultRow row = ResultRow.ForHit(HitIn(@"\\server\share\2024", "AB-C-0704.pdf"), showVolume: false);
        Assert.Equal(FileKinds.Glyph("AB-C-0704.pdf"), row.Icon);
        Assert.Equal(@"\\server\share\2024", row.Detail);
        Assert.Equal(@"\\server\share\2024\AB-C-0704.pdf", row.ToolTipText);
    }
}
