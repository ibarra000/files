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
    public void A_rescan_just_requested_shows_scanning_before_any_volume_reports()
    {
        string text = StatusText.For([State("drawings", null)], queryProblem: null, rescanRequested: true);
        Assert.Equal("Scanning… · drawings not read yet", text);
    }

    [Fact]
    public void A_volume_that_needs_attention_is_still_named()
    {
        string text = StatusText.For([State("drawings", 120), State("reports", null)], queryProblem: null);
        Assert.Equal("Scanning… · reports not read yet", text);
    }

    [Theory]
    [InlineData(false, false, "Open")]
    [InlineData(true, false, "Search live")]
    [InlineData(true, true, "Open")]
    public void The_hints_offer_a_live_search_only_when_nothing_matched(bool noHits, bool liveRunning, string expected)
    {
        IReadOnlyList<KeyHint> hints = StatusText.Hints(noHits, liveRunning);
        // The most useful key comes first, since a narrow panel shows only the first.
        Assert.Equal(expected, hints[0].Label);
    }

    [Fact]
    public void The_footer_says_how_many_files_are_indexed_and_when()
    {
        Assert.Equal($"{1_199_227:N0} files indexed · updated just now", StatusText.Indexed(1_199_227, DateTime.UtcNow));
        Assert.Equal("1 file indexed", StatusText.Indexed(1, null));
    }

    [Fact]
    public void The_placeholder_says_how_many_files_can_be_searched()
    {
        Assert.Equal("Type to search", StatusText.Placeholder(0));
        // Grouped the way this PC writes numbers.
        Assert.Equal($"Search {1_234_567:N0} files", StatusText.Placeholder(1_234_567));
    }
}
