# Reborn UI v1 font

One 1024×1024 RGBA atlas contains 1,001 proportional DejaVu Sans 2.37 glyphs,
at 24 px in 32 px cells. The UI scales these to its centralized type tokens.
Both former font roles share this texture. No runtime TTF loader is required.

The official public DejaVu release supplied the TTF, not a system/private font.
See `provenance.json` for its URL and SHA-256 and `DejaVuSans.LICENSE` for the
upstream license. Regenerate with `tools/assets/build_atlas.py` and the extracted
official release directory. Build-only dependencies: Pillow 11.3, CairoSVG 2.8.2.

Coverage: basic/extended Latin U+0000–024F (controls are sanitized), Greek
U+0370–03FF, Cyrillic U+0400–04FF, selected punctuation and U+FFFD. Unsupported
characters map to U+FFFD; they never become arbitrary texture coordinates.
CJK, emoji, shaping and bidirectional layout remain unavailable.

The older `reborn-display.rgba` asset is historical and is not included by the
UI or installed by the UI v1 candidate.
