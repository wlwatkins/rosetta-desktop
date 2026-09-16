"""
Generates assets/rosetta.ico from the same design the tray icon draws at runtime:
a rounded accent tile with three "text lines", the middle one highlighted.

Written with the standard library only -- an ICO is just a directory of PNGs,
and a PNG is a handful of zlib-compressed chunks -- so the build needs no image
dependency for a file that changes about once a year.

    python tools/make_icon.py
"""

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "rosetta.ico"
SIZES = (16, 24, 32, 48, 64, 128, 256)

TILE = (0x2B, 0x53, 0x9C)
TILE_TOP = (0x3A, 0x6B, 0xC4)
LINE = (0xC8, 0xD6, 0xF2)
ACCENT = (0x5B, 0x9D, 0xFF)

# Geometry in a 32x32 reference grid, scaled per size.
INSET = 1.0
RADIUS = 7.0
BARS = (
    (10.0, 7.0, 25.0, LINE),
    (15.0, 7.0, 21.0, ACCENT),
    (20.0, 7.0, 23.0, LINE),
)
BAR_H = 3.0


def _coverage(px, py, size, supersample=4):
    """Fraction of a pixel inside the rounded tile, by supersampling."""
    scale = size / 32.0
    inset = INSET * scale
    radius = RADIUS * scale
    inner = size - inset * 2
    hits = 0
    for sy in range(supersample):
        for sx in range(supersample):
            x = px + (sx + 0.5) / supersample - inset
            y = py + (sy + 0.5) / supersample - inset
            if x < 0 or y < 0 or x >= inner or y >= inner:
                continue
            cx = min(x, inner - x)
            cy = min(y, inner - y)
            if cx < radius and cy < radius:
                dx = radius - cx
                dy = radius - cy
                if math.hypot(dx, dy) > radius:
                    continue
            hits += 1
    return hits / (supersample * supersample)


def _bar_coverage(px, py, size, top, x0, x1, supersample=4):
    scale = size / 32.0
    t, b = top * scale, (top + BAR_H) * scale
    l, r = x0 * scale, x1 * scale
    # Round the bar ends a little at larger sizes.
    hits = 0
    for sy in range(supersample):
        for sx in range(supersample):
            x = px + (sx + 0.5) / supersample
            y = py + (sy + 0.5) / supersample
            if l <= x < r and t <= y < b:
                hits += 1
    return hits / (supersample * supersample)


def render(size):
    """Straight (non-premultiplied) RGBA rows."""
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            a = _coverage(x, y, size)
            if a <= 0.0:
                row += b"\0\0\0\0"
                continue
            # A subtle vertical lift, so the tile does not read as flat.
            t = y / max(size - 1, 1)
            base = tuple(
                int(round(TILE_TOP[i] + (TILE[i] - TILE_TOP[i]) * t)) for i in range(3)
            )
            r, g, b = base
            for top, x0, x1, colour in BARS:
                cov = _bar_coverage(x, y, size, top, x0, x1)
                if cov > 0:
                    r = int(round(r + (colour[0] - r) * cov))
                    g = int(round(g + (colour[1] - g) * cov))
                    b = int(round(b + (colour[2] - b) * cov))
            row += bytes((r, g, b, int(round(a * 255))))
        rows.append(bytes(row))
    return rows


def png(size, rows):
    raw = b"".join(b"\0" + r for r in rows)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main():
    images = [(s, png(s, render(s))) for s in SIZES]

    header = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    offset = len(header) + 16 * len(images)
    for size, data in images:
        # 0 means 256 in an icon directory entry.
        dim = 0 if size >= 256 else size
        entries += struct.pack(
            "<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset
        )
        offset += len(data)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_bytes(header + entries + b"".join(d for _, d in images))
    print(f"wrote {OUT} ({OUT.stat().st_size / 1024:.1f}KB, sizes {', '.join(map(str, SIZES))})")


if __name__ == "__main__":
    main()
