"""Write a one-colour PNG (default 4×4): `solid-png.py OUT RRGGBB [W H]`.

Used by build-iso.sh for the boot menu's selection pill (GRUB themes need
a picture for it). Standard library only.
"""
import struct
import sys
import zlib

out, hexcol = sys.argv[1], sys.argv[2]
w, h = (int(sys.argv[3]), int(sys.argv[4])) if len(sys.argv) > 4 else (4, 4)
r, g, b = (int(hexcol[i : i + 2], 16) for i in (0, 2, 4))
raw = b"".join(b"\x00" + bytes((r, g, b)) * w for _ in range(h))


def chunk(kind: bytes, data: bytes) -> bytes:
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


png = (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw))
    + chunk(b"IEND", b"")
)
with open(out, "wb") as f:
    f.write(png)
