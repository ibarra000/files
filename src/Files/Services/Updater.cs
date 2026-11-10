using System.ComponentModel;
using System.Diagnostics;
using System.Reflection;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Files.Services;

/// <summary>The release manifest, latest.json, published beside each installer.</summary>
public sealed record Manifest(
    [property: JsonPropertyName("version")] string Version,
    [property: JsonPropertyName("msi")] string Msi,
    [property: JsonPropertyName("sha256")] string Sha256,
    [property: JsonPropertyName("notes")] string? Notes)
{
    /// <summary>Parses and checks a manifest; returns null when it is not usable.</summary>
    public static Manifest? Parse(string json)
    {
        Manifest? manifest;
        try
        {
            manifest = JsonSerializer.Deserialize<Manifest>(json);
        }
        catch (JsonException)
        {
            return null;
        }
        // Missing fields arrive as null despite the non-nullable types.
        if (manifest?.Version is null || manifest.Msi is null || manifest.Sha256 is null || !System.Version.TryParse(manifest.Version, out _))
        {
            return null;
        }
        bool plainName = manifest.Msi.Length > 0 && manifest.Msi == Path.GetFileName(manifest.Msi) && !manifest.Msi.Contains("..");
        bool validDigest = manifest.Sha256.Length == 64 && manifest.Sha256.All(Uri.IsHexDigit);
        return plainName && validDigest ? manifest : null;
    }

    public bool IsNewerThan(Version current) => System.Version.Parse(Version) > current;
}

/// <summary>
/// Looks for a newer release in a shared folder or on GitHub, and installs it:
/// the installer is verified, then a copy of this program waits for us to
/// quit, runs msiexec and starts the new version.
/// </summary>
public sealed class Updater(Func<Settings> settings, string stagingFolder)
{
    public const string ApplyFlag = "--apply-update";
    private const string Releases = "https://github.com/ibarra000/files/releases";

    /// <summary>
    /// SHA-1 thumbprint of the release-signing certificate (assets/files-signing.cer).
    /// Only installers signed with it are installed. A new certificate needs a
    /// release signed with the old one that carries the new thumbprint.
    /// </summary>
    public const string PublisherThumbprint = "5EAA01DFACB29C29C8D7C21F961F616CFB6B3728";
    private const long MaxInstallerBytes = 200L * 1024 * 1024;
    private const int RebootRequired = 3010;
    private const int RebootStarted = 1641;
    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromMinutes(5) };

    public static Version Current { get; } = Assembly.GetExecutingAssembly().GetName().Version ?? new Version(0, 0);

    /// <summary>The newer release, if any. Throws on network or file errors.</summary>
    public async Task<Manifest?> CheckAsync(CancellationToken cancel)
    {
        string folder = settings().UpdateFolder;
        string json = folder.Length > 0
            ? await File.ReadAllTextAsync(Path.Combine(folder, "latest.json"), cancel)
            : await Http.GetStringAsync($"{Releases}/latest/download/latest.json", cancel);
        Manifest? manifest = Manifest.Parse(json);
        return manifest is not null && manifest.IsNewerThan(Current) ? manifest : null;
    }

    /// <summary>Downloads and verifies the installer, starts the helper and returns; the caller then quits.</summary>
    public async Task InstallAsync(Manifest manifest, CancellationToken cancel)
    {
        Directory.CreateDirectory(stagingFolder);
        string msi = Path.Combine(stagingFolder, manifest.Msi);
        string folder = settings().UpdateFolder;
        if (folder.Length > 0)
        {
            await Task.Run(() => File.Copy(Path.Combine(folder, manifest.Msi), msi, overwrite: true), cancel);
        }
        else
        {
            using HttpResponseMessage response = await Http.GetAsync($"{Releases}/download/v{manifest.Version}/{manifest.Msi}", HttpCompletionOption.ResponseHeadersRead, cancel);
            response.EnsureSuccessStatusCode();
            if (response.Content.Headers.ContentLength > MaxInstallerBytes)
            {
                throw new IOException("The installer is larger than expected");
            }
            // Closed before the checks below open the file again.
            await using (FileStream file = File.Create(msi))
            {
                await response.Content.CopyToAsync(file, cancel);
            }
        }
        if (!await Task.Run(() => HasDigest(msi, manifest.Sha256), cancel))
        {
            File.Delete(msi);
            throw new IOException("The downloaded installer is damaged (its SHA-256 does not match)");
        }
        // The digest only proves the download is complete; the signature proves who made it.
        if (!await Task.Run(() => Signature.IsSignedBy(msi, PublisherThumbprint), cancel))
        {
            File.Delete(msi);
            throw new IOException("The installer is not signed by the files publisher, so it was not installed");
        }

        // The running executable cannot be replaced while it runs, so a copy applies the update.
        string self = Environment.ProcessPath!;
        string helper = Path.Combine(stagingFolder, "files-update.exe");
        File.Copy(self, helper, overwrite: true);
        var start = new ProcessStartInfo(helper) { UseShellExecute = false };
        foreach (string argument in new[] { ApplyFlag, msi, Environment.ProcessId.ToString(), self })
        {
            start.ArgumentList.Add(argument);
        }
        Process.Start(start)?.Dispose();
    }

    public static bool HasDigest(string path, string sha256)
    {
        using FileStream file = File.OpenRead(path);
        return Convert.ToHexStringLower(SHA256.HashData(file)) == sha256.ToLowerInvariant();
    }

    /// <summary>The helper: waits for the old process, installs, and relaunches only if msiexec succeeded.</summary>
    public static int Apply(string msi, int waitForPid, string relaunch)
    {
        try
        {
            return Install(msi, waitForPid, relaunch);
        }
        catch (Exception error) when (error is Win32Exception or IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            System.Windows.MessageBox.Show($"The update could not be installed:\n\n{error.Message}", "files");
            return 1;
        }
    }

    private static int Install(string msi, int waitForPid, string relaunch)
    {
        try
        {
            using Process old = Process.GetProcessById(waitForPid);
            old.WaitForExit(TimeSpan.FromSeconds(60));
        }
        catch (ArgumentException)
        {
            // Already gone.
        }
        var install = new ProcessStartInfo("msiexec.exe") { UseShellExecute = false };
        foreach (string argument in new[] { "/i", msi, "/qb" })
        {
            install.ArgumentList.Add(argument);
        }
        using Process msiexec = Process.Start(install) ?? throw new InvalidOperationException("msiexec did not start");
        msiexec.WaitForExit();
        bool installed = msiexec.ExitCode is 0 or RebootRequired or RebootStarted;
        if (!installed)
        {
            // Not relaunched, so a failed install never looks like it worked.
            System.Windows.MessageBox.Show($"The update was not installed (msiexec exit code {msiexec.ExitCode}).", "files");
            return msiexec.ExitCode;
        }
        Process.Start(new ProcessStartInfo(relaunch) { UseShellExecute = true })?.Dispose();
        TryDelete(msi);
        return 0;
    }

    private static void TryDelete(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Left in the staging folder; the next update overwrites it.
        }
    }
}
