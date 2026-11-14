using System.Drawing.Imaging;
using System.Runtime.InteropServices;

namespace Files.Services;

/// <summary>
/// Draws a <see cref="Mesh"/> shaded, from the same isometric view as STEP's wireframe, with the model's
/// edges over it where they can be seen. A small software renderer: a depth buffer at twice the size,
/// lit from over the viewer's left shoulder and from the eye, and the result averaged down.
/// </summary>
internal static class ShadedDrawing
{
    private const int Margin = 16;
    private const int Supersample = 2;
    private const double Ambient = 0.32, Headlight = 0.3, Diffuse = 0.42, Shine = 0.2, ShineSharpness = 30;
    private const double EdgeInk = 0.72;
    private const int White = 0xFFFFFF;

    /// <summary>Pixels tried, all triangles together, before the picture is given up (many large triangles over each other).</summary>
    private const long MaxPixelWork = 200_000_000;

    /// <summary>Edge points drawn, counting each placed copy, before the rest of the edges are left out.</summary>
    private const long MaxEdgePoints = 4_000_000;
    private static readonly int Ink = Rgb(38, 42, 50);

    /// <summary>A muted colour per part, so an assembly's parts tell apart; a single part is the first.</summary>
    private static readonly int[] Palette =
    [
        Rgb(150, 168, 190), Rgb(204, 170, 122), Rgb(148, 182, 158), Rgb(194, 150, 160),
        Rgb(172, 162, 204), Rgb(204, 194, 138), Rgb(138, 184, 196),
    ];

    private static readonly Vector3 Eye = new Vector3(1, 1, 1).Unit;
    private static readonly Vector3 Up = new Vector3(-1, -1, 2).Unit;
    private static readonly Vector3 Right = new Vector3(-1, 1, 0).Unit;
    private static readonly Vector3 Light = (Eye + Up * 0.6 - Right * 0.35).Unit;
    private static readonly Vector3 Halfway = (Light + Eye).Unit;

    private static int Rgb(int r, int g, int b) => (r << 16) | (g << 8) | b;

    /// <summary>PNG bytes <paramref name="width"/> pixels wide (height as LineDrawing chooses it); null when the mesh has no size or is too costly to draw.</summary>
    public static byte[]? Render(Mesh mesh, int width, CancellationToken cancel = default)
    {
        if (Bounds(mesh) is not { } bounds)
        {
            return null;
        }
        (double left, double right, double bottom, double top, double near, double far) = bounds;
        double across = right - left, up = top - bottom;
        if (across <= 0 && up <= 0)
        {
            return null;
        }
        int height = (int)Math.Clamp(across > 0 ? width * up / across : width, width / 4.0, width * 1.25);
        double scale = Math.Min(
            across > 0 ? (width - 2 * Margin) / across : double.MaxValue,
            up > 0 ? (height - 2 * Margin) / up : double.MaxValue);
        if (!double.IsFinite(scale) || scale <= 0)
        {
            return null;
        }
        var canvas = new Canvas(width * Supersample, height * Supersample, cancel)
        {
            Scale = scale * Supersample,
            OffsetX = ((width - across * scale) / 2) * Supersample,
            OffsetY = ((height - up * scale) / 2) * Supersample,
            Left = left,
            Bottom = bottom,
        };
        // An edge lies on its faces, so it is drawn where it is level with them; a little more lets it win over them.
        double bias = (near - far) * 0.004 + 2.5 / canvas.Scale;
        Dictionary<int, int> colours = Colours(mesh);
        foreach ((int solid, Frame at, int part) in mesh.Placements)
        {
            if (!canvas.Fill(mesh.Solids[solid], at, Palette[colours[part] % Palette.Length]))
            {
                return null;
            }
        }
        long edgePoints = 0;
        foreach ((int solid, Frame at, _) in mesh.Placements)
        {
            foreach (Vector3[] edge in mesh.Solids[solid].Edges)
            {
                if ((edgePoints += edge.Length) > MaxEdgePoints)
                {
                    break; // the shading is drawn; the edges past this are left out
                }
                cancel.ThrowIfCancellationRequested();
                canvas.Line(edge, at, bias);
            }
        }
        return canvas.Png(Supersample);
    }

