using Files.Core;
using Files.Services;
using Files.UI;

namespace Files.Tests;

public sealed class RowActionsTests
{
    private static readonly VolumeIndex Index = new VolumeIndex.Builder(@"C:\v").Build();
    private static readonly Settings WithProgram = new() { Viewers = [new Viewer("sketch", "sketch.exe")] };

    private static ResultRow FileRow() => ResultRow.ForHit(new Hit("v", Index, 0, "plan.pdf", @"C:\v\2024", 0, 4, 0), showVolume: false);

    private static List<string> Labels(IEnumerable<RowAction> actions) => actions.Select(action => action.Label).ToList();

    [Fact]
    public void A_file_offers_opening_showing_copying_and_pinning()
    {
        IReadOnlyList<RowAction> actions = RowActions.For(FileRow(), WithProgram, [], search: "plan");
        Assert.Equal(
            ["Open", "Open with default app", "Open with sketch", "Show in folder", "Copy path", "Copy name",
             "Pin to favorites", "Pin its folder to favorites", "Save this search"],
            Labels(actions));
        Assert.Equal("Ctrl D", actions.Single(a => a.Kind == ActionKind.Favorite).Gesture);
    }

    [Fact]
    public void A_pinned_file_offers_to_unpin_it()
    {
        IReadOnlyList<RowAction> actions = RowActions.For(FileRow(), WithProgram, [new Favorite(FavoriteKind.File, @"C:\v\2024\plan.pdf")], search: "");
        Assert.Contains("Remove from favorites", Labels(actions));
        Assert.DoesNotContain("Save this search", Labels(actions));
    }

    [Fact]
    public void A_folder_is_not_opened_with_a_program()
    {
        var hit = new Hit("v", Index, 1, "2024", @"C:\v", 0, 4, 0) { IsFolder = true };
        IReadOnlyList<RowAction> actions = RowActions.For(ResultRow.ForHit(hit, false), WithProgram, [], search: "");
        Assert.DoesNotContain("Open with sketch", Labels(actions));
        Assert.Equal(new Favorite(FavoriteKind.Folder, @"C:\v\2024"), RowActions.FavoriteOf(ResultRow.ForHit(hit, false)));
    }

    [Fact]
    public void A_recent_file_can_be_removed_from_recent()
    {
        RowAction last = RowActions.For(ResultRow.ForOpened(@"C:\v\plan.pdf"), WithProgram, [], search: "").Last();
        Assert.Equal(ActionKind.RemoveRecent, last.Kind);
        Assert.True(last.Danger);
        Assert.Equal("Del", last.Gesture);
    }

    [Fact]
    public void A_saved_search_runs_or_is_removed()
    {
        var saved = new Favorite(FavoriteKind.Search, "ext:pdf plan");
        IReadOnlyList<RowAction> actions = RowActions.For(ResultRow.ForFavorite(saved, missing: false), WithProgram, [saved], search: "");
        Assert.Equal(["Run this search", "Remove from favorites"], Labels(actions));
    }

    [Fact]
    public void Section_labels_have_no_actions()
    {
        Assert.Empty(RowActions.For(ResultRow.ForHeader("Files"), WithProgram, [], search: ""));
    }

    [Fact]
    public void Typing_filters_actions_by_label_and_drops_a_leading_separator()
    {
        IReadOnlyList<RowAction> filtered = RowActions.Filter(RowActions.For(FileRow(), WithProgram, [], search: ""), "copy");
        Assert.Equal(["Copy path", "Copy name"], Labels(filtered));
        Assert.False(filtered[0].SeparatorBefore);
    }
}
