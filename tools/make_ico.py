"""Assemble les PNG de branding/ en une icone Windows (.ico).

Un .ico est un conteneur trivial : un en-tete, une entree de 16 octets par
image, puis les images bout a bout. Windows Vista et au-dela acceptent des
entrees compressees en PNG, ce qui evite d'ecrire des BMP a la main.
"""
import struct
import sys
from pathlib import Path

SIZES = [256, 128, 64, 48, 32, 24, 16]


def build(folder: Path, out: Path) -> None:
    images = []
    for size in SIZES:
        data = (folder / f"acrux-icon-{size}.png").read_bytes()
        images.append((size, data))
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = b""
    body = b""
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size == 256 else size,
            0 if size == 256 else size,
            0,
            0,
            1,
            32,
            len(data),
            offset,
        )
        body += data
        offset += len(data)
    out.write_bytes(header + entries + body)
    print("icone ecrite :", out, out.stat().st_size, "octets")


if __name__ == "__main__":
    folder = Path(sys.argv[1] if len(sys.argv) > 1 else "branding")
    build(folder, folder / "acrux.ico")
