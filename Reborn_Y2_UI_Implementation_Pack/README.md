# Reborn Y2 — implementation asset pack

This pack is based on the **very first Reborn concept** from the conversation: dark charcoal surfaces, warm-gold focus/accent, serif display typography, restrained audiophile metadata, and strong physical-navigation focus states.

## Folder guide

- `references_480x360/`
  Exact first-concept screens scaled to the Y2 display resolution. Use these as visual targets, **not as raster UI backgrounds**.
- `icons_svg/`
  Lightweight 24×24 monochrome SVG icons designed to be recolored at runtime.
- `tokens/`
  Framework-neutral JSON tokens, a C header, and a Qt/QSS reference theme.
- `components/`
  Concrete component/layout dimensions for a 480×360 implementation.
- `artwork_samples/`
  A representative artwork crop from the concept for prototyping only.
- `docs/`
  Navigation and implementation notes.

## Important implementation principle

Do not reproduce the generated concept by placing the whole mockup as one bitmap. Rebuild it as native UI:
1. native text,
2. dynamic album art,
3. vector/raster icons,
4. native focusable rows/cards,
5. runtime playback metadata.

That keeps the interface fast, sharp, localizable, and usable with the click wheel.

## Fonts

Font files are intentionally **not included**.

Recommended visual pairing:
- Display / album / artist titles: `Cormorant Garamond`, `Noto Serif`, or another high-contrast serif.
- UI / metadata: `Inter`, `Noto Sans`, or `DejaVu Sans`.

For an embedded Linux build, `Noto Serif + Noto Sans` is a safe practical pair if already available in your image. If you want the closest premium look, use an appropriately licensed high-contrast serif plus Inter.

## Screen behavior

### Now Playing
Essential information only:
- art
- title / artist / album
- codec + sample rate/bit depth
- elapsed / remaining
- playback state
- volume
- output + gain

The circular playback controls in the concept are visual state indicators. On the Y2 they should map to the actual hardware buttons rather than behave as touch controls.

### Library
Prefer a focused list or small card grid where the wheel always has one obvious target. Do not make all visible album tiles look equally interactive.

### Settings
Use one selected row at a time. Values live on the right. Confirm opens a detail picker or toggles the setting when unambiguous.

## Performance guidance for Y2

- Pre-scale album art to the actual display size rather than decoding full-resolution images every frame.
- Cache thumbnails.
- Avoid live blur where possible; use a precomputed darkened artwork background.
- Use integer pixel coordinates.
- Limit glow to the selected element.
- Prefer one composited animation at a time.
- Render text from cached glyph atlases if your stack supports it.
- Use 30 FPS only where animation is actually needed; static menus do not need continuous redraw.

## Licensing note

The mockup artwork and fictional artist/album content were generated as part of the concept. For a shipping product, replace sample art with the user's actual music artwork and ensure any chosen fonts/icons meet your licensing requirements.
