using System.Windows;
using Forms = System.Windows.Forms;

namespace Files.UI;

/// <summary>The notification-area icon and its menu.</summary>
public sealed class Tray : IDisposable
{
    private readonly Forms.NotifyIcon _icon;
    private readonly Forms.ToolStripMenuItem _install;

    public Tray(App app)
    {
        var menu = new Forms.ContextMenuStrip();
        menu.Items.Add("Show files", null, (_, _) => app.ShowPanel());
        menu.Items.Add("Settings", null, (_, _) => app.ShowSettings());
        _install = new Forms.ToolStripMenuItem("Install update", null, async (_, _) => Notify(await app.InstallUpdateAsync()));
        menu.Items.Add(_install);
        menu.Items.Add(new Forms.ToolStripSeparator());
        menu.Items.Add("Quit files", null, (_, _) => app.Quit());

        using Stream iconStream = Application.GetResourceStream(new Uri("pack://application:,,,/files.ico")).Stream;
        _icon = new Forms.NotifyIcon
        {
            Icon = new System.Drawing.Icon(iconStream),
            Text = "files",
            ContextMenuStrip = menu,
            Visible = true,
        };
        _icon.MouseClick += (_, e) =>
        {
            if (e.Button == Forms.MouseButtons.Left)
            {
                app.TogglePanel();
            }
        };
        menu.Opening += (_, _) =>
        {
            _install.Visible = app.UpdateOffer is not null;
            _install.Text = $"Install version {app.UpdateOffer?.Version} and restart";
            Paint(menu, app.IsDark);
        };
    }

    /// <summary>The menu in the panel's colors: WinForms draws it light, whatever the theme, unless told otherwise.</summary>
    private static void Paint(Forms.ContextMenuStrip menu, bool dark)
    {
        var colors = new MenuColors(dark);
        menu.Renderer = new Forms.ToolStripProfessionalRenderer(colors) { RoundedEdges = false };
        menu.BackColor = colors.ToolStripDropDownBackground;
        foreach (Forms.ToolStripItem item in menu.Items)
        {
            item.ForeColor = colors.Text;
        }
    }

    /// <summary>The panel's light and dark colors (UI/PanelTheme.*.xaml) for a WinForms menu.</summary>
    private sealed class MenuColors(bool dark) : Forms.ProfessionalColorTable
    {
        private readonly System.Drawing.Color _fill = dark ? Rgb(0x2A2A2E) : Rgb(0xFFFFFF);
        private readonly System.Drawing.Color _stroke = dark ? Rgb(0x45454C) : Rgb(0xCBC8C0);
        private readonly System.Drawing.Color _hover = dark ? Rgb(0x38383E) : Rgb(0xEDECE8);

        public System.Drawing.Color Text { get; } = dark ? Rgb(0xECECEE) : Rgb(0x1F1E1D);

        public override System.Drawing.Color ToolStripDropDownBackground => _fill;
        public override System.Drawing.Color ImageMarginGradientBegin => _fill;
        public override System.Drawing.Color ImageMarginGradientMiddle => _fill;
        public override System.Drawing.Color ImageMarginGradientEnd => _fill;
        public override System.Drawing.Color MenuBorder => _stroke;
        public override System.Drawing.Color MenuItemBorder => _hover;
        public override System.Drawing.Color MenuItemSelected => _hover;
        public override System.Drawing.Color MenuItemSelectedGradientBegin => _hover;
        public override System.Drawing.Color MenuItemSelectedGradientEnd => _hover;
        public override System.Drawing.Color SeparatorDark => _stroke;
        public override System.Drawing.Color SeparatorLight => _fill;

        private static System.Drawing.Color Rgb(int rgb) => System.Drawing.Color.FromArgb(255, System.Drawing.Color.FromArgb(rgb));
    }

    public void Notify(string? message)
    {
        if (!string.IsNullOrEmpty(message))
        {
            _icon.ShowBalloonTip(5000, "files", message, Forms.ToolTipIcon.None);
        }
    }

    public void Dispose()
    {
        _icon.Visible = false;
        _icon.Dispose();
    }
}
