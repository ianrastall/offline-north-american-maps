"""Draws the Offline US Maps application icon: a folded road map with a location pin.

Usage: python tools/make-icon.py  (writes src/OfflineMaps.App/Assets/AppIcon.ico)
"""
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src" / "OfflineMaps.App" / "Assets" / "AppIcon.ico"
S = 1024  # drawing size; scaled down for each icon size


def draw() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Three map panels, folded: alternating light and shaded paper.
    top, bottom = 200, 900
    xs = [96, 373, 650, 928]
    tilt = 60
    shades = [(236, 229, 214), (214, 205, 187), (236, 229, 214)]
    for i in range(3):
        y0 = top + (tilt if i % 2 else 0)
        y1 = top + (0 if i % 2 else tilt)
        poly = [(xs[i], y0), (xs[i + 1], y1), (xs[i + 1], bottom + (y1 - top)), (xs[i], bottom + (y0 - top))]
        d.polygon(poly, fill=shades[i], outline=(120, 110, 96), width=14)
    # Roads: a highway across and a minor road.
    d.line([(110, 700), (330, 610), (520, 690), (760, 560), (915, 600)], fill=(214, 118, 52), width=46, joint="curve")
    d.line([(450, 280), (470, 470), (430, 660), (470, 930)], fill=(160, 150, 134), width=24, joint="curve")
    # Water.
    d.ellipse([(690, 300), (850, 420)], fill=(140, 180, 206))
    # Location pin.
    cx, cy, r = 600, 360, 120
    d.polygon([(cx - r * 0.86, cy + r * 0.5), (cx + r * 0.86, cy + r * 0.5), (cx, cy + r * 2.15)], fill=(196, 52, 44))
    d.ellipse([(cx - r, cy - r), (cx + r, cy + r)], fill=(196, 52, 44))
    d.ellipse([(cx - r * 0.42, cy - r * 0.42), (cx + r * 0.42, cy + r * 0.42)], fill=(255, 255, 255))
    return img


def main() -> None:
    big = draw()
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    images = [big.resize((n, n), Image.Resampling.LANCZOS) for n in sizes]
    OUT.parent.mkdir(parents=True, exist_ok=True)
    images[-1].save(OUT, format="ICO", sizes=[(n, n) for n in sizes], append_images=images[:-1])
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
