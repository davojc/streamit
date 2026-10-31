"""Draws StreamIt's icon (crates/streamit-app/assets/icon/streamit.svg) with Pillow:
streamit.ico (16-256 px, for the exe) and streamit.png (256 px, for the window).

    python tools/make_icon.py

The shapes are the SVG's, in its 256-unit space, drawn 4x larger and scaled down so the
edges are smooth.
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

SCALE = 4
OUT = Path(__file__).resolve().parent.parent / "crates" / "streamit-app" / "assets" / "icon"
BLUE = (0x25, 0x63, 0xEB, 255)
WHITE = (255, 255, 255, 255)
WAVE = (0x9C, 0xC2, 0xFF, 255)
RED = (0xE5, 0x48, 0x4D, 255)
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def s(v):
    return v * SCALE


def dot(d, x, y, r, fill):
    d.ellipse([s(x - r), s(y - r), s(x + r), s(y + r)], fill=fill)


def wave(d, x0, y0, y1, r, width, fill):
    """The SVG arc from (x0, y0) to (x0, y1), radius r, bulging right, round caps."""
    half = (y1 - y0) / 2
    cx, cy = x0 - math.sqrt(r * r - half * half), y0 + half
    a = math.degrees(math.atan2(-half, x0 - cx))
    outer = r + width / 2
    d.arc([s(cx - outer), s(cy - outer), s(cx + outer), s(cy + outer)], a, -a, fill=fill, width=s(width))
    dot(d, x0, y0, width / 2, fill)
    dot(d, x0, y1, width / 2, fill)


def draw():
    img = Image.new("RGBA", (s(256), s(256)), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([s(8), s(8), s(248), s(248)], radius=s(52), fill=BLUE)
    # The play mark, its corners rounded by a 10-unit round-joined stroke.
    tri = [(74, 78), (74, 178), (150, 128)]
    d.polygon([(s(x), s(y)) for x, y in tri], fill=WHITE)
    for (x0, y0), (x1, y1) in zip(tri, tri[1:] + tri[:1]):
        d.line([s(x0), s(y0), s(x1), s(y1)], fill=WHITE, width=s(10))
    for x, y in tri:
        dot(d, x, y, 5, WHITE)
    wave(d, 172, 92, 164, 52, 16, WHITE)
    wave(d, 196, 70, 186, 82, 16, WAVE)
    # The live dot, ringed in the tile's blue.
    dot(d, 206, 50, 24, BLUE)
    dot(d, 206, 50, 16, RED)
    return img


def main():
    big = draw()
    sizes = [big.resize((n, n), Image.LANCZOS) for n in SIZES]
    sizes[-1].save(OUT / "streamit.png")
    sizes[-1].save(OUT / "streamit.ico", sizes=[(n, n) for n in SIZES], append_images=sizes[:-1])
    print(f"wrote {OUT / 'streamit.ico'} and {OUT / 'streamit.png'}")


if __name__ == "__main__":
    main()
