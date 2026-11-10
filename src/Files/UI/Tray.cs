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
        };
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
