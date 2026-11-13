using System.Windows;
using System.Windows.Controls;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using Files.Services;

namespace Files.UI;

/// <summary>
/// The preview pane (Ctrl+P): the selected file's size, date and type, with its
/// first lines, image or entries. Read off the UI thread, only for the selection.
/// </summary>
public partial class SearchWindow
{
    /// <summary>The pane's width; the window widens by this much to show it.</summary>
    private const double PreviewWidth = 400;
    private const int PreviewImageWidth = 360;
    private static readonly TimeSpan PreviewDelay = TimeSpan.FromMilliseconds(120);

    private DispatcherTimer? _previewTimer;
    private CancellationTokenSource? _previewCancel;

    /// <summary>How much wider the window was made for the pane, taken off again when it hides or is saved.</summary>
    private double _previewAdded;

    private bool PreviewShown => PreviewPane.Visibility == Visibility.Visible;

    private void TogglePreview()
    {
        bool show = !PreviewShown;
        ApplyPreview(show);
        _app.ApplySettings(_app.Settings with { ShowPreview = show });
    }

    /// <summary>Shows or hides the pane, widening or narrowing the window by its width while it is a normal window.</summary>
    private void ApplyPreview(bool show)
    {
        if (show == PreviewShown)
        {
            return;
        }
        PreviewPane.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        if (WindowState == WindowState.Normal && IsVisible)
        {
            if (show)
            {
                WidenForPreview();
            }
            else
            {
                Width = Math.Max(MinWidth, Width - _previewAdded);
                _previewAdded = 0;
            }
        }
        if (show)
        {
            SchedulePreview();
        }
    }

    /// <summary>Adds the pane's width, as far as the screen allows, moving left if the window would run off it.</summary>
    private void WidenForPreview()
    {
        Rect area = SystemParameters.WorkArea;
        double wider = Math.Min(Width + PreviewWidth, area.Width);
        _previewAdded = wider - Width;
        Width = wider;
        if (Left + Width > area.Right)
        {
            Left = Math.Max(area.Left, area.Right - Width);
        }
    }

    private void OnResultSelectionChanged(object sender, SelectionChangedEventArgs e) => SchedulePreview();

    /// <summary>Reads the selection after a short pause, so moving quickly through results does not read each one.</summary>
    private void SchedulePreview()
    {
        if (!PreviewShown)
        {
            return;
        }
        if (_previewTimer is null)
        {
            _previewTimer = new DispatcherTimer { Interval = PreviewDelay };
            _previewTimer.Tick += async (_, _) =>
            {
                _previewTimer.Stop();
                await ShowPreviewAsync(SelectedRow);
            };
        }
        _previewTimer.Stop();
        _previewTimer.Start();
    }

    private async Task ShowPreviewAsync(ResultRow? row)
    {
        _previewCancel?.Cancel();
        if (row?.Path is not string path)
        {
            ShowPreviewHeader(row?.Icon ?? "", row?.Before ?? "", row?.Detail ?? "");
            ShowPreviewNote(row?.Item is null ? "Nothing selected" : "A saved search: Enter runs it");
            return;
        }
        var cancel = new CancellationTokenSource();
        _previewCancel = cancel;
        ShowPreviewHeader(row.Icon, row.Before + row.Match + row.After, row.Detail);
        ShowPreviewNote("Reading…");
        PreviewInfo info;
        BitmapImage? image = null;
        try
        {
            info = await Preview.LoadAsync(path, cancel.Token);
            if (info.IsImage)
            {
                image = await Task.Run(() => LoadImage(path), cancel.Token);
            }
        }
        catch (OperationCanceledException)
        {
            return;
        }
        if (cancel.IsCancellationRequested || SelectedRow != row)
        {
            return;
        }
        ShowPreviewInfo(info, image);
    }

    /// <summary>A small copy of the image, decoded off the UI thread; null when it cannot be read.</summary>
    private static BitmapImage? LoadImage(string path)
    {
        try
        {
            var image = new BitmapImage();
            image.BeginInit();
            image.CacheOption = BitmapCacheOption.OnLoad;
            image.DecodePixelWidth = PreviewImageWidth;
            image.UriSource = new Uri(path);
            image.EndInit();
            image.Freeze();
            return image;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or NotSupportedException
            or UriFormatException or ArgumentException or InvalidOperationException)
        {
            return null;
        }
    }

    private void ShowPreviewHeader(string icon, string name, string folder)
    {
        PreviewIcon.Text = icon;
        PreviewName.Text = name;
        PreviewPath.Text = folder;
        PreviewSize.Text = "–";
        PreviewModified.Text = "–";
        PreviewKind.Text = "–";
    }

    private void ShowPreviewNote(string note)
    {
        PreviewText.Visibility = Visibility.Collapsed;
        PreviewImage.Visibility = Visibility.Collapsed;
        PreviewImage.Source = null;
        PreviewNote.Text = note;
        PreviewNote.Visibility = Visibility.Visible;
    }

    private void ShowPreviewInfo(PreviewInfo info, BitmapImage? image)
    {
        PreviewSize.Text = info.Size is long size ? Preview.FormatSize(size) : "–";
        PreviewModified.Text = info.Modified?.ToString("g") ?? "–";
        PreviewKind.Text = info.Kind;
        IReadOnlyList<string>? lines = info.Lines ?? info.Entries;
        if (image is not null)
        {
            ShowPreviewNote("");
            PreviewImage.Source = image;
            PreviewImage.Visibility = Visibility.Visible;
            PreviewNote.Visibility = Visibility.Collapsed;
        }
        else if (lines is { Count: > 0 })
        {
            PreviewNumbers.Text = info.Lines is null ? "" : string.Join("\n", Enumerable.Range(1, lines.Count));
            PreviewLines.Text = string.Join("\n", lines);
            PreviewText.ScrollToHome();
            PreviewText.Visibility = Visibility.Visible;
            PreviewImage.Visibility = Visibility.Collapsed;
            PreviewNote.Visibility = Visibility.Collapsed;
        }
        else
        {
            ShowPreviewNote(info.IsImage ? "Could not show this image" : info.Note ?? "");
        }
    }

    private void OnPreviewOpen(object sender, RoutedEventArgs e) => Accept();

    private void OnPreviewReveal(object sender, RoutedEventArgs e) => Reveal();

    private void OnPreviewCopy(object sender, RoutedEventArgs e) => CopySelected(path: true);
}
