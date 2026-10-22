"""Render every Qwen Desk icon from src/logo.svg. Requires Pillow.

Run normally to regenerate assets, or pass --check to detect stale assets.
The intentionally small SVG subset keeps the renderer dependency-free beyond Pillow.
"""
import argparse
from io import BytesIO
from math import hypot
from pathlib import Path
import xml.etree.ElementTree as ET

from PIL import Image, ImageDraw, ImageChops

ROOT = Path(__file__).resolve().parents[1]
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)

def render():
    svg = ET.parse(ROOT / "src/logo.svg").getroot()
    if svg.attrib.get("viewBox") != "0 0 512 512":
        raise ValueError("Expected the canonical 512-unit icon viewBox")
    scale = 4
    image = Image.new("RGBA", (512 * scale, 512 * scale))
    draw = ImageDraw.Draw(image)
    for element in svg:
        tag = element.tag.rsplit("}", 1)[-1]
        attr = element.attrib
        def number(key):
            return float(attr[key]) * scale
        if tag == "rect":
            x, y, width, height = (number(k) for k in ("x", "y", "width", "height"))
            draw.rounded_rectangle((x, y, x + width, y + height), radius=number("rx"), fill=attr["fill"])
        elif tag == "line":
            x1, y1, x2, y2 = (number(k) for k in ("x1", "y1", "x2", "y2"))
            length = hypot(x2 - x1, y2 - y1)
            if not length:
                raise ValueError("Icon line must have a non-zero length")
            half = number("stroke-width") / 2
            dx, dy = -(y2 - y1) / length * half, (x2 - x1) / length * half
            draw.polygon(((x1 + dx, y1 + dy), (x2 + dx, y2 + dy), (x2 - dx, y2 - dy), (x1 - dx, y1 - dy)), fill=attr["stroke"])
        else:
            raise ValueError(f"Unsupported icon element: {tag}")
    return image

def same_pixels(actual, expected):
    return actual.size == expected.size and ImageChops.difference(actual.convert("RGBA"), expected.convert("RGBA")).getbbox(alpha_only=False) is None

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    directory = ROOT / "src-tauri/icons"
    directory.mkdir(parents=True, exist_ok=True)
    image = render()
    for size in SIZES:
        target = directory / f"{size}x{size}.png"
        resized = image.resize((size, size), Image.Resampling.LANCZOS)
        if args.check:
            with Image.open(target) as actual:
                if not same_pixels(actual, resized):
                    raise SystemExit(f"Stale icon: {target}")
        else:
            resized.save(target)
    ico_bytes = BytesIO()
    image.save(ico_bytes, format="ICO", sizes=[(size, size) for size in SIZES])
    target = directory / "icon.ico"
    if args.check:
        with Image.open(target) as actual, Image.open(BytesIO(ico_bytes.getvalue())) as expected:
            if actual.ico.sizes() != expected.ico.sizes():
                raise SystemExit("The Windows icon is missing DPI sizes")
            for size in expected.ico.sizes():
                if not same_pixels(actual.ico.getimage(size), expected.ico.getimage(size)):
                    raise SystemExit(f"Stale Windows icon frame: {size}")
        print(f"Verified {len(SIZES)} PNG sizes and all ICO frames against src/logo.svg")
    else:
        target.write_bytes(ico_bytes.getvalue())
        print(f"Generated {len(SIZES)} PNG sizes and icon.ico from src/logo.svg")

if __name__ == "__main__":
    main()
