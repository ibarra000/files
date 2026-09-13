#!/usr/bin/env python3
"""Draws the application icon.

Committed and re-runnable rather than a binary that appeared in the tree: an
icon nobody can regenerate is one nobody can change, and "why is the blue
slightly different from the panel's" is a question that should have an answer
in the repository rather than in somebody's image editor.

Writes two things, because two things need it and neither should be derived
from the other at run time:

  assets/files.ico      every size Windows asks for, for the executable, the
                        Start menu, the taskbar and Alt-Tab
  assets/files-32.rgba  raw RGBA at 32x32, which is what `tray-icon` takes

Pure standard library: `struct` and `zlib` are enough to write a PNG, and an
.ico is a header plus a list of PNGs. Adding Pillow to build an icon would mean
a Python environment is part of building a Rust program.

Run from the repository root:

    python tools/make_icon.py
"""

import os
import struct
import zlib

# The panel's accent blue, and its lightest text. Taken from
# `src/gui/theme.rs` so the icon and the thing it launches are the same colour;
# if that palette moves, this is the one other place to change.
ACCENT = (0x6F, 0xB3, 0xFF)
ACCENT_DEEP = (0x1E, 0x5F, 0xB8)
INK = (0xF2, 0xF5, 0xFA)

# How much bigger the icon is drawn before being scaled down. Antialiasing by
# supersampling, which is 15 lines rather than a dependency.
SS = 8

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def rounded_square(x, y, size, radius):
    """Signed coverage test for a rounded square at the origin."""
    # Distance from the rounded-rectangle boundary, negative inside.
    inner = size / 2 - radius
    dx = max(abs(x - size / 2) - inner, 0.0)
    dy = max(abs(y - size / 2) - inner, 0.0)
    return (dx * dx + dy * dy) ** 0.5 - radius


def draw(size):
    """One icon, as a list of RGBA rows.

    A rounded square in the panel's accent blue, with three bars in it - a
    list, which is what the program shows you, in the shape Windows 11 gives
    every other app icon. Deliberately not a magnifying glass: at sixteen
    pixels a magnifier is four grey dots and a smudge, and this has to be
    recognisable in a notification area.
    """
    big = size * SS
    radius = big * 0.22

    # The three bars, as fractions of the icon: top, left, width, height.
    # Decreasing width, which reads as a list rather than as a flag.
    bars = [(0.30, 0.24, 0.52, 0.09), (0.455, 0.24, 0.40, 0.09), (0.61, 0.24, 0.28, 0.09)]
    bars_px = [
        (t * big, l * big, w * big, h * big)
        for (t, l, w, h) in bars
    ]

    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = a = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    x = px * SS + sx + 0.5
                    y = py * SS + sy + 0.5
                    if rounded_square(x, y, big, radius) > 0:
                        continue
                    # A vertical gradient, so the tile has some depth rather
                    # than reading as a flat swatch.
                    t = y / big
                    cr = ACCENT[0] + (ACCENT_DEEP[0] - ACCENT[0]) * t
                    cg = ACCENT[1] + (ACCENT_DEEP[1] - ACCENT[1]) * t
                    cb = ACCENT[2] + (ACCENT_DEEP[2] - ACCENT[2]) * t
                    for (bt, bl, bw, bh) in bars_px:
                        if bl <= x <= bl + bw and bt <= y <= bt + bh:
                            cr, cg, cb = INK
                            break
                    r += cr
                    g += cg
                    b += cb
                    a += 255.0
            n = SS * SS
            if a == 0:
                row += bytes((0, 0, 0, 0))
                continue
            # Averaged over the covered samples only, so an edge pixel keeps
            # the colour of the shape rather than fading towards black.
            covered = a / 255.0
            row += bytes(
                (
                    int(round(r / covered)),
                    int(round(g / covered)),
                    int(round(b / covered)),
                    int(round(a / n)),
                )
            )
        rows.append(bytes(row))
    return rows


def png(rows, size):
    """A PNG, written by hand."""

    def chunk(tag, data):
        out = struct.pack(">I", len(data)) + tag + data
        return out + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + row for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    assets = os.path.join(root, "assets")
    os.makedirs(assets, exist_ok=True)

    images = []
    for size in SIZES:
        rows = draw(size)
        images.append((size, png(rows, size)))
        if size == 32:
            with open(os.path.join(assets, "files-32.rgba"), "wb") as f:
                f.write(b"".join(rows))

    # ICONDIR, then one ICONDIRENTRY per image, then the images.
    offset = 6 + 16 * len(images)
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    for size, data in images:
        # 0 means 256 in a byte-sized field, which is the whole reason .ico
        # tops out where it does.
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)

    with open(os.path.join(assets, "files.ico"), "wb") as f:
        f.write(header + entries + b"".join(data for _, data in images))

    print("wrote assets/files.ico and assets/files-32.rgba")


if __name__ == "__main__":
    main()