    /// <summary>
    /// Each part's place in the palette, in the order parts are first placed. A part is one colour however
    /// many bodies it has (a bearing's rings and balls), and every copy of it is the same colour.
    /// </summary>
    private static Dictionary<int, int> Colours(Mesh mesh)
    {
        var colours = new Dictionary<int, int>();
        foreach ((_, _, int part) in mesh.Placements)
        {
            colours.TryAdd(part, colours.Count);
        }
        return colours;
    }

    private static (double Left, double Right, double Bottom, double Top, double Near, double Far)? Bounds(Mesh mesh)
    {
        double left = double.MaxValue, right = double.MinValue, bottom = double.MaxValue, top = double.MinValue;
        double near = double.MinValue, far = double.MaxValue;
        void Take(Vector3 point)
        {
            System.Windows.Point flat = StepPreview.Isometric(point);
            double depth = StepPreview.Depth(point);
            (left, right) = (Math.Min(left, flat.X), Math.Max(right, flat.X));
            (bottom, top) = (Math.Min(bottom, flat.Y), Math.Max(top, flat.Y));
            (far, near) = (Math.Min(far, depth), Math.Max(near, depth));
        }
        foreach ((int solid, Frame at, _) in mesh.Placements)
        {
            float[] points = mesh.Solids[solid].Points;
            for (int i = 0; i + 2 < points.Length; i += 3)
            {
                Take(at.Point(new Vector3(points[i], points[i + 1], points[i + 2])));
            }
            foreach (Vector3[] edge in mesh.Solids[solid].Edges)
            {
                foreach (Vector3 point in edge)
                {
                    Take(at.Point(point));
                }
            }
        }
        return left <= right && double.IsFinite(right - left) && double.IsFinite(top - bottom) && double.IsFinite(near - far)
            ? (left, right, bottom, top, near, far)
            : null;
    }

    private sealed class Canvas(int width, int height, CancellationToken cancel)
    {
        private long _pixelWork;
        private readonly float[] _depth = Enumerable.Repeat(float.NegativeInfinity, width * height).ToArray();
        private readonly int[] _colour = Enumerable.Repeat(White, width * height).ToArray();
        private readonly bool[] _edge = new bool[width * height];

        public required double Scale { get; init; }
        public required double OffsetX { get; init; }
        public required double OffsetY { get; init; }
        public required double Left { get; init; }
        public required double Bottom { get; init; }

        /// <summary>Where a point falls on the canvas (y down), and its depth (larger is nearer).</summary>
        private (double X, double Y, double Z) Project(Vector3 point)
        {
            System.Windows.Point flat = StepPreview.Isometric(point);
            return (OffsetX + (flat.X - Left) * Scale, height - OffsetY - (flat.Y - Bottom) * Scale, StepPreview.Depth(point));
        }

        /// <summary>Fills a placed solid's triangles; false when the picture has cost more than <see cref="MaxPixelWork"/>.</summary>
        public bool Fill(SolidMesh mesh, Frame at, int colour)
        {
            float[] p = mesh.Points, n = mesh.Normals;
            Span<(double X, double Y, double Z)> corners = stackalloc (double, double, double)[3];
            Span<(double Diffuse, double Shine)> light = stackalloc (double, double)[3];
            for (int t = 0; t + 8 < p.Length; t += 9)
            {
                for (int k = 0; k < 3; k++)
                {
                    int i = t + 3 * k;
                    corners[k] = Project(at.Point(new Vector3(p[i], p[i + 1], p[i + 2])));
                    light[k] = Lit(at.Direction(new Vector3(n[i], n[i + 1], n[i + 2])).Unit);
                }
                if ((t & 0x7FFF) == 0)
                {
                    cancel.ThrowIfCancellationRequested();
                }
                if (!Triangle(corners, light, colour))
                {
                    return false;
                }
            }
            return true;
        }

