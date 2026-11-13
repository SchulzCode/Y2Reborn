# Button Mapper: plan

Owner request 2026-11-13: a custom button mapper with an animated UI. Any button the Y2 has can be given any
action Reborn has, in two stages (**screen on** and **screen off**). Presets, with **Reborn Default** (how the
buttons work today) always there to go back to. Above all, nobody may lock themselves out of the player.

Status: **phases 0-2 and most of 3 built (2026-11-13), owner test build; not published.** Assessed against
Beta 9.3.28. The owner left every decision in section 7 to the proposals ("do all as you think it is best").
What was built differs from the first draft where section 8 says so.

## 1. Is it possible today?

| Part | Possible today? | Why |
|---|---|---|
| Screen on: remap the buttons, the wheel and the volume buttons | **Yes, Reborn only** | Every press already passes one place, `reborn_platform::input::ActionRouter::route` (physical event → `Action`); the UI then gives the action its meaning per screen. A table can replace the hard-coded `match` there. No kernel or platform change |
| Screen off, player awake (music playing, or the first minutes after a pause) | **Yes, Reborn only** | The router already knows `screen_on`; "Buttons While Screen Off" (Locked / Buttons / Wheel and Buttons) is a fixed version of exactly this stage |
| Screen off, player **asleep** (paused for a while, then the system suspended) | **No, only Power works** | Power is the only wake source the kernel enables (`mtk-pmic-keys` wakeup=enabled). The five front buttons are **polled GPIOs** (`gpio-keys-polled`, every 20 ms, `kernel/dts/innioasis-y2.dts:185`): no interrupt, so nothing can wake on them. The wheel (EINT 55) and the volume buttons (keypad, SPI 116) have interrupts and the SPM's suspend wake mask already includes `WAKE_SRC_KP` and `WAKE_SRC_EINT` (`spm-suspend-policy.h:13`), but neither device is wake-enabled, and the sleep helper treats every wake as "wake the screen" |
| Power | Yes technically, **but it must stay fixed** (see 3) | Power is the only button that wakes the player from sleep, and Power held (Quick Menu, Power Off) is the guaranteed way out |

What playing music means for the stages: while a song plays the player never sleeps, so the "screen off" stage
covers listening completely. It is the paused, asleep player where only Power works. Waking on other buttons is a
kernel and power project of its own (phase 4); everything else needs no kernel change.

### The buttons

| Button | Kernel device | Press / hold / repeat today |
|---|---|---|
| Select (centre) | `Y2 navigation buttons`, GPIO 54, polled | press, hold |
| Back / Menu (top) | GPIO 7, polled | press, hold |
| Previous (left) | GPIO 6, polled | press, hold, repeat |
| Next (right) | GPIO 9, polled | press, hold, repeat |
| Play/Pause (bottom) | GPIO 10, polled | press, hold |
| Wheel turn (two directions) | `APT32F click-wheel`, EINT 55 | detents with acceleration |
| Volume + / Volume − (side) | `mt6582-keypad` | press, repeat |
| Power (side) | `mtk-pmic-keys`, PMIC EINT 25 | press, hold; **the PMIC resets the player after a ~10 s hold, in hardware** |

Holding the wheel ring together with Select etc. (chords) is not possible on all pairs: the polled GPIO keys read
fine together, but nobody has tested which combinations the hardware reports. See 6.

## 2. Refinements (things the request did not name)

1. **Contexts, not just two stages.** Today one button already means different things in different places: the
   wheel scrolls a list but sets the volume (or seeks) on Now Playing; Select picks a row but plays and pauses in
   the dark. A mapper with only "on" and "off" would force one meaning everywhere. Proposal: three layers,
   **Menus** (every list and page), **Now Playing** (falls back to Menus for anything left unset), **Screen
   off**. The owner's two stages are Menus + Now Playing (screen on) and Screen off.
2. **Gestures.** Each button gets **Press** and **Hold**; Previous/Next/volume also **Hold repeats**. Optional
   **Double press**, off by default: it makes a single press of that button wait ~300 ms, so it is only armed
   on buttons that have a double press mapped.
