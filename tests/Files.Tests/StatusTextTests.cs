using Files.Data;
using Files.Services;
using Files.UI;

namespace Files.Tests;

public sealed class StatusTextTests
{
    private static VolumeState State(string name, int? scannedSoFar)
    {
        var volume = new Volume(1, Guid.NewGuid(), @"\\server\share", "share", name, VolumeStatus.Online, null, null);
        return new VolumeState(volume, null, scannedSoFar, null);
    }

    [Fact]
    public void Volumes_being_read_show_one_scanning_message_without_their_names()
    {
        string text = StatusText.For([State("drawings", 120), State("reports", 40)], queryProblem: null);
        Assert.Equal("Scanning…", text);
    }

    [Fact]
    public void A_volume_that_needs_attention_is_still_named()
    {
        string text = StatusText.For([State("drawings", 120), State("reports", null)], queryProblem: null);
        Assert.Equal("Scanning… · reports not read yet", text);
    }

    [Theory]
    [InlineData(false, false, "Enter to open")]
    [InlineData(true, false, "Shift+Enter to search live")]
    [InlineData(true, true, "Enter to open")]
    public void The_hint_offers_a_live_search_only_when_nothing_matched(bool noHits, bool liveRunning, string expected)
    {
        Assert.Equal(expected, StatusText.Hint(noHits, liveRunning));
    }
}