        /// <summary>
        /// A key light over the left shoulder and a weaker one from the eye, so no face that can be seen is black.
        /// Both sides of a face are lit alike, so which way a file's faces point never matters.
        /// </summary>
        private static (double Diffuse, double Shine) Lit(Vector3 normal) =>
            (Ambient + Headlight * Math.Abs(normal.Dot(Eye)) + Diffuse * Math.Abs(normal.Dot(Light)), Shine * Math.Pow(Math.Abs(normal.Dot(Halfway)), ShineSharpness));

        private bool Triangle(Span<(double X, double Y, double Z)> c, Span<(double Diffuse, double Shine)> light, int colour)
        {
            double area = Edge(c[0], c[1], c[2].X, c[2].Y);
            if (Math.Abs(area) < 1e-9 || !double.IsFinite(area))
            {
                return true;
            }
            int x0 = Math.Max(0, (int)Math.Floor(Math.Min(c[0].X, Math.Min(c[1].X, c[2].X))));
            int x1 = Math.Min(width - 1, (int)Math.Ceiling(Math.Max(c[0].X, Math.Max(c[1].X, c[2].X))));
            int y0 = Math.Max(0, (int)Math.Floor(Math.Min(c[0].Y, Math.Min(c[1].Y, c[2].Y))));
            int y1 = Math.Min(height - 1, (int)Math.Ceiling(Math.Max(c[0].Y, Math.Max(c[1].Y, c[2].Y))));
            // The triangle's share of the picture's work: about its area, and a row's worth per row it crosses.
            _pixelWork += (long)Math.Abs(area) / 2 + Math.Max(0, y1 - y0 + 1);
            if (_pixelWork > MaxPixelWork)
            {
                return false;
            }
            const double Inside = -1e-7;
            for (int y = y0; y <= y1; y++)
            {
                double py = y + 0.5;
                // Each weight is a straight line along the row: where all three are at least 0 is one span, found
                // without trying each pixel (a long, thin triangle across a face would otherwise try thousands).
                double w0At = Edge(c[1], c[2], 0, py) / area, w0Per = (Edge(c[1], c[2], 1, py) / area) - w0At;
                double w1At = Edge(c[2], c[0], 0, py) / area, w1Per = (Edge(c[2], c[0], 1, py) / area) - w1At;
                double low = x0, high = x1 + 1;
                Narrow(w0At, w0Per, Inside, ref low, ref high);
                Narrow(w1At, w1Per, Inside, ref low, ref high);
                Narrow(1 - w0At - w1At, -w0Per - w1Per, Inside, ref low, ref high);
                int first = Math.Max(x0, (int)Math.Ceiling(low - 0.5)), last = Math.Min(x1, (int)Math.Floor(high - 0.5));
                for (int x = first; x <= last; x++)
                {
                    double px = x + 0.5;
                    double w0 = w0At + w0Per * px, w1 = w1At + w1Per * px, w2 = 1 - w0 - w1;
                    if (w0 < Inside || w1 < Inside || w2 < Inside)
                    {
                        continue;
                    }
                    float z = (float)(w0 * c[0].Z + w1 * c[1].Z + w2 * c[2].Z);
                    int at = y * width + x;
                    if (z <= _depth[at])
                    {
                        continue;
                    }
                    _depth[at] = z;
                    double diffuse = w0 * light[0].Diffuse + w1 * light[1].Diffuse + w2 * light[2].Diffuse;
                    double shine = w0 * light[0].Shine + w1 * light[1].Shine + w2 * light[2].Shine;
                    _colour[at] = Shade(colour, diffuse, shine);
                }
            }
            return true;
        }

        /// <summary>Narrows [low, high] to where at + per·x is at least <paramref name="least"/>.</summary>
        private static void Narrow(double at, double per, double least, ref double low, ref double high)
        {
            if (per > 0)
            {
                low = Math.Max(low, (least - at) / per);
            }
            else if (per < 0)
            {
                high = Math.Min(high, (least - at) / per);
            }
            else if (at < least)
            {
                high = low - 1; // never inside on this row
            }
        }

