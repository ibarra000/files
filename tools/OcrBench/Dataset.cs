using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;
using Files.Core;

namespace OcrBench;

/// <summary>One crop to read: a code drawn in a line of text, the way it looks on screen.</summary>
public sealed record Sample(int Id, string Code, string Font, int SizePx, bool Dark, bool Jpeg);

/// <summary>
/// Codes made to be hard: letters and digits that look alike (0/O, 1/I/l, 5/S, 8/B, 2/Z, 6/G)
/// in the shapes of drawing and part numbers. Fixed seed, so every run reads the same crops.
/// </summary>
public static class Dataset
{
    public const int CropWidth = 600;
    public const int CropHeight = 200;
    private const string Prefix = "see ";
    private const string Suffix = " for details";

    private static readonly string[] Fonts = ["Segoe UI", "Consolas", "Arial", "Courier New", "Tahoma"];
    private static readonly int[] Sizes = [11, 13, 15];

    // Weighted towards the characters that are read wrongly.
    private const string Letters = "OOIISSBBZZGGDQLACEHKMNPRTUX";
    private const string Digits = "0011558822660123456789";

    private static readonly string[] Shapes =
    [
        "LL-L-DDDD", "LDD-LLD-LD", "DDDD-LLL-DDL", "LLDDDD_RevL", "LL-DDDD-DDL", "LLL-DDDDD", "DDLL-DLDL", "L-DDD-LL-DD",
    ];

    public static IReadOnlyList<Sample> Build(int codes, int variantsPerCode, int seed = 62)
    {
        var random = new Random(seed);
        var samples = new List<Sample>();
        var used = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        while (used.Count < codes)
        {
            string code = Code(random);
            if (!used.Add(code))
            {
                continue;
            }
            for (int v = 0; v < variantsPerCode; v++)
            {
                samples.Add(new Sample(samples.Count, code, Fonts[random.Next(Fonts.Length)], Sizes[random.Next(Sizes.Length)],
                    Dark: random.Next(2) == 0, Jpeg: random.Next(3) == 0));
            }
        }
        return samples;
    }

    /// <summary>
    /// An index like a drawing share: every code as a PDF, many other codes of the same shapes,
    /// and for a fifth of the codes a twin that differs in one look-alike character, so guessing
    /// at characters can find the wrong file.
    /// </summary>
    public static VolumeIndex Index(IReadOnlyList<Sample> samples, int distractors, int seed = 620)
    {
        var random = new Random(seed);
        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (string code in samples.Select(s => s.Code).Distinct())
        {
            names.Add(code + ".pdf");
            if (random.Next(5) == 0 && Twin(code, random) is string twin)
            {
                names.Add(twin + ".pdf");
            }
        }
        while (names.Count < distractors)
        {
            names.Add(Code(random) + (random.Next(3) == 0 ? ".dwg" : ".pdf"));
        }
        var builder = new VolumeIndex.Builder(@"\\bench\drawings");
        foreach (var folder in names.Order(StringComparer.Ordinal).Chunk(500).Select((chunk, i) => (Name: $"jobs\\{i:D4}", Files: chunk)))
        {
            builder.AddFolder(folder.Name, folder.Files.Order(StringComparer.Ordinal));
        }
        return builder.Build();
    }

    /// <summary>
    /// The crop around the mouse, drawn at screen size with ClearType and enlarged with bicubic
    /// scaling as ScreenText.Grab does, as PNG; and where the mouse points (the middle of the code), in the enlarged crop.
    /// </summary>
    public static (byte[] Png, System.Windows.Point Pointer) Render(Sample sample, int scale)
    {
        using var screen = new Bitmap(CropWidth, CropHeight);
        float codeLeft;
        float codeWidth;
        const float Left = 40;
        const float Top = 90;
        using (var graphics = Graphics.FromImage(screen))
        {
            graphics.Clear(sample.Dark ? Color.FromArgb(30, 30, 32) : Color.White);
            graphics.TextRenderingHint = TextRenderingHint.ClearTypeGridFit;
            using var font = new Font(sample.Font, sample.SizePx, GraphicsUnit.Pixel);
            using var brush = new SolidBrush(sample.Dark ? Color.FromArgb(230, 230, 230) : Color.Black);
            StringFormat format = StringFormat.GenericTypographic;
            graphics.DrawString(Prefix + sample.Code + Suffix, font, brush, Left, Top, format);
            codeLeft = Left + graphics.MeasureString(Prefix, font, int.MaxValue, format).Width;
            codeWidth = graphics.MeasureString(sample.Code, font, int.MaxValue, format).Width;
        }
        using Bitmap source = sample.Jpeg ? Degrade(screen) : new Bitmap(screen);
        using var image = new Bitmap(CropWidth * scale, CropHeight * scale);
        using (var graphics = Graphics.FromImage(image))
        {
            graphics.InterpolationMode = InterpolationMode.HighQualityBicubic;
            graphics.PixelOffsetMode = PixelOffsetMode.HighQuality;
            graphics.DrawImage(source, 0, 0, image.Width, image.Height);
        }
        using var png = new MemoryStream();
        image.Save(png, ImageFormat.Png);
        var pointer = new System.Windows.Point((codeLeft + codeWidth / 2) * scale, (Top + sample.SizePx / 2.0) * scale);
        return (png.ToArray(), pointer);
    }

    /// <summary>JPEG at low quality, like a remote desktop or a screenshot of a screenshot.</summary>
    private static Bitmap Degrade(Bitmap screen)
    {
        ImageCodecInfo jpeg = ImageCodecInfo.GetImageEncoders().First(codec => codec.FormatID == ImageFormat.Jpeg.Guid);
        using var parameters = new EncoderParameters(1);
        parameters.Param[0] = new EncoderParameter(Encoder.Quality, 35L);
        var stream = new MemoryStream();
        screen.Save(stream, jpeg, parameters);
        stream.Position = 0;
        return new Bitmap(stream);
    }

    private static string Code(Random random)
    {
        string shape = Shapes[random.Next(Shapes.Length)];
        var chars = shape.Select(c => c switch
        {
            'L' => Letters[random.Next(Letters.Length)],
            'D' => Digits[random.Next(Digits.Length)],
            _ => c,
        });
        string code = new(chars.ToArray());
        // Shapes with "Rev" keep their word; a few codes carry a lowercase l, the hardest of all.
        return random.Next(8) == 0 ? code.Replace('I', 'l') : code;
    }

    private static string? Twin(string code, Random random)
    {
        int[] at = Enumerable.Range(0, code.Length).Where(i => Confusion.Alternatives(code[i]).Length > 0).ToArray();
        if (at.Length == 0)
        {
            return null;
        }
        int i = at[random.Next(at.Length)];
        char[] chars = code.ToCharArray();
        string alternatives = Confusion.Alternatives(chars[i]);
        chars[i] = alternatives[random.Next(alternatives.Length)];
        return new string(chars);
    }
}
