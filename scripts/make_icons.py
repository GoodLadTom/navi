"""Navi's logo (src-tauri/icons/logo-source.png, a white firefly on transparent) on a
flat lilac tile, for the app and tray icons and the sidebar. The mascot that flies in while you talk
is drawn separately in ui/js/firefly.js and is not touched here.

    python scripts/make_icons.py
    npx tauri icon src-tauri/icons/logo-tile-1024.png -o src-tauri/icons

Writes src-tauri/icons/logo-tile-1024.png (the input for `tauri icon`, which makes every app icon
size), src-tauri/icons/tray.png and ui/img/navi-logo.png. Delete the android and ios folders
`tauri icon` also creates; Navi does not use them.
"""
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'src-tauri' / 'icons' / 'logo-source.png'
TILE = (128, 100, 214)      # #8064D6: between the accent and accent ink, so white reads at 16 px
RADIUS = 0.22               # corner radius as a share of the tile
MARK = 0.70                 # the logo's width as a share of the tile
MARGIN = 0.05               # transparent space round the tile, like other taskbar icons


def mark() -> Image.Image:
    """The logo in pure white, cropped to its shape. Only its alpha is used, which drops the grey
    anti-aliasing fringe the source has round each shape."""
    src = Image.open(SOURCE).convert('RGBA')
    alpha = src.getchannel('A')
    box = alpha.point(lambda v: 255 if v > 8 else 0).getbbox()
    alpha = alpha.crop(box)
    white = Image.new('RGBA', alpha.size, (255, 255, 255, 0))
    white.putalpha(alpha)
    return white


def tile(size: int, margin: float = MARGIN) -> Image.Image:
    """A size x size image: the rounded lilac tile with the logo centred on it."""
    big = size * 4  # drawn large and scaled down for smooth edges
    out = Image.new('RGBA', (big, big), (0, 0, 0, 0))
    inset = round(big * margin)
    side = big - 2 * inset
    ImageDraw.Draw(out).rounded_rectangle((inset, inset, inset + side - 1, inset + side - 1), radius=round(side * RADIUS), fill=TILE + (255,))
    m = mark()
    w = round(side * MARK)
    h = round(m.height * w / m.width)
    m = m.resize((w, h), Image.LANCZOS)
    # The firefly's body sits low in its shape, so it is nudged up a little to look centred.
    out.alpha_composite(m, (inset + (side - w) // 2, inset + (side - h) // 2 - round(side * 0.02)))
    return out.resize((size, size), Image.LANCZOS)


def contrast(a, b) -> float:
    def lum(c):
        ch = [v / 255 for v in c]
        ch = [v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4 for v in ch]
        return 0.2126 * ch[0] + 0.7152 * ch[1] + 0.0722 * ch[2]
    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


if __name__ == '__main__':
    tile(1024).save(ROOT / 'src-tauri' / 'icons' / 'logo-tile-1024.png')
    tile(256, margin=0.0).save(ROOT / 'src-tauri' / 'icons' / 'tray.png')
    tile(128, margin=0.0).save(ROOT / 'ui' / 'img' / 'navi-logo.png')
    print(f'white on the tile: {contrast((255, 255, 255), TILE):.2f}:1')
