namespace Files.Data;

/// <summary>How a folder the user picked becomes a volume root and type.</summary>
public static class VolumeRoot
{
    /// <summary>
    /// A full path with backslashes and no trailing separator (except on a
    /// drive root such as <c>C:\</c>), lowercased for network shares, whose
    /// names Windows compares without case.
    /// </summary>
    public static string Normalize(string folder)
    {
        string full = Path.GetFullPath(folder.Trim()).Replace('/', '\\');
        string root = Path.GetPathRoot(full) ?? "";
        string trimmed = full.Length > root.Length ? full.TrimEnd('\\') : full;
        if (TypeOf(trimmed) == "smb")
        {
            return trimmed.ToLowerInvariant();
        }
        bool hasDriveLetter = trimmed.Length >= 2 && trimmed[1] == ':';
        if (hasDriveLetter)
        {
            // c:\data and C:\data are the same folder.
            return char.ToUpperInvariant(trimmed[0]) + trimmed[1..];
        }
        return trimmed;
    }

    /// <summary>"smb" for UNC paths and mapped drives, "nfs" for NFS mounts, otherwise "local".</summary>
    public static string TypeOf(string root)
    {
        if (root.StartsWith(@"\\", StringComparison.Ordinal))
        {
            return "smb";
        }
        try
        {
            var drive = new DriveInfo(root);
            if (drive.DriveType != DriveType.Network)
            {
                return "local";
            }
            return drive.DriveFormat.Equals("NFS", StringComparison.OrdinalIgnoreCase) ? "nfs" : "smb";
        }
        catch (Exception error) when (error is IOException or ArgumentException or UnauthorizedAccessException)
        {
            // A disconnected mapped drive cannot be asked; mapped drives are SMB far more often.
            return "smb";
        }
    }
}
