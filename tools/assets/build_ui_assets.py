#!/usr/bin/env python3
"""Build the UI v3 coverage-mask atlases from Open Sans and drawn vector shapes.

Outputs (one byte per texel, sampled by the renderer as luminance-alpha):

  assets/fonts/reborn-regular.a8   1024-wide page: Regular/Light exact-size glyph sets
  assets/fonts/reborn-semibold.a8  1024-wide page: SemiBold exact-size glyph sets
  assets/fonts/reborn-fallback.a8  1024-wide page: every other supported glyph at 24 px
  assets/icons/reborn-masks.a8     shapes, gradients, ring arcs and icons
  crates/reborn-ui/src/fontdata.rs and maskdata.rs   the matching metrics/rectangles

Glyphs are rendered once per (weight, pixel size), so the renderer draws every
character 1:1 on the pixel grid instead of scaling one bitmap. Open Sans is
licensed under the SIL OFL 1.1 (assets/fonts/OpenSans-OFL.txt).

Usage: build_ui_assets.py [--fonts DIR]    (default /usr/share/fonts/TTF)
"""
import argparse
import hashlib
import json
import math
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
SS = 8  # supersampling for drawn shapes

# --------------------------------------------------------------------------- fonts

ASCII = [c for c in range(32, 127)]
LATIN1 = list(range(160, 256))
LATIN_A = list(range(0x100, 0x180))
PUNCT = [0x2022, 0x2026, 0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0xB7, 0x203A, 0x2039, 0x20AC]
CORE = sorted(set(ASCII + LATIN1 + LATIN_A + PUNCT))
CORE_NARROW = sorted(set(ASCII + LATIN1 + [0x2022, 0x2026, 0x2013, 0x2014, 0x2019]))
CAPS = [ord(c) for c in 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789#']
WORDMARK = [ord(c) for c in 'reborn']
# Everything the former DejaVu atlas covered.
ALL = ([c for c in range(0x20, 0x250) if not 0x7F <= c <= 0x9F] + list(range(0x370, 0x500))
       + [0x2026, 0x2022, 0x2013, 0x2014, 0x2190, 0x2191, 0x2192, 0x2193, 0xFFFD])

# name, page, file weight, pixel size, charset
SETS = [
    ('R11', 0, 'Regular', 11, CORE),
    ('R13', 0, 'Regular', 13, CORE),
    ('R15', 0, 'Regular', 15, CORE),
    ('R17', 0, 'Regular', 17, CORE),
    ('L23', 0, 'Light', 23, CORE_NARROW),
    ('L44', 0, 'Light', 44, WORDMARK),
    ('S10', 1, 'SemiBold', 10, CORE_NARROW),
    ('S13', 1, 'SemiBold', 13, CORE),
    ('S15', 1, 'SemiBold', 15, CORE),
    ('S17', 1, 'SemiBold', 17, CORE),
    ('S20', 1, 'SemiBold', 20, CORE_NARROW),
    ('S25', 1, 'SemiBold', 25, CORE_NARROW),
    ('S28', 1, 'SemiBold', 28, ASCII),
    ('S36', 1, 'SemiBold', 36, CAPS),
]
PAGE_W = 1024


def load_font(fonts: Path, weight: str, px: float):
    return ImageFont.truetype(str(fonts / f'OpenSans-{weight}.ttf'), px)


def glyph_image(fonts, weight, px, ch, size, origin, ss):
    """Render `ch` at `px` (supersampled by ss) into a size (w, h) mask, baseline at origin."""
    font = load_font(fonts, weight, px * ss)
    img = Image.new('L', (size[0] * ss, size[1] * ss), 0)
    ImageDraw.Draw(img).text((origin[0] * ss, origin[1] * ss), chr(ch), font=font, fill=255, anchor='ls')
    return img.reduce(ss) if ss > 1 else img


def build_set(fonts, weight, px, chars, ss):
    """Return (cell_w, cell_h, ascent, pad, [(ch, mask, advance64)])."""
    chars = sorted(set(chars))
    probe = load_font(fonts, weight, px * 8)
    lo = hi = top = bot = 0
    for ch in chars:
        l, t, r, b = probe.getbbox(chr(ch), anchor='ls')
        lo, hi = min(lo, l / 8), max(hi, r / 8)
        top, bot = min(top, t / 8), max(bot, b / 8)
    pad = int(math.ceil(-lo)) + 1
    ascent = int(math.ceil(-top)) + 1
    cell_w = pad + int(math.ceil(hi)) + 2
    cell_h = ascent + int(math.ceil(bot)) + 2
    adv_font = load_font(fonts, weight, 2048)
    glyphs = []
    for ch in chars:
        mask = glyph_image(fonts, weight, px, ch, (cell_w, cell_h), (pad, ascent), ss)
        advance = adv_font.getlength(chr(ch)) / 2048 * px
        glyphs.append((ch, mask, int(round(advance * 64))))
    return cell_w, cell_h, ascent, pad, glyphs


def pack_page(placements, width=PAGE_W):
    """Shelf-pack [(name, [masks...])] sets; returns (page image, {set: [(x, y)]})."""
    y = 0
    positions = {}
    out = []
    for name, cell_w, cell_h, masks in placements:
        cols = width // cell_w
        rows = -(-len(masks) // cols)
        pos = []
        for i, mask in enumerate(masks):
            x0, y0 = (i % cols) * cell_w, y + (i // cols) * cell_h
            pos.append((x0, y0))
            out.append((mask, x0, y0))
        positions[name] = pos
        y += rows * cell_h
    height = (y + 7) // 8 * 8
    page = Image.new('L', (width, height), 0)
    for mask, x0, y0 in out:
        page.paste(mask, (x0, y0))
    return page, positions


def build_fonts(fonts: Path, ss: int):
    pages = {0: [], 1: []}
    meta = {}
    for name, page, weight, px, chars in SETS:
        cell_w, cell_h, ascent, pad, glyphs = build_set(fonts, weight, px, chars, ss)
        pages[page].append((name, cell_w, cell_h, [g[1] for g in glyphs]))
        meta[name] = dict(page=page, size=px, cell_w=cell_w, cell_h=cell_h, ascent=ascent, pad=pad,
                          glyphs=[(g[0], g[2]) for g in glyphs], weight=weight)
    images, layout = {}, {}
    for page in (0, 1):
        images[page], positions = pack_page(pages[page])
        layout.update(positions)
    # Fallback: everything the core sets lack, at 24 px in 32 px cells.
    rest = sorted(set(ALL) | set(CORE))
    fb_font = ('Regular', 24)
    fb_adv = load_font(fonts, 'Regular', 2048)
    fb_masks, fb_meta = [], []
    for ch in rest:
        fb_masks.append(glyph_image(fonts, fb_font[0], fb_font[1], ch, (32, 32), (2, 25), ss))
        fb_meta.append((ch, int(round(fb_adv.getlength(chr(ch)) / 2048 * 24 * 64))))
    images[2], positions = pack_page([('FB', 32, 32, fb_masks)])
    layout['FB'] = positions['FB']
    return images, meta, layout, fb_meta


# --------------------------------------------------------------------------- masks

class Sheet:
    """Shelf packer for the mask atlas."""

    def __init__(self, width=512):
        self.width = width
        self.x = self.y = self.row = 0
        self.items = []

    def add(self, name, img):
        w, h = img.size
        if self.x + w > self.width:
            self.x, self.y, self.row = 0, self.y + self.row, 0
        self.items.append((name, img, self.x, self.y))
        self.x += w + 1
        self.row = max(self.row, h + 1)

    def finish(self):
        height = (self.y + self.row + 7) // 8 * 8
        sheet = Image.new('L', (self.width, height), 0)
        rects = {}
        for name, img, x, y in self.items:
            sheet.paste(img, (x, y))
            rects[name] = (x, y, img.size[0], img.size[1])
        return sheet, rects


class Pen:
    """Draw in a w x h cell with float coordinates, supersampled."""

    def __init__(self, w, h=None):
        h = h or w
        self.w, self.h = w, h
        self.img = Image.new('L', (w * SS, h * SS), 0)
        self.d = ImageDraw.Draw(self.img)

    def p(self, v):
        return v * SS

    def line(self, pts, width=2.0, cap=True):
        pts = [(self.p(x), self.p(y)) for x, y in pts]
        self.d.line(pts, fill=255, width=int(self.p(width)), joint='curve')
        if cap:
            r = self.p(width) / 2
            for x, y in (pts[0], pts[-1]):
                self.d.ellipse([x - r, y - r, x + r, y + r], fill=255)

    def poly(self, pts):
        self.d.polygon([(self.p(x), self.p(y)) for x, y in pts], fill=255)

    def rrect(self, x0, y0, x1, y1, r, fill=255, outline=None, width=1.0):
        self.d.rounded_rectangle([self.p(x0), self.p(y0), self.p(x1), self.p(y1)], self.p(r), fill=fill,
                                 outline=255 if outline else None, width=int(self.p(width)))

    def ellipse(self, cx, cy, r, fill=255, outline=False, width=1.0):
        self.d.ellipse([self.p(cx - r), self.p(cy - r), self.p(cx + r), self.p(cy + r)],
                       fill=None if outline else fill, outline=255 if outline else None,
                       width=int(self.p(width)))

    def arc(self, cx, cy, r, a0, a1, width=2.0):
        self.d.arc([self.p(cx - r), self.p(cy - r), self.p(cx + r), self.p(cy + r)], a0, a1, fill=255,
                   width=int(self.p(width)))

    def erase_rrect(self, x0, y0, x1, y1, r):
        self.d.rounded_rectangle([self.p(x0), self.p(y0), self.p(x1), self.p(y1)], self.p(r), fill=0)

    def out(self):
        return self.img.reduce(SS)


def shape_disc(n=64):
    pen = Pen(n)
    pen.ellipse(n / 2, n / 2, n / 2 - 0.02)
    return pen.out()


def shape_ring(n, stroke):
    pen = Pen(n)
    pen.ellipse(n / 2, n / 2, n / 2 - 0.02, outline=True, width=stroke)
    return pen.out()


def shape_glow(n=64):
    img = Image.new('L', (n, n))
    px = img.load()
    for y in range(n):
        for x in range(n):
            r = math.hypot(x + 0.5 - n / 2, y + 0.5 - n / 2) / (n / 2)
            t = max(0.0, 1 - r)
            px[x, y] = int(255 * t * t * (3 - 2 * t))
    return img


def shape_ramp(w, h, vertical, reverse):
    img = Image.new('L', (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            t = (y + 0.5) / h if vertical else (x + 0.5) / w
            if reverse:
                t = 1 - t
            s = 1 - t
            px[x, y] = int(255 * s * s * (3 - 2 * s))
    return img


def shape_arc(n, stroke, turn):
    """A clockwise arc from 12 o'clock covering `turn` (0..1) of the circle, round caps."""
    pen = Pen(n)
    r = n / 2 - stroke / 2 - 0.5
    cx = cy = n / 2
    a0, a1 = -90, -90 + 360 * turn
    pen.arc(cx, cy, r, a0, a1, stroke)
    for a in (a0, a1):
        x, y = cx + r * math.cos(math.radians(a)), cy + r * math.sin(math.radians(a))
        pen.ellipse(x, y, stroke / 2)
    return pen.out()


# Icons: each draws into a Pen of its own size, white on transparent.

def ic_back(p):
    p.line([(20, 6), (10, 16), (20, 26)], 3.0)


def ic_chevron(p):
    p.line([(11, 7), (19, 16), (11, 25)], 2.8)


def ic_check(p):
    p.line([(7, 17), (13, 23), (25, 9)], 3.2)


def ic_close(p):
    p.line([(9, 9), (23, 23)], 3.0)
    p.line([(23, 9), (9, 23)], 3.0)


def ic_play(p, s):
    k = s / 32
    p.poly([(10 * k, 5 * k), (10 * k, 27 * k), (27 * k, 16 * k)])


def ic_pause(p, s):
    k = s / 32
    p.rrect(8 * k, 6 * k, 14 * k, 26 * k, 1.5 * k)
    p.rrect(18 * k, 6 * k, 24 * k, 26 * k, 1.5 * k)


def ic_prev(p, s):
    k = s / 32
    p.poly([(26 * k, 7 * k), (26 * k, 25 * k), (11 * k, 16 * k)])
    p.rrect(6 * k, 7 * k, 9.4 * k, 25 * k, 1.2 * k)


def ic_next(p, s):
    k = s / 32
    p.poly([(6 * k, 7 * k), (6 * k, 25 * k), (21 * k, 16 * k)])
    p.rrect(22.6 * k, 7 * k, 26 * k, 25 * k, 1.2 * k)


def arrow(p, tip, d, size=3.6):
    x, y = tip
    dx, dy = d
    px, py = -dy, dx
    p.poly([(x, y), (x - dx * size + px * size, y - dy * size + py * size),
            (x - dx * size - px * size, y - dy * size - py * size)])


def ic_shuffle(p):
    p.line([(4, 10), (10, 10), (20, 22), (24, 22)], 2.2)
    p.line([(4, 22), (10, 22), (20, 10), (24, 10)], 2.2)
    arrow(p, (28, 10), (1, 0))
    arrow(p, (28, 22), (1, 0))


def ic_repeat(p):
    p.arc(16, 16, 9.5, 205, 335, 2.2)
    p.arc(16, 16, 9.5, 25, 155, 2.2)
    arrow(p, (26.5, 12), (0, 1), 3.4)
    arrow(p, (5.5, 20), (0, -1), 3.4)


def ic_repeat_one(p):
    ic_repeat(p)
    p.line([(14.4, 13.2), (16.6, 12), (16.6, 20)], 1.8)


def ic_gapless(p):
    p.rrect(5, 11, 14, 21, 2.2)
    p.rrect(18, 11, 27, 21, 2.2)


def ic_eq(p):
    for x, ky in ((9, 12), (16, 21), (23, 14)):
        p.line([(x, 6), (x, 26)], 1.8)
        p.ellipse(x, ky, 3.0)


def ic_norm(p):
    for i, h in enumerate((5, 10, 16, 10, 5)):
        x = 7 + i * 4.4
        p.rrect(x, 16 - h / 2, x + 2.4, 16 + h / 2, 1.2)


def ic_bluetooth(p):
    p.line([(9, 11), (22, 21.5), (16, 27), (16, 5), (22, 10.5), (9, 21)], 2.1)


def ic_wifi(p):
    for r in (5, 10, 15):
        p.arc(16, 25, r, 232, 308, 2.4)
    p.ellipse(16, 25, 2.1)


def ic_lock(p):
    p.rrect(7, 14, 25, 27, 3)
    p.arc(16, 13, 6, 180, 360, 2.6)
    p.line([(10, 13), (10, 14)], 2.6, cap=False)
    p.line([(22, 13), (22, 14)], 2.6, cap=False)


def ic_bolt(p):
    p.poly([(18, 3), (7, 18), (14, 18), (12, 29), (25, 12), (17.5, 12)])


def ic_folder(p):
    p.rrect(3, 8, 29, 27, 3.5)
    p.rrect(3, 5, 14, 12, 2.5)


def ic_note(p):
    p.ellipse(10.5, 24, 4.4)
    p.line([(14.4, 24), (14.4, 5)], 2.4, cap=False)
    p.poly([(14.4, 5), (25, 8.5), (25, 14), (14.4, 10.5)])


def ic_speaker(p):
    p.poly([(5, 12), (11, 12), (18, 6), (18, 26), (11, 20), (5, 20)])
    p.arc(18, 16, 6.5, -48, 48, 2.2)
    p.arc(18, 16, 11.5, -45, 45, 2.2)


def ic_headphones(p):
    p.arc(16, 17, 10, 180, 360, 2.6)
    p.line([(6, 17), (6, 23)], 2.6)
    p.line([(26, 17), (26, 23)], 2.6)
    p.rrect(4, 19, 10, 27, 2.5)
    p.rrect(22, 19, 28, 27, 2.5)


def ic_sd(p):
    p.poly([(8, 3), (21, 3), (26, 8), (26, 29), (8, 29)])
    for x in (12, 16, 20):
        p.erase_rrect(x - 0.9, 6, x + 0.9, 11, 0.8)


def ic_usb(p):
    p.line([(16, 27), (16, 6)], 2.2)
    p.line([(16, 20), (9, 14), (9, 11)], 2.0)
    p.line([(16, 17), (23, 12), (23, 10)], 2.0)
    p.ellipse(9, 10, 2.2)
    p.rrect(21, 6, 25, 10, 0.8)
    p.poly([(16, 2), (12.2, 8), (19.8, 8)])
    p.ellipse(16, 27, 2.6)


def ic_warning(p):
    p.poly([(16, 4), (30, 27), (2, 27)])
    p.erase_rrect(14.9, 12, 17.1, 20, 1.1)
    p.d.ellipse([p.p(14.7), p.p(21.4), p.p(17.3), p.p(24)], fill=0)


def ic_info(p):
    p.ellipse(16, 16, 12.5)
    p.d.ellipse([p.p(14.6), p.p(8.4), p.p(17.4), p.p(11.2)], fill=0)
    p.erase_rrect(14.8, 13, 17.2, 23.5, 1.2)


def ic_sun(p):
    p.ellipse(16, 16, 5.5)
    for a in range(0, 360, 45):
        c, s = math.cos(math.radians(a)), math.sin(math.radians(a))
        p.line([(16 + 9.5 * c, 16 + 9.5 * s), (16 + 13 * c, 16 + 13 * s)], 2.2)


def ic_drive(p):
    p.rrect(3, 9, 29, 24, 4)
    p.erase_rrect(7, 18.5, 18, 20.5, 1)
    p.ellipse(24, 19.5, 1.6, fill=0)


def ic_power(p):
    p.arc(16, 17, 9.5, -52, 232, 2.8)
    p.line([(16, 4), (16, 15)], 2.8)


def ic_download(p):
    p.line([(16, 5), (16, 20)], 2.8)
    p.line([(9, 14), (16, 21), (23, 14)], 2.8)
    p.line([(6, 26), (26, 26)], 2.8)


def ic_refresh(p):
    p.arc(16, 16, 9.5, 20, 300, 2.6)
    arrow(p, (22.6, 5.6), (1, 0.4), 4.2)


def ic_search(p):
    p.ellipse(14, 14, 8.5, outline=True, width=2.6)
    p.line([(20.5, 20.5), (27, 27)], 3.0)


def ic_disc(p):
    p.ellipse(16, 16, 12, outline=True, width=2.2)
    p.ellipse(16, 16, 3.5)


def ic_crossfade(p):
    p.ellipse(12, 16, 8.5, outline=True, width=2.2)
    p.ellipse(20, 16, 8.5, outline=True, width=2.2)


def ic_person(p):
    p.ellipse(16, 11, 6)
    p.arc(16, 30, 12, 195, 345, 5)


# Keyboard keys (Beta 5): Shift (outline; filled while on), Delete, Show/Hide.
SHIFT_ARROW = [(16, 5), (27, 17), (21, 17), (21, 27), (11, 27), (11, 17), (5, 17), (16, 5)]


def ic_shift(p):
    p.line(SHIFT_ARROW, 2.4)


def ic_shift_on(p):
    p.poly(SHIFT_ARROW[:-1])


def ic_backspace(p):
    p.line([(11, 7), (28, 7), (28, 25), (11, 25), (3, 16), (11, 7)], 2.2)
    p.line([(15, 12), (23, 20)], 2.2)
    p.line([(23, 12), (15, 20)], 2.2)


def ic_eye(p):
    p.arc(16, 33, 21, 230, 310, 2.4)
    p.arc(16, 1, 21, 50, 130, 2.4)
    p.ellipse(16, 17, 3.6)


def ic_eye_off(p):
    ic_eye(p)
    p.line([(6, 27), (26, 7)], 2.4)


# Radio (Beta 5.1): favourite stars, countries, popular, a live broadcast.
def star_points(cx=16, cy=16.8, outer=12.5, inner=5.2):
    pts = []
    for k in range(10):
        r = outer if k % 2 == 0 else inner
        a = math.radians(-90 + 36 * k)
        pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    return pts


def ic_star(p):
    pts = star_points()
    p.line(pts + [pts[0]], 2.0)


def ic_star_on(p):
    p.poly(star_points())


def ic_globe(p):
    p.ellipse(16, 16, 11.5, outline=True, width=2.2)
    p.line([(4.8, 16), (27.2, 16)], 2.0)
    # The meridian: a lens of two arcs.
    p.arc(31, 16, 19, 143, 217, 2.0)
    p.arc(1, 16, 19, 323, 37, 2.0)


def ic_trend(p):
    p.line([(5, 23), (12, 15.5), (17, 20), (26, 10)], 2.6)
    arrow(p, (27.5, 8.5), (0.707, -0.707), 4.2)


def ic_broadcast(p):
    p.ellipse(16, 16, 3.2)
    for r in (7.5, 12.5):
        p.arc(16, 16, r, -42, 42, 2.4)
        p.arc(16, 16, r, 138, 222, 2.4)


# Playlists: Favorites' heart (outline, filled) and New Playlist's plus.
def heart_points(n=48):
    pts = []
    for k in range(n):
        t = 2 * math.pi * k / n
        x = 16 * math.sin(t) ** 3
        y = 13 * math.cos(t) - 5 * math.cos(2 * t) - 2 * math.cos(3 * t) - math.cos(4 * t)
        pts.append((16 + x * 0.72, 15.5 - y * 0.72))
    return pts


def ic_heart(p):
    pts = heart_points()
    p.line(pts + [pts[0]], 2.2)


def ic_heart_on(p):
    p.poly(heart_points())


def ic_plus(p):
    p.line([(16, 6), (16, 26)], 2.8)
    p.line([(6, 16), (26, 16)], 2.8)


# Settings → Skin (Beta 7.5): a painter's palette with its thumb hole and paints.
def ic_palette(p):
    p.ellipse(16, 16, 12.5)
    p.ellipse(24.2, 24.2, 5.4, fill=0)
    p.ellipse(19.6, 21.2, 2.3, fill=0)
    for x, y in ((9.6, 14.2), (13.6, 8.6), (20.4, 8.8), (9.8, 20.6)):
        p.ellipse(x, y, 2.1, fill=0)


# Settings → Customize (Beta 8.18): three sliders with their knobs.
def ic_sliders(p):
    for y, knob in ((9, 20), (16, 11), (23, 18)):
        p.line([(5, y), (27, y)], 2.2)
        p.ellipse(knob, y, 3.6)


# The sleep timer running (Beta 8.36, #29): a crescent moon.
def ic_moon(p):
    p.ellipse(15, 16, 10.5)
    p.ellipse(20.5, 11.5, 9.0, fill=0)


ICONS = [
    ('back', 32, ic_back), ('chevron', 32, ic_chevron), ('check', 32, ic_check), ('close', 32, ic_close),
    ('play', 32, lambda p: ic_play(p, 32)), ('pause', 32, lambda p: ic_pause(p, 32)),
    ('prev', 32, lambda p: ic_prev(p, 32)), ('next', 32, lambda p: ic_next(p, 32)),
    ('play_l', 48, lambda p: ic_play(p, 48)), ('pause_l', 48, lambda p: ic_pause(p, 48)),
    ('prev_l', 48, lambda p: ic_prev(p, 48)), ('next_l', 48, lambda p: ic_next(p, 48)),
    ('shuffle', 32, ic_shuffle), ('repeat', 32, ic_repeat), ('repeat_one', 32, ic_repeat_one),
    ('gapless', 32, ic_gapless), ('eq', 32, ic_eq), ('norm', 32, ic_norm),
    ('bluetooth', 32, ic_bluetooth), ('wifi', 32, ic_wifi), ('lock', 32, ic_lock), ('bolt', 32, ic_bolt),
    ('folder', 32, ic_folder), ('note', 32, ic_note), ('speaker', 32, ic_speaker),
    ('headphones', 32, ic_headphones), ('sd', 32, ic_sd), ('usb', 32, ic_usb), ('warning', 32, ic_warning),
    ('info', 32, ic_info), ('sun', 32, ic_sun), ('drive', 32, ic_drive), ('power', 32, ic_power),
    ('download', 32, ic_download), ('refresh', 32, ic_refresh), ('search', 32, ic_search),
    ('vinyl', 32, ic_disc), ('person', 32, ic_person), ('crossfade', 32, ic_crossfade),
    ('shift', 32, ic_shift), ('shift_on', 32, ic_shift_on), ('backspace', 32, ic_backspace),
    ('eye', 32, ic_eye), ('eye_off', 32, ic_eye_off),
    ('star', 32, ic_star), ('star_on', 32, ic_star_on), ('globe', 32, ic_globe),
    ('trend', 32, ic_trend), ('broadcast', 32, ic_broadcast),
    ('heart', 32, ic_heart), ('heart_on', 32, ic_heart_on), ('plus', 32, ic_plus),
    ('palette', 32, ic_palette), ('sliders', 32, ic_sliders), ('moon', 32, ic_moon),
]
ARC_FRAMES = 48
ARC_SIZE = 96
# Pixel-exact corner sprites: filled quarter discs and quarter rings, per radius.
CORNER_RADII = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 18, 20, 22, 24, 28, 32]
CORNER_RING_WIDTHS = [1, 2, 3]
ORIENTS = ['tl', 'tr', 'bl', 'br']


def corner_sprite(r, orient, ring=None):
    """An r x r quarter of a disc (or ring of width `ring`) whose circle centre is the inner corner."""
    pen = Pen(r)
    cx = r if orient[1] == 'l' else 0
    cy = r if orient[0] == 't' else 0
    if ring is None:
        pen.ellipse(cx, cy, r)
    else:
        pen.ellipse(cx, cy, r, outline=True, width=ring)
    return pen.out()


def build_masks():
    sheet = Sheet(512)
    sheet.add('disc', shape_disc(64))
    sheet.add('ring_thin', shape_ring(64, 3))
    sheet.add('ring_med', shape_ring(64, 6))
    sheet.add('ring_thick', shape_ring(64, 10))
    sheet.add('glow', shape_glow(64))
    sheet.add('ramp_down', shape_ramp(4, 64, True, False))
    sheet.add('ramp_up', shape_ramp(4, 64, True, True))
    sheet.add('ramp_right', shape_ramp(64, 4, False, False))
    sheet.add('ramp_left', shape_ramp(64, 4, False, True))
    for name, size, draw in ICONS:
        pen = Pen(size)
        draw(pen)
        sheet.add(name, pen.out())
    for k in range(1, ARC_FRAMES + 1):
        sheet.add(f'arc_{k:02}', shape_arc(ARC_SIZE, 4.0, k / ARC_FRAMES))
    for r in CORNER_RADII:
        for o in ORIENTS:
            sheet.add(f'corner_{r}_{o}', corner_sprite(r, o))
            sheet.add(f'icorner_{r}_{o}', ImageChops.invert(corner_sprite(r, o)))
            for w in CORNER_RING_WIDTHS:
                if w < r:
                    sheet.add(f'cring_{r}_{w}_{o}', corner_sprite(r, o, w))
    return sheet.finish()


# --------------------------------------------------------------------------- emit

def rust_const(name):
    return name.upper()


def emit_fontdata(meta, layout, fb_meta, heights, path):
    out = ['// Generated by tools/assets/build_ui_assets.py from Open Sans; do not edit.',
           '// Glyph = (char, x, y, advance in 1/64 px) in the set\'s atlas page.',
           '#![allow(clippy::unreadable_literal, dead_code)]', '',
           'pub type Glyph = (u32, u16, u16, u16);', '',
           '#[derive(Clone, Copy, Debug)]',
           'pub struct SetData {',
           '    pub page: u8,', '    pub size: u8,', '    pub cell_w: u8,', '    pub cell_h: u8,',
           '    pub ascent: u8,', '    pub pad: u8,', '    pub glyphs: &\'static [Glyph],', '}', '',
           f'pub const PAGE_HEIGHTS: [usize; 3] = [{heights[0]}, {heights[1]}, {heights[2]}];', '']
    for name, m in meta.items():
        rows = ', '.join(f'({ch}, {x}, {y}, {adv})'
                         for (ch, adv), (x, y) in zip(m['glyphs'], layout[name]))
        out += ['#[rustfmt::skip]', f'pub const {rust_const(name)}: SetData = SetData {{ page: {m["page"]}, size: {m["size"]}, '
                f'cell_w: {m["cell_w"]}, cell_h: {m["cell_h"]}, ascent: {m["ascent"]}, pad: {m["pad"]}, glyphs: &[{rows}] }};', '']
    rows = ', '.join(f'({ch}, {x}, {y}, {adv})' for (ch, adv), (x, y) in zip(fb_meta, layout['FB']))
    out += ['/// 24 px Regular fallback cells (32x32, glyph origin at (2, 25)).', '#[rustfmt::skip]',
            f'pub const FALLBACK: &[Glyph] = &[{rows}];', '']
    path.write_text('\n'.join(out))


def emit_maskdata(rects, size, path):
    out = ['// Generated by tools/assets/build_ui_assets.py; do not edit.',
           '// Source rectangles [x, y, w, h] in the mask atlas.', '#![allow(dead_code)]', '',
           f'pub const MASK_SIZE: (usize, usize) = ({size[0]}, {size[1]});',
           f'pub const ARC_FRAMES: usize = {ARC_FRAMES};', f'pub const ARC_SIZE: f32 = {ARC_SIZE}.;', '']
    for name, (x, y, w, h) in rects.items():
        if name.startswith(('arc_', 'corner_', 'cring_', 'icorner_')):
            continue
        out.append(f'pub const {name.upper()}: [u16; 4] = [{x}, {y}, {w}, {h}];')
    radii = ', '.join(str(r) for r in CORNER_RADII)
    corners = ', '.join('[' + ', '.join('[{}, {}, {}, {}]'.format(*rects[f'corner_{r}_{o}']) for o in ORIENTS) + ']'
                        for r in CORNER_RADII)
    inverse = ', '.join('[' + ', '.join('[{}, {}, {}, {}]'.format(*rects[f'icorner_{r}_{o}']) for o in ORIENTS) + ']'
                        for r in CORNER_RADII)
    zero = '[0, 0, 0, 0]'
    rings = ', '.join('[' + ', '.join('[' + ', '.join(
        ('[{}, {}, {}, {}]'.format(*rects[f'cring_{r}_{w}_{o}']) if f'cring_{r}_{w}_{o}' in rects else zero)
        for o in ORIENTS) + ']' for w in CORNER_RING_WIDTHS) + ']' for r in CORNER_RADII)
    out += ['', f'pub const CORNER_RADII: [u8; {len(CORNER_RADII)}] = [{radii}];',
            f'pub const CORNER_RING_WIDTHS: [u8; {len(CORNER_RING_WIDTHS)}] = {list(CORNER_RING_WIDTHS)};',
            '/// Filled quarter discs, orientation order tl, tr, bl, br.', '#[rustfmt::skip]',
            f'pub const CORNERS: [[[u16; 4]; 4]; {len(CORNER_RADII)}] = [{corners}];',
            '/// Inverse quarter discs (opaque outside the circle), same order.', '#[rustfmt::skip]',
            f'pub const INVERSE_CORNERS: [[[u16; 4]; 4]; {len(CORNER_RADII)}] = [{inverse}];',
            '/// Quarter rings by width, orientation order tl, tr, bl, br.', '#[rustfmt::skip]',
            f'pub const CORNER_RINGS: [[[[u16; 4]; 4]; {len(CORNER_RING_WIDTHS)}]; {len(CORNER_RADII)}] = [{rings}];']
    arcs = ', '.join(f'[{rects[f"arc_{k:02}"][0]}, {rects[f"arc_{k:02}"][1]}, {ARC_SIZE}, {ARC_SIZE}]'
                     for k in range(1, ARC_FRAMES + 1))
    out += ['', '#[rustfmt::skip]', f'pub const ARCS: [[u16; 4]; {ARC_FRAMES}] = [{arcs}];', '']
    path.write_text('\n'.join(out))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--fonts', type=Path, default=Path('/usr/share/fonts/TTF'))
    parser.add_argument('--supersample', type=int, default=4, help='glyph supersampling (1 = hinted 1:1)')
    args = parser.parse_args()
    images, meta, layout, fb_meta = build_fonts(args.fonts, args.supersample)
    names = {0: 'regular', 1: 'semibold', 2: 'fallback'}
    heights = []
    for page, image in images.items():
        (ROOT / f'assets/fonts/reborn-{names[page]}.a8').write_bytes(image.tobytes())
        heights.append(image.size[1])
    emit_fontdata(meta, layout, fb_meta, heights, ROOT / 'crates/reborn-ui/src/fontdata.rs')
    sheet, rects = build_masks()
    (ROOT / 'assets/icons/reborn-masks.a8').write_bytes(sheet.tobytes())
    emit_maskdata(rects, sheet.size, ROOT / 'crates/reborn-ui/src/maskdata.rs')
    used = {w: (ROOT / f'assets/fonts/OpenSans-{w}.ttf') for w in ('Regular', 'Light', 'SemiBold')}
    provenance = {
        'family': 'Open Sans', 'license': 'SIL Open Font License 1.1 (assets/fonts/OpenSans-OFL.txt)',
        'source': 'Open Sans 3.x as packaged by ttf-opensans; build input only, no font file is installed',
        'ttf_sha256': {w: hashlib.sha256((args.fonts / f'OpenSans-{w}.ttf').read_bytes()).hexdigest()
                       for w in used},
        'sets': {n: {'page': m['page'], 'px': m['size'], 'weight': m['weight'], 'glyphs': len(m['glyphs']),
                     'cell': [m['cell_w'], m['cell_h']]} for n, m in meta.items()},
        'fallback': {'px': 24, 'glyphs': len(fb_meta)}, 'page_heights': heights,
        'mask_atlas': list(sheet.size), 'supersample': args.supersample, 'runtime_font_files': False,
    }
    (ROOT / 'assets/fonts/provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    print('pages', heights, 'masks', sheet.size, 'sets', len(meta), 'fallback glyphs', len(fb_meta))


if __name__ == '__main__':
    main()
