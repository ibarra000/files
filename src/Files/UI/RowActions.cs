using Files.Core;
using Files.Services;

namespace Files.UI;

public enum ActionKind
{
    Open,
    OpenDefault,
    OpenWith,
    Reveal,
    CopyPath,
    CopyName,
    Favorite,
    FavoriteFolder,
    SaveSearch,
    RemoveRecent,
    RunSearch,
    Copy,
    Cut,
    Paste,
    Rename,
    Delete,
    Undo,
}

/// <summary>One line of the action menu (Tab or right-click): what it says, its shortcut, and what it does.</summary>
public sealed record RowAction(string Label, string Gesture, ActionKind Kind, bool SeparatorBefore = false, bool Danger = false, Viewer? Viewer = null);

/// <summary>The actions a row offers, and the menu's filter as the user types.</summary>
public static class RowActions
{
    // The file keys Settings cannot change (they edit the search too), as the menu writes them.
    public const string CutKeys = "Ctrl X";
    public const string PasteKeys = "Ctrl V";
    public const string UndoKeys = "Ctrl Z";
    public const string DeleteKeys = "Del";

    /// <param name="canPaste">Files are on the clipboard, so a result can take them.</param>
    /// <param name="undo">The label for taking back the last file operation, or null if there is none.</param>
    public static IReadOnlyList<RowAction> For(ResultRow row, Settings settings, IReadOnlyList<Favorite> favorites, string search,
        bool canPaste = false, string? undo = null)
    {
        var actions = new List<RowAction>();
        PanelKeys keys = settings.Shortcuts;
        if (row.Item is { Kind: FavoriteKind.Search })
        {
            actions.Add(new RowAction("Run this search", "↵", ActionKind.RunSearch));
            actions.Add(new RowAction("Remove from favorites", keys.Label(PanelCommand.Favorite), ActionKind.Favorite, SeparatorBefore: true));
            return actions;
        }
        if (row.Path is null)
        {
            return actions;
        }
        actions.Add(new RowAction("Open", "↵", ActionKind.Open));
        actions.Add(new RowAction("Open with default app", "", ActionKind.OpenDefault));
        bool isFolder = row.Hit?.IsFolder == true || row.Item?.Kind == FavoriteKind.Folder;
        if (!isFolder)
        {
            actions.AddRange(settings.Viewers.Select(viewer => new RowAction($"Open with {viewer.Name}", "", ActionKind.OpenWith, Viewer: viewer)));
        }
        actions.Add(new RowAction("Show in folder", keys.Label(PanelCommand.Reveal), ActionKind.Reveal, SeparatorBefore: true));
        // Files are copied, moved, renamed and deleted from search results only; the home list keeps Ctrl+C for the path.
        bool isResult = row.Hit is not null;
        if (isResult)
        {
            AddClipboard(actions, keys, canPaste);
        }
        actions.Add(new RowAction("Copy path", isResult ? "" : keys.Label(PanelCommand.CopyPath), ActionKind.CopyPath));
        actions.Add(new RowAction("Copy name", keys.Label(PanelCommand.CopyName), ActionKind.CopyName));
        AddFavorites(actions, row, favorites, search, keys);
        if (isResult)
        {
            actions.Add(new RowAction("Rename…", keys.Label(PanelCommand.Rename), ActionKind.Rename, SeparatorBefore: true));
            actions.Add(new RowAction("Delete", DeleteKeys, ActionKind.Delete, Danger: true));
            if (undo is not null)
            {
                actions.Add(new RowAction(undo, UndoKeys, ActionKind.Undo, SeparatorBefore: true));
            }
        }
        if (row.Kind == RowKind.Opened)
        {
            actions.Add(new RowAction("Remove from recent", DeleteKeys, ActionKind.RemoveRecent, SeparatorBefore: true, Danger: true));
        }
        return actions;
    }

    /// <summary>Copy and Cut put the file itself on the clipboard, with its path for a text box; Paste here takes files from it.</summary>
    private static void AddClipboard(List<RowAction> actions, PanelKeys keys, bool canPaste)
    {
        actions.Add(new RowAction("Copy", keys.Label(PanelCommand.CopyPath), ActionKind.Copy));
        actions.Add(new RowAction("Cut", CutKeys, ActionKind.Cut));
        if (canPaste)
        {
            actions.Add(new RowAction("Paste here", PasteKeys, ActionKind.Paste));
        }
    }

    /// <summary>What Ctrl+D pins or unpins for a row: the file or folder of a result, or the favorite or recent item itself.</summary>
    public static Favorite? FavoriteOf(ResultRow row)
    {
        if (row.Hit is Hit hit)
        {
            return new Favorite(hit.IsFolder ? FavoriteKind.Folder : FavoriteKind.File, hit.Path);
        }
        return row.Item;
    }

    /// <summary>The actions whose label holds <paramref name="text"/>, ignoring case; a separator never comes first.</summary>
    public static IReadOnlyList<RowAction> Filter(IReadOnlyList<RowAction> actions, string text)
    {
        List<RowAction> kept = actions
            .Where(action => action.Label.Contains(text.Trim(), StringComparison.OrdinalIgnoreCase))
            .ToList();
        if (kept.Count > 0 && kept[0].SeparatorBefore)
        {
            kept[0] = kept[0] with { SeparatorBefore = false };
        }
        return kept;
    }

    private static void AddFavorites(List<RowAction> actions, ResultRow row, IReadOnlyList<Favorite> favorites, string search, PanelKeys keys)
    {
        if (FavoriteOf(row) is Favorite favorite)
        {
            string label = Favorites.Contains(favorites, favorite) ? "Remove from favorites" : "Pin to favorites";
            actions.Add(new RowAction(label, keys.Label(PanelCommand.Favorite), ActionKind.Favorite, SeparatorBefore: true));
        }
        if (row.Hit is not { IsFolder: false } hit)
        {
            return;
        }
        bool folderPinned = Favorites.Contains(favorites, new Favorite(FavoriteKind.Folder, hit.Folder));
        actions.Add(new RowAction(folderPinned ? "Remove its folder from favorites" : "Pin its folder to favorites", "", ActionKind.FavoriteFolder));
        string typed = search.Trim();
        if (typed.Length > 0)
        {
            bool saved = Favorites.Contains(favorites, new Favorite(FavoriteKind.Search, typed));
            actions.Add(new RowAction(saved ? "Remove this saved search" : "Save this search", "", ActionKind.SaveSearch));
        }
    }
}
