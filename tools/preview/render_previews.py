#!/usr/bin/env python3
"""Rasterize deterministic Reborn Quad previews for host-side visual review."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw


WIDTH, HEIGHT = 480, 360


def rgba(value: int) -> tuple[int, int, int, int]:
    return ((value >> 24) & 255, (value >> 16) & 255, (value >> 8) & 255, value & 255)


def texture(path: Path, width: int, height: int) -> Image.Image:
    return Image.frombytes("RGBA", (width, height), path.read_bytes())


def draw_quad(canvas: Image.Image, quad: dict, ui_font: Image.Image, display_font: Image.Image, icons: Image.Image, artwork: Image.Image) -> None:
    x, y = int(round(quad["x"])), int(round(quad["y"]))
    w, h = max(1, int(round(quad["w"]))), max(1, int(round(quad["h"])))
    tint = rgba(quad["color"])
    if quad.get("artwork"):
        layer = artwork.resize((w, h), Image.Resampling.BILINEAR).convert("RGBA")
        layer.putalpha(ImageChops.multiply(layer.getchannel("A"), Image.new("L", (w, h), tint[3])))
    elif quad.get("icon") is not None:
        index = int(quad["icon"])
        source = icons.crop(((index % 6) * 32, (index // 6) * 32, (index % 6 + 1) * 32, (index // 6 + 1) * 32))
        layer = source.resize((w, h), Image.Resampling.BILINEAR).convert("RGBA")
        mask = layer.getchannel("A")
        layer = Image.new("RGBA", (w, h), tint)
        layer.putalpha(ImageChops.multiply(mask, Image.new("L", (w, h), tint[3])))
    elif quad.get("glyph") is not None:
        index = int(quad["glyph"])
        source = (display_font if quad.get("display_font") else ui_font).crop(((index % 16) * 16, (index // 16) * 16, (index % 16 + 1) * 16, (index // 16 + 1) * 16))
        layer = source.resize((w, h), Image.Resampling.BILINEAR).convert("RGBA")
        mask = layer.getchannel("A")
        layer = Image.new("RGBA", (w, h), tint)
        layer.putalpha(ImageChops.multiply(mask, Image.new("L", (w, h), tint[3])))
    else:
        layer = Image.new("RGBA", (w, h), tint)
    canvas.alpha_composite(layer, (x, y))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("preview_dir", type=Path)
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--artwork", type=Path, default=Path("Reborn_Y2_UI_Implementation_Pack/artwork_samples/northark_a_brighter_silence_sample.png"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    output = args.output or args.preview_dir / "png"
    output.mkdir(parents=True, exist_ok=True)
    ui_font = texture(root / "assets/fonts/reborn-ui.rgba", 256, 128)
    display_font = texture(root / "assets/fonts/reborn-display.rgba", 256, 128)
    icons = texture(root / "assets/icons/reborn-icons.rgba", 192, 160)
    artwork = Image.open(root / args.artwork).convert("RGBA")
    for spec in sorted(args.preview_dir.glob("*.json")):
        if spec.name == "manifest.json":
            continue
        document = json.loads(spec.read_text())
        canvas = Image.new("RGBA", (WIDTH, HEIGHT), (9, 11, 13, 255))
        for quad in document["quads"]:
            draw_quad(canvas, quad, ui_font, display_font, icons, artwork)
        canvas.convert("RGB").save(output / f"{spec.stem}.png")
    print(f"wrote preview PNGs to {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
