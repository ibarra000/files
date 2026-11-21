using System.Windows.Input;
using Files.Services;

namespace Files.Tests;

public sealed class PanelKeysTests
{
    private static PanelKeys Keys(params (string Command, string Keys)[] bindings)
    {
        (PanelKeys? keys, IReadOnlyList<string> problems) = PanelKeys.From(bindings.ToDictionary(b => b.Command, b => b.Keys), "Ctrl+Shift+Space", "Ctrl+Alt+Space");
        Assert.Empty(problems);
        return keys!;
    }

    private static string Refused(string command, string keys)
    {
        (PanelKeys? refused, IReadOnlyList<string> problems) = PanelKeys.From(new Dictionary<string, string> { [command] = keys }, "Ctrl+Shift+Space", "Ctrl+Alt+Space");
        Assert.Null(refused);
        return Assert.Single(problems);
    }

    [Fact]
    public void Every_command_has_a_default_and_finds_its_keys()
    {
        foreach (PanelCommand command in Enum.GetValues<PanelCommand>())
        {
            (ModifierKeys modifiers, Key key) = PanelKeys.Parse(PanelKeys.Defaults.Bindings[command])!.Value;
            Assert.Equal(command, PanelKeys.Defaults.Find(modifiers, key));
        }
    }

    [Fact]
    public void The_defaults_are_the_keys_the_panel_always_had()
    {
        PanelKeys keys = PanelKeys.Defaults;
        Assert.Equal(PanelCommand.Reveal, keys.Find(ModifierKeys.Control, Key.Enter));
        Assert.Equal(PanelCommand.LiveSearch, keys.Find(ModifierKeys.Shift, Key.Enter));
        Assert.Equal(PanelCommand.Preview, keys.Find(ModifierKeys.Control, Key.P));
        Assert.Equal(PanelCommand.Rescan, keys.Find(ModifierKeys.None, Key.F5));
        Assert.Equal(PanelCommand.Settings, keys.Find(ModifierKeys.Control, Key.OemComma));
        Assert.Equal(PanelCommand.Filter3, keys.Find(ModifierKeys.Control, Key.D3));
        Assert.Null(keys.Find(ModifierKeys.None, Key.P));
    }

    [Fact]
    public void Rename_is_on_F2_and_can_move_but_the_file_keys_that_edit_text_cannot_be_taken()
    {
        Assert.Equal(PanelCommand.Rename, PanelKeys.Defaults.Find(ModifierKeys.None, Key.F2));
        Assert.Equal(PanelCommand.Rename, Keys(("Rename", "Ctrl+R")).Find(ModifierKeys.Control, Key.R));
        Assert.Contains("needed", Refused("Rename", "Ctrl+X"));
        Assert.Contains("needed", Refused("Rename", "Ctrl+Z"));
        Assert.Contains("needed", Refused("Rename", "Shift+Delete"));
    }

    [Fact]
    public void Labels_are_written_as_the_menu_and_footer_write_them()
    {
        PanelKeys keys = PanelKeys.Defaults;
        Assert.Equal("Ctrl ↵", keys.Label(PanelCommand.Reveal));
        Assert.Equal("Shift ↵", keys.Label(PanelCommand.LiveSearch));
        Assert.Equal("Ctrl D", keys.Label(PanelCommand.Favorite));
        Assert.Equal("Ctrl C", keys.Label(PanelCommand.CopyPath));
        Assert.Equal("Ctrl Shift C", keys.Label(PanelCommand.CopyName));
        Assert.Equal("F5", keys.Label(PanelCommand.Rescan));
        Assert.Equal("Ctrl 1", keys.Label(PanelCommand.Filter1));
        Assert.Equal("Ctrl+,", keys.Label(PanelCommand.Settings, "+"));
    }

    [Fact]
    public void A_rebound_command_answers_to_its_new_keys_only()
    {
        PanelKeys keys = Keys(("Preview", "F6"));
        Assert.Equal(PanelCommand.Preview, keys.Find(ModifierKeys.None, Key.F6));
        Assert.Null(keys.Find(ModifierKeys.Control, Key.P));
        Assert.Equal("F6", keys.Label(PanelCommand.Preview));
        Assert.Equal(PanelCommand.Favorite, keys.Find(ModifierKeys.Control, Key.D));
    }

