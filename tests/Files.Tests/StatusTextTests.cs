using Files.Core;
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
    public void A_live_only_volume_is_never_reported_as_not_read_yet()
    {
        VolumeState state = State("archive", null);
        VolumeState liveOnly = state with { Volume = state.Volume with { LiveOnly = true } };
        Assert.Equal("", StatusText.For([liveOnly], queryProblem: null));
    }

    [Fact]
    public void The_footer_points_to_live_search_for_live_only_volumes()
    {
        Assert.Equal($"{10:N0} files indexed · 1 live-only volume: Shift+Enter searches it", StatusText.Indexed(10, null, liveOnly: 1));
        Assert.Equal("2 live-only volumes: Shift+Enter searches them", StatusText.Indexed(0, null, liveOnly: 2));
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
    [InlineData(true, true, "Stop")]
    [InlineData(false, true, "Open")]
    public void The_hints_offer_a_live_search_when_nothing_matched_and_never_open_with_nothing_to_open(bool noHits, bool liveRunning, string expected)
    {
        IReadOnlyList<KeyHint> hints = StatusText.Hints(noHits, liveRunning);
        // The most useful key comes first, since a narrow panel shows only the first.
        Assert.Equal(expected, hints[0].Label);
    }

    [Fact]
    public void The_hints_and_the_first_run_note_show_the_keys_set_in_settings()
    {
        PanelKeys keys = new Settings { PanelKeys = new Dictionary<string, string> { ["Preview"] = "F6", ["LiveSearch"] = "Ctrl+L" } }.Shortcuts;
        Assert.Contains(new KeyHint("F6", "Preview"), StatusText.Hints(noHits: false, liveRunning: false, keys: keys));
        Assert.Equal(new KeyHint("Ctrl L", "Search live"), StatusText.Hints(noHits: true, liveRunning: false, keys: keys)[0]);
        Assert.Equal("No volumes yet · press F9 to add a folder", StatusText.For([], null, settingsKeys: "F9"));
    }

    [Fact]
    public void The_header_counts_the_results_and_says_how_long_the_search_took()
    {
        Assert.Equal("1 result · 9 ms", StatusText.Count(1, TimeSpan.FromMilliseconds(9.4)));
        Assert.Equal($"{14_200:N0} results · 0 ms", StatusText.Count(14_200, TimeSpan.FromMilliseconds(0.2)));
        Assert.Equal("No results", StatusText.Count(0, TimeSpan.FromMilliseconds(3)));
        // A live search reports its own time in the footer.
        Assert.Equal("3 results", StatusText.Count(3, took: null));
    }

    [Fact]
    public void An_invalid_search_or_a_live_search_that_found_nothing_offers_to_clear()
    {
        Assert.Equal(new KeyHint("Esc", "Clear"), Assert.Single(StatusText.Hints(noHits: true, liveRunning: false, invalid: true)));
        // Searching live again would read every volume again for the same answer.
        Assert.Equal(new KeyHint("Esc", "Clear"), Assert.Single(StatusText.Hints(noHits: true, liveRunning: false, liveFinished: true)));
    }

    [Fact]
    public void A_stopped_live_search_that_found_nothing_offers_to_search_on()
    {
        IReadOnlyList<KeyHint> hints = StatusText.Hints(noHits: true, liveRunning: false, liveFinished: true, liveStopped: true);

        Assert.Equal([new KeyHint("Shift ↵", "Search live"), new KeyHint("Esc", "Clear")], hints);
    }

    [Fact]
    public void The_volume_menu_key_is_offered_only_with_the_volume_menu()
    {
        Assert.Contains(new KeyHint("Alt ↓", "Volumes"), StatusText.Hints(noHits: false, liveRunning: false, volumeMenu: true));
        Assert.DoesNotContain(new KeyHint("Alt ↓", "Volumes"), StatusText.Hints(noHits: false, liveRunning: false));
    }

    [Fact]
    public void The_count_says_searching_until_a_running_live_search_finds_something()
    {
        Assert.Equal("Searching…", StatusText.Count(0, took: null, stillSearching: true));
        Assert.Equal("No results", StatusText.Count(0, took: null));
        Assert.Equal("2 results", StatusText.Count(2, took: null, stillSearching: true));
    }

    [Fact]
    public void Live_progress_counts_what_the_header_counts()
    {
        Assert.StartsWith($"Searching live · {1_204:N0} matches in", StatusText.ForLive(1_204, 30, null, null, running: true, stopped: false, capped: false, WalkSkips.None, []));
        Assert.StartsWith("Live search · 1 match in", StatusText.ForLive(1, 30, null, null, running: false, stopped: false, capped: false, WalkSkips.None, []));
    }

    [Fact]
    public void A_running_live_search_says_which_folder_it_is_reading()
    {
        string running = StatusText.ForLive(0, 3_410, @"Share\Projects\2024\Line 3", null, running: true, stopped: false, capped: false, WalkSkips.None, []);
        Assert.Equal($@"Searching live · 0 matches in {3_410:N0} folders · reading …\2024\Line 3 · Esc to stop", running);

        string done = StatusText.ForLive(0, 3_410, @"Share\Projects", null, running: false, stopped: false, capped: false, WalkSkips.None, []);
        Assert.DoesNotContain("reading", done);
    }

    [Fact]
    public void A_live_search_says_what_it_could_not_read_while_it_runs_and_after()
    {
        string done = StatusText.ForLive(3, 30, null, null, running: false, stopped: false, capped: false, new WalkSkips(12, 1, 0), []);
        Assert.EndsWith("· 12 folders could not be read · 1 link not followed", done);
        string running = StatusText.ForLive(3, 30, "Share", null, running: true, stopped: false, capped: false, new WalkSkips(12, 0, 0), []);
        Assert.EndsWith("· Esc to stop · 12 folders could not be read", running);
    }

    [Fact]
    public void A_live_search_names_the_folder_a_slow_server_has_kept_it_waiting_on()
    {
        string running = StatusText.ForLive(0, 5, @"Share\x", (@"Share\Projects\2024\Line 3", TimeSpan.FromSeconds(12.4)),
            running: true, stopped: false, capped: false, WalkSkips.None, []);

        Assert.Equal(@"Searching live · 0 matches in 5 folders · waiting on …\2024\Line 3 (12s) · Esc to stop", running);
    }

    [Fact]
    public void A_deep_folder_is_shortened_to_its_last_two_parts()
    {
        Assert.Equal("Share", StatusText.LastFolders("Share"));
        Assert.Equal(@"Share\a", StatusText.LastFolders(@"Share\a"));
        Assert.Equal(@"…\b\c", StatusText.LastFolders(@"Share\a\b\c"));
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

    private static VolumeState Read(VolumeStatus status, VolumeError? error = null, string? problem = null, DateTime? lastRead = null) =>
        new(new Volume(1, Guid.NewGuid(), @"\\server\share", "share", "drawings", status, error, lastRead ?? DateTime.UtcNow),
            SearchTests.Index(@"\\server\share"), null, problem);

    [Fact]
    public void Each_volume_state_has_one_wording_for_the_panel_and_settings()
    {
        VolumeState live = State("archive", null);
        Assert.Equal("Live search only", StatusText.Describe(live with { Volume = live.Volume with { LiveOnly = true } }));
        Assert.Equal($"Scanning… {1_200:N0} files", StatusText.Describe(State("drawings", 1_200)));
        Assert.Equal("Never read", StatusText.Describe(State("drawings", null)));
        Assert.Equal("Access denied", StatusText.Describe(Read(VolumeStatus.Offline, VolumeError.PermissionDenied, "denied")));
        Assert.Equal("Drive or server not found", StatusText.Describe(Read(VolumeStatus.Offline, VolumeError.DriveNotFound, "gone")));
        Assert.Equal("Folder not found", StatusText.Describe(Read(VolumeStatus.Offline, VolumeError.PathNotFound, "gone")));
        Assert.Equal("Offline · the share is busy", StatusText.Describe(Read(VolumeStatus.Offline, problem: "the share is busy")));
        Assert.Equal("Online", StatusText.Describe(Read(VolumeStatus.Online)));
    }

    [Fact]
    public void Results_from_an_unreachable_volume_say_when_their_list_was_read()
    {
        var listed = new DateTime(2026, 11, 21, 9, 0, 0, DateTimeKind.Utc);
        VolumeState offline = Read(VolumeStatus.Offline, VolumeError.DriveNotFound, "gone", listed);
        Assert.Equal($"offline · list from {listed.ToLocalTime():d MMM HH:mm}", StatusText.Stale(offline));
        Assert.Null(StatusText.Stale(Read(VolumeStatus.Online)));
    }

    [Fact]
    public void An_unreachable_volume_keeps_its_mark_while_it_is_tried_again()
    {
        VolumeState offline = Read(VolumeStatus.Offline, VolumeError.DriveNotFound, "gone", DateTime.UtcNow.AddHours(-3));
        string? mark = StatusText.Stale(offline);
        Assert.NotNull(mark);
        Assert.Equal(mark, StatusText.Stale(offline with { Queued = true }));
        Assert.Equal(mark, StatusText.Stale(offline with { ScannedSoFar = 40 }));
    }

    [Fact]
    public void A_read_cut_off_when_the_app_closed_is_not_marked_offline()
    {
        VolumeState interrupted = Read(VolumeStatus.Scanning);
        Assert.Null(StatusText.Stale(interrupted));
        Assert.Equal("Interrupted", StatusText.Describe(interrupted));
        Assert.Equal("Waiting to be read", StatusText.Describe(interrupted with { Queued = true }));
    }

    [Fact]
    public void The_volume_menu_says_when_a_volume_was_read_and_when_it_is_read_next()
    {
        VolumeState online = Read(VolumeStatus.Online, lastRead: DateTime.UtcNow.AddMinutes(-5).AddSeconds(-10));
        Assert.Equal("Online · read 5 min ago · next in 55 min", StatusText.MenuLine(online, DateTime.UtcNow.AddMinutes(55).AddSeconds(10)));
        Assert.Equal("Online · read 5 min ago · next on F5", StatusText.MenuLine(online, null));
        Assert.Equal("Online · read 5 min ago · next in 2 h", StatusText.MenuLine(online, DateTime.UtcNow.AddHours(2).AddMinutes(1)));
        Assert.Equal("Never read · next now", StatusText.MenuLine(State("drawings", null), DateTime.MinValue));
        Assert.Equal($"Scanning… {40:N0} files", StatusText.MenuLine(State("drawings", 40), DateTime.MinValue));
    }
}
