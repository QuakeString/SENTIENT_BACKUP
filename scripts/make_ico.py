#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
#
# Licensed under the Apache License, Version 2.0.
"""Build a Windows-friendly multi-size icon.ico from a square SVG.

`cargo tauri icon` emits an .ico whose every entry (incl. 16-48px) is
PNG-encoded. Windows renders the window title bar from that fine, but NSIS's
installer icon and the taskbar/Start-menu shortcut want classic BMP/DIB-encoded
small sizes and otherwise show a blank/solid placeholder. This writes small
sizes as 32-bit BMP DIB and 256 as PNG — the maximally compatible layout.

Needs `resvg` on PATH to rasterize each size. Usage:
    python scripts/make_ico.py <square.svg> <out.ico>
"""
import struct
import subprocess
import sys
import tempfile
import zlib

BMP_SIZES = [16, 24, 32, 48, 64, 128]
PNG_SIZES = [256]


def render(svg, size, tmp):
    out = f"{tmp}/{size}.png"
    subprocess.run(["resvg", "--width", str(size), "--height", str(size), svg, out],
                   check=True, capture_output=True)
    return open(out, "rb").read()


def decode_rgba(png):
    assert png[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
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
    assert bd == 8 and ct == 6, f"expected 8-bit RGBA, got depth={bd} type={ct}"
    raw = zlib.decompress(idat)
    bpp, stride = 4, w * 4
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
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 255
        elif ft == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 255
        elif ft == 3:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 255
        elif ft == 4:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                c = prev[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + paeth(a, prev[i], c)) & 255
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, bytes(out)


def bmp_dib(w, h, rgba):
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    color = bytearray()
    for y in range(h - 1, -1, -1):  # bottom-up, BGRA
        for x in range(w):
            r, g, b, a = rgba[(y * w + x) * 4:(y * w + x) * 4 + 4]
            color += bytes((b, g, r, a))
    mask_stride = ((w + 31) // 32) * 4
    mask = bytearray()
    for y in range(h - 1, -1, -1):
        bits = bytearray(mask_stride)
        for x in range(w):
            if rgba[(y * w + x) * 4 + 3] < 128:  # transparent -> mask bit set
                bits[x >> 3] |= 0x80 >> (x & 7)
        mask += bits
    return header + bytes(color) + bytes(mask)


def main():
    svg, out = sys.argv[1], sys.argv[2]
    entries = []  # (w, h, data)
    with tempfile.TemporaryDirectory() as tmp:
        for s in BMP_SIZES:
            w, h, rgba = decode_rgba(render(svg, s, tmp))
            entries.append((s, s, bmp_dib(w, h, rgba)))
        for s in PNG_SIZES:
            entries.append((s, s, render(svg, s, tmp)))

    count = len(entries)
    offset = 6 + 16 * count
    directory = b""
    blob = b""
    for (w, h, data) in entries:
        directory += struct.pack("<BBBBHHII", w & 0xFF, h & 0xFF, 0, 0, 1, 32, len(data), offset)
        blob += data
        offset += len(data)
    with open(out, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, count) + directory + blob)
    print(f"wrote {out}: {count} entries ({'+'.join(str(s) for s in BMP_SIZES)} BMP, "
          f"{'+'.join(str(s) for s in PNG_SIZES)} PNG)")


if __name__ == "__main__":
    main()
