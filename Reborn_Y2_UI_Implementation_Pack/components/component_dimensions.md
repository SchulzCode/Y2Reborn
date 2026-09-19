# Reborn component dimensions (480×360)

## Status bar
- Height: 30 px
- Left: `Reborn | Y2`
- Center: optional screen title
- Right: output status, battery, clock
- Keep this visually quiet. Do not make it navigable.

## List row
- Height: 44–52 px
- Left icon/thumb: 24–40 px
- Primary text: 16 px
- Secondary text: 12 px
- Right value/chevron: 12–14 px
- Selected: 2 px warm-gold outline + very subtle warm fill
- One focus target at a time.

## Focus rule
- Wheel rotation: previous / next item
- Confirm: activate/open
- Back: parent screen
- Menu: context options
- Long press may be used only for optional shortcuts.
- Never require swipe, drag, hover, or multi-point gestures.

## Cards
- Radius: 10–14 px
- Border: #2A3037, 1 px
- Background: #15191E
- Selected card: #211B12 + #FFD17B 2 px outline

## Progress
- Track: 4 px
- Fill: #E6B965
- Time labels: 12 px secondary
- Seeking should be a dedicated hardware-button mode; do not imply touch scrubbing.

## Now Playing layout
Recommended 480×360 implementation:
- Status: y=0..30
- Content: y=36..280
- Bottom hardware-status strip: y=306..360
- Album art: ~168×168
- Song title: 24–28 px serif
- Artist: 18–20 px
- Album / codec line: 12–14 px
