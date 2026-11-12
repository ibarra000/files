using System.ComponentModel;
using System.Diagnostics;

namespace Files.Services;

/// <summary>What to open a file with.</summary>
public abstract record OpenWith
{
    public sealed record Default : OpenWith;

    public sealed record Program(Viewer Viewer) : OpenWith;

    /// <summary>The choice for Enter, from <see cref="Settings.EnterOpens"/>.</summary>
    public static OpenWith ForEnter(Settings settings)
    {
        Viewer? viewer = settings.Viewers.FirstOrDefault(v => v.Name == settings.EnterOpens);
        return viewer is null ? new Default() : new Program(viewer);
    }
}

/// <summary>Opens files and folders. Every method returns a message for the user, or null when all went well.</summary>
public static class Opener
{
    public static string? Open(string path, OpenWith with)
    {
        if (!Path.Exists(path))
        {
            return $"{Path.GetFileName(path)} no longer exists";
        }
        if (with is OpenWith.Program program && File.Exists(path))
        {
            // File names cannot contain a double quote, so quoting is enough.
            return Start(new ProcessStartInfo(program.Viewer.Program, $"\"{path}\"") { UseShellExecute = true });
        }
        // The Windows default: the file's program, or Explorer for a folder.
        return Start(new ProcessStartInfo(path) { UseShellExecute = true });
    }

    public static string? Reveal(string path)
    {
        // explorer.exe parses its own command line, so the path is quoted by hand.
        return Start(new ProcessStartInfo("explorer.exe", $"/select,\"{path}\""));
    }

    private static string? Start(ProcessStartInfo info)
    {
        try
        {
            Process.Start(info)?.Dispose();
            return null;
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or FileNotFoundException)
        {
            return $"Could not open {Path.GetFileName(info.FileName)}: {error.Message}";
        }
    }
}
