# /// script
# dependencies = ["resvg-py"]
# ///
from pathlib import Path

from resvg_py import svg_to_bytes

assets = Path(__file__).resolve().parents[1] / "assets"
wave = (assets / "wave.svg").read_text(encoding="utf-8")
for color in ("black", "white"):
    svg = wave.replace("currentColor", color)
    for size in (16, 24, 32, 64):
        png = svg_to_bytes(svg_string=svg, width=size, height=size)
        (assets / f"wave-{color}-{size}.png").write_bytes(png)