3. **Semantic actions, not raw keys.** "Next" stays smart: next song, next chapter in a book (or 30 s, #103), next
   station on FM, next film. The mapper chooses *what*, Reborn keeps *how* per context.
4. **Some pages always use Reborn Default**: the mapper itself, the Quick Menu, the on-screen keyboard (its
   arrows are the four ring buttons), pairing codes, alerts and the turn-off hold. These are the rescue path and
   the places where a remap would make a page unusable.
5. **Feedback in the dark.** With the screen off a remapped button gives no sign of what it did. Use the haptics
   motor: one short tick for an action that was done, a double tick for "nothing mapped here", optionally a
   distinct pattern per action group. (Haptics already exist, Beta 8.41.)
6. **Pocket safety.** Screen-off actions are easy to trigger by accident. The screen-off layer keeps its
   "Locked" preset as a one-tap choice, and actions that change state permanently (Delete, Clear Queue,
   Power Off, Forget…) are not offered in the screen-off layer at all.
7. **Migration of today's settings.** "Wheel on Now Playing" and "Buttons While Screen Off" become parts of the
   mapping. A player that changed them gets a preset "My Buttons" made from its current settings on the first
   start, so nothing changes under anyone's fingers. The two old rows become shortcuts into the mapper.
8. **Presets as files.** Presets are small JSON files; Reborn Default and a few built-ins ship with Reborn, the
   user's own live on `/data`. Later: export to and import from Internal Storage (`Buttons` folder), so people
   can share layouts (like skins and EQ profiles).
9. **External controls are out of scope here**: Bluetooth headphone buttons (AVRCP), a wired headset button
   (parked PR #11). They could become a fourth layer later; the action catalogue is built so they can use it.
10. **Rollback safety.** `/data` must stay readable by the previous version (Restore Previous System): the
    mapping is a new, separate file; an older Reborn ignores it and uses its own defaults.
11. **Timing for people.** The hold time (650 ms today) could be adjustable (short / normal / long) for people
    who press slowly. Small, cheap, belongs on the same page.
12. **Skins** never change the mapping, but the mapper page and the button help screen are drawn by Reborn, not
    the skin (as Wi-Fi Share), so they look the same in every skin.

## 3. Nobody gets locked out: the safety design

The rules, from the hardest to the softest. Each one alone already prevents a lockout; together they make one
impossible even with a corrupt file.

1. **Power is not mappable.** Power short = sleep/wake, Power hold = Quick Menu then Power Off, stay as they are.
   Power is the only wake source, and the PMIC's ~10 s hold reset works below any software. Proposal for later:
   allow a **double press of Power** (screen on only) as an extra mappable gesture.
2. **The Quick Menu is the guaranteed way back.** It opens with Power held (fixed), it always uses Reborn Default
   inside, and it gets a tile **Buttons** with **Reborn Default** as its first choice. So from any state: hold
   Power → turn the wheel → Select → Reborn Default. The Quick Menu itself cannot be remapped away because
   Power is not mappable.
3. **The mapper page uses Reborn Default** while it is open, so whatever was saved, the page that fixes it always
   works the same way.
4. **A layout must pass a reachability check before it can be saved.** Reborn's UI is a pure model
   (`Ui::action` on an `AppModel`), so the check does not guess, it *proves*: starting from Home it explores the
   screens using only the inputs of the new layout and must be able to (a) reach every screen in the settings
   tree, (b) go back to Home from each, (c) scroll a long list both ways, (d) play, pause and leave Now Playing.
   A layout that fails is refused with the reason in words ("Nothing goes Back. Give Back to a button."). Host
   tests run this check over every shipped preset and thousands of random layouts.
5. **Keep or revert.** After saving, a sheet asks "Keep these buttons?" with a 15-second countdown. It must be
   confirmed with the **new** layout's Select; otherwise the old layout comes back by itself (as screen
   resolution changes on a computer).
6. **Start-up escape.** Holding **Back** (in its hardware position, whatever it is mapped to) while Reborn starts
   starts with Reborn Default for that session and offers to make it permanent. The start-up splash says so
   when a custom layout is active.
7. **Bad files fall back.** An unreadable file, an unknown version or an action this Reborn does not know (an
   older Reborn after a rollback, a newer preset file) falls back to Reborn Default for that binding, logs it, and
   the error report shows it. The reachability check runs again at load; a stored layout that fails it is not used.
8. **Reset everything still resets the buttons** (System → Reset & Maintenance → Reset Settings).

## 4. The design

### Model

- `reborn_core::buttons`: `Control` (Select, Back, Previous, Next, PlayPause, WheelForward, WheelBack, VolumeUp,
  VolumeDown), `Gesture` (Press, Hold, HoldRepeat, Double), `Layer` (Menus, NowPlaying, ScreenOff),
  `Command` (the catalogue below), `Layout` (`(Layer, Control, Gesture) → Command`), `Preset` (name, built-in or
  own, a `Layout`), `validate(&Layout) -> Result<(), Problem>`.
- `ActionRouter::route` keeps the event machinery (holds, repeats, keyboard arrows, the screen-off swallow rules)
  and looks the command up in the active `Layout` instead of its `match`. Reborn Default is the table that
  reproduces today's `match` exactly, and a test proves it: every input sequence of the current router tests gives
  the same actions through the Reborn Default table.
- Storage: `/data/reborn/state/buttons.json` (active preset name, own presets, hold time). Additive; no change to
  `settings` records, so the `/data` gate keeps passing.

### Command catalogue (first version)

Each command carries what it may do: which layers offer it, whether it repeats when held, whether it needs the
screen (then it lights the screen first), whether it is "destructive" (never in Screen off).

| Group | Commands |
|---|---|
| Navigation | Select, Back, Home, Up/scroll back, Down/scroll forward, Context menu (Options), Now Playing, Queue |
| Playback | Play/Pause, Next, Previous, Seek forward, Seek back, Restart song, Stop, Shuffle on/off, Repeat (cycle), Shuffle Albums, Play something (shuffle all) |
| Volume and sound | Volume up, Volume down, Mute, Next output (Bluetooth / wired / speaker), EQ on/off |
| Books, radio, films | Next/previous chapter, Rewind 30 s, Speed (cycle), Next/previous station, FM ⇄ Internet, Favourite station |
| Player | Screen off, Brightness up/down, Sleep timer (cycle 15/30/45/60/off), Wi-Fi on/off, Bluetooth on/off, Reconnect last headphones, Quick Menu |
| Open | Home places (Music, Audiobooks, Videos, Playlists, Radio, Search, Settings), Search keyboard |
| Nothing | Do nothing (for pocket safety) |

The catalogue maps onto what already exists (`Action`, `Effect`, the Quick Menu actions); a new command is
added in one place and shows up in the mapper automatically.

### Built-in presets

- **Reborn Default**: exactly today. Read-only.
- **Pocket**: as Default with the screen on; in the dark only the volume buttons and Play/Pause work.
- **Wheel Seeks**: the wheel moves through the song on Now Playing (today's setting, as a preset).
- **One Hand**: Next/Previous on the volume buttons when the screen is off, volume on the wheel.
- **My Buttons**: made once from a player's current settings during migration (only if they differ).

## 5. The UI (animated)

Place: **Settings → Customize → Buttons** (rename "Wheel & Buttons" to **Buttons**; it keeps Wheel Click and
Haptics), per the settings map rule. Rows: **Layout** (preset picker), **Screen On ›**, **Screen Off ›**,
**Hold Time ›**, Wheel Click, Haptics.

1. **The player face.** Screen On / Screen Off open a drawing of the Y2: the wheel ring with its four buttons,
   the centre, Power and the volume buttons on the side. Each button shows its current command as a small label
   on a leader line. The labels slide in one after another on open (`motion.rs` springs, as the Quick Menu tiles).
2. **Press a button to change it.** The page listens to the real buttons: pressing one makes it pulse on the
   drawing, its label lifts into a card "**Next** · Press → Next song · Hold → Seek forward". Turning the wheel
   selects the wheel the same way. The page uses Reborn Default for its own controls, so a pressed button is both
   "picked" and does nothing else; for people who prefer it, a row **Choose with the Wheel** walks the buttons
   one by one instead.
3. **Pick the gesture, then the command.** The card flips to the command list, grouped as in the catalogue, the
   current one marked, with a one-line hint for each. Choosing animates the new label back to the button.
4. **Changed buttons glow** in the accent colour; a "Changed from Reborn Default: 3" line leads to **Reset**.
5. **Save** runs the reachability check (a short sweep animation over the face while it runs, ~50 ms), then the
   Keep-or-revert sheet with its countdown ring.
6. **Try it.** A "Try Buttons" mode lights each button on the drawing and shows its command as you press it,
   without doing anything, so people can learn a layout.
7. **Screen off** has the same face, dimmed, with a moon badge, and offers only screen-off commands.
8. **Help at any time:** holding Select on the face shows a legend card of the whole layout.

## 6. Phases

0. **Audit + facts** (docs only): roadmap/gap entry (a new user-facing subsystem); button combination test on
   the owner's player (which chords the hardware reports, ghosting); the settings-map change.
1. **Model and router** (no UI): `reborn_core::buttons`, Reborn Default as a table, router reads the table,
   equivalence tests against today's router, `buttons.json` with fallbacks, the reachability checker and its
   random-layout tests. Shipped invisibly: behaviour identical.
2. **Mapper UI**: Customize → Buttons, the face, capture, command lists, presets, Keep-or-revert, Quick Menu
   tile, start-up escape, migration of the two old settings, haptic feedback in the dark. Preview images via
   `reborn-preview`, device test with `rebornctl` input injection (every lockout rule tried on the player).
3. **Extras**: double press, hold time, more built-in presets, export/import on Internal Storage, an owner page
   (Y2Linux `docs/BUTTONS.md`).
4. **Buttons while asleep** (kernel + platform, separate go): read the stock kernel for the navigation GPIOs'
   EINT lines (GPIO 6/7/9/10/54) and whether stock woke on them; move the five buttons from polled to
   interrupt-driven `gpio-keys` with `wakeup-source` where the hardware allows, wake-enable the wheel and the
   keypad; teach the sleep helper a "button wake" that delivers the button to Reborn and goes back to sleep
   without lighting the screen when the command does not need it; measure drain (pocket presses waking the
   player) and wake latency (the first press would act after ~0.5-1 s). Hardware evidence first (AGENTS rule:
   new hardware subsystem → audit), and only the buttons that prove reliable are offered as "works while asleep".

## 7. Open decisions for the owner

| Question | Proposal |
|---|---|
| Power stays fixed? | Yes (lockout rule 1); a double press of Power mappable later |
| Layers | Menus + Now Playing (screen on) and Screen off, rather than a single screen-on layer |
| Can the menus' navigation (Select/Back/scroll) be moved? | Yes, but only through the reachability check and Keep-or-revert |
| Buttons while asleep (phase 4) | Separate project after phases 1-3; drain and wake latency decide which buttons |
| Double press | Phase 3, per button, off unless mapped |
| Export/import of presets | Phase 3 |

## 8. As built (2026-11-13)

| Part | Where | Notes |
|---|---|---|
| Model | `reborn-core/src/buttons.rs` (+ `buttons_tests.rs`) | `Control`, `Gesture` (Press, Hold), `Layer`, `Command` (32 offered + the internal `RebornDefault`), `Layout` (`resolve` with Now Playing → Menus fallback, `check`, `changed`), presets (`built_in`, `ButtonSettings` with up to 4 own), `migrate` of the two older settings, `ButtonContext`. `NowPlayingWheel` / `ScreenOffButtons` moved here and stay readable for a rollback |
| Storage | `Settings::buttons` in the session | Not a separate `buttons.json`: the session is what Reset Settings and Restore Previous System already handle; additive, so an older Reborn ignores it. A stored binding Reborn does not know leaves that button as in Reborn Default |
| Router | `reborn-platform/src/input.rs` `ActionRouter` | A button answers in the layer and layout it went down in; a press acts at the release unless the button has no hold and its command repeats (volume), then at once and on every repeat; holds act at the hold and repeat when their command repeats. Power and the keyboard keep their own code. All 45 earlier router tests pass unchanged with Reborn Default. `InputManager` reports holds and repeats for every button and takes the hold time; Power keeps 650 ms |
| Pages | `reborn-ui/src/buttons_page.rs`, `views_buttons.rs` | Customize > Buttons (layout, In Menus, On Now Playing, Screen Off, Save/Discard or Delete, Hold Time, Wheel Click, Haptics); face pages draw the player with a gliding glow and a sliding card; Previous, Next, Play/Pause and volume pick themselves on a face page |
| Safety | as section 3 | Start-up escape: **Back held 5 s within the first minute after Reborn starts** (a held key at start sends no event, so "hold while starting" was not possible without an ioctl). The Quick Menu gets a **Buttons** tile (**Use Default**) while another layout is in use; the bottom row then has four tiles (Sleep Timer, Buttons, Restart, Power Off). The Keep sheet has no live countdown; Back on it (or no answer) reverts after 15 s |
| Dark feedback | `app/reborn/src/main.rs` `input_ticks` | One haptic tick when a button acted in the dark (not volume, not Power, not repeats), if Haptics is on. No double tick for "nothing mapped" |
| Not built | — | Double press (it delays every single press of that button), export/import of presets, "Try Buttons", buttons while asleep (phase 4) |

### Device test (owner's player, test builds 257 and 258, 2026-11-13)

Input injected as evdev records into the real input devices, so the new router ran (rebornctl actions bypass it):

- The two older settings were at their defaults: migration chose Reborn Default (`migrated: true`).
- Wheel and Select reached Customize > Buttons; Next pressed on the In Menus face picked Next (queue unmoved).
- Next press → Shuffle On/Off, Save: the Keep sheet came with the new layout active; left alone, the old layout was
  back after 15 s and stored. Done again and Keep chosen with the physical Select: "own:0" stored; on Customize
  the physical Next toggled Shuffle instead of skipping.
- Screen Off: Centre → Shuffle; in the dark the Centre toggled Shuffle with the screen staying off; Back did
  nothing; Power woke the screen.
- Power held (injected on the PMIC keys) opened the Quick Menu; on 257 the Buttons row was drawn over Power Off
  (fixed in 258: its own tile); on 258 the tile's Select brought back Reborn Default.
- Not testable by injection: holds of the five front buttons (the polled GPIO driver reports the release within
  20 ms), hence the Back start-up escape and front-button holds are host-tested only. The haptic tick in the dark
  was not felt (nobody at the player).
- Afterwards: Reborn Default, the test layout deleted, Shuffle off again.
