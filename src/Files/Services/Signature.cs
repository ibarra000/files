using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;

namespace Files.Services;

/// <summary>
/// Checks a file's Authenticode signature: the file must be unmodified since
/// it was signed, and signed by one particular certificate.
/// </summary>
/// <remarks>
/// Releases are signed with a self-signed certificate, which Windows does not
/// trust by default. So a valid signature from an untrusted root is accepted,
/// and trust comes from pinning the certificate's thumbprint instead: a
/// signature from any other certificate, trusted or not, is refused.
/// </remarks>
public static class Signature
{
    private const uint UiNone = 2;
    private const uint RevocationNone = 0;
    private const uint ChoiceFile = 1;
    private const uint NoRevocationCheck = 0x10;
    private const int Valid = 0;
    private const int UntrustedRoot = unchecked((int)0x800B0109);
    private static readonly Guid GenericVerifyV2 = new("00AAC56B-CD44-11d0-8CC2-00C04FC295EE");

    public static bool IsSignedBy(string path, string thumbprint)
    {
        if (!HasIntactSignature(path))
        {
            return false;
        }
        try
        {
#pragma warning disable SYSLIB0057 // The replacement loaders read certificate files, not signed files.
            using X509Certificate signer = X509Certificate.CreateFromSignedFile(path);
#pragma warning restore SYSLIB0057
            return signer.GetCertHashString().Equals(thumbprint, StringComparison.OrdinalIgnoreCase);
        }
        catch (CryptographicException)
        {
            return false;
        }
    }

    /// <summary>Whether the signature is present and the file matches it, whoever signed it.</summary>
    private static bool HasIntactSignature(string path)
    {
        var file = new FileInfo
        {
            Size = (uint)Marshal.SizeOf<FileInfo>(),
            FilePath = Marshal.StringToCoTaskMemUni(path),
        };
        IntPtr filePointer = Marshal.AllocCoTaskMem(Marshal.SizeOf<FileInfo>());
        try
        {
            Marshal.StructureToPtr(file, filePointer, false);
            var data = new TrustData
            {
                Size = (uint)Marshal.SizeOf<TrustData>(),
                UiChoice = UiNone,
                RevocationChecks = RevocationNone,
                UnionChoice = ChoiceFile,
                File = filePointer,
                ProviderFlags = NoRevocationCheck,
            };
            Guid action = GenericVerifyV2;
            int result = WinVerifyTrust(IntPtr.Zero, ref action, ref data);
            return result is Valid or UntrustedRoot;
        }
        finally
        {
            Marshal.FreeCoTaskMem(filePointer);
            Marshal.FreeCoTaskMem(file.FilePath);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct FileInfo
    {
        public uint Size;
        public IntPtr FilePath;
        public IntPtr FileHandle;
        public IntPtr KnownSubject;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct TrustData
    {
        public uint Size;
        public IntPtr PolicyCallbackData;
        public IntPtr SipClientData;
        public uint UiChoice;
        public uint RevocationChecks;
        public uint UnionChoice;
        public IntPtr File;
        public uint StateAction;
        public IntPtr StateData;
        public IntPtr UrlReference;
        public uint ProviderFlags;
        public uint UiContext;
        public IntPtr SignatureSettings;
    }

    [DllImport("wintrust.dll", CharSet = CharSet.Unicode)]
    private static extern int WinVerifyTrust(IntPtr window, ref Guid action, ref TrustData data);
}
