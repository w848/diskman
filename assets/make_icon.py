"""Draw diskman's icon: a drive as a ring, orange for the used part and blue
for the free part, on the app's dark background.

Each size is drawn on its own (not shrunk from the big one) so the small
sizes Explorer and the taskbar use stay sharp.

    python assets/make_icon.py

writes assets/diskman.ico (16 to 256 px) and assets/diskman.png (256 px).
Needs Pillow (pip install pillow).
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

BG = (0x1B, 0x1F, 0x24, 255)
ORANGE = (0xF0, 0x7A, 0x3A, 255)
BLUE = (0x61, 0xAF, 0xEF, 255)
USED = 0.72  # how much of the ring is orange
SUPERSAMPLE = 8
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def draw(size: int) -> Image.Image:
    s = size * SUPERSAMPLE
    image = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(image)
    u = s / 16  # one pixel of the 16 px design

    # The tile, with corners rounded like Windows 11 icons.
    d.rounded_rectangle([0, 0, s - 1, s - 1], radius=max(2, size // 5) * SUPERSAMPLE, fill=BG)

    # The ring: the used part from the top, clockwise, then the free part.
    outer = 5.6 * u
    inner = 2.4 * u
    c = s / 2
    box = [c - outer, c - outer, c + outer, c + outer]
    split = -90 + 360 * USED
    d.pieslice(box, -90, split, fill=ORANGE)
    d.pieslice(box, split, 270, fill=BLUE)
    d.ellipse([c - inner, c - inner, c + inner, c + inner], fill=BG)

    # Gaps of even width where the colors meet; too small to see below 32 px.
    if size >= 32:
        gap = 0.55 * u
        for angle in (-90, split):
            a = math.radians(angle)
            end = (c + (outer + u) * math.cos(a), c + (outer + u) * math.sin(a))
            d.line([(c, c), end], fill=BG, width=round(gap))
        d.ellipse([c - inner, c - inner, c + inner, c + inner], fill=BG)

    return image.resize((size, size), Image.LANCZOS)


def main() -> None:
    here = Path(__file__).parent
    images = [draw(size) for size in SIZES]
    big = images[-1]
    big.save(here / "diskman.png")
    big.save(here / "diskman.ico", format="ICO", sizes=[(n, n) for n in SIZES], append_images=images[:-1])
    print(f"wrote {here / 'diskman.ico'} ({', '.join(map(str, SIZES))} px) and {here / 'diskman.png'}")


if __name__ == "__main__":
    main()
