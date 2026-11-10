using System.Security.Cryptography.X509Certificates;
using Files.Services;

namespace Files.Tests;

public sealed class SignatureTests
{
    // dotnet.exe carries an embedded Authenticode signature on every machine that runs these tests.
    private static readonly string SignedFile = Environment.ProcessPath is { } host && host.EndsWith("dotnet.exe", StringComparison.OrdinalIgnoreCase)
        ? host
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "dotnet", "dotnet.exe");

    private static string SignerOf(string path)
    {
#pragma warning disable SYSLIB0057
        using X509Certificate signer = X509Certificate.CreateFromSignedFile(path);
#pragma warning restore SYSLIB0057
        return signer.GetCertHashString();
    }

    [Fact]
    public void A_file_signed_by_the_pinned_certificate_passes()
    {
        Assert.True(Signature.IsSignedBy(SignedFile, SignerOf(SignedFile)));
    }

    [Fact]
    public void A_valid_signature_from_another_certificate_is_refused()
    {
        Assert.False(Signature.IsSignedBy(SignedFile, Updater.PublisherThumbprint));
    }

    [Fact]
    public void A_file_changed_after_signing_is_refused()
    {
        using var temp = new TempFolder();
        string copy = Path.Combine(temp.Path, "changed.exe");
        File.Copy(SignedFile, copy);
        byte[] bytes = File.ReadAllBytes(copy);
        bytes[bytes.Length / 3] ^= 0xFF;
        File.WriteAllBytes(copy, bytes);

        Assert.False(Signature.IsSignedBy(copy, SignerOf(SignedFile)));
    }

    [Fact]
    public void An_unsigned_file_is_refused()
    {
        using var temp = new TempFolder();
        Assert.False(Signature.IsSignedBy(temp.File("plain.msi", "not an installer"), Updater.PublisherThumbprint));
    }
}
