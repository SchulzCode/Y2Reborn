# Reborn icon atlas

`reborn-icons.rgba` is a build-time raster atlas made directly from the
monochrome SVG files in `Reborn_Y2_UI_Implementation_Pack/icons_svg/`.

The atlas is 192×160 pixels: 6 columns × 5 rows of 32×32 cells. The native
GLES2 renderer samples one cell per icon, so runtime never needs an SVG parser.
