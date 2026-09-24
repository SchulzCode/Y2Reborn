# Reborn Simple icon atlas

`src/` preserves the monochrome SVG sources used from the owner-supplied
Reborn Y2 Simple UI Asset Pack. `provenance.json` records each source hash and
the stable slot mapping; unused legacy names alias a supplied Simple icon.

`reborn-icons.rgba` is 192×160: six columns by five rows of 32×32 cells.
Regenerate with `tools/assets/build_atlas.py`. Runtime uses GLES textures and
needs no SVG parser, Python or CairoSVG. This atlas contains no sample artwork.
