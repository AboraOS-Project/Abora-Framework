#!/usr/bin/env python3
"""Convert a PNG into the raw logo format read by abora-boot.

Usage: logo-to-raw.py in.png out.rgba

Output: b"ABR1", width and height as little-endian u32, then RGBA8 pixels.
Supports non-interlaced 8-bit greyscale, greyscale+alpha, RGB and RGBA PNGs,
which covers the artwork this project ships; anything else is refused. Only
the standard library is needed so ISO builds do not depend on image tools.
"""
import struct
import sys
import zlib


def decode(data: bytes):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("not a PNG file")
    pos, idat, header = 8, [], None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"IEND":
            break
    if header is None:
        raise SystemExit("PNG has no header")
    width, height, depth, color, _, _, interlace = header
    channels = {0: 1, 4: 2, 2: 3, 6: 4}.get(color)
    if depth != 8 or interlace != 0 or channels is None:
        raise SystemExit(f"unsupported PNG (depth {depth}, colour type {color}, interlace {interlace}); use 8-bit non-interlaced")
    raw = zlib.decompress(b"".join(idat))
    stride = width * channels
    rows, prev = [], bytearray(stride)
    i = 0
    for _ in range(height):
        ftype, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = line[x - channels] if x >= channels else 0
            b = prev[x]
            c = prev[x - channels] if x >= channels else 0
            if ftype == 1:
                line[x] = (line[x] + a) & 255
            elif ftype == 2:
                line[x] = (line[x] + b) & 255
            elif ftype == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif ftype == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
            elif ftype != 0:
                raise SystemExit(f"bad PNG filter {ftype}")
        rows.append(line)
        prev = line
    out = bytearray()
    for line in rows:
        for x in range(width):
            px = line[x * channels:(x + 1) * channels]
            if channels == 1:
                out += bytes((px[0], px[0], px[0], 255))
            elif channels == 2:
                out += bytes((px[0], px[0], px[0], px[1]))
            elif channels == 3:
                out += bytes((*px, 255))
            else:
                out += bytes(px)
    return width, height, bytes(out)


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    width, height, rgba = decode(open(sys.argv[1], "rb").read())
    with open(sys.argv[2], "wb") as f:
        f.write(b"ABR1" + struct.pack("<II", width, height) + rgba)
    print(f"logo: {width}x{height} -> {sys.argv[2]}")


if __name__ == "__main__":
    main()
