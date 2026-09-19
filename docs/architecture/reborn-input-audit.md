# Reborn physical input audit

The Y2 UI consumes semantic actions only. Raw Linux input records stop at
`reborn-platform::input::InputManager`.

| Physical control | Linux source / event | Normalized event | Semantic action |
| --- | --- | --- | --- |
| Click wheel clockwise | `APT32F click-wheel`, `EV_KEY KEY_DOWN`/`KEY_PAGEDOWN`, or `EV_REL REL_WHEEL > 0` | `WheelClockwise(steps)` | `WheelClockwise(steps)` in lists; volume up in Now Playing |
| Click wheel counter-clockwise | `APT32F click-wheel`, `EV_KEY KEY_UP`/`KEY_PAGEUP`, or `EV_REL REL_WHEEL < 0` | `WheelCounterClockwise(steps)` | `WheelCounterClockwise(steps)` in lists; volume down in Now Playing |
| Center / Select | `Y2 navigation buttons`, `EV_KEY 28` | `Press/Release(Select)` | `Select` on release; long release becomes `ContextMenu` |
| Back | `Y2 navigation buttons`, `EV_KEY 158` | `Press/Release(Back)` | `Back`; long press goes Home |
| Previous | `Y2 navigation buttons`, `EV_KEY 105`, or any device `EV_KEY 165` | `Press/Release(Previous)` | Previous track; long press seeks backward |
| Next | `Y2 navigation buttons`, `EV_KEY 106`, or any device `EV_KEY 163` | `Press/Release(Next)` | Next track; long press seeks forward |
| Play / Pause | `Y2 navigation buttons`, `EV_KEY 164`, or any device `EV_KEY 57` | `Press/Release(PlayPause)` | Toggle playback; long press opens Now Playing |
| Power | `mtk-pmic-keys`, `EV_KEY 116` | `Press/Release(Power)` | Short press sleeps/wakes display; long press opens power menu |
| Volume down | Any accepted input device, `EV_KEY 114` | `Press/Repeat(VolumeDown)` | Volume down; never navigation |
| Volume up | Any accepted input device, `EV_KEY 115` | `Press/Repeat(VolumeUp)` | Volume up; never navigation |

## Timing owned by the input boundary

- Long press: 650 ms.
- Repeat begins: 400 ms.
- Repeat cadence: 90 ms.
- Wheel acceleration: up to six semantic steps for same-direction events
  within 180 ms.

Long press is emitted once and recorded by `ActionRouter`; the matching
release is consumed, so a long press cannot also trigger its short action.

## Screen-off policy

`ActionRouter::route` receives the authoritative screen-on flag. While the
screen is off, wheel/navigation/select/back actions produce no semantic action.
Volume and playback controls remain global and do not wake the display. Only a
Power release while the screen is off produces `ScreenWake`.

Playback buttons and volume are routed above the current screen, so ordinary
navigation and Back cannot stop playback. The UI only returns typed `Effect`
values; service calls remain in `app/reborn`.
