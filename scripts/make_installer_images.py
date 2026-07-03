#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
#
# Licensed under the Apache License, Version 2.0.
"""Generate the NSIS installer branding bitmaps from the app logo.

NSIS (MUI2) wants 24-bit BMPs:
  - sidebar (welcome/finish panel): 164 x 314
  - header  (inner-page banner):    150 x 57

Renders the SVG at the needed size with resvg, alpha-composites it over a solid
brand background, and writes a bottom-up 24-bit BMP. Output committed under
src-tauri/installer/ and referenced from tauri.conf.json.

Usage:  python scripts/make_installer_images.py <icon.svg>
"""
import os
import struct
import subprocess
import sys
import tempfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "src-tauri", "installer")
TEAL = (0x3A, 0x5B, 0x63)   # --accent2, matches the app header
WHITE = (0xFF, 0xFF, 0xFF)


def render(svg, size, tmp):
    out = os.path.join(tmp, f"{size}.png")
    subprocess.run(["resvg", "--width", str(size), "--height", str(size), svg, out],
                   check=True, capture_output=True)
    return decode_rgba(open(out, "rb").read())


def decode_rgba(png):
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    pos, w, h, bd, ct, idat = 8, None, None, None, None, b""
    while pos < len(png):
        (ln,) = struct.unpack(">I", png[pos:pos + 4])
        typ = png[pos + 4:pos + 8]
        chunk = png[pos + 8:pos + 8 + ln]
        pos += 12 + ln
        if typ == b"IHDR":
            w, h, bd, ct = struct.unpack(">IIBB", chunk[:10])
        elif typ == b"IDAT":
            idat += chunk
        elif typ == b"IEND":
            break
    assert bd == 8 and ct == 6
    raw = zlib.decompress(idat)
    stride = w * 4
    out = bytearray(h * stride)
    prev = bytearray(stride)
    p = 0

    def paeth(a, b, c):
        q = a + b - c
        pa, pb, pc = abs(q - a), abs(q - b), abs(q - c)
        return a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)

    for y in range(h):
        ft = raw[p]; p += 1
        line = bytearray(raw[p:p + stride]); p += stride
        if ft == 1:
            for i in range(4, stride):
                line[i] = (line[i] + line[i - 4]) & 255
        elif ft == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 255
        elif ft == 3:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 255
        elif ft == 4:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                c = prev[i - 4] if i >= 4 else 0
                line[i] = (line[i] + paeth(a, prev[i], c)) & 255
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, bytes(out)


def canvas(w, h, bg):
    return [bg] * (w * h)


def paste(px, cw, ch, logo, lw, lh, ox, oy):
    for ly in range(lh):
        for lx in range(lw):
            r, g, b, a = logo[(ly * lw + lx) * 4:(ly * lw + lx) * 4 + 4]
            cx, cy = ox + lx, oy + ly
            if 0 <= cx < cw and 0 <= cy < ch:
                br, bgc, bb = px[cy * cw + cx]
                px[cy * cw + cx] = (
                    (r * a + br * (255 - a)) // 255,
                    (g * a + bgc * (255 - a)) // 255,
                    (b * a + bb * (255 - a)) // 255,
                )


def write_bmp24(path, w, h, px):
    stride = (w * 3 + 3) & ~3
    body = bytearray()
    for y in range(h - 1, -1, -1):  # bottom-up
        row = bytearray()
        for x in range(w):
            r, g, b = px[y * w + x]
            row += bytes((b, g, r))
        row += b"\x00" * (stride - w * 3)
        body += row
    fileheader = b"BM" + struct.pack("<IHHI", 14 + 40 + len(body), 0, 0, 14 + 40)
    infoheader = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(body), 2835, 2835, 0, 0)
    open(path, "wb").write(fileheader + infoheader + body)
    print(f"  {os.path.basename(path)}: {w}x{h}")


def main():
    svg = sys.argv[1]
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        # sidebar 164x314 — teal panel, logo centered upper
        lw, lh, logo = render(svg, 132, tmp)
        px = canvas(164, 314, TEAL)
        paste(px, 164, 314, logo, lw, lh, (164 - lw) // 2, 46)
        write_bmp24(os.path.join(OUT, "sidebar.bmp"), 164, 314, px)

        # header 150x57 — white, small logo right-aligned
        hw, hh, hlogo = render(svg, 48, tmp)
        px = canvas(150, 57, WHITE)
        paste(px, 150, 57, hlogo, hw, hh, 150 - hw - 6, (57 - hh) // 2)
        write_bmp24(os.path.join(OUT, "header.bmp"), 150, 57, px)


if __name__ == "__main__":
    main()