    [Fact]
    public void Missing_and_unknown_entries_keep_the_defaults()
    {
        PanelKeys keys = Keys(("NoSuchCommand", "Ctrl+K"));
        Assert.Equal(PanelKeys.Defaults.Bindings, keys.Bindings);
        (PanelKeys? fromNull, IReadOnlyList<string> problems) = PanelKeys.From(null, "Ctrl+Shift+Space", "Off");
        Assert.Empty(problems);
        Assert.Equal(PanelKeys.Defaults.Bindings, fromNull!.Bindings);
    }

    [Fact]
    public void Two_commands_on_the_same_keys_are_refused()
    {
        Assert.Contains("Preview", Refused("Preview", "Ctrl+D"));
    }

    [Fact]
    public void Keys_the_panel_needs_for_moving_and_typing_are_refused()
    {
        Assert.Contains("needed", Refused("Preview", "Down"));
        Assert.Contains("needed", Refused("Preview", "Enter"));
        Assert.Contains("needed", Refused("Preview", "Ctrl+V"));
        Assert.Contains("needed", Refused("Preview", "Ctrl+O"));
        Assert.Contains("needed", Refused("Preview", "Alt+Down"));
    }

    [Fact]
    public void A_letter_needs_a_modifier_but_a_function_key_stands_alone()
    {
        Assert.Contains("needs Ctrl or Alt", Refused("Preview", "P"));
        Assert.Equal(PanelCommand.Rescan, Keys(("Rescan", "F9")).Find(ModifierKeys.None, Key.F9));
    }

    [Fact]
    public void Keys_held_by_a_system_wide_shortcut_or_by_windows_are_refused()
    {
        Assert.Contains("Open search", Refused("Preview", "Ctrl+Shift+Space"));
        Assert.Contains("belongs to Windows", Refused("Preview", "Ctrl+Escape"));
        Assert.Contains("not a shortcut", Refused("Preview", "Ctrl+Nonsense"));
    }

    [Fact]
    public void Shift_with_a_typing_key_is_refused_but_shift_enter_is_free()
    {
        Assert.Contains("needs Ctrl or Alt", Refused("Preview", "Shift+A"));
        Assert.Contains("needs Ctrl or Alt", Refused("Preview", "Shift+Space"));
        Assert.Equal(PanelCommand.Preview, Keys(("Preview", "Shift+F6")).Find(ModifierKeys.Shift, Key.F6));
    }

    [Fact]
    public void Keys_the_panel_moves_with_are_refused_with_any_modifier_it_moves_with()
    {
        Assert.Contains("needed", Refused("Preview", "Ctrl+Down"));
        Assert.Contains("needed", Refused("Preview", "Ctrl+PageUp"));
        Assert.Contains("needed", Refused("Preview", "Shift+Escape"));
        Assert.Equal(PanelCommand.Preview, Keys(("Preview", "Alt+Up")).Find(ModifierKeys.Alt, Key.Up));
    }

    [Fact]
    public void A_key_written_as_a_number_is_not_a_key()
    {
        Assert.Null(PanelKeys.Parse("Ctrl+6"));
        Assert.Null(PanelKeys.Parse("6"));
        Assert.Contains("not a shortcut", Refused("Preview", "Ctrl+9999"));
    }

    [Fact]
    public void A_system_wide_shortcut_on_a_panel_key_does_not_drop_the_others()
    {
        var settings = new Settings { Hotkey = "Ctrl+P", PanelKeys = new Dictionary<string, string> { ["Rescan"] = "F9" } };
        Assert.Equal(PanelCommand.Rescan, settings.Shortcuts.Find(ModifierKeys.None, Key.F9));
    }

    [Fact]
    public void Settings_keep_their_panel_keys_and_a_bad_file_falls_back_to_the_defaults()
    {
        var settings = new Settings { PanelKeys = new Dictionary<string, string> { ["Preview"] = "F6" } };
        Assert.Equal(PanelCommand.Preview, settings.Shortcuts.Find(ModifierKeys.None, Key.F6));
        var clashing = new Settings { PanelKeys = new Dictionary<string, string> { ["Preview"] = "Ctrl+D" } };
        Assert.Equal(PanelKeys.Defaults.Bindings, clashing.Shortcuts.Bindings);
    }

    [Fact]
    public void Copy_is_saved_under_its_old_key_so_earlier_settings_still_load()
    {
        var settings = new Settings { PanelKeys = new Dictionary<string, string> { ["CopyPath"] = "Ctrl+K" } };
        Assert.Equal(PanelCommand.CopyPath, settings.Shortcuts.Find(ModifierKeys.Control, Key.K));
        Assert.Equal("Copy", PanelKeys.Name(PanelCommand.CopyPath));
    }
}
