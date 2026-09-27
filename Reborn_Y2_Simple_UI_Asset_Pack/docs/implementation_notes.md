# Implementation notes

- Redraw only when state changes.
- Avoid continuous animation in static menus.
- Pre-scale album art.
- Cache small thumbnails.
- Prefer pre-darkened background images over runtime blur.
- Keep glow to the single focused element.
- One sans-serif family is enough for this concept.
- Suggested fonts: Inter, Noto Sans, DejaVu Sans. Font files are not bundled.
- The sample artwork is for prototyping only; use real embedded cover art at runtime.
