#!/usr/bin/env python3
"""Build the shipped example skins in skins/ (one folder per skin; Classic has its own script).

Each skin is written as skin.json, small generated pictures and Latin subsets
of open-licence fonts (Open Sans / Noto / Pixelify Sans / DM Sans / Syne, SIL OFL 1.1;
IBM Plex Mono is copied whole). The skins share one set
of layout builders and differ in palette, fonts, pictures and a few screens.
Deterministic: rerun after editing a style. Needs Pillow and fontTools
(pyftsubset), the Noto/Open Sans TTFs of the build host and the fonts in
assets/fonts (pixelify, dmsans, syne, plexmono).

Usage: build_skins.py [skin id ...]   (no id: every skin)
"""

from __future__ import annotations

import json
import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "skins"
FONTS = {
    "OpenSans-Bold": "/usr/share/fonts/TTF/OpenSans-Bold.ttf",
    "OpenSans-SemiBold": "/usr/share/fonts/TTF/OpenSans-SemiBold.ttf",
    "OpenSans-Regular": "/usr/share/fonts/TTF/OpenSans-Regular.ttf",
    "NotoSans-Regular": "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "NotoSans-Medium": "/usr/share/fonts/noto/NotoSans-Medium.ttf",
    "NotoSans-Bold": "/usr/share/fonts/noto/NotoSans-Bold.ttf",
    "NotoSans-Black": "/usr/share/fonts/noto/NotoSans-Black.ttf",
    "NotoSans-Light": "/usr/share/fonts/noto/NotoSans-Light.ttf",
    "NotoSansMono-Regular": "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "NotoSansMono-Bold": "/usr/share/fonts/noto/NotoSansMono-Bold.ttf",
    "NotoSerif-Regular": "/usr/share/fonts/noto/NotoSerif-Regular.ttf",
    "NotoSerif-Bold": "/usr/share/fonts/noto/NotoSerif-Bold.ttf",
    "NotoSerif-Italic": "/usr/share/fonts/noto/NotoSerif-Italic.ttf",
    **{f"PixelifySans-{w}": str(ROOT / f"assets/fonts/pixelify/PixelifySans-{w}.ttf")
       for w in ("Regular", "Medium", "SemiBold", "Bold")},
    "DMSans-Medium": str(ROOT / "assets/fonts/dmsans/DMSans[opsz,wght].ttf"),
    "DMSans-Bold": str(ROOT / "assets/fonts/dmsans/DMSans[opsz,wght].ttf"),
    "Syne-ExtraBold": str(ROOT / "assets/fonts/syne/Syne[wght].ttf"),
    "IBMPlexMono-Medium": str(ROOT / "assets/fonts/plexmono/IBMPlexMono-Medium.ttf"),
}
# Variable fonts are cut to a static instance at these axes before subsetting.
INSTANCES = {
    "DMSans-Medium": {"opsz": 14, "wght": 500},
    "DMSans-Bold": {"opsz": 14, "wght": 700},
    "Syne-ExtraBold": {"wght": 800},
}
LICENCES = {
    "OpenSans": ROOT / "assets/fonts/OpenSans-OFL.txt",
    "Noto": Path("/usr/share/licenses/noto-fonts/LICENSE"),
    "PixelifySans": ROOT / "assets/fonts/pixelify/PixelifySans-OFL.txt",
    "DMSans": ROOT / "assets/fonts/dmsans/DMSans-OFL.txt",
    "Syne": ROOT / "assets/fonts/syne/Syne-OFL.txt",
    "IBMPlexMono": ROOT / "assets/fonts/plexmono/IBMPlexMono-OFL.txt",
}
FAMILIES = ("OpenSans", "PixelifySans", "DMSans", "Syne", "IBMPlexMono", "Noto")
# IBM Plex has the Reserved Font Name "Plex": a subset would be a Modified
# Version that may not carry the name, so the published file is copied as is.
UNMODIFIED = {"IBMPlexMono"}
# The characters a skin font carries (reborn-ui skin::atlas::charset).
UNICODES = "U+0020-007E,U+00A0-017F,U+2009,U+2013,U+2014,U+2018,U+2019,U+201C,U+201D,U+2022,U+2026,U+20AC,U+2122,U+2190,U+2192,U+25B6,U+FFFD"


# ------------------------------------------------------------------ pictures

def rgba(h: str) -> tuple[int, int, int, int]:
    h = h.lstrip("#")
    a = int(h[6:8], 16) if len(h) == 8 else 255
    return (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16), a)


def mix(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def vgrad(w, h, stops):
    """Vertical gradient through [(t, colour), …]."""
    im = Image.new("RGBA", (w, h))
    px = im.load()
    stops = [(t, rgba(c)) for t, c in stops]
    for y in range(h):
        t = y / max(1, h - 1)
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t0 <= t <= t1:
                c = mix(c0, c1, (t - t0) / max(1e-6, t1 - t0))
                break
        for x in range(w):
            px[x, y] = c
    return im


def hgrad(w, h, left, right):
    im = Image.new("RGBA", (w, h))
    a, b = rgba(left), rgba(right)
    for x in range(w):
        c = mix(a, b, x / max(1, w - 1))
        for y in range(h):
            im.putpixel((x, y), c)
    return im


def rounded(im, r):
    """Round the corners of `im` (alpha), supersampled."""
    w, h = im.size
    s = 4
    m = Image.new("L", (w * s, h * s), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, w * s - 1, h * s - 1], r * s, fill=255)
    m = m.resize((w, h), Image.Resampling.LANCZOS)
    out = im.copy()
    alpha = Image.eval(im.getchannel("A"), lambda v: v)
    out.putalpha(Image.composite(alpha, Image.new("L", (w, h), 0), m))
    return out


def line(im, y, colour):
    for x in range(im.size[0]):
        im.putpixel((x, y), rgba(colour))
    return im


def frame(im, colour, width=1):
    w, h = im.size
    d = ImageDraw.Draw(im)
    for i in range(width):
        d.rectangle([i, i, w - 1 - i, h - 1 - i], outline=rgba(colour))
    return im


def bevel(w, h, face, light, dark, inset=False):
    im = Image.new("RGBA", (w, h), rgba(face))
    d = ImageDraw.Draw(im)
    a, b = (dark, light) if inset else (light, dark)
    d.line([(0, 0), (w - 1, 0)], fill=rgba(a))
    d.line([(0, 0), (0, h - 1)], fill=rgba(a))
    d.line([(0, h - 1), (w - 1, h - 1)], fill=rgba(b))
    d.line([(w - 1, 0), (w - 1, h - 1)], fill=rgba(b))
    return im


def glow(size, colour, strength=1.0):
    im = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    c = rgba(colour)
    px = im.load()
    r = size / 2
    for y in range(size):
        for x in range(size):
            d = math.hypot(x + 0.5 - r, y + 0.5 - r) / r
            a = max(0.0, 1 - d) ** 2 * 255 * strength
            px[x, y] = (c[0], c[1], c[2], min(255, round(a)))
    return im


def battery_pictures(outline):
    b = Image.new("RGBA", (28, 14), (0, 0, 0, 0))
    d = ImageDraw.Draw(b)
    d.rectangle([0, 0, 24, 13], outline=rgba(outline))
    d.rectangle([25, 4, 27, 9], fill=rgba(outline))
    return b


# ------------------------------------------------------------------ elements

def T(x, y, w, h, text, font, color="text", align="left", **kw):
    e = {"type": "text", "x": x, "y": y, "w": w, "h": h, "text": text, "font": font, "color": color}
    if align != "left":
        e["align"] = align
    e.update(kw)
    return e


def R(x, y, w, h, color, **kw):
    e = {"type": "rect", "x": x, "y": y, "w": w, "h": h, "color": color}
    e.update(kw)
    return e


def I(x, y, w, h, image, **kw):
    e = {"type": "image", "x": x, "y": y, "w": w, "h": h, "image": image}
    e.update(kw)
    return e


def P(name, **kw):
    e = {"type": "part", "name": name}
    e.update(kw)
    return e


def when(cond, e):
    e = dict(e)
    e["if"] = f"{cond} & {e['if']}" if "if" in e else cond
    return e


class Style:
    """Everything the shared builders need; skins override what differs."""

    def __init__(self, **kw):
        # Fonts by role; each skin maps them in `fonts`.
        self.case = None          # "upper"/"lower" for labels
        self.prefix = ""          # text before a focused label
        self.row_h = 34
        self.top = 32             # where the content starts under the bar
        self.pad = 14
        self.fg = "text"
        self.muted = "muted"
        self.dim = "dim"
        self.ffg = "text"         # focused label colour
        self.fmuted = "muted"     # focused detail colour
        self.icon_fg = "dim"
        self.icon_ffg = "text"
        self.highlight = lambda w, h: [R(0, 0, w, h, "accent")]
        self.scroll = {"x": -7, "w": 4, "track": "glass/10", "thumb": "muted"}
        self.sheet_bg = "sheet"
        self.sheet_frame = "dim"
        self.sheet_radius = 10
        self.scrim = "#00000080"
        self.toast_bg = "#2B2E33EB"
        self.toast_fg = "#FFFFFF"
        self.toast_error = "#B3261EEB"
        self.track = "track"
        self.fill = "accent"
        self.bar_radius = 3
        self.bar_track_image = None
        self.bar_fill_image = None
        self.bar_slice = None
        self.__dict__.update(kw)


def label(st, x, y, w, h, font, color, focused=False):
    text = (st.prefix if focused else "") + "{label}"
    e = T(x, y, w, h, text, font, color)
    if st.case:
        e["case"] = st.case
    return e


