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
            ["Open", "Open with default app", "Open with sketch", "Show in folder", "Copy", "Cut", "Copy path", "Copy name",
             "Pin to favorites", "Pin its folder to favorites", "Save this search", "Rename…", "Delete"],
            Labels(actions));
        Assert.Equal("Ctrl D", actions.Single(a => a.Kind == ActionKind.Favorite).Gesture);
    }

    [Fact]
    public void The_menu_shows_the_keys_set_in_settings()
    {
        Settings rebound = WithProgram with { PanelKeys = new Dictionary<string, string> { ["Reveal"] = "Ctrl+E", ["Favorite"] = "F8" } };
        IReadOnlyList<RowAction> actions = RowActions.For(FileRow(), rebound, [], search: "");
        Assert.Equal("Ctrl E", actions.Single(a => a.Kind == ActionKind.Reveal).Gesture);
        Assert.Equal("F8", actions.Single(a => a.Kind == ActionKind.Favorite).Gesture);
        Assert.Equal("Ctrl C", actions.Single(a => a.Kind == ActionKind.Copy).Gesture);
    }

    [Fact]
    public void A_result_can_be_copied_cut_renamed_and_deleted_with_their_keys()
    {
        IReadOnlyList<RowAction> actions = RowActions.For(FileRow(), WithProgram, [], search: "");
        Assert.Equal("Ctrl X", actions.Single(a => a.Kind == ActionKind.Cut).Gesture);
        Assert.Equal("F2", actions.Single(a => a.Kind == ActionKind.Rename).Gesture);
        RowAction delete = actions.Single(a => a.Kind == ActionKind.Delete);
        Assert.Equal("Del", delete.Gesture);
        Assert.True(delete.Danger);
        Assert.DoesNotContain(actions, a => a.Kind is ActionKind.Paste or ActionKind.Undo);
    }

    [Fact]
    public void Files_on_the_clipboard_and_an_operation_to_take_back_add_paste_and_undo()
    {
        IReadOnlyList<RowAction> actions = RowActions.For(FileRow(), WithProgram, [], search: "", canPaste: true, undo: "Undo move");
        List<string> labels = Labels(actions);
        Assert.Equal(labels.IndexOf("Cut") + 1, labels.IndexOf("Paste here"));
        Assert.Equal("Ctrl V", actions.Single(a => a.Kind == ActionKind.Paste).Gesture);
        RowAction undo = actions.Last();
        Assert.Equal(("Undo move", "Ctrl Z", ActionKind.Undo), (undo.Label, undo.Gesture, undo.Kind));
    }

    [Fact]
    public void The_home_list_keeps_copying_the_path_and_offers_no_file_operations()
    {
        IReadOnlyList<RowAction> actions = RowActions.For(ResultRow.ForOpened(@"C:\v\plan.pdf"), WithProgram, [], search: "", canPaste: true, undo: "Undo copy");
        Assert.Equal("Ctrl C", actions.Single(a => a.Kind == ActionKind.CopyPath).Gesture);
        Assert.DoesNotContain(actions, a => a.Kind is ActionKind.Copy or ActionKind.Cut or ActionKind.Paste or ActionKind.Rename or ActionKind.Delete or ActionKind.Undo);
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
        Assert.Equal(["Copy", "Copy path", "Copy name"], Labels(filtered));
        Assert.False(filtered[0].SeparatorBefore);
    }
}
