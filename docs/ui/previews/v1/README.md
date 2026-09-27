# Reborn UI v1 native previews

<!-- knowledge-base-scope: ui-design-/-scoped-candidate -->
> **Historical record.** The dates, candidate identity, "current" claims,
> next steps and permissions below belong to this recorded boundary. See
> [current state](../../../CURRENT_REBORN_STATE.md) for the latest physically observed result.

69 individual PNGs at exactly 480×360, plus a contact sheet preserving the native
screen size. All content is isolated preview fixture data, including the artwork;
it is not installed in the player. These are actual production layout/text/icon
quads rasterized with the same shipped atlases, not pasted concept screenshots.
Host rasterization is not proof of GLES blending or physical panel quality.

Three visual passes inspected every screen at native size. The final pass also
inspected the added partial-scan, missing-metadata, Wi-Fi transition and update
operation/error states, and all changed collection/capability/Bluetooth layouts.
Focus, clipping, density, text, contrast, icon clarity and hardware-only navigation
were reviewed. The manifest records image hashes and the implementation revision.
Boot and locked install preparation intentionally have no interactive focus;
every interactive fixture asserts exactly one focus target before rasterization.

Regenerate from the application repository:

```sh
cargo run --locked --bin reborn-preview -- out/ui-v1-preview-quads
python3 tools/preview/render_previews.py out/ui-v1-preview-quads --output docs/ui/previews/v1
python3 tests/ui-candidate.py
```

The executable is a host review tool and is excluded from the installed root.
There is no decorative lock preview: the device blanks its panel and restores the
same route on wake. See the owner qualification checklist before physical review.
