#!/usr/bin/env python3
"""Generates icon.png — the add-on's square icon.

A blue microchip: a hollow rounded square with two detached, rounded "pins"
on each of the four sides, and a house silhouette nested inside the square —
signalling that this is a self-hosted, private, in-your-own-home alternative to
the vendor cloud. Rendered with pure stdlib (no Pillow/cairo): shapes are
defined as simple inside/outside predicates (rounded rects, a triangle),
supersampled for anti-aliasing, and the RGBA buffer is hand-encoded into a PNG
via zlib.

Run from anywhere: `python3 tools/make-icon.py` (re-writes the add-on's
icon.png). This generator lives outside addon/ on purpose — it is not part of
the shipped add-on.
"""
import math
import os
import struct
import zlib

SIZE = 256
SS = 4  # supersampling for anti-aliasing
BLUE = (0x21, 0x86, 0xEB)


def in_round_rect(px, py, x0, y0, x1, y1, r):
    """True if the point is inside a rounded rect (anti-aliasing via SS)."""
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    hw, hh = (x1 - x0) / 2, (y1 - y0) / 2
    qx = abs(px - cx) - (hw - r)
    qy = abs(py - cy) - (hh - r)
    ax, ay = max(qx, 0.0), max(qy, 0.0)
    return math.hypot(ax, ay) + min(max(qx, qy), 0.0) - r <= 0


def in_triangle(px, py, x0, y0, x1, y1, x2, y2):
    """True if the point is inside triangle (x0,y0)-(x1,y1)-(x2,y2)."""
    d1 = (px - x1) * (y0 - y1) - (x0 - x1) * (py - y1)
    d2 = (px - x2) * (y1 - y2) - (x1 - x2) * (py - y2)
    d3 = (px - x0) * (y2 - y0) - (x2 - x0) * (py - y0)
    has_neg = (d1 < 0) or (d2 < 0) or (d3 < 0)
    has_pos = (d1 > 0) or (d2 > 0) or (d3 > 0)
    return not (has_neg and has_pos)


# Center chip body as a hollow square (outer rect minus inner cutout).
body_t = 14  # stroke thickness
ring = ((78, 78, 178, 178, 16), (78 + body_t, 78 + body_t, 178 - body_t, 178 - body_t, 8))

# Pin geometry.
pin_t = 14          # thickness
pin_len = 36        # length
pin_r = pin_t / 2   # fully rounded caps
gap = 12            # detachment gap from body
off = 22            # offset of each pin from center along the edge

# Square edges: left=78 right=178 top=78 bottom=178, center=128.
L, R, T, B, C = 78, 178, 78, 178, 128

shapes = []
# Left pins (horizontal).
for cy in (C - off, C + off):
    x1 = L - gap
    shapes.append((x1 - pin_len, cy - pin_t / 2, x1, cy + pin_t / 2, pin_r))
# Right pins.
for cy in (C - off, C + off):
    x0 = R + gap
    shapes.append((x0, cy - pin_t / 2, x0 + pin_len, cy + pin_t / 2, pin_r))
# Top pins (vertical).
for cx in (C - off, C + off):
    y1 = T - gap
    shapes.append((cx - pin_t / 2, y1 - pin_len, cx + pin_t / 2, y1, pin_r))
# Bottom pins.
for cx in (C - off, C + off):
    y0 = B + gap
    shapes.append((cx - pin_t / 2, y0, cx + pin_t / 2, y0 + pin_len, pin_r))

# A plain house silhouette, nested in the square's hollow interior (cutout
# spans ~92..164): a roof triangle on top of a rounded body.
roof = (100, 128, 156, 128, 128, 101)      # base-left, base-right, apex
house_body = (108, 126, 148, 155, 6)       # rounded rect


def in_house(px, py):
    return in_triangle(px, py, *roof) or in_round_rect(px, py, *house_body)


def covered(px, py):
    # Hollow square: inside outer ring but not inside inner cutout.
    (ox0, oy0, ox1, oy1, orr), (ix0, iy0, ix1, iy1, irr) = ring
    if (in_round_rect(px, py, ox0, oy0, ox1, oy1, orr) and
            not in_round_rect(px, py, ix0, iy0, ix1, iy1, irr)):
        return True
    for (x0, y0, x1, y1, r) in shapes:
        if in_round_rect(px, py, x0, y0, x1, y1, r):
            return True
    return in_house(px, py)


raw = bytearray()
inv = 1.0 / SS
for y in range(SIZE):
    raw.append(0)  # filter type: none
    for x in range(SIZE):
        hits = 0
        for sy in range(SS):
            py = y + (sy + 0.5) * inv
            for sx in range(SS):
                px = x + (sx + 0.5) * inv
                if covered(px, py):
                    hits += 1
        a = int(round(hits / (SS * SS) * 255))
        raw.extend((BLUE[0], BLUE[1], BLUE[2], a))


def chunk(tag, data):
    return (struct.pack(">I", len(data)) + tag + data +
            struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff))


ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
       chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))

out = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "..", "addon", "aquilo-server", "icon.png")
with open(out, "wb") as f:
    f.write(png)
print("wrote", out, len(png), "bytes")
