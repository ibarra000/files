using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using Files.Core;

namespace Files.UI;

/// <summary>The last reads of the screen, to search for again or copy; from the tray menu or Settings.</summary>
public partial class ScreenTextWindow : Window
{
    private readonly App _app;

    public ScreenTextWindow(App app)
    {
        _app = app;
        InitializeComponent();
        Show(_app.ScreenTexts);
        _app.ScreenTextsChanged += OnHistoryChanged;
        Closed += (_, _) => _app.ScreenTextsChanged -= OnHistoryChanged;
    }

    /// <summary>One read as the list shows it.</summary>
    public sealed record Row(ScreenTextEntry Entry, string Heading, IReadOnlyList<LineRow> Lines);

    /// <summary>A line, marked when the engine was unsure of it.</summary>
    public sealed record LineRow(string Text, string Mark, string? Hint);

    private void OnHistoryChanged() => Dispatcher.Invoke(() => Show(_app.ScreenTexts));

    private void OnFilter(object sender, TextChangedEventArgs e) => Show(_app.ScreenTexts);

    private void Show(IReadOnlyList<ScreenTextEntry> history)
    {
        Entries.ItemsSource = ScreenTextHistory.Filter(history, Filter.Text).Select(ToRow).ToList();
        ClearButton.IsEnabled = history.Count > 0;
        Note.Text = history.Count == 0
            ? "Nothing read yet. Text read from the screen is kept here, the newest first."
            : _app.Settings.RememberSearches ? "" : "History is paused (Settings → General), so new reads are not kept.";
    }

    private static Row ToRow(ScreenTextEntry entry)
    {
        string when = entry.TimeUtc.ToLocalTime().ToString("g");
        string heading = entry.Source.Length > 0 ? $"{when} · {entry.Source}" : when;
        IReadOnlyList<LineRow> lines = entry.Lines
            .Select(line => line.IsUnsure
                ? new LineRow(line.Text, " ?", $"Text recognition was unsure of this line ({line.Confidence:P0}); check it before relying on it")
                : new LineRow(line.Text, "", null))
            .ToList();
        return new Row(entry, heading, lines);
    }

    private void OnCopy(object sender, RoutedEventArgs e)
    {
        if (RowOf(sender) is not Row row)
        {
            return;
        }
        try
        {
            Clipboard.SetText(row.Entry.Text);
            Note.Text = "Copied.";
        }
        catch (COMException)
        {
            Note.Text = "The clipboard is in use by another app; try again.";
        }
    }

    private void OnSearch(object sender, RoutedEventArgs e)
    {
        if (RowOf(sender) is Row row)
        {
            _app.SearchFor(row.Entry.Lines[0].Text);
        }
    }

    private void OnClear(object sender, RoutedEventArgs e) => _app.ClearScreenTexts();

    private static Row? RowOf(object sender) => (sender as FrameworkElement)?.DataContext as Row;
}
