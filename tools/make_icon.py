#!/usr/bin/env python3
"""Builds the application icons from `assets/open.ico`.

Writes:

  assets/files.ico      the sizes Windows asks for, for the executable, the
                        Start menu, the taskbar and the installer
  assets/files-32.rgba  raw 32x32 RGBA, for the tray and window icons

Standard library only, so building the icon needs no Python packages.

Run from the repository root:

    python tools/make_icon.py
"""

import os
import struct
import zlib

SIZES = [16, 20, 24, 32, 40, 48, 64]
RGBA_SIZE = 32


def read_ico(path):
    """The largest image in an .ico, as (size, rows of RGBA bytes).

    Handles both PNG entries and 32-bit BMP entries with an alpha channel.
    """
    data = open(path, "rb").read()
    _, kind, count = struct.unpack("<HHH", data[:6])
    if kind != 1 or count == 0:
        raise ValueError(f"{path} is not an icon")
    entries = []
    for i in range(count):
        w, h, _, _, _, bpp, size, offset = struct.unpack(
            "<BBBBHHII", data[6 + 16 * i : 22 + 16 * i]
        )
        entries.append((w or 256, h or 256, bpp, data[offset : offset + size]))
    w, h, bpp, image = max(entries, key=lambda e: e[0] * e[1])
    if w != h:
        raise ValueError(f"{path}: the largest image is not square ({w}x{h})")
    if image[:4] == b"\x89PNG":
        return w, read_png(image)
    if bpp != 32:
        raise ValueError(f"{path}: BMP entries must be 32-bit, found {bpp}-bit")
    header = struct.unpack("<I", image[:4])[0]
    pixels = image[header : header + w * h * 4]
    # Bottom-up BGRA. The AND mask that follows is redundant with alpha.
    rows = []
    for y in range(h):
        src = pixels[(h - 1 - y) * w * 4 : (h - y) * w * 4]
        row = bytearray()
        for x in range(w):
            b, g, r, a = src[4 * x : 4 * x + 4]
            row += bytes((r, g, b, a))
        rows.append(bytes(row))
    return w, rows


def read_png(data):
    """Rows of RGBA bytes from an 8-bit RGBA, non-interlaced PNG."""
    pos, idat, w = 8, b"", 0
    while pos < len(data):
        length, tag = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + length]
        if tag == b"IHDR":
            w, h, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if (depth, colour, interlace) != (8, 6, 0):
                raise ValueError("only 8-bit RGBA non-interlaced PNGs are supported")
        elif tag == b"IDAT":
            idat += body
        pos += 12 + length
    raw, stride, rows, prev = zlib.decompress(idat), w * 4, [], bytes(w * 4)
    for y in range(len(raw) // (stride + 1)):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - 4] if i >= 4 else 0
            b = prev[i]
            c = prev[i - 4] if i >= 4 else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        rows.append(bytes(line))
        prev = line
    return rows


def resize(rows, src, dst):
    """Area-weighted resample on premultiplied alpha, so edges keep their colour."""
    scale = src / dst
    out = []
    for oy in range(dst):
        y0, y1 = oy * scale, (oy + 1) * scale
        row = bytearray()
        for ox in range(dst):
            x0, x1 = ox * scale, (ox + 1) * scale
            r = g = b = a = area = 0.0
            for sy in range(int(y0), min(src, int(y1 + 0.999999))):
                wy = min(y1, sy + 1) - max(y0, sy)
                if wy <= 0:
                    continue
                line = rows[sy]
                for sx in range(int(x0), min(src, int(x1 + 0.999999))):
                    wx = min(x1, sx + 1) - max(x0, sx)
                    if wx <= 0:
                        continue
                    weight = wx * wy
                    pr, pg, pb, pa = line[4 * sx : 4 * sx + 4]
                    alpha = pa / 255.0
                    r += pr * alpha * weight
                    g += pg * alpha * weight
                    b += pb * alpha * weight
                    a += pa * weight
                    area += weight
            if a == 0:
                row += bytes(4)
                continue
            coverage = a / 255.0
            row += bytes(
                (
                    min(255, round(r / coverage)),
                    min(255, round(g / coverage)),
                    min(255, round(b / coverage)),
                    min(255, round(a / area)),
                )
            )
        out.append(bytes(row))
    return out


def png(rows, size):
    """An 8-bit RGBA PNG."""

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
    size, master = read_ico(os.path.join(assets, "open.ico"))

    images = []
    for target in SIZES:
        rows = master if target == size else resize(master, size, target)
        images.append((target, png(rows, target)))
        if target == RGBA_SIZE:
            with open(os.path.join(assets, "files-32.rgba"), "wb") as f:
                f.write(b"".join(rows))

    # ICONDIR, then one ICONDIRENTRY per image, then the images.
    offset = 6 + 16 * len(images)
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    for target, data in images:
        dim = 0 if target >= 256 else target  # 0 encodes 256
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)

    with open(os.path.join(assets, "files.ico"), "wb") as f:
        f.write(header + entries + b"".join(data for _, data in images))

    print(f"wrote assets/files.ico and assets/files-32.rgba from a {size}x{size} source")


if __name__ == "__main__":
    main()
