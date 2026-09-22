"""Convertit une capture PPM du harnais en PNG, entiere ou recadree."""
import pathlib, struct, sys, zlib

src, dst = sys.argv[1], sys.argv[2]
step = int(sys.argv[3]) if len(sys.argv) > 3 else 2
crop = [int(v) for v in sys.argv[4].split(',')] if len(sys.argv) > 4 else None
data = pathlib.Path(src).read_bytes()
parts = data.split(b'\n', 3)
w, h = map(int, parts[1].split())
pix = parts[3]
x0, y0, cw, ch = crop if crop else (0, 0, w, h)
rows = []
for y in range(y0, y0 + ch, step):
    row = bytearray()
    base = y * w * 3
    for x in range(x0, x0 + cw, step):
        i = base + x * 3
        row += pix[i:i + 3]
    rows.append(bytes(row))
ow = len(rows[0]) // 3
raw = b''.join(b'\x00' + r for r in rows)
def chunk(tag, d):
    return struct.pack('>I', len(d)) + tag + d + struct.pack('>I', zlib.crc32(tag + d) & 0xffffffff)
out = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', ow, len(rows), 8, 2, 0, 0, 0))
out += chunk(b'IDAT', zlib.compress(raw, 6)) + chunk(b'IEND', b'')
pathlib.Path(dst).write_bytes(out)
print(ow, len(rows))
