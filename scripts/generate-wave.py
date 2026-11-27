# /// script
# dependencies = ["resvg-py==0.5.0"]
# ///
from pathlib import Path
from struct import Struct, pack

from resvg_py import svg_to_bytes

assets = Path(__file__).resolve().parents[1] / "assets"
wave = (assets / "wave.svg").read_text(encoding="utf-8")
for color in ("black", "white"):
    svg = wave.replace("currentColor", color)
    for size in (16, 24, 32, 64):
        png = svg_to_bytes(svg_string=svg, width=size, height=size)
        (assets / f"wave-{color}-{size}.png").write_bytes(png)

frames = []
for source, sizes in (("tide-small.svg", (16, 20, 24)), ("tide.svg", (32, 40, 48, 64, 256))):
    svg = (assets / source).read_text(encoding="utf-8")
    for size in sizes:
        frames.append((size, svg_to_bytes(svg_string=svg, width=size, height=size)))

header = pack("<HHH", 0, 1, len(frames))
entry = Struct("<BBBBHHII")
offset = len(header) + entry.size * len(frames)
directory = bytearray()
for size, png in frames:
    dimension = 0 if size == 256 else size
    directory.extend(entry.pack(dimension, dimension, 0, 0, 1, 32, len(png), offset))
    offset += len(png)

(assets / "tide.ico").write_bytes(header + directory + b"".join(png for _, png in frames))