        private static double Edge((double X, double Y, double Z) a, (double X, double Y, double Z) b, double x, double y) =>
            ((b.X - a.X) * (y - a.Y)) - ((b.Y - a.Y) * (x - a.X));

        private static int Shade(int colour, double diffuse, double shine)
        {
            int Channel(int shift) => (int)Math.Clamp(((colour >> shift) & 0xFF) * diffuse + 255 * shine, 0, 255);
            return (Channel(16) << 16) | (Channel(8) << 8) | Channel(0);
        }

        /// <summary>An edge, about one final pixel wide, marked wherever it is not behind a face.</summary>
        public void Line(Vector3[] points, Frame at, double bias)
        {
            for (int i = 1; i < points.Length; i++)
            {
                (double X, double Y, double Z) a = Project(at.Point(points[i - 1])), b = Project(at.Point(points[i]));
                int steps = (int)Math.Ceiling(Math.Max(Math.Abs(b.X - a.X), Math.Abs(b.Y - a.Y)));
                if (steps > 4 * (width + height))
                {
                    continue; // a damaged point far off the picture
                }
                for (int s = 0; s <= steps; s++)
                {
                    double t = steps == 0 ? 0 : (double)s / steps;
                    Mark(a.X + (b.X - a.X) * t, a.Y + (b.Y - a.Y) * t, a.Z + (b.Z - a.Z) * t + bias);
                }
            }
        }

        private void Mark(double x, double y, double z)
        {
            int left = (int)Math.Floor(x - 0.5), top = (int)Math.Floor(y - 0.5);
            for (int dy = 0; dy < Supersample; dy++)
            {
                for (int dx = 0; dx < Supersample; dx++)
                {
                    int px = left + dx, py = top + dy;
                    if (px < 0 || py < 0 || px >= width || py >= height)
                    {
                        continue;
                    }
                    int at = py * width + px;
                    if (z >= _depth[at])
                    {
                        _edge[at] = true;
                    }
                }
            }
        }

        /// <summary>The canvas averaged down by <paramref name="factor"/> each way, as PNG.</summary>
        public byte[] Png(int factor)
        {
            int outWidth = width / factor, outHeight = height / factor;
            var pixels = new int[outWidth * outHeight];
            for (int y = 0; y < outHeight; y++)
            {
                for (int x = 0; x < outWidth; x++)
                {
                    int r = 0, g = 0, b = 0;
                    for (int dy = 0; dy < factor; dy++)
                    {
                        for (int dx = 0; dx < factor; dx++)
                        {
                            int at = (y * factor + dy) * width + x * factor + dx;
                            int c = _edge[at] ? Mix(_colour[at], Ink, EdgeInk) : _colour[at];
                            (r, g, b) = (r + ((c >> 16) & 0xFF), g + ((c >> 8) & 0xFF), b + (c & 0xFF));
                        }
                    }
                    int samples = factor * factor;
                    pixels[y * outWidth + x] = unchecked((int)0xFF000000) | Rgb(r / samples, g / samples, b / samples);
                }
            }
            using var picture = new System.Drawing.Bitmap(outWidth, outHeight, PixelFormat.Format32bppArgb);
            BitmapData data = picture.LockBits(new System.Drawing.Rectangle(0, 0, outWidth, outHeight), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
            try
            {
                for (int y = 0; y < outHeight; y++)
                {
                    Marshal.Copy(pixels, y * outWidth, data.Scan0 + y * data.Stride, outWidth);
                }
            }
            finally
            {
                picture.UnlockBits(data);
            }
            using var png = new MemoryStream();
            picture.Save(png, ImageFormat.Png);
            return png.ToArray();
        }

        private static int Mix(int from, int to, double amount)
        {
            int Channel(int shift) => (int)(((from >> shift) & 0xFF) * (1 - amount) + ((to >> shift) & 0xFF) * amount);
            return (Channel(16) << 16) | (Channel(8) << 8) | Channel(0);
        }
    }
}
