"""Regenerate every icon file in assets/ from assets/icon.svg. Needs rsvg-convert (librsvg)."""

import struct
import subprocess
from pathlib import Path

ASSETS = Path(__file__).resolve().parent.parent / "assets"
SVG = ASSETS / "icon.svg"


def render(size: int) -> bytes:
    return subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), SVG],
        check=True,
        capture_output=True,
    ).stdout


# Linux packages and the window icon.
for size in [32, 128, 256, 512]:
    (ASSETS / f"icon-{size}.png").write_bytes(render(size))

# Windows .ico, built by hand so every size is stored as compressed PNG data (14 KB)
# rather than raw pixels, which common tools do for 256px (372 KB).
sizes = [16, 24, 32, 48, 64, 128, 256]
pngs = [render(size) for size in sizes]
header = struct.pack("<HHH", 0, 1, len(sizes))
offset = len(header) + 16 * len(sizes)
entries = b""
for size, png in zip(sizes, pngs):
    dim = 0 if size == 256 else size  # the format stores 256 as 0
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
(ASSETS / "icon.ico").write_bytes(header + entries + b"".join(pngs))