def row_elements(st, w, focused):
    """One list row of width `w` at height st.row_h."""
    h = st.row_h
    fg, muted = (st.ffg, st.fmuted) if focused else (st.fg, st.muted)
    icon = st.icon_ffg if focused else st.icon_fg
    a = h - 8
    out = list(st.highlight(w, h)) if focused else []
    two = h >= 30
    out.append({"type": "group", "if": "row_art", "elements": [
        {"type": "art", "x": 6, "y": 4, "size": a, "source": "row"},
        when("!has_detail", label(st, a + 14, 0, w - a - 50, h, "menu", fg, focused)),
        when("has_detail", label(st, a + 14, round(h * 0.04), w - a - 50, round(h * 0.52), "title2", fg, focused)),
        when("has_detail", T(a + 14, round(h * 0.5), w - a - 50, round(h * 0.44), "{detail}", "small", muted)),
    ] if two else [
        {"type": "art", "x": 6, "y": 4, "size": a, "source": "row"},
        label(st, a + 14, 0, w - a - 50, h, "menu", fg, focused)]})
    dw = 170 if w >= 400 else 56
    plain = []
    for cond, lw in (("has_detail", w - st.pad - dw - 40), ("!has_detail", w - st.pad - 40)):
        if focused:
            plain.append(when(cond, label(st, st.pad, 0, lw, h, "menu", fg, True)))
        else:
            plain.append(when(f"{cond} & !disabled", label(st, st.pad, 0, lw, h, "menu", fg)))
            plain.append(when(f"{cond} & disabled", label(st, st.pad, 0, lw, h, "menu", st.dim)))
    plain += [
        T(w - dw - 34, 0, dw, h, "{detail}", "detail", muted, "right"),
        T(w - 134, 0, 100, h, "On", "detail", muted, "right", **{"if": "toggle & on & !has_detail"}),
        T(w - 134, 0, 100, h, "Off", "detail", muted, "right", **{"if": "toggle & !on & !has_detail"}),
    ]
    out.append({"type": "group", "if": "!row_art", "elements": plain})
    s = 16 if h < 40 else 22
    out += [
        {"type": "icon", "if": "checked", "x": w - 30, "y": (h - s) // 2, "size": s, "icon": "check", "color": icon if focused else "accent"},
        {"type": "icon", "if": "chevron & !checked", "x": w - 26, "y": (h - 12) // 2, "size": 12, "icon": "chevron", "color": icon},
        {"type": "icon", "if": "current", "x": w - 30, "y": (h - s) // 2, "size": s, "icon": "speaker", "color": icon if focused else "accent"},
    ]
    return out


def rows_list(st, y, rows, w=480, x=0, cond=None, source="rows"):
    e = {"type": "list", "x": x, "y": y, "w": w, "rows": rows, "row_height": st.row_h,
         "row": [P("row" if w == 480 else f"row{w}")], "focused": [P("row_focused" if w == 480 else f"row{w}_focused")]}
    if source != "rows":
        e["source"] = source
    if st.scroll and source == "rows":
        sc = dict(st.scroll)
        sc["x"] = w + sc["x"]
        e["scrollbar"] = sc
    if cond:
        e["if"] = cond
    return e


def bar(st, x, y, w, h, value):
    e = {"type": "bar", "x": x, "y": y, "w": w, "h": h, "value": value}
    if st.bar_track_image:
        e["track_image"] = st.bar_track_image
        e["fill_image"] = st.bar_fill_image
        if st.bar_slice:
            e["slice"] = st.bar_slice
    else:
        e.update(track=st.track, fill=st.fill, radius=st.bar_radius)
    return e


def list_layout(st):
    top = st.top
    rows = (360 - top) // st.row_h
    status_rows = (360 - top - 34) // st.row_h
    return [
        P("titlebar"), P("title"),
        rows_list(st, top, rows, cond="!has_status & !empty"),
        rows_list(st, top, status_rows, cond="has_status & !empty"),
        R(0, 325, 480, 1, "glass/20", **{"if": "has_status"}),
        T(14, 326, 452, 34, "{status}", "small", "muted", lines=2, line_height=15, **{"if": "has_status"}),
        T(40, 120, 400, 32, "{empty_title}", "big", "text", "center", **{"if": "empty"}),
        T(60, 158, 360, 60, "{empty_body}", "body", "muted", "center", lines=3, line_height=20, **{"if": "empty"}),
        R(150, 250, 180, 34, "accent", radius=17, **{"if": "empty & has_action"}),
        T(150, 250, 180, 34, "{empty_action}", "title2", "accent_text", "center", **{"if": "empty & has_action"}),
    ]


def page_layout(st):
    top = st.top
    def rows_from(y):
        return max(1, (360 - y - 34) // st.row_h)
    return [
        P("titlebar"), P("title"),
        T(16, top + 4, 448, 30, "{page_title}", "big"),
        T(16, top + 34, 448, 0, "{page_body}", "body", "muted", lines=3, line_height=19, **{"if": "has_body"}),
        {"type": "list", "x": 0, "y": top + 94, "w": 480, "rows": 4, "row_height": 24, "source": "facts",
         "row": [R(16, 0, 448, 1, "glass/15", **{"if": "!first"}),
                 T(16, 0, 190, 24, "{label}", "small", "muted"),
                 T(206, 0, 258, 24, "{detail}", "small", "text", "right")]},
        rows_list(st, top + 196, rows_from(top + 196), cond="has_facts"),
        rows_list(st, top + 96, rows_from(top + 96), cond="!has_facts & has_body"),
        rows_list(st, top + 38, rows_from(top + 38), cond="!has_facts & !has_body"),
        R(0, 325, 480, 1, "glass/20", **{"if": "has_status"}),
        T(14, 326, 452, 34, "{status}", "small", "muted", lines=2, line_height=15, **{"if": "has_status"}),
    ]


def home_split(st, title="Reborn", panel=None):
    w = 240
    rows = (360 - st.top) // st.row_h
    return (panel or [
        R(240, st.top, 240, 360 - st.top, "panel"),
        {"type": "art", "if": "has_art", "x": 280, "y": st.top + 24, "size": 160},
        {"type": "icon", "if": "!has_art", "x": 320, "y": st.top + 64, "size": 80, "icon": "note", "color": "dim"},
        T(250, st.top + 196, 220, 22, "{title}", "title2", "text", "center", **{"if": "has_track"}),
        T(250, st.top + 218, 220, 20, "{artist}", "body", "muted", "center", **{"if": "has_track"}),
        bar(st, 280, st.top + 250, 160, 8, "progress") | {"if": "has_track"},
        T(250, st.top + 196, 220, 22, "Nothing playing", "title2", "muted", "center", **{"if": "!has_track"}),
    ]) + [rows_list(st, st.top, rows, w=w), P("titlebar"), T(60, 0, 360, st.top, title, "bartitle", "bar_text", "center")]


def sheet_layout(st):
    rh = st.row_h
    rows = max(3, min(5, 170 // rh))
    return [
        R(0, 0, 480, 360, st.scrim),
        R(60, 40, 360, 280, st.sheet_bg, radius=st.sheet_radius),
        R(60, 40, 360, 280, st.sheet_frame, radius=st.sheet_radius, stroke=1),
        T(76, 48, 328, 30, "{sheet_title}", "big", "text", "center", **{"if": "!danger"}),
        T(76, 48, 328, 30, "{sheet_title}", "big", "danger", "center", **{"if": "danger"}),
        T(76, 80, 328, 38, "{sheet_body}", "body", "muted", "center", lines=2, line_height=19),
        {"type": "list", "x": 70, "y": 124, "w": 340, "rows": rows, "row_height": rh, "source": "sheet",
         "row": [P("row340")], "focused": [P("row340_focused")]},
    ]


def toast_layout(st):
    return [
        R(80, 292, 320, 48, st.toast_bg, radius=10, **{"if": "!error"}),
        R(80, 292, 320, 48, st.toast_error, radius=10, **{"if": "error"}),
        T(94, 296, 292, 40, "{message}", "title2", st.toast_fg, "center", lines=2, line_height=20),
    ]


def volume_layout(st):
    return [
        R(40, 294, 400, 46, st.sheet_bg, radius=10),
        R(40, 294, 400, 46, st.sheet_frame, radius=10, stroke=1),
        {"type": "icon", "x": 54, "y": 306, "size": 22, "icon": "speaker", "color": "text"},
        bar(st, 88, 312, 300, 10, "volume"),
        T(392, 294, 40, 46, "{volume}", "small", "muted", "right"),
    ]


def battery_part(x, y, outline_image="battery", fill_image="battery_fill", low_image="battery_low"):
    return [
        {"type": "bar", "if": "!battery_low", "x": x + 2, "y": y + 2, "w": 22, "h": 10, "value": "battery", "fill_image": fill_image},
        {"type": "bar", "if": "battery_low", "x": x + 2, "y": y + 2, "w": 22, "h": 10, "value": "battery", "fill_image": low_image},
        I(x, y, 28, 14, outline_image),
    ]


# ------------------------------------------------------------------ writing

class Skin:
    def __init__(self, sid, name, description):
        self.id = sid
        self.dir = OUT / sid
        self.name = name
        self.description = description
        self.images: dict[str, Image.Image] = {}
        self.fonts: dict[str, tuple[str, int, str | None]] = {}
        self.palette: dict = {}
        self.colors: dict = {}
        self.parts: dict = {}
        self.screens: dict = {}

    def write(self):
        if self.dir.exists():
            shutil.rmtree(self.dir)
        (self.dir / "images").mkdir(parents=True)
        (self.dir / "fonts").mkdir()
        images = {}
        for name, im in sorted(self.images.items()):
            path = f"images/{name}.png"
            im.save(self.dir / path, optimize=True)
            images[name] = path
        families = set()
        fonts = {}
        for role, (src, size, charset) in sorted(self.fonts.items()):
            path = f"fonts/{src}.ttf"
            family = next(f for f in FAMILIES if src.startswith(f) or f == "Noto")
            if not (self.dir / path).exists() and family in UNMODIFIED:
                shutil.copyfile(FONTS[src], self.dir / path)
            elif not (self.dir / path).exists():
                source = font_input(src, self.dir / path)
                subprocess.run(["pyftsubset", source, f"--unicodes={UNICODES}",
                                f"--output-file={self.dir / path}", "--layout-features=kern",
                                "--name-IDs=*", "--name-legacy", "--notdef-outline"], check=True)
                if source != FONTS[src]:
                    Path(source).unlink()
            families.add(family)
            fonts[role] = {"file": path, "size": size} | ({"charset": charset} if charset else {})
        for fam in sorted(families):
            shutil.copy(LICENCES[fam], self.dir / f"fonts/{fam}-OFL.txt")
        doc = {"format": 1, "name": self.name, "author": "Reborn", "version": "1.0",
               "description": self.description, "palette": self.palette, "colors": self.colors,
               "fonts": fonts, "images": images, "parts": self.parts, "screens": self.screens}
        # Independent copies: the finishing steps change elements in place.
        doc = json.loads(json.dumps(doc))
        if self.id in CLOCKS:
            add_clock(self.id, doc)
            add_live(self.id, doc)
            add_keyboard(self.id, doc)
            add_heart(self.id, doc)
        add_sleep(self.id, doc)
        add_status(self.id, doc)
        (self.dir / "skin.json").write_text(json.dumps(doc, indent=1, ensure_ascii=False) + "\n")


def font_input(src, out):
    """The file pyftsubset reads: the font, or a static instance of a variable font."""
    if src not in INSTANCES:
        return FONTS[src]
    from fontTools.ttLib import TTFont
    from fontTools.varLib import instancer

    font = TTFont(FONTS[src], recalcTimestamp=False)
    instancer.instantiateVariableFont(font, INSTANCES[src], inplace=True)
    tmp = out.with_suffix(".instance.ttf")
    font.save(tmp)
    return str(tmp)


def standard(skin: Skin, st: Style, now_playing, home=None):
    """Rows, list, page, sheet, toast and volume from `st`, plus the skin's own screens."""
    skin.parts["row"] = row_elements(st, 480, False)
    skin.parts["row_focused"] = row_elements(st, 480, True)
    skin.parts["row240"] = row_elements(st, 240, False)
    skin.parts["row240_focused"] = row_elements(st, 240, True)
    skin.parts["row340"] = row_elements(st, 340, False)
    skin.parts["row340_focused"] = row_elements(st, 340, True)
    skin.screens["list"] = list_layout(st)
    skin.screens["page"] = page_layout(st)
    skin.screens["sheet"] = sheet_layout(st)
    skin.screens["toast"] = toast_layout(st)
    skin.screens["volume"] = volume_layout(st)
    skin.screens["now_playing"] = now_playing
    skin.screens["home"] = home if home is not None else home_split(st)
    prune(skin)


def prune(skin: Skin):
    """Drop parts no screen reaches (Reborn warns about unused parts)."""
    used: set[str] = set()
    todo = [json.dumps(skin.screens)]
    while todo:
        text = todo.pop()
        for name, body in skin.parts.items():
            if name not in used and f'"name": "{name}"' in text:
                used.add(name)
                todo.append(json.dumps(body))
    for name in list(skin.parts):
        if name not in used:
            del skin.parts[name]


# ------------------------------------------------------------------ finishing steps
# Screens added after the first skins (clock, radio, keyboard, Favorites),
# applied to every finished skin in each skin's own style.

def split(elements, match, **changes):
    """The element `match` finds stays as it is without a clock; with one, changed."""
    for i, e in enumerate(elements):
        if match(e):
            a, b = json.loads(json.dumps(e)), json.loads(json.dumps(e))
            a["if"] = f"{e['if']} & !has_clock" if "if" in e else "!has_clock"
            b["if"] = f"{e['if']} & has_clock" if "if" in e else "has_clock"
            b.update(changes)
            elements[i:i + 1] = [a, b]
            return
    raise SystemExit("split: no element matched")


def is_title(e):
    return "{screen}" in e.get("text", "")


def clock(**kw):
    return {"type": "text", "if": "has_clock", **kw}


IPOD = {"x": 32, "y": 0, "w": 60, "h": 30, "text": "{clock}", "font": "bartitle", "color": "bar_text"}
# The time in each title bar, in the skin's idiom; `title` narrows the title beside it.
CLOCKS = {
    **{sid: (IPOD, {"x": 100, "w": 280}) for sid in
       ("mini-blue", "mini-gold", "mini-green", "mini-pink", "mini-silver", "nano-glass")},
    "cassette": ({"x": 32, "y": 0, "w": 64, "h": 32, "text": "{clock}", "font": "bartitle",
                  "color": "bar_text", "tracking": 1}, {"x": 104, "w": 272}),
    "paper": ({"x": 36, "y": 0, "w": 60, "h": 34, "text": "{clock}", "font": "small", "color": "muted"}, None),
    "neon": ({"x": 322, "y": 0, "w": 60, "h": 38, "text": "{clock}", "font": "small", "color": "muted",
              "align": "right"}, {"w": 296}),
    "walkman": ({"x": 316, "y": 0, "w": 60, "h": 33, "text": "{clock}", "font": "small", "color": "orange",
                 "align": "right"}, {"w": 286}),
    "terminal": (None, {"w": 280}),
    "big-type": (None, {"w": 224}),
    "zune": ({"x": 190, "y": 0, "w": 100, "h": 26, "text": "{clock}", "font": "small", "color": "muted",
              "align": "center"}, None),
    "winamp": (None, None),
}


def add_clock(sid, doc):
    parts = doc["parts"]
    bar, title = CLOCKS[sid]
    if bar:
        parts["titlebar"].append(clock(**bar))
    if title:
        split(parts["title"], is_title, **title)
    if sid == "terminal":
        # A prompt-style [09:41] before [72%].
        split(parts["titlebar"], lambda e: e.get("text") == "[{battery}%]",
              x=290, w=182, text="[{clock}] [{battery}%]")
    if sid == "big-type":
        # Large time before the battery; the bolt moves left of it.
        split(parts["titlebar"], lambda e: e.get("text") == "{battery}%", x=376, w=90)
        parts["titlebar"].append(clock(x=268, y=0, w=98, h=42, text="{clock}", font="bartitle",
                                       color="text", align="right"))
        split(parts["titlebar"], lambda e: e.get("icon") == "bolt", x=240)
    if sid == "winamp":
        # A small LCD box where the left stripes begin.
        split(parts["titlebar"], lambda e: e.get("type") == "image" and e.get("x") == 8 and e.get("w") == 120,
              x=62, w=66)
        parts["titlebar"].append({"type": "rect", "if": "has_clock", "x": 8, "y": 4, "w": 48, "h": 16,
                                  "color": "#000000"})
        parts["titlebar"].append({"type": "text", "if": "has_clock", "x": 8, "y": 4, "w": 48, "h": 16,
                                  "text": "{clock}", "font": "small", "color": "lcd", "align": "center"})



def status(x, y, size, gap, on, dim, charge="success", cond=None):
    e = {"type": "indicators", "x": x, "y": y, "size": size, "gap": gap, "color": on, "dim": dim,
         "charge": charge}
    return {**e, "if": cond} if cond else e


IPOD_STATUS = {"bar": [status(436, 7, 16, 4, "bar_text", "bar_text/45")],
               "clock": {"w": 72}, "title": {None: {"x": 120, "w": 240}}}
# The status icons Reborn's bar shows left of the battery (charging, a computer, Bluetooth and
# Wi-Fi: Y2Linux #70, #73) in each title bar's idiom, and what moves aside for them: titles keep
# clear of four icons, clocks get room for "12:55 PM" (Y2Linux #72). Keys of "title": the title's
# "if" (None: every title).
STATUS = {
    **{sid: IPOD_STATUS for sid in
       ("mini-blue", "mini-gold", "mini-green", "mini-pink", "mini-silver", "nano-glass")},
    "classic": {"bar": [status(436, 7, 16, 4, "text", "text/40")],
                "clock": {"w": 72}, "title": {None: {"x": 120, "w": 240}}},
    "cassette": {"bar": [status(436, 9, 14, 4, "bar_text", "bar_text/45")],
                 "clock": {"w": 80}, "title": {None: {"x": 112, "w": 256}}},
    "bunny-bow": {"bar": [status(398, 10, 16, 4, "text", "muted")],
                  "clock": {"w": 76}, "title": {None: {"x": 162, "w": 156}}},
    "big-type": {"bar": [status(262, 10, 22, 6, "text", "text/45", "yellow", "has_clock"),
                         status(410, 10, 22, 6, "text", "text/45", "yellow", "!has_clock")],
                 "clock": {"x": 266, "w": 100},
                 "title": {"has_clock": {"w": 140}, "!has_clock": {"w": 286}}},
    "neon": {"bar": [status(406, 11, 16, 4, "text", "muted")],
             "clock": {"x": 266},
             "title": {"has_clock": {"w": 244}, "!has_clock": {"w": 308}}},
    "paper": {"bar": [status(426, 10, 14, 4, "text", "muted")],
              "title": {None: {"x": 122, "w": 236}}},
    "terminal": {"bar": [status(344, 7, 14, 4, "text", "text/45", cond="has_clock"),
                         status(424, 7, 14, 4, "text", "text/45", cond="!has_clock")],
                 "title": {"has_clock": {"w": 258}, "!has_clock": {"w": 340}}},
    "walkman": {"bar": [status(380, 10, 14, 4, "text", "muted")],
                "clock": {"x": 248},
                "title": {"has_clock": {"w": 222}, "!has_clock": {"w": 286}}},
    "winamp": {"bar": [status(404, 5, 14, 3, "bar_text", "bar_text/45")],
               "clock": {"w": 58}, "title": {None: {"x": 140, "w": 200}}},
    "zune": {"bar": [status(336, 6, 13, 4, "text", "muted")]},
}


def add_status(sid, doc):
    spec = STATUS[sid]
    bar = doc["parts"]["titlebar"]
    # The indicators draw charging and Bluetooth themselves.
    bar[:] = [e for e in bar if e.get("icon") not in ("bolt", "bluetooth")]
    for e in bar:
        if "{clock}" in e.get("text", ""):
            e.update(spec.get("clock", {}))
    for e in doc["parts"]["title"]:
        for cond, changes in spec.get("title", {}).items():
            if is_title(e) and (cond is None or e.get("if") == cond):
                e.update(changes)
    if sid == "bunny-bow":
        # The percentage stays while charging; the bolt is among the indicators now.
        for e in bar:
            if e.get("text") == "{battery}%":
                e.pop("if", None)
    if sid == "winamp":
        # The LCD clock box widens; the right stripes give their place to the icons.
        for e in bar:
            if e.get("if") == "has_clock" and e.get("w") == 48:
                e["w"] = 58
            if e.get("image") == "stripes" and e.get("x") == 62:
                e.update(x=70, w=58)
            if e.get("image") == "stripes" and e.get("x") == 352:
                e["if"] = "!charging & !bluetooth & !wifi"
    bar.extend(spec["bar"])

# A station has no queue position: the station (title bars) or On Air instead.
LIVE_TEXT = {
    "Song {queue_index} of {queue_length}": "On Air",
    "Side A · {queue_index}/{queue_length}": "{station}",
    "{queue_index} of {queue_length}": "{station}",
    "Track {queue_index} of {queue_length}": "On Air",
    "QUEUE  : {queue_index}/{queue_length}": "STATION: {station}",
    "{queue_index} / {queue_length}": "On Air",
    "{queue_index}. {artist} - {title}": "{artist} - {title}",
    "-{duration}": "LIVE",
}
ON_AIR_BODY = ("paper", "walkman", "zune", "big-type")


def add_live(sid, doc):
    def walk(elements):
        i = 0
        while i < len(elements):
            e = elements[i]
            text = e.get("text")
            if text in LIVE_TEXT:
                a, b = json.loads(json.dumps(e)), json.loads(json.dumps(e))
                a["if"] = f"{e['if']} & !live" if "if" in e else "!live"
                b["if"] = f"{e['if']} & live" if "if" in e else "live"
                live = LIVE_TEXT[text]
                # Zune's "1 of 6" shares its text with the title-bar skins.
                b["text"] = "On Air" if live == "{station}" and sid in ON_AIR_BODY else live
                elements[i:i + 1] = [a, b]
                i += 2
                continue
            if "elements" in e:
                walk(e["elements"])
            i += 1
    walk(doc["screens"]["now_playing"])


def add_keyboard(sid, doc):
    """The skin's title bar and the keyboard's purpose over Reborn's keys."""
    native = {"type": "native", "part": "screen", "recolor": False}
    if sid == "big-type":
        native["y"] = 4  # under the 44-pixel bar
    layout = [native, {"type": "part", "name": "titlebar"}]
    if sid == "zune":
        layout.append({"type": "text", "x": 10, "y": 0, "w": 170, "h": 26, "text": "{sheet_title}",
                       "font": "small", "color": "text", "case": "lower"})
    else:
        for e in doc["parts"]["title"]:
            e = json.loads(json.dumps(e))
            if "text" in e:
                e["text"] = e["text"].replace("{screen}", "{sheet_title}")
            layout.append(e)
    doc["screens"]["keyboard"] = layout


def add_sleep(sid, doc):
    """A running sleep timer joins the output line (#29): "Headphone Jack ·
    Sleeps in 23 min", as Reborn's own Now Playing says it."""
    def walk(elements):
        i = 0
        while i < len(elements):
            e = elements[i]
            text = e.get("text", "")
            if e.get("type") == "text" and "{output}" in text and "sleep" not in e.get("if", ""):
                a, b = json.loads(json.dumps(e)), json.loads(json.dumps(e))
                a["if"] = f"{e['if']} & !sleep" if "if" in e else "!sleep"
                b["if"] = f"{e['if']} & sleep" if "if" in e else "sleep"
                b["text"] = text.replace("{output}", "{output} · {sleep}")
                elements[i:i + 1] = [a, b]
                i += 2
                continue
            if "elements" in e:
                walk(e["elements"])
            i += 1
    walk(doc["screens"]["now_playing"])


HEART = "#FF5A7A"
# Where each skin marks a Favorites song: the end of its quality line.
HEARTS = {
    **{sid: {"x": 442, "y": 153, "size": 16, "color": HEART} for sid in
       ("mini-blue", "mini-gold", "mini-green", "mini-pink", "mini-silver", "nano-glass")},
    "cassette": {"x": 428, "y": 144, "size": 16, "color": HEART},
    "neon": {"x": 448, "y": 175, "size": 16, "color": HEART},
    "paper": {"x": 446, "y": 170, "size": 16, "color": "text"},
    "walkman": {"x": 450, "y": 163, "size": 16, "color": "orange"},
    "winamp": {"x": 442, "y": 68, "size": 16, "color": "lcd"},
    "terminal": {"x": 298, "y": 119, "size": 16, "color": "text"},
    "big-type": {"x": 438, "y": 302, "size": 24, "color": "yellow"},
    "zune": {"x": 448, "y": 188, "size": 16, "color": "magenta"},
}


def add_heart(sid, doc):
    doc["screens"]["now_playing"].append({"type": "icon", "if": "favorite", "icon": "heart", **HEARTS[sid]})


# ------------------------------------------------------------------ the skins

def nano_glass():
    s = Skin("nano-glass", "Nano Glass", "Black glass with a silver-blue highlight, a dark partner to Classic.")
    s.palette = {"bg": "#000000", "text": "#FFFFFF", "muted": "#AEB3BB", "dim": "#8C929B", "accent": "#4FA3FF",
                 "glass": "#FFFFFF", "scrim": "#000000", "sheet": "#1C1E22", "track": "#33363B",
                 "danger": "#FF6B5E", "success": "#4CD07A", "warning": "#FFC24A"}
    s.colors = {"bar_text": "#FFFFFF", "panel": "#0E0F11", "accent_text": "#FFFFFF", "white": "#FFFFFF", "pale": "#E4EEFF"}
    s.fonts = {"menu": ("OpenSans-Bold", 17, None), "title2": ("OpenSans-Bold", 15, None),
               "bartitle": ("OpenSans-Bold", 15, None), "big": ("OpenSans-Bold", 19, None),
               "detail": ("OpenSans-Regular", 15, None), "body": ("OpenSans-Regular", 14, None),
               "small": ("OpenSans-SemiBold", 12, None)}
    bar_im = vgrad(4, 30, [(0, "#6B6F76"), (0.48, "#34373C"), (0.5, "#1D1F22"), (1, "#111214")])
    s.images = {"titlebar": line(bar_im, 29, "#000000"),
                "highlight": line(line(vgrad(4, 34, [(0, "#B9D8FF"), (0.48, "#6EA6F2"), (0.5, "#3F7FE0"), (1, "#2F6BCB")]), 0, "#D6E8FF"), 33, "#1F4F9E"),
                "track": frame(vgrad(8, 12, [(0, "#1C1D20"), (1, "#3A3D42")]), "#55595F"),
                "fill": frame(vgrad(8, 12, [(0, "#A9CCFF"), (0.5, "#5B95EC"), (1, "#2D66C4")]), "#2357AE"),
                "battery": battery_pictures("#D0D3D8"), "battery_fill": vgrad(4, 10, [(0, "#9BE38B"), (1, "#3AA32C")]),
                "battery_low": vgrad(4, 10, [(0, "#FF9C8A"), (1, "#D93025")]),
                "floor": vgrad(4, 60, [(0, "#00000000"), (1, "#1A1C20FF")])}
    st = Style(row_h=33, top=30, ffg="white", fmuted="pale", icon_ffg="white",
               highlight=lambda w, h: [I(0, 0, w, h, "highlight")],
               bar_track_image="track", bar_fill_image="fill", bar_slice=[3, 3, 3, 3],
               toast_bg="#2B2E33F0", sheet_frame="#4A4E55")
    s.parts["titlebar"] = [I(0, 0, 480, 30, "titlebar"),
                           {"type": "icon", "if": "playing", "x": 10, "y": 7, "size": 16, "icon": "play", "color": "accent"},
                           {"type": "icon", "if": "paused", "x": 10, "y": 7, "size": 16, "icon": "pause", "color": "accent"},
                           {"type": "icon", "if": "charging", "x": 420, "y": 7, "size": 16, "icon": "bolt", "color": "success"},
                           *battery_part(440, 8)]
    s.parts["title"] = [T(60, 0, 360, 30, "{screen}", "bartitle", "bar_text", "center")]
    np = [P("titlebar"), T(60, 0, 360, 30, "{queue_index} of {queue_length}", "bartitle", "bar_text", "center"),
          {"type": "art", "x": 24, "y": 46, "size": 176},
          I(24, 222, 176, 30, "floor"),
          T(222, 50, 236, 50, "{title}", "big", lines=2, line_height=25),
          T(222, 104, 236, 22, "{artist}", "detail"),
          T(222, 126, 236, 22, "{album}", "detail", "muted"),
          T(222, 152, 236, 18, "{quality}", "small", "dim", case="upper"),
          {"type": "icon", "if": "shuffle", "x": 222, "y": 180, "size": 20, "icon": "shuffle", "color": "accent"},
          {"type": "icon", "if": "repeat", "x": 252, "y": 180, "size": 20, "icon": "repeat_mode", "color": "accent"},
          bar(st, 24, 266, 432, 12, "progress"),
          T(24, 282, 100, 20, "{elapsed}", "small"), T(356, 282, 100, 20, "{remaining}", "small", "text", "right"),
          T(24, 314, 432, 34, "{output}", "body", "muted", "center")]
    standard(s, st, np)
    return s


def walkman():
    s = Skin("walkman", "Walkman", "Black with a bold orange accent, after the portable players of the 2000s.")
    s.palette = {"bg": "#0B0B0B", "text": "#F2F2F2", "muted": "#B5B5B5", "dim": "#8E8E8E", "accent": "#FF7A1A",
                 "glass": "#FFFFFF", "sheet": "#1A1A1A", "track": "#333333"}
    s.colors = {"bar_text": "#FF7A1A", "panel": "#141414", "accent_text": "#0B0B0B", "orange": "#FF7A1A"}
    s.fonts = {"menu": ("NotoSans-Bold", 19, None), "title2": ("NotoSans-Bold", 15, None),
               "bartitle": ("NotoSans-Bold", 13, None), "big": ("NotoSans-Bold", 21, None),
               "detail": ("NotoSans-Regular", 15, None), "body": ("NotoSans-Regular", 14, None),
               "small": ("NotoSans-Medium", 12, None), "time": ("NotoSans-Bold", 16, None)}
    s.images = {"battery": battery_pictures("#B5B5B5"), "battery_fill": vgrad(4, 10, [(0, "#FF9A4A"), (1, "#FF7A1A")]),
                "battery_low": vgrad(4, 10, [(0, "#FF6B5E"), (1, "#D93025")])}
    st = Style(row_h=37, top=36, pad=22, ffg="orange", fmuted="orange", icon_ffg="orange",
               highlight=lambda w, h: [R(0, 6, 5, h - 12, "orange"), R(0, 0, w, h, "#FFFFFF0D")],
               scroll={"x": -5, "w": 3, "track": "#222222", "thumb": "orange"}, bar_radius=2,
               toast_bg="#1E1E1EF0", toast_fg="#FF7A1A", sheet_frame="#FF7A1A", sheet_radius=4)
    s.parts["titlebar"] = [R(0, 33, 480, 2, "orange"),
                           {"type": "icon", "if": "playing", "x": 384, "y": 10, "size": 14, "icon": "play", "color": "orange"},
                           {"type": "icon", "if": "paused", "x": 384, "y": 10, "size": 14, "icon": "pause", "color": "orange"},
                           T(400, 0, 34, 33, "{battery}", "small", "muted", "right"),
                           *battery_part(440, 10)]
    s.parts["title"] = [T(22, 0, 340, 33, "{screen}", "bartitle", "bar_text", case="upper", tracking=2)]
    np = [P("titlebar"), T(22, 0, 340, 33, "Now playing", "bartitle", "bar_text", case="upper", tracking=2),
          {"type": "art", "x": 22, "y": 50, "size": 196},
          T(236, 52, 230, 54, "{title}", "big", lines=2, line_height=27),
          T(236, 112, 230, 22, "{artist}", "detail", "orange"),
          T(236, 134, 230, 22, "{album}", "detail", "muted"),
          T(236, 162, 230, 18, "{quality}", "small", "dim", case="upper"),
          T(236, 190, 230, 22, "{queue_index} / {queue_length}", "small", "dim"),
          {"type": "icon", "if": "shuffle", "x": 236, "y": 220, "size": 18, "icon": "shuffle", "color": "orange"},
          {"type": "icon", "if": "repeat", "x": 262, "y": 220, "size": 18, "icon": "repeat_mode", "color": "orange"},
          {"type": "bar", "x": 22, "y": 276, "w": 436, "h": 4, "value": "progress", "track": "#333333", "fill": "orange", "knob": "orange", "knob_radius": 6},
          T(22, 290, 100, 24, "{elapsed}", "time"), T(358, 290, 100, 24, "{remaining}", "time", "muted", "right"),
          T(22, 322, 436, 30, "{output}", "small", "dim", "center", case="upper", tracking=1)]
    home = [R(300, 36, 1, 324, "#262626"),
            {"type": "art", "if": "has_art", "x": 320, "y": 60, "size": 140},
            T(306, 210, 168, 22, "{title}", "title2", "text", "center", **{"if": "has_track"}),
            T(306, 232, 168, 20, "{artist}", "body", "orange", "center", **{"if": "has_track"}),
            {"type": "bar", "if": "has_track", "x": 320, "y": 266, "w": 140, "h": 3, "value": "progress", "track": "#333333", "fill": "orange"},
            T(306, 140, 168, 22, "No music playing", "small", "dim", "center", **{"if": "!has_track"}),
            rows_list(st, 36, 8, w=300), P("titlebar"), T(22, 0, 300, 33, "Reborn", "bartitle", "bar_text", case="upper", tracking=2)]
    s.parts["row300"] = row_elements(st, 300, False)
    s.parts["row300_focused"] = row_elements(st, 300, True)
    standard(s, st, np, home)
    return s


def mini(colour_id, name, bar_top, bar_bottom, hl_top, hl_bottom, accent):
    s = Skin(f"mini-{colour_id}", f"Mini {name}", f"Classic menus in {name.lower()}, after the coloured minis.")
    s.palette = {"bg": "#FFFFFF", "text": "#000000", "muted": "#4F545C", "dim": "#6B7079", "accent": accent,
                 "glass": "#000000", "scrim": "#FFFFFF", "sheet": "#FFFFFF", "track": "#D5D7DB",
                 "danger": "#C5221F", "success": "#1E8E3E", "warning": "#A35A00"}
    s.colors = {"bar_text": "#000000", "panel": bar_top, "accent_text": "#FFFFFF", "white": "#FFFFFF", "pale": "#FFFFFFD0"}
    s.fonts = {"menu": ("OpenSans-Bold", 17, None), "title2": ("OpenSans-Bold", 15, None),
               "bartitle": ("OpenSans-Bold", 15, None), "big": ("OpenSans-Bold", 19, None),
               "detail": ("OpenSans-Regular", 15, None), "body": ("OpenSans-Regular", 14, None),
               "small": ("OpenSans-SemiBold", 12, None)}
    s.images = {"titlebar": line(vgrad(4, 30, [(0, "#FFFFFF"), (1, bar_top)]), 29, bar_bottom),
                "highlight": line(vgrad(4, 33, [(0, hl_top), (1, hl_bottom)]), 0, "#FFFFFF80"),
                "track": frame(vgrad(8, 12, [(0, "#C9CBCF"), (1, "#F1F2F4")]), "#8F939A"),
                "fill": frame(vgrad(8, 12, [(0, hl_top), (1, hl_bottom)]), bar_bottom),
                "battery": battery_pictures("#4A4E55"), "battery_fill": vgrad(4, 10, [(0, "#9BE38B"), (1, "#3AA32C")]),
                "battery_low": vgrad(4, 10, [(0, "#FF9C8A"), (1, "#D93025")])}
    st = Style(row_h=33, top=30, ffg="white", fmuted="pale", icon_ffg="white",
               highlight=lambda w, h: [I(0, 0, w, h, "highlight")],
               bar_track_image="track", bar_fill_image="fill", bar_slice=[3, 3, 3, 3],
               scroll={"x": -7, "w": 5, "track": "#E4E6E9", "thumb": hl_bottom}, sheet_frame=bar_bottom, scrim="#00000059")
    s.parts["titlebar"] = [I(0, 0, 480, 30, "titlebar"),
                           {"type": "icon", "if": "playing", "x": 10, "y": 7, "size": 16, "icon": "play", "color": hl_bottom},
                           {"type": "icon", "if": "paused", "x": 10, "y": 7, "size": 16, "icon": "pause", "color": hl_bottom},
                           {"type": "icon", "if": "charging", "x": 420, "y": 7, "size": 16, "icon": "bolt", "color": "success"},
                           *battery_part(440, 8)]
    s.parts["title"] = [T(60, 0, 360, 30, "{screen}", "bartitle", "bar_text", "center")]
    np = [P("titlebar"), T(60, 0, 360, 30, "{queue_index} of {queue_length}", "bartitle", "bar_text", "center"),
          {"type": "art", "x": 24, "y": 48, "size": 180},
          T(222, 50, 236, 50, "{title}", "big", lines=2, line_height=25),
          T(222, 104, 236, 22, "{artist}", "detail"), T(222, 126, 236, 22, "{album}", "detail", "muted"),
          T(222, 152, 236, 18, "{quality}", "small", "dim", case="upper"),
          {"type": "icon", "if": "shuffle", "x": 222, "y": 180, "size": 20, "icon": "shuffle", "color": "accent"},
          {"type": "icon", "if": "repeat", "x": 252, "y": 180, "size": 20, "icon": "repeat_mode", "color": "accent"},
          bar(st, 24, 252, 432, 12, "progress"),
          T(24, 268, 100, 20, "{elapsed}", "small"), T(356, 268, 100, 20, "{remaining}", "small", "text", "right"),
          R(0, 302, 480, 1, "#C8CBD0"), T(24, 310, 432, 40, "{output}", "body", "muted", "center")]
    home = home_split(st, panel=[
        {"type": "gradient", "x": 240, "y": 30, "w": 240, "h": 330, "top": "#FFFFFF", "bottom": bar_top},
        R(240, 30, 1, 330, bar_bottom),
        {"type": "art", "if": "has_art", "x": 270, "y": 58, "size": 180},
        {"type": "icon", "if": "!has_art", "x": 320, "y": 108, "size": 80, "icon": "note", "color": hl_bottom},
        T(250, 246, 220, 22, "{title}", "title2", "text", "center", **{"if": "has_track"}),
        T(250, 268, 220, 20, "{artist}", "body", "muted", "center", **{"if": "has_track"}),
        bar(st, 270, 300, 180, 10, "progress") | {"if": "has_track"},
        T(250, 246, 220, 22, "Nothing playing", "title2", "muted", "center", **{"if": "!has_track"})])
    standard(s, st, np, home)
    return s


def terminal():
    s = Skin("terminal", "Terminal", "Green phosphor on black, in a monospace font.")
    s.palette = {"bg": "#050805", "text": "#3DFF6E", "muted": "#2BC456", "dim": "#21A045", "accent": "#3DFF6E",
                 "glass": "#3DFF6E", "scrim": "#000000", "sheet": "#050805", "track": "#123A1C",
                 "danger": "#FF5050", "success": "#3DFF6E", "warning": "#FFD23D"}
    s.colors = {"bar_text": "#3DFF6E", "panel": "#071007", "accent_text": "#050805", "ink": "#050805"}
    s.fonts = {"menu": ("NotoSansMono-Regular", 16, None), "title2": ("NotoSansMono-Bold", 14, None),
               "bartitle": ("NotoSansMono-Bold", 14, None), "big": ("NotoSansMono-Bold", 18, None),
               "detail": ("NotoSansMono-Regular", 14, None), "body": ("NotoSansMono-Regular", 13, None),
               "small": ("NotoSansMono-Regular", 12, None)}
    s.images = {"scan": Image.new("RGBA", (2, 2), (0, 0, 0, 0))}
    px = s.images["scan"]
    px.putpixel((0, 1), (0, 0, 0, 70))
    px.putpixel((1, 1), (0, 0, 0, 70))
    st = Style(row_h=30, top=32, prefix="> ", ffg="ink", fmuted="ink", icon_ffg="ink",
               highlight=lambda w, h: [R(0, 0, w, h, "text")],
               scroll={"x": -6, "w": 4, "track": "#0F2A15", "thumb": "text"}, bar_radius=0,
               toast_bg="#050805F0", toast_fg="#3DFF6E", toast_error="#3A0000F0", sheet_frame="text", sheet_radius=0,
               scrim="#000000B0", fill="text", track="#123A1C")
    s.parts["titlebar"] = [R(0, 29, 480, 1, "text"),
                           T(380, 0, 92, 29, "[{battery}%]", "small", "text", "right")]
    s.parts["title"] = [T(10, 0, 360, 29, "reborn:~/{screen} $", "bartitle", "bar_text")]
    np = [P("titlebar"), T(10, 0, 360, 29, "reborn:~/now_playing $", "bartitle", "bar_text"),
          T(16, 44, 300, 22, "TITLE  : {title}", "detail"), T(16, 68, 300, 22, "ARTIST : {artist}", "detail"),
          T(16, 92, 300, 22, "ALBUM  : {album}", "detail"), T(16, 116, 300, 22, "FORMAT : {quality}", "detail", "muted"),
          T(16, 140, 300, 22, "QUEUE  : {queue_index}/{queue_length}", "detail", "muted"),
          T(16, 164, 300, 22, "STATE  : {playback}", "detail", "muted", case="upper"),
          T(16, 188, 300, 22, "MODE   : shuffle", "detail", "muted", **{"if": "shuffle"}),
          {"type": "art", "x": 330, "y": 44, "size": 134},
          R(328, 42, 138, 138, "text", stroke=1),
          T(16, 236, 448, 20, "PROGRESS", "small", "muted"),
          R(16, 258, 448, 16, "text", stroke=1),
          {"type": "bar", "x": 19, "y": 261, "w": 442, "h": 10, "value": "progress", "fill": "text"},
          T(16, 280, 448, 22, "[{elapsed} / {duration}]", "detail"),
          T(16, 318, 448, 24, "out: {output}_", "small", "dim"),
          I(0, 0, 480, 360, "scan")]
    standard(s, st, np, home=[rows_list(st, 32, 10),
                              P("titlebar"), T(10, 0, 360, 29, "reborn:~ $", "bartitle", "bar_text"),
                              T(250, 300, 214, 24, "> {title}", "small", "dim", "right", **{"if": "has_track"}),
                              I(0, 0, 480, 360, "scan")])
    for k in ("list", "page"):
        s.screens[k] = s.screens[k] + [I(0, 0, 480, 360, "scan")]
    return s


def winamp():
    s = Skin("winamp", "Winamp", "A dark beveled player with a green digital display and a playlist.")
    s.palette = {"bg": "#2B2B3A", "text": "#E8E8F0", "muted": "#B7B7C8", "dim": "#9A9AB0", "accent": "#00E000",
                 "glass": "#FFFFFF", "scrim": "#000000", "sheet": "#2B2B3A", "track": "#1A1A24"}
    s.colors = {"bar_text": "#F0E6A0", "panel": "#000000", "accent_text": "#000000", "lcd": "#00E000",
                "lcd_dim": "#00A800", "sel": "#2B3BD8", "white": "#FFFFFF"}
    s.fonts = {"menu": ("NotoSansMono-Bold", 15, None), "title2": ("NotoSansMono-Bold", 13, None),
               "bartitle": ("NotoSans-Bold", 12, None), "big": ("NotoSans-Bold", 18, None),
               "detail": ("NotoSansMono-Regular", 13, None), "body": ("NotoSans-Regular", 13, None),
               "small": ("NotoSansMono-Regular", 11, None), "lcd": ("NotoSansMono-Bold", 38, None)}
    s.images = {"titlebar": vgrad(4, 24, [(0, "#4A4A70"), (0.5, "#262640"), (1, "#14141F")]),
                "stripes": Image.new("RGBA", (6, 24), (0, 0, 0, 0)),
                "panel": bevel(16, 16, "#000000", "#14141C", "#5A5A78", inset=True),
                "frame": bevel(16, 16, "#2B2B3A", "#5C5C7A", "#14141C"),
                "seek_track": bevel(8, 10, "#1B1B26", "#0E0E14", "#4A4A64", inset=True),
                "seek_fill": vgrad(8, 10, [(0, "#E8D57A"), (1, "#9C8A2E")]),
                "battery": battery_pictures("#B7B7C8"), "battery_fill": vgrad(4, 10, [(0, "#3BE33B"), (1, "#00A800")]),
                "battery_low": vgrad(4, 10, [(0, "#FF6B5E"), (1, "#D93025")])}
    for y in (8, 11, 14):
        for x in range(6):
            s.images["stripes"].putpixel((x, y), rgba("#C9B95A"))
    st = Style(row_h=26, top=44, pad=12, fg="lcd", muted="lcd_dim", dim="lcd_dim", ffg="white", fmuted="white",
               icon_fg="lcd_dim", icon_ffg="white", highlight=lambda w, h: [R(0, 0, w, h, "sel")],
               scroll={"x": -8, "w": 6, "track": "#14141C", "thumb": "#5A5A78"},
               bar_track_image="seek_track", bar_fill_image="seek_fill", bar_slice=[2, 2, 2, 2],
               sheet_bg="#2B2B3A", sheet_frame="#5C5C7A", sheet_radius=0, toast_bg="#000000F0", toast_fg="#00E000")
    s.parts["titlebar"] = [I(0, 0, 480, 24, "titlebar"), I(8, 0, 120, 24, "stripes"), I(352, 0, 50, 24, "stripes"),
                           T(410, 0, 30, 24, "{battery}", "small", "bar_text", "right"), *battery_part(444, 5)]
    s.parts["title"] = [T(130, 0, 220, 24, "REBORN · {screen}", "bartitle", "bar_text", "center", case="upper", tracking=1)]
    s.parts["playlist"] = [I(4, 28, 472, 328, "frame", slice=[3, 3, 3, 3]), I(8, 40, 464, 312, "panel", slice=[3, 3, 3, 3])]
    np = [P("titlebar"), T(130, 0, 220, 24, "REBORN", "bartitle", "bar_text", "center", tracking=2),
          I(4, 28, 472, 328, "frame", slice=[3, 3, 3, 3]),
          I(12, 34, 456, 116, "panel", slice=[3, 3, 3, 3]),
          {"type": "icon", "if": "playing", "x": 22, "y": 48, "size": 14, "icon": "play", "color": "lcd"},
          {"type": "icon", "if": "paused", "x": 22, "y": 48, "size": 14, "icon": "pause", "color": "lcd"},
          T(40, 40, 170, 50, "{elapsed}", "lcd", "lcd"),
          T(214, 42, 244, 22, "{queue_index}. {artist} - {title}", "detail", "lcd"),
          T(214, 66, 244, 20, "{quality}", "small", "lcd_dim", case="upper"),
          T(214, 88, 244, 20, "{album}", "small", "lcd_dim"),
          T(22, 112, 200, 26, "shuffle", "small", "lcd", **{"if": "shuffle"}),
          T(92, 112, 200, 26, "repeat", "small", "lcd", **{"if": "repeat"}),
          T(334, 112, 124, 26, "-{duration}", "small", "lcd_dim", "right"),
          bar(st, 12, 160, 456, 12, "progress"),
          I(12, 184, 160, 164, "panel", slice=[3, 3, 3, 3]),
          {"type": "art", "x": 18, "y": 190, "size": 148},
          I(182, 184, 286, 164, "panel", slice=[3, 3, 3, 3]),
          T(194, 192, 262, 20, "VOLUME", "small", "lcd_dim"),
          {"type": "bar", "x": 194, "y": 214, "w": 262, "h": 8, "value": "volume", "track_image": "seek_track", "fill_image": "seek_fill", "slice": [2, 2, 2, 2]},
          T(194, 236, 262, 20, "OUTPUT", "small", "lcd_dim"),
          T(194, 256, 262, 22, "{output}", "detail", "lcd"),
          T(194, 288, 262, 20, "{playback}", "small", "lcd", case="upper")]
    home = [P("playlist"), rows_list(st, 44, 11, w=464, x=8), P("titlebar"),
            T(130, 0, 220, 24, "REBORN", "bartitle", "bar_text", "center", tracking=2)]
    s.parts["row464"] = row_elements(st, 464, False)
    s.parts["row464_focused"] = row_elements(st, 464, True)
    standard(s, st, np, home)
    out = [P("playlist")]
    for e in s.screens["list"]:
        if e.get("type") == "list":
            e = rows_list(st, 44, 11 if e["if"].startswith("!has_status") else 10, w=464, x=8, cond=e["if"])
        out.append(e)
    s.screens["list"] = out
    prune(s)
    return s


def paper():
    s = Skin("paper", "Paper", "Black ink on warm paper with a book typeface, like an e-reader.")
    s.palette = {"bg": "#F3EFE6", "text": "#161513", "muted": "#47443E", "dim": "#625E56", "accent": "#161513",
                 "glass": "#161513", "scrim": "#F3EFE6", "sheet": "#F3EFE6", "track": "#D2CCBF",
                 "danger": "#9E1B12", "success": "#2E6B2E", "warning": "#7A4A00"}
    s.colors = {"bar_text": "#161513", "panel": "#ECE7DC", "accent_text": "#F3EFE6", "paper": "#F3EFE6", "rule": "#CFC8B9"}
    s.fonts = {"menu": ("NotoSerif-Regular", 18, None), "title2": ("NotoSerif-Bold", 15, None),
               "bartitle": ("NotoSerif-Bold", 15, None), "big": ("NotoSerif-Bold", 21, None),
               "detail": ("NotoSerif-Italic", 15, None), "body": ("NotoSerif-Regular", 14, None),
               "small": ("NotoSerif-Regular", 12, None)}
    st = Style(row_h=38, top=36, pad=18, ffg="paper", fmuted="paper", icon_ffg="paper", icon_fg="muted",
               highlight=lambda w, h: [R(6, 2, w - 12, h - 4, "text")],
               scroll={"x": -5, "w": 2, "track": "rule", "thumb": "text"}, bar_radius=0, fill="text", track="rule",
               sheet_frame="text", sheet_radius=0, scrim="#F3EFE6C0", toast_bg="#161513F0", toast_fg="#F3EFE6")
    s.parts["titlebar"] = [R(18, 34, 444, 1, "text"), T(380, 0, 82, 34, "{battery} %", "small", "muted", "right"),
                           {"type": "icon", "if": "playing", "x": 18, "y": 11, "size": 12, "icon": "play", "color": "muted"},
                           {"type": "icon", "if": "paused", "x": 18, "y": 11, "size": 12, "icon": "pause", "color": "muted"}]
    s.parts["title"] = [T(80, 0, 320, 34, "{screen}", "bartitle", "bar_text", "center")]
    s.parts["rule"] = []
    np = [P("titlebar"), T(80, 0, 320, 34, "Now Playing", "bartitle", "bar_text", "center"),
          {"type": "art", "x": 24, "y": 52, "size": 170}, R(23, 51, 172, 172, "text", stroke=1),
          T(214, 54, 248, 56, "{title}", "big", lines=2, line_height=28),
          T(214, 114, 248, 24, "{artist}", "detail"), T(214, 138, 248, 22, "{album}", "body", "muted"),
          T(214, 168, 248, 20, "{quality}", "small", "dim"),
          T(214, 192, 248, 20, "Track {queue_index} of {queue_length}", "small", "dim"),
          {"type": "bar", "x": 24, "y": 258, "w": 432, "h": 3, "value": "progress", "track": "rule", "fill": "text"},
          T(24, 266, 100, 22, "{elapsed}", "small"), T(356, 266, 100, 22, "{remaining}", "small", "text", "right"),
          R(18, 306, 444, 1, "rule"), T(24, 312, 432, 36, "{output}", "small", "muted", "center")]
    home = [P("titlebar"), T(80, 0, 320, 34, "Reborn", "bartitle", "bar_text", "center"),
            rows_list(st, 38, 7, w=480),
            R(18, 308, 444, 1, "rule", **{"if": "has_track"}),
            T(18, 310, 444, 28, "{title} · {artist}", "detail", "text", "center", **{"if": "has_track"}),
            {"type": "bar", "if": "has_track", "x": 140, "y": 344, "w": 200, "h": 2, "value": "progress", "track": "rule", "fill": "text"}]
    standard(s, st, np, home)
    return s


def big_type():
    s = Skin("big-type", "Big Type", "Five large rows, yellow on black, for reading at a glance.")
    s.palette = {"bg": "#000000", "text": "#FFFFFF", "muted": "#E6E6E6", "dim": "#C4C4C4", "accent": "#FFD600",
                 "glass": "#FFFFFF", "sheet": "#111111", "track": "#3A3A3A"}
    s.colors = {"bar_text": "#FFD600", "panel": "#111111", "accent_text": "#000000", "ink": "#000000", "yellow": "#FFD600"}
    s.fonts = {"menu": ("NotoSans-Black", 30, None), "title2": ("NotoSans-Bold", 22, None),
               "bartitle": ("NotoSans-Bold", 22, None), "big": ("NotoSans-Black", 28, None),
               "detail": ("NotoSans-Bold", 20, None), "body": ("NotoSans-Bold", 18, None),
               "small": ("NotoSans-Bold", 16, None)}
    st = Style(row_h=63, top=44, pad=16, ffg="ink", fmuted="ink", icon_ffg="ink", icon_fg="yellow",
               highlight=lambda w, h: [R(4, 3, w - 8, h - 6, "yellow", radius=8)],
               scroll={"x": -6, "w": 5, "track": "#262626", "thumb": "yellow"}, bar_radius=6, fill="yellow",
               sheet_frame="yellow", toast_bg="#FFD600F5", toast_fg="#000000", scrim="#000000C8")
    s.parts["titlebar"] = [R(0, 42, 480, 2, "yellow"), T(330, 0, 136, 42, "{battery}%", "bartitle", "text", "right"),
                           {"type": "icon", "if": "charging", "x": 330, "y": 10, "size": 22, "icon": "bolt", "color": "yellow"}]
    s.parts["title"] = [T(14, 0, 320, 42, "{screen}", "bartitle", "bar_text")]
    np = [P("titlebar"), T(14, 0, 320, 42, "Playing", "bartitle", "bar_text", **{"if": "playing"}),
          T(14, 0, 320, 42, "Paused", "bartitle", "bar_text", **{"if": "!playing"}),
          T(16, 54, 448, 80, "{title}", "big", lines=2, line_height=38),
          T(16, 140, 448, 30, "{artist}", "detail", "yellow"),
          T(16, 172, 448, 28, "{album}", "body", "muted"),
          {"type": "bar", "x": 16, "y": 226, "w": 448, "h": 22, "value": "progress", "track": "#3A3A3A", "fill": "yellow", "radius": 11},
          T(16, 254, 200, 34, "{elapsed}", "title2"), T(264, 254, 200, 34, "{remaining}", "title2", "text", "right"),
          T(16, 300, 448, 30, "Song {queue_index} of {queue_length}", "body", "muted"),
          T(16, 328, 448, 28, "{output}", "small", "dim")]
    home = [P("titlebar"), T(14, 0, 320, 42, "Reborn", "bartitle", "bar_text"), rows_list(st, 44, 5)]
    standard(s, st, np, home)
    return s


def cassette():
    s = Skin("cassette", "Cassette", "Warm beige and brown with a tape for Now Playing.")
    s.palette = {"bg": "#E9DFC9", "text": "#2B1D12", "muted": "#5A4431", "dim": "#6C5743", "accent": "#B8471F",
                 "glass": "#2B1D12", "scrim": "#E9DFC9", "sheet": "#F3EBDA", "track": "#CDBFA3",
                 "danger": "#9E1B12", "success": "#3D6B2E", "warning": "#7A4A00"}
    s.colors = {"bar_text": "#F3EBDA", "panel": "#DCCFB3", "accent_text": "#F3EBDA", "cream": "#F3EBDA", "rust": "#B8471F"}
    s.fonts = {"menu": ("NotoSansMono-Bold", 17, None), "title2": ("NotoSansMono-Bold", 14, None),
               "bartitle": ("NotoSansMono-Bold", 14, None), "big": ("NotoSansMono-Bold", 18, None),
               "detail": ("NotoSansMono-Regular", 14, None), "body": ("NotoSans-Regular", 14, None),
               "small": ("NotoSansMono-Regular", 12, None)}
    # The tape: brown shell, cream label with ruled lines, a window with two reels.
    tape = Image.new("RGBA", (448, 230), (0, 0, 0, 0))
    d = ImageDraw.Draw(tape)
    d.rounded_rectangle([0, 0, 447, 229], 18, fill=rgba("#3B2A1E"))
    d.rounded_rectangle([14, 12, 433, 150], 8, fill=rgba("#F3EBDA"))
    d.rectangle([14, 26, 433, 30], fill=rgba("#B8471F"))
    for y in (70, 96, 122):
        d.line([(126, y), (420, y)], fill=rgba("#CDBFA3"), width=1)
    d.rounded_rectangle([110, 160, 337, 214], 24, fill=rgba("#1E1610"))
    for cx in (150, 297):
        d.ellipse([cx - 22, 165, cx + 22, 209], fill=rgba("#E9DFC9"))
        d.ellipse([cx - 9, 178, cx + 9, 196], fill=rgba("#1E1610"))
        for a in range(0, 360, 60):
            x, y = cx + 7 * math.cos(math.radians(a)), 187 + 7 * math.sin(math.radians(a))
            d.ellipse([x - 2, y - 2, x + 2, y + 2], fill=rgba("#E9DFC9"))
    d.rectangle([180, 182, 267, 192], fill=rgba("#5A3E28"))
    for x, y in ((24, 196), (414, 196), (24, 22), (414, 22)):
        d.ellipse([x - 5, y - 5, x + 5, y + 5], fill=rgba("#6C5743"))
    s.images = {"tape": tape,
                "label": rounded(vgrad(40, 30, [(0, "#C9572B"), (1, "#A23C17")]), 6),
                "strip": vgrad(4, 32, [(0, "#4A3426"), (1, "#3B2A1E")]),
                "battery": battery_pictures("#F3EBDA"), "battery_fill": vgrad(4, 10, [(0, "#E9DFC9"), (1, "#CDBFA3")]),
                "battery_low": vgrad(4, 10, [(0, "#FF9C8A"), (1, "#D93025")])}
    st = Style(row_h=34, top=32, pad=16, ffg="cream", fmuted="cream", icon_ffg="cream",
               highlight=lambda w, h: [I(4, 2, w - 8, h - 4, "label", slice=[8, 8, 8, 8])],
               scroll={"x": -6, "w": 4, "track": "#D8CCB2", "thumb": "#5A4431"}, fill="rust", track="#CDBFA3",
               sheet_frame="#5A4431", toast_bg="#3B2A1EF0", toast_fg="#F3EBDA", scrim="#2B1D1260")
    s.parts["titlebar"] = [I(0, 0, 480, 32, "strip"), *battery_part(440, 9),
                           {"type": "icon", "if": "playing", "x": 12, "y": 9, "size": 14, "icon": "play", "color": "cream"},
                           {"type": "icon", "if": "paused", "x": 12, "y": 9, "size": 14, "icon": "pause", "color": "cream"}]
    s.parts["title"] = [T(60, 0, 360, 32, "{screen}", "bartitle", "bar_text", "center", case="upper", tracking=2)]
    np = [P("titlebar"), T(60, 0, 360, 32, "Side A · {queue_index}/{queue_length}", "bartitle", "bar_text", "center", tracking=1),
          I(16, 44, 448, 230, "tape"),
          {"type": "art", "x": 34, "y": 82, "size": 96},
          T(144, 58, 300, 26, "{title}", "big", "text"),
          T(144, 88, 300, 24, "{artist}", "detail", "muted"),
          T(144, 114, 300, 24, "{album}", "detail", "muted"),
          T(144, 140, 300, 24, "{quality}", "small", "dim", case="upper"),
          {"type": "bar", "x": 199, "y": 228, "w": 80, "h": 6, "value": "progress", "track": "#5A3E28", "fill": "#E9DFC9"},
          {"type": "bar", "x": 24, "y": 292, "w": 432, "h": 8, "value": "progress", "track": "track", "fill": "rust", "radius": 4},
          T(24, 304, 100, 22, "{elapsed}", "small"), T(356, 304, 100, 22, "{remaining}", "small", "text", "right"),
          T(24, 328, 432, 28, "{output}", "small", "dim", "center")]
    home = home_split(st, panel=[
        R(240, 32, 240, 328, "panel"), R(240, 32, 2, 328, "#CDBFA3"),
        {"type": "art", "if": "has_art", "x": 280, "y": 56, "size": 160},
        R(278, 54, 164, 164, "#3B2A1E", stroke=2, **{"if": "has_art"}),
        {"type": "icon", "if": "!has_art", "x": 320, "y": 96, "size": 80, "icon": "note", "color": "dim"},
        T(250, 232, 220, 22, "{title}", "title2", "text", "center", **{"if": "has_track"}),
        T(250, 254, 220, 20, "{artist}", "small", "muted", "center", **{"if": "has_track"}),
        {"type": "bar", "if": "has_track", "x": 280, "y": 286, "w": 160, "h": 6, "value": "progress", "track": "track", "fill": "rust", "radius": 3},
        T(250, 232, 220, 22, "No tape inserted", "title2", "muted", "center", **{"if": "!has_track"})])
    standard(s, st, np, home)
    return s


def neon():
    s = Skin("neon", "Neon Night", "Synthwave purple with pink and cyan glow.")
    s.palette = {"bg": "#12081F", "text": "#F7EEFF", "muted": "#CBB2E8", "dim": "#AA8ECB", "accent": "#FF3EA5",
                 "glass": "#FFFFFF", "scrim": "#000000", "sheet": "#1E0F33", "track": "#3A2357"}
    s.colors = {"bar_text": "#3EF0FF", "panel": "#1A0C2C", "accent_text": "#FFFFFF", "cyan": "#3EF0FF", "white": "#FFFFFF", "pink": "#FF3EA5"}
    s.fonts = {"menu": ("NotoSans-Bold", 18, None), "title2": ("NotoSans-Bold", 15, None),
               "bartitle": ("NotoSans-Bold", 13, None), "big": ("NotoSans-Black", 21, None),
               "detail": ("NotoSans-Regular", 15, None), "body": ("NotoSans-Regular", 14, None),
               "small": ("NotoSans-Medium", 12, None)}
    hl = rounded(hgrad(64, 34, "#FF3EA5", "#7B3EFF"), 12)
    s.images = {"sky": vgrad(4, 120, [(0, "#2C0F4A"), (1, "#12081F")]),
                "highlight": hl, "glow": glow(240, "#FF3EA5", 0.9), "glow_cyan": glow(200, "#3EF0FF", 0.5),
                "fill": rounded(hgrad(64, 10, "#3EF0FF", "#FF3EA5"), 5),
                "track": rounded(Image.new("RGBA", (16, 10), rgba("#3A2357")), 5),
                "battery": battery_pictures("#CBB2E8"), "battery_fill": hgrad(4, 10, "#3EF0FF", "#FF3EA5"),
                "battery_low": vgrad(4, 10, [(0, "#FF6B5E"), (1, "#D93025")])}
    st = Style(row_h=36, top=40, pad=18, ffg="white", fmuted="white", icon_ffg="white",
               highlight=lambda w, h: [I(6, 1, w - 12, h - 2, "highlight", slice=[13, 13, 13, 13])],
               scroll={"x": -6, "w": 4, "track": "#2A1745", "thumb": "cyan"},
               bar_track_image="track", bar_fill_image="fill", bar_slice=[5, 4, 5, 4],
               sheet_frame="pink", sheet_radius=14, toast_bg="#2A0F45F0", toast_fg="#3EF0FF")
    sky = [I(0, 0, 480, 120, "sky")]
    s.parts["titlebar"] = [{"type": "gradient", "x": 18, "y": 38, "w": 444, "h": 2, "top": "cyan", "bottom": "pink"},
                           T(392, 0, 42, 38, "{battery}", "small", "muted", "right"), *battery_part(440, 12)]
    s.parts["title"] = [T(18, 0, 360, 38, "{screen}", "bartitle", "bar_text", case="upper", tracking=3)]
    np = [*sky, P("titlebar"), T(18, 0, 360, 38, "Now playing", "bartitle", "bar_text", case="upper", tracking=3),
          I(-10, 20, 260, 260, "glow"), {"type": "art", "x": 30, "y": 60, "size": 180, "radius": 0},
          I(330, 120, 200, 200, "glow_cyan"),
          T(232, 62, 232, 56, "{title}", "big", lines=2, line_height=28),
          T(232, 122, 232, 22, "{artist}", "detail", "pink"),
          T(232, 146, 232, 22, "{album}", "detail", "muted"),
          T(232, 174, 232, 18, "{quality}", "small", "cyan", case="upper", tracking=1),
          {"type": "icon", "if": "shuffle", "x": 232, "y": 202, "size": 20, "icon": "shuffle", "color": "cyan"},
          {"type": "icon", "if": "repeat", "x": 262, "y": 202, "size": 20, "icon": "repeat_mode", "color": "cyan"},
          bar(st, 30, 268, 420, 10, "progress"),
          T(30, 284, 100, 20, "{elapsed}", "small", "cyan"), T(350, 284, 100, 20, "{remaining}", "small", "cyan", "right"),
          T(30, 316, 420, 30, "{output}", "small", "dim", "center", case="upper", tracking=2)]
    home = [*sky, I(270, 60, 240, 240, "glow"),
            {"type": "art", "if": "has_art", "x": 300, "y": 90, "size": 150},
            T(270, 252, 200, 22, "{title}", "title2", "text", "center", **{"if": "has_track"}),
            T(270, 274, 200, 20, "{artist}", "small", "pink", "center", **{"if": "has_track"}),
            T(270, 170, 200, 22, "Press play", "title2", "muted", "center", **{"if": "!has_track"}),
            rows_list(st, 44, 8, w=260), P("titlebar"),
            T(18, 0, 360, 38, "Reborn", "bartitle", "bar_text", case="upper", tracking=3)]
    s.parts["row260"] = row_elements(st, 260, False)
    s.parts["row260_focused"] = row_elements(st, 260, True)
    standard(s, st, np, home)
    for k in ("list", "page"):
        s.screens[k] = [*sky] + s.screens[k]
    return s


def zune():
    s = Skin("zune", "Zune", "Huge lowercase headings, plain lists and a magenta accent.")
    s.palette = {"bg": "#000000", "text": "#FFFFFF", "muted": "#A6A6A6", "dim": "#8A8A8A", "accent": "#EC008C",
                 "glass": "#FFFFFF", "sheet": "#141414", "track": "#2E2E2E"}
    s.colors = {"bar_text": "#3A3A3A", "panel": "#000000", "accent_text": "#FFFFFF", "ghost": "#2E2E2E", "magenta": "#EC008C"}
    s.fonts = {"huge": ("NotoSans-Light", 64, "ascii"), "menu": ("NotoSans-Light", 28, None),
               "title2": ("NotoSans-Regular", 16, None), "bartitle": ("NotoSans-Regular", 14, None),
               "big": ("NotoSans-Light", 24, None), "detail": ("NotoSans-Light", 16, None),
               "body": ("NotoSans-Regular", 14, None), "small": ("NotoSans-Regular", 12, None)}
    st = Style(row_h=38, top=86, pad=20, case="lower", fg="dim", muted="#6E6E6E", ffg="text", fmuted="muted",
               icon_fg="dim", icon_ffg="magenta", highlight=lambda w, h: [], scroll=None, fill="magenta",
               bar_radius=0, sheet_frame="#333333", sheet_radius=0, toast_bg="#EC008CF0", toast_fg="#FFFFFF")
    s.parts["titlebar"] = [T(380, 0, 92, 26, "{battery}", "small", "muted", "right"),
                           {"type": "icon", "if": "playing", "x": 340, "y": 7, "size": 12, "icon": "play", "color": "magenta"}]
    s.parts["title"] = [T(10, 2, 900, 84, "{screen}", "huge", "bar_text", case="lower")]
    np = [T(-6, 0, 1000, 92, "{artist}", "huge", "ghost", case="lower"), P("titlebar"),
          {"type": "art", "x": 20, "y": 98, "size": 170},
          T(208, 100, 256, 32, "{title}", "big", "text", case="lower"),
          T(208, 134, 256, 22, "{artist}", "detail", "muted", case="lower"),
          T(208, 156, 256, 22, "{album}", "detail", "dim", case="lower"),
          T(208, 186, 256, 20, "{queue_index} of {queue_length}", "small", "dim"),
          {"type": "icon", "if": "shuffle", "x": 208, "y": 214, "size": 18, "icon": "shuffle", "color": "magenta"},
          {"type": "icon", "if": "repeat", "x": 236, "y": 214, "size": 18, "icon": "repeat_mode", "color": "magenta"},
          {"type": "bar", "x": 20, "y": 290, "w": 440, "h": 3, "value": "progress", "track": "#2E2E2E", "fill": "magenta"},
          T(20, 298, 100, 22, "{elapsed}", "small", "muted"), T(360, 298, 100, 22, "{remaining}", "small", "muted", "right"),
          T(20, 326, 440, 26, "{output}", "small", "dim", case="lower")]
    home = [T(10, 2, 900, 84, "reborn", "huge", "bar_text"), P("titlebar"),
            rows_list(st, 86, 7, w=300),
            {"type": "art", "if": "has_art", "x": 320, "y": 110, "size": 140},
            T(316, 258, 156, 22, "{title}", "title2", "text", case="lower", **{"if": "has_track"}),
            T(316, 280, 156, 20, "{artist}", "small", "magenta", case="lower", **{"if": "has_track"})]
    s.parts["row300"] = row_elements(st, 300, False)
    s.parts["row300_focused"] = row_elements(st, 300, True)
    standard(s, st, np, home)
    s.screens["sheet"] = [R(0, 0, 480, 360, "#000000E6"),
                          T(10, 6, 900, 84, "{sheet_title}", "huge", "#5A5A5A", case="lower"),
                          T(24, 92, 432, 38, "{sheet_body}", "body", "muted", lines=2, line_height=19),
                          {"type": "list", "x": 0, "y": 136, "w": 480, "rows": 5, "row_height": 38, "source": "sheet",
                           "row": [P("row")], "focused": [P("row_focused")]}]
    prune(s)
    return s


# ------------------------------------------------------------------ Bunny Bow
# Pixel art on a half-size grid, doubled without smoothing so every art pixel
# is 2 × 2 on the screen. Inspired by the cute pixel themes of Rockbox; all of
# it is drawn here.

INK = "#6B2450"
BUNNY_COLOURS = {
    "k": INK, "w": "#FFFFFF", "p": "#FFB3CC", "c": "#FF8DB5", "r": "#E8364F", "R": "#B0243F",
    "d": "#FF8FB8", "D": "#E8689A", "y": "#FFD85C", "s": "#F6B9D0", "l": "#FFE0EC",
}


def sprite(rows, colours=BUNNY_COLOURS):
    """A picture from rows of letters ('.' is clear)."""
    im = Image.new("RGBA", (len(rows[0]), len(rows)), (0, 0, 0, 0))
    for y, row in enumerate(rows):
        for x, ch in enumerate(row):
            if ch != ".":
                im.putpixel((x, y), rgba(colours[ch]))
    return im


def doubled(im, k=2):
    return im.resize((im.width * k, im.height * k), Image.Resampling.NEAREST)


def blob(d, box, fill, ink=INK):
    """A pixel oval with a one-pixel outline."""
    x0, y0, x1, y1 = box
    d.ellipse(box, fill=rgba(ink))
    d.ellipse((x0 + 1, y0 + 1, x1 - 1, y1 - 1), fill=rgba(fill))


SPARKLE_PX = ["..k..", "..k..", "kk.kk", "..k..", "..k.."]
HEART_PX = [".kk.kk.", "kccwcck", "kccccck", ".kccck.", "..kck..", "...k..."]
FLOWER_PX = [".k.k.", "kpkpk", ".kyk.", "kpkpk", ".k.k."]


def stamp(im, rows, x, y):
    im.alpha_composite(sprite(rows), (x, y))


def bunny():
    """A round bunny with a red bow, a pink dress and a little music player."""
    im = Image.new("RGBA", (72, 84), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    # Dithered shadow under the feet.
    for y in range(76, 83):
        for x in range(14, 58):
            if ((x - 36) / 22) ** 2 + ((y - 79.5) / 3.6) ** 2 <= 1 and (x + y) % 2 == 0:
                im.putpixel((x, y), rgba("#F2A7C4"))
    # Ears, the right one a little lower, pink inside.
    blob(d, (21, 1, 32, 34), "#FFFFFF")
    blob(d, (37, 4, 48, 37), "#FFFFFF")
    d.ellipse((24, 7, 29, 29), fill=rgba("#FFB3CC"))
    d.ellipse((40, 10, 45, 32), fill=rgba("#FFB3CC"))
    # Body in a pink dress, arms, feet.
    blob(d, (23, 55, 49, 79), "#FF8FB8")
    blob(d, (17, 58, 26, 67), "#FFFFFF")
    blob(d, (24, 74, 34, 82), "#FFFFFF")
    blob(d, (38, 74, 48, 82), "#FFFFFF")
    # Head.
    blob(d, (10, 22, 61, 60), "#FFFFFF")
    # Eyes with a shine, nose, mouth, cheeks.
    for ex in (25, 44):
        d.rectangle((ex, 38, ex + 2, 42), fill=rgba(INK))
        im.putpixel((ex + 1, 39), rgba("#FFFFFF"))
    d.rectangle((35, 45, 36, 45), fill=rgba("#FF8DB5"))
    for x, y in ((33, 47), (34, 48), (35, 48), (36, 48), (37, 48), (38, 47)):
        im.putpixel((x, y), rgba(INK))
    d.ellipse((16, 45, 21, 48), fill=rgba("#FF8DB5"))
    d.ellipse((50, 45, 55, 48), fill=rgba("#FF8DB5"))
    # The bow on the left ear: two loops, wide at their ends, and a knot.
    for side in (-1, 1):
        def at(points, side=side):
            return [(25 + side * dx, y) for dx, y in points]
        d.polygon(at([(1, 17), (11, 12), (14, 14), (14, 26), (11, 28), (1, 24)]), fill=rgba(INK))
        d.polygon(at([(2, 18), (11, 14), (12, 15), (12, 25), (11, 26), (2, 23)]), fill=rgba("#E8364F"))
        d.line(at([(4, 20), (10, 17)]), fill=rgba("#B0243F"))
        d.line(at([(4, 22), (10, 24)]), fill=rgba("#B0243F"))
        im.putpixel((25 + side * 11, 15), rgba("#FFFFFF"))
    d.rounded_rectangle((22, 16, 28, 25), 2, fill=rgba(INK))
    d.rectangle((23, 17, 27, 24), fill=rgba("#B0243F"))
    im.putpixel((24, 18), rgba("#E8364F"))
    # A heart on the dress.
    stamp(im, [".w.w.", "wwwww", ".www.", "..w.."], 33, 63)
    # The music player in the right hand: screen and click wheel.
    d.rectangle((48, 55, 59, 71), fill=rgba(INK))
    d.rectangle((49, 56, 58, 70), fill=rgba("#FFFFFF"))
    d.rectangle((50, 57, 57, 61), fill=rgba("#FFB3CC"))
    blob(d, (50, 63, 57, 70), "#FFFFFF")
    im.putpixel((53, 66), rgba(INK))
    im.putpixel((54, 66), rgba(INK))
    blob(d, (46, 60, 52, 67), "#FFFFFF")  # the hand over the player
    # Sparkles, hearts and flowers around.
    stamp(im, SPARKLE_PX, 2, 30)
    stamp(im, HEART_PX, 60, 20)
    stamp(im, FLOWER_PX, 4, 64)
    stamp(im, SPARKLE_PX, 63, 56)
    return doubled(im)


def cloud(width=78):
    """A puffy cloud with a dithered underside and sparkles."""
    h = 34
    im = Image.new("RGBA", (width, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    puffs = [(4, 14, 26, 32), (14, 5, 40, 32), (32, 1, 58, 32), (48, 7, 72, 32), (60, 14, width - 2, 32)]
    for box in puffs:
        d.ellipse(box, fill=rgba(INK))
    d.rectangle((8, 22, width - 6, 32), fill=rgba(INK))
    for x0, y0, x1, y1 in puffs:
        d.ellipse((x0 + 1, y0 + 1, x1 - 1, y1 - 1), fill=rgba("#FFFFFF"))
    d.rectangle((9, 23, width - 7, 31), fill=rgba("#FFFFFF"))
    for y in range(26, 32):
        for x in range(9, width - 6):
            if (x + y) % 2 == 0 and im.getpixel((x, y))[:3] == (255, 255, 255):
                im.putpixel((x, y), rgba("#F6B9D0"))
    return doubled(im)


def pixel_round(w, h, r, fill, ink=INK, shine=None):
    """A rounded box with a one-pixel outline, for nine-slicing."""
    im = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.rounded_rectangle((0, 0, w - 1, h - 1), r, fill=rgba(ink))
    d.rounded_rectangle((1, 1, w - 2, h - 2), max(0, r - 1), fill=rgba(fill))
    if shine:
        d.line((r, 2, w - 1 - r, 2), fill=rgba(shine))
    return doubled(im)


def decor(rows_at, size):
    """Small stamps (sparkles, hearts, flowers) on a clear picture."""
    im = Image.new("RGBA", size, (0, 0, 0, 0))
    for rows, x, y in rows_at:
        stamp(im, rows, x, y)
    return doubled(im)


def bunny_bow():
    s = Skin("bunny-bow", "Bunny Bow",
             "Pastel pink pixel art: a bunny with a bow, clouds, sparkles and hearts.")
    s.palette = {"bg": "#FFE4EF", "text": INK, "muted": "#8E3D6B", "dim": "#A2557F", "accent": "#FF5C9A",
                 "glass": INK, "scrim": "#FFE4EF", "sheet": "#FFF7FA", "track": "#FFFFFF",
                 "danger": "#C81E45", "success": "#1E7F4C", "warning": "#9A5A00"}
    s.colors = {"bar_text": INK, "panel": "#FFF7FA", "accent_text": INK, "white": "#FFFFFF",
                "cream": "#FFF7FA", "bow": "#E8364F", "pinkfill": "#FFB8D2"}
    s.fonts = {"menu": ("PixelifySans-Medium", 20, None), "title2": ("PixelifySans-SemiBold", 17, None),
               "bartitle": ("PixelifySans-SemiBold", 18, None), "big": ("PixelifySans-Bold", 22, None),
               "detail": ("PixelifySans-Regular", 16, None), "body": ("PixelifySans-Regular", 16, None),
               "small": ("PixelifySans-Medium", 14, None), "clock": ("PixelifySans-Bold", 30, None)}
    s.images = {
        "bunny": bunny(),
        "cloud": cloud(),
        "frame": pixel_round(12, 12, 4, "#FFF7FA"),
        "highlight": pixel_round(12, 17, 6, "#FFB8D2", shine="#FFFFFF"),
        "track": pixel_round(10, 7, 3, "#FFFFFF"),
        "fill": pixel_round(10, 7, 3, "#FF8FB8", shine="#FFD3E2"),
        "battery": doubled(sprite([
            "kkkkkkkkkkkk..", "k..........k..", "k..........kk.", "k..........kk.",
            "k..........kk.", "k..........k..", "kkkkkkkkkkkk.."])),
        "battery_fill": vgrad(4, 10, [(0, "#FFA3C4"), (1, "#FF6FA0")]),
        "battery_low": vgrad(4, 10, [(0, "#FF8A8A"), (1, "#D7264B")]),
        "sparkles": decor([(SPARKLE_PX, 0, 6), (HEART_PX, 9, 0), (SPARKLE_PX, 20, 9), (FLOWER_PX, 30, 2)], (36, 16)),
        "hearts": decor([(HEART_PX, 0, 0), (SPARKLE_PX, 10, 1), (HEART_PX, 18, 0)], (25, 7)),
    }
    st = Style(row_h=34, top=40, pad=30, ffg="text", fmuted="muted", icon_ffg="text", icon_fg="dim",
               highlight=lambda w, h: [I(4, 1, w - 8, h - 2, "highlight", slice=[10, 10, 10, 10]),
                                       {"type": "icon", "if": "!row_art", "x": 11, "y": (h - 12) // 2,
                                        "size": 12, "icon": "heart", "color": "bow"}],
               scroll={"x": -8, "w": 4, "track": "#F6C6D8", "thumb": "accent"},
               bar_track_image="track", bar_fill_image="fill", bar_slice=[6, 6, 6, 6],
               sheet_bg="cream", sheet_frame=INK, sheet_radius=12, scrim="#FFE4EFB0",
               toast_bg=INK + "F0", toast_fg="#FFF7FA", toast_error="#C81E45F0")
    s.parts["titlebar"] = [
        {"type": "icon", "if": "playing", "x": 12, "y": 10, "size": 16, "icon": "play", "color": "text"},
        {"type": "icon", "if": "paused", "x": 12, "y": 10, "size": 16, "icon": "pause", "color": "text"},
        T(34, 0, 64, 36, "{clock}", "bartitle", "text", **{"if": "has_clock"}),
        {"type": "icon", "if": "charging", "x": 412, "y": 10, "size": 16, "icon": "bolt", "color": "bow"},
        T(352, 0, 82, 36, "{battery}%", "small", "muted", "right", **{"if": "!charging"}),
        *battery_part(440, 11),
        R(8, 36, 464, 2, "text"),
        I(212, 30, 50, 14, "hearts"),
    ]
    s.parts["title"] = [T(100, 0, 280, 32, "{screen}", "bartitle", "text", "center")]
    np = [P("titlebar"), T(100, 0, 280, 32, "Now Playing", "bartitle", "text", "center"),
          I(2, 40, 156, 68, "cloud"),
          I(84, 198, 72, 32, "sparkles"),
          I(4, 192, 144, 168, "bunny"),
          I(150, 46, 322, 266, "frame", slice=[8, 8, 8, 8]),
          {"type": "art", "x": 162, "y": 60, "size": 118, "radius": 6},
          R(161, 59, 120, 120, INK, radius=6, stroke=2),
          T(292, 58, 168, 52, "{title}", "big", lines=2, line_height=26),
          T(292, 112, 168, 22, "{artist}", "detail", "muted"),
          T(292, 134, 168, 22, "{album}", "detail", "dim"),
          T(292, 158, 140, 18, "{quality}", "small", "dim"),
          {"type": "icon", "if": "favorite", "x": 440, "y": 159, "size": 16, "icon": "heart", "color": "bow"},
          {"type": "icon", "if": "shuffle", "x": 162, "y": 190, "size": 18, "icon": "shuffle", "color": "text"},
          {"type": "icon", "if": "repeat", "x": 188, "y": 190, "size": 18, "icon": "repeat_mode", "color": "text"},
          T(292, 186, 168, 24, "{queue_index} of {queue_length}", "small", "muted", "right", **{"if": "!live"}),
          T(292, 186, 168, 24, "On Air", "small", "bow", "right", **{"if": "live"}),
          bar(st, 162, 228, 298, 14, "progress"),
          T(162, 250, 120, 22, "{elapsed}", "small"),
          T(340, 250, 120, 22, "{remaining}", "small", "text", "right"),
          T(162, 278, 298, 26, "{output}", "small", "muted", "center"),
          {"type": "icon", "if": "playing", "x": 302, "y": 318, "size": 20, "icon": "play", "color": "text"},
          {"type": "icon", "if": "paused", "x": 302, "y": 318, "size": 20, "icon": "pause", "color": "text"}]
    rows = (360 - st.top - 44) // st.row_h
    home = [I(318, 44, 156, 68, "cloud"),
            T(318, 66, 156, 40, "{clock}", "clock", "text", "center", **{"if": "has_clock"}),
            I(330, 118, 72, 32, "sparkles"),
            I(330, 192, 144, 168, "bunny"),
            rows_list(st, st.top + 2, rows, w=310),
            R(8, 318, 310, 2, "text"),
            {"type": "art", "if": "has_track", "x": 12, "y": 325, "size": 28, "radius": 4},
            T(48, 322, 270, 18, "{title}", "small", "text", **{"if": "has_track"}),
            T(48, 339, 270, 18, "{artist}", "small", "muted", **{"if": "has_track"}),
            T(16, 322, 300, 34, "Nothing playing yet", "small", "muted", **{"if": "!has_track"}),
            P("titlebar"), T(100, 0, 280, 32, "Bunny Bow", "bartitle", "text", "center")]
    s.parts["row310"] = row_elements(st, 310, False)
    s.parts["row310_focused"] = row_elements(st, 310, True)
    standard(s, st, np, home)
    s.screens["volume"] = [I(40, 290, 400, 52, "frame", slice=[8, 8, 8, 8]),
                           {"type": "icon", "x": 56, "y": 305, "size": 22, "icon": "speaker", "color": "text"},
                           bar(st, 90, 309, 296, 14, "volume"),
                           T(390, 290, 40, 52, "{volume}", "small", "muted", "right")]
    s.screens["keyboard"] = [{"type": "native", "part": "screen", "recolor": False},
                             P("titlebar"), T(100, 0, 280, 32, "{sheet_title}", "bartitle", "text", "center")]
    prune(s)
    return s



# ------------------------------------------------------------------ Gallery
# Covers first: every screen is washed from the top in the colour of the cover
# playing ("accent": "cover"); titles in Syne, lists in DM Sans, small capitals
# in IBM Plex Mono; rounded glass cards and a pill around the focused row.

def gallery_row(st, w, focused):
    """A row of width `w`: covers inset so they sit inside the focus pill."""
    h = st.row_h
    r = (h - 2) // 2
    out = [R(0, 1, w, h - 2, "accent/26", radius=r), R(0, 1, w, h - 2, "accent", radius=r, stroke=2)] if focused else []
    a = h - 14
    fg = "text"
    out.append({"type": "group", "if": "row_art", "elements": [
        {"type": "art", "x": 10, "y": 7, "size": a, "source": "row"},
        T(a + 22, 0, w - a - 58, h, "{label}", "menu", fg, **{"if": "!has_detail"}),
        T(a + 22, 3, w - a - 58, 22, "{label}", "title2", fg, **{"if": "has_detail"}),
        T(a + 22, 23, w - a - 58, 18, "{detail}", "small", "muted", **{"if": "has_detail"}),
    ]})
    dw = 170 if w >= 400 else 70
    plain = [
        T(st.pad, 0, w - st.pad - dw - 44, h, "{label}", "menu", fg, **{"if": "has_detail & !disabled"}),
        T(st.pad, 0, w - st.pad - 50, h, "{label}", "menu", fg, **{"if": "!has_detail & !disabled"}),
        T(st.pad, 0, w - st.pad - 50, h, "{label}", "menu", "dim", **{"if": "disabled"}),
        T(w - dw - 36, 0, dw, h, "{detail}", "detail", "muted", "right", **{"if": "!current"}),
        T(w - dw - 44, 0, dw, h, "{detail}", "detail", "muted", "right", **{"if": "current"}),
        # A switch for On/Off rows.
        R(w - 56, (h - 20) // 2, 36, 20, "accent", radius=10, **{"if": "toggle & on & !has_detail"}),
        R(w - 38, (h - 16) // 2, 16, 16, "text", radius=8, **{"if": "toggle & on & !has_detail"}),
        R(w - 56, (h - 20) // 2, 36, 20, "glass/18", radius=10, **{"if": "toggle & !on & !has_detail"}),
        R(w - 54, (h - 16) // 2, 16, 16, "muted", radius=8, **{"if": "toggle & !on & !has_detail"}),
    ]
    out.append({"type": "group", "if": "!row_art", "elements": plain})
    out += [
        {"type": "icon", "if": "checked", "x": w - 34, "y": (h - 18) // 2, "size": 18, "icon": "check",
         "color": "text" if focused else "accent"},
        {"type": "icon", "if": "chevron & !checked", "x": w - 28, "y": (h - 12) // 2, "size": 12,
         "icon": "chevron", "color": "text" if focused else "dim"},
        {"type": "icon", "if": "current", "x": w - 34, "y": (h - 18) // 2, "size": 18, "icon": "speaker",
         "color": "text" if focused else "accent"},
    ]
    return out


def gallery():
    s = Skin("gallery", "Gallery",
             "Covers first: dark glass tinted by the album playing, large display type.")
    s.palette = {"bg": "#0B0C10", "text": "#F4F5F8", "muted": "#A9AEBD", "dim": "#848A9C", "accent": "cover",
                 "glass": "#FFFFFF", "scrim": "#000000", "sheet": "#17181F", "track": "#2A2C36",
                 "danger": "#FF7A66", "success": "#3CC878", "warning": "#FFBE3C"}
    s.colors = {"ink": "#0B0C10", "deep": "#060609", "pane": "#FFFFFF12", "edge": "#FFFFFF24",
                "toast": "#24262F", "panel": "#15161D", "heart": HEART}
    s.fonts = {"display": ("Syne-ExtraBold", 30, "ascii"), "hero": ("Syne-ExtraBold", 22, "ascii"),
               "menu": ("DMSans-Medium", 17, None), "title2": ("DMSans-Bold", 15, None),
               "detail": ("DMSans-Medium", 14, None), "small": ("DMSans-Medium", 12, None),
               "mono": ("IBMPlexMono-Medium", 11, None), "time": ("IBMPlexMono-Medium", 13, None)}
    s.images = {"bloom": glow(64, "#FFFFFF")}
    st = Style(row_h=44, top=76, pad=18)
    s.parts["back"] = [
        R(0, 0, 480, 360, "bg"),
        I(-160, -230, 560, 470, "bloom", tint="accent/60"),
        {"type": "gradient", "x": 0, "y": 0, "w": 480, "h": 360, "top": "accent/26", "bottom": "bg"},
    ]
    # The time on the left; radios, charge and battery on the right.
    s.parts["titlebar"] = [
        T(18, 6, 90, 24, "{clock}", "mono", "muted", tracking=1, **{"if": "has_clock"}),
        {"type": "icon", "if": "wifi_connected", "x": 336, "y": 10, "size": 16, "icon": "wifi", "color": "muted"},
        {"type": "icon", "if": "bluetooth_connected", "x": 358, "y": 10, "size": 16, "icon": "bluetooth",
         "color": "muted"},
        {"type": "icon", "if": "charging", "x": 378, "y": 10, "size": 16, "icon": "bolt", "color": "success"},
        T(392, 6, 42, 24, "{battery}%", "mono", "muted", "right"),
        R(440, 12, 22, 12, "muted", radius=3, stroke=1),
        R(463, 16, 2, 4, "muted"),
        {"type": "bar", "if": "!battery_low", "x": 442, "y": 14, "w": 18, "h": 8, "value": "battery",
         "track": "#00000000", "fill": "text", "radius": 1},
        {"type": "bar", "if": "battery_low", "x": 442, "y": 14, "w": 18, "h": 8, "value": "battery",
         "track": "#00000000", "fill": "danger", "radius": 1},
    ]
    s.parts["title"] = [T(18, 26, 444, 46, "{screen}", "display")]
    s.parts["mini"] = [
        R(16, 302, 448, 50, "panel", radius=16), R(16, 302, 448, 50, "edge", radius=16, stroke=1),
        {"type": "art", "x": 25, "y": 309, "size": 36},
        T(72, 306, 330, 22, "{title}", "title2"),
        T(72, 326, 330, 18, "{artist}", "small", "muted"),
        R(414, 309, 36, 36, "accent", radius=18),
        {"type": "icon", "x": 423, "y": 318, "size": 18, "icon": "play_pause", "color": "ink"},
        {"type": "bar", "x": 32, "y": 349, "w": 368, "h": 2, "value": "progress", "track": "glass/10",
         "fill": "accent", "radius": 1},
    ]
    for w in (448, 264, 344):
        s.parts[f"row{w}"] = gallery_row(st, w, False)
        s.parts[f"row{w}_focused"] = gallery_row(st, w, True)

    def rows(y, n, cond, w=448, x=16):
        e = rows_list(st, y, n, w=w, x=x, cond=cond)
        e["scrollbar"] = {"x": w + 6, "w": 3, "track": "glass/8", "thumb": "glass/40"}
        return e

    status = [R(18, 302, 444, 1, "glass/15", **{"if": "has_status"}),
              T(18, 306, 444, 50, "{status}", "small", "muted", lines=2, line_height=17, **{"if": "has_status"})]
    s.screens["list"] = [
        P("back"), P("titlebar"), P("title"),
        rows(76, 5, "has_track & !has_status & !empty"),
        rows(76, 6, "!has_track & !has_status & !empty"),
        rows(76, 5, "has_status & !empty"),
        *status,
        T(40, 112, 400, 36, "{empty_title}", "hero", "text", "center", **{"if": "empty"}),
        T(60, 152, 360, 60, "{empty_body}", "detail", "muted", "center", lines=3, line_height=20,
          **{"if": "empty"}),
        R(150, 228, 180, 40, "accent", radius=20, **{"if": "empty & has_action"}),
        T(150, 228, 180, 40, "{empty_action}", "title2", "ink", "center", **{"if": "empty & has_action"}),
        P("mini", **{"if": "has_track & !has_status"}),
    ]
    s.screens["home"] = [
        P("back"), P("titlebar"), T(18, 26, 264, 46, "Reborn", "display"),
        rows(76, 6, None, w=264),
        R(296, 78, 168, 266, "pane", radius=20), R(296, 78, 168, 266, "edge", radius=20, stroke=1),
        {"type": "art", "if": "has_art", "x": 310, "y": 92, "size": 140},
        R(310, 92, 140, 140, "glass/8", radius=12, **{"if": "!has_art"}),
        {"type": "icon", "if": "!has_art", "x": 352, "y": 134, "size": 56, "icon": "note", "color": "dim"},
        T(310, 238, 140, 16, "Now Playing", "mono", "muted", case="upper", tracking=1, **{"if": "has_track & !live"}),
        T(310, 238, 140, 16, "On Air", "mono", "accent", case="upper", tracking=1, **{"if": "live"}),
        T(310, 254, 140, 22, "{title}", "title2", **{"if": "has_track"}),
        T(310, 275, 140, 18, "{artist}", "small", "muted", **{"if": "has_track"}),
        {"type": "bar", "if": "has_track", "x": 310, "y": 304, "w": 140, "h": 3, "value": "progress",
         "track": "glass/18", "fill": "accent", "radius": 1},
        T(310, 312, 70, 18, "{elapsed}", "mono", "muted", **{"if": "has_track"}),
        T(380, 312, 70, 18, "{remaining}", "mono", "muted", "right", **{"if": "has_track"}),
        T(310, 246, 140, 60, "Nothing playing", "title2", "muted", "center", **{"if": "!has_track"}),
    ]

    def fact_row(h):
        return [R(0, 0, 444, 1, "glass/12", **{"if": "!first"}),
                T(0, 0, 190, h, "{label}", "mono", "muted", case="upper", tracking=1),
                T(190, 0, 254, h, "{detail}", "detail", "text", "right")]

    def actions(y, cond):
        # As many rows as fit above the status line, or down to the foot.
        return [rows(y, max(1, (300 - y) // st.row_h), f"{cond} & has_status"),
                rows(y, max(1, (356 - y) // st.row_h), f"{cond} & !has_status")]

    s.screens["page"] = [
        P("back"), P("titlebar"), T(18, 26, 444, 46, "{page_title}", "display"),
        T(18, 76, 444, 0, "{page_body}", "detail", "muted", lines=2, line_height=19, **{"if": "has_body"}),
        {"type": "list", "if": "has_body", "x": 18, "y": 120, "w": 444, "rows": 4, "row_height": 24,
         "source": "facts", "row": fact_row(24)},
        {"type": "list", "if": "!has_body", "x": 18, "y": 80, "w": 444, "rows": 6, "row_height": 26,
         "source": "facts", "row": fact_row(26)},
        *actions(222, "has_facts & has_body"),
        *actions(244, "has_facts & !has_body"),
        *actions(122, "!has_facts & has_body"),
        *actions(80, "!has_facts & !has_body"),
        *status,
    ]
    s.screens["now_playing"] = [
        P("back"), P("titlebar"),
        T(130, 6, 220, 24, "{queue_index} of {queue_length}", "mono", "muted", "center", case="upper",
          tracking=2, **{"if": "!live"}),
        T(130, 6, 220, 24, "On Air", "mono", "accent", "center", case="upper", tracking=2, **{"if": "live"}),
        {"type": "art", "x": 20, "y": 40, "size": 176},
        T(214, 42, 248, 16, "Playing from", "mono", "muted", case="upper", tracking=2, **{"if": "!live"}),
        T(214, 42, 248, 16, "Live radio", "mono", "muted", case="upper", tracking=2, **{"if": "live"}),
        T(214, 58, 248, 22, "{album}", "detail"),
        T(214, 84, 248, 62, "{title}", "hero", lines=2, line_height=29),
        T(214, 148, 248, 24, "{artist}", "menu", "muted"),
        T(214, 178, 222, 18, "{quality}", "mono", "dim", case="upper", tracking=1),
        {"type": "icon", "if": "favorite", "x": 444, "y": 179, "size": 16, "icon": "heart", "color": "heart"},
        {"type": "bar", "x": 22, "y": 228, "w": 436, "h": 4, "value": "progress", "track": "glass/18",
         "fill": "text", "radius": 2, "knob": "text", "knob_radius": 6},
        T(20, 238, 100, 18, "{elapsed}", "time", "muted"),
        T(360, 238, 100, 18, "{remaining}", "time", "muted", "right"),
        {"type": "icon", "if": "shuffle", "x": 36, "y": 274, "size": 18, "icon": "shuffle", "color": "accent"},
        {"type": "icon", "if": "!shuffle", "x": 36, "y": 274, "size": 18, "icon": "shuffle", "color": "dim"},
        {"type": "icon", "x": 156, "y": 271, "size": 24, "icon": "prev_large", "color": "text"},
        R(212, 257, 56, 56, "accent", radius=28),
        {"type": "icon", "x": 226, "y": 271, "size": 28, "icon": "play_pause_large", "color": "ink"},
        {"type": "icon", "x": 300, "y": 271, "size": 24, "icon": "next_large", "color": "text"},
        {"type": "icon", "if": "repeat", "x": 426, "y": 274, "size": 18, "icon": "repeat_mode", "color": "accent"},
        {"type": "icon", "if": "!repeat", "x": 426, "y": 274, "size": 18, "icon": "repeat", "color": "dim"},
        R(16, 322, 448, 32, "pane", radius=16),
        {"type": "icon", "x": 30, "y": 330, "size": 16, "icon": "headphones", "color": "muted"},
        T(56, 322, 330, 32, "{output}", "small", "muted"),
        T(390, 322, 60, 32, "{volume}", "time", "text", "right"),
    ]
    s.screens["sheet"] = [
        R(0, 0, 480, 360, "scrim/72"),
        R(56, 34, 368, 292, "panel", radius=20), R(56, 34, 368, 292, "edge", radius=20, stroke=1),
        T(76, 46, 328, 34, "{sheet_title}", "hero", **{"if": "!danger"}),
        T(76, 46, 328, 34, "{sheet_title}", "hero", "danger", **{"if": "danger"}),
        T(76, 80, 328, 38, "{sheet_body}", "small", "muted", lines=2, line_height=18),
        {"type": "list", "x": 68, "y": 122, "w": 344, "rows": 4, "row_height": 44, "source": "sheet",
         "row": [P("row344")], "focused": [P("row344_focused")]},
    ]
    # Questions have longer headings: two lines of display type.
    s.screens["dialog"] = [
        R(0, 0, 480, 360, "scrim/72"),
        R(56, 46, 368, 268, "panel", radius=20), R(56, 46, 368, 268, "edge", radius=20, stroke=1),
        T(76, 56, 328, 56, "{sheet_title}", "hero", lines=2, line_height=26, **{"if": "!danger"}),
        T(76, 56, 328, 56, "{sheet_title}", "hero", "danger", lines=2, line_height=26, **{"if": "danger"}),
        T(76, 114, 328, 38, "{sheet_body}", "small", "muted", lines=2, line_height=18),
        {"type": "list", "x": 68, "y": 158, "w": 344, "rows": 3, "row_height": 44, "source": "sheet",
         "row": [P("row344")], "focused": [P("row344_focused")]},
    ]
    # Toasts take the place of the mini player, the volume line that of the foot pill.
    s.screens["toast"] = [
        R(16, 302, 448, 50, "toast", radius=18, **{"if": "!error"}),
        R(16, 302, 448, 50, "danger", radius=18, **{"if": "error"}),
        R(16, 302, 448, 50, "edge", radius=18, stroke=1),
        T(36, 302, 408, 50, "{message}", "title2", "text", "center", lines=2, line_height=18, **{"if": "!error"}),
        T(36, 302, 408, 50, "{message}", "title2", "ink", "center", lines=2, line_height=18, **{"if": "error"}),
    ]
    s.screens["volume"] = [
        R(16, 318, 448, 38, "panel", radius=19), R(16, 318, 448, 38, "edge", radius=19, stroke=1),
        {"type": "icon", "x": 30, "y": 329, "size": 16, "icon": "speaker", "color": "muted"},
        {"type": "bar", "x": 58, "y": 335, "w": 340, "h": 4, "value": "volume", "track": "glass/18",
         "fill": "accent", "radius": 2},
        T(404, 318, 46, 38, "{volume}", "time", "text", "right"),
    ]
    s.screens["keyboard"] = [
        {"type": "native", "part": "screen", "recolor": False}, P("titlebar"),
        T(120, 6, 210, 24, "{sheet_title}", "title2", "text", "center"),
    ]
    prune(s)
    return s


def main(ids):
    OUT.mkdir(exist_ok=True)
    skins = [nano_glass(), walkman(), terminal(), winamp(), paper(), big_type(), cassette(), neon(), zune(),
             bunny_bow(), gallery()]
    for colour_id, name, bt, bb, ht, hb, acc in [
        ("silver", "Silver", "#D5D9DF", "#9AA0A8", "#B9C0C9", "#7D858F", "#5E6670"),
        ("pink", "Pink", "#F7C6D9", "#D9799E", "#F29BBE", "#D9558A", "#C2386F"),
        ("blue", "Blue", "#BFDDF5", "#6FA6D6", "#8EC2EE", "#3E86C9", "#2F6FAD"),
        ("green", "Green", "#CFE9B5", "#86B75E", "#A7D97E", "#5C9E33", "#3F7F1F"),
        ("gold", "Gold", "#EEDFAE", "#C4A24C", "#E3C66E", "#B48D24", "#8A6A12"),
    ]:
        skins.append(mini(colour_id, name, bt, bb, ht, hb, acc))
    for skin in skins:
        if ids and skin.id not in ids:
            continue
        skin.write()
        print("wrote", skin.dir.relative_to(ROOT))


if __name__ == "__main__":
    main(sys.argv[1:])
