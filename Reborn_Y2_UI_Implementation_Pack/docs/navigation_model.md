# Hardware navigation model

## Primary controls
- Wheel clockwise: next focusable item / increase value
- Wheel counter-clockwise: previous focusable item / decrease value
- Center/confirm: open / activate / accept
- Back: return to parent screen
- Menu: contextual actions
- Play/Pause: immediate global transport action
- Previous/Next: immediate track action when music is active
- Volume keys/wheel mode: adjust volume without leaving current screen

## Focus behavior
- Exactly one item is focused.
- Focus survives returning to a screen.
- Initial focus should be the currently active or most likely item.
- Focus does not jump when metadata changes.
- A disabled row remains visible but cannot receive focus.
- When opening a modal/list picker, focus starts on the current value.

## Recommended screen flow
Boot
→ Main / Library
→ Albums / Artists / Songs
→ Album / Artist detail
→ Now Playing
→ Queue

Menu key from most screens:
Now Playing / Queue / Add to favorites / Track info / Settings

Back never performs destructive actions.
