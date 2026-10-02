# Boot progress and display hand-off

<!-- knowledge-base-scope: source-contract -->
> **Source contract, software-validated only.** Not flashed. The first boot of
> a build containing it is the only way to measure splash-visible, first-frame
> and fade times on the device.

## Ownership

The early splash (Y2Linux `tools/graphics/reborn-splash.c`, started from the
initramfs) owns the display from early boot until Reborn has rendered its
**first complete UI frame**. Reborn never presents a boot screen of its own as
its first frame. The sequence is:

1. The splash shows the Reborn wordmark, one thin white bar and a short status
   line. The bar fills left to right to coarse positions of real milestones.
2. Reborn finishes starting, then renders the real Home frame with the full boot
   screen over it (`reborn_ui::boot_transition`, remaining = 1). The bar is
   already full and the status is "Starting Reborn" (milestone `ready`).
3. Reborn sends `READY 1`. The splash completes bar and status if they are not
   already there, drops DRM master and replies `RELEASED 1`. There is no timer.
4. Reborn takes master, presents that frame, and sends `PRESENTED 1`; the splash
   destroys its buffer and exits. The first presented frame is pixel-identical
   to the splash's last frame.
5. Reborn dissolves the cover over `BOOT_FADE_FRAMES` (7) frames, ≈0.24 s, then
   only the UI is drawn. `application_ready` is announced after the first frame
   is presented, not before.

## Why the logo used to stay up

Reborn handed off as soon as its renderer existed (`graphics_ready`) and
presented `boot_frame()`, a static copy of the logo. It then ran the rest of
startup on its render thread (media sources, database and scanner, audio,
Wi-Fi and Bluetooth workers, control server, diagnostics, platform client,
artwork, AVRCP) and only drew again when that loop began. The splash was gone,
so nothing animated and the logo stayed for the whole remainder of startup;
the 6-step dissolve started only afterwards, and `application_ready` was
announced for the logo frame. The startup path stalling, not a fixed delay,
set how long it stayed; no on-device timing exists yet.

## Milestones

Producers write a milestone name to `/run/reborn-splash/phase`: the initramfs
through `y2_stage` (its existing stage names) and Reborn through
`startup_phase` (`reborn_platform::contract::boot_milestone`). The table lives
in `reborn_ui::BOOT_PHASES`; `reborn-preview` exports it with the rendered
text to `docs/ui/previews/v2/boot-layout.json`, and Y2Linux
`tools/graphics/make-splash-mark.py` generates `reborn-splash-mark.h` from it.

| Milestone | Status line | Bar |
| --- | --- | --- |
| `start` | Starting system | empty |
| `storage_discovery` … `fsck_complete` | Preparing storage | 6 → 25 % |
| `root_data_mounted` … `model_restored` | Starting system | 34 → 47 % |
| `graphics_ready`, `storage_ready` | Loading music library | 60, 66 % |
| `library_workers_ready`, `core_services_ready` | Starting audio | 73, 78 % |
| `audio_ready` | Starting connectivity | 84 % |
| `radio_workers_ready`, `runtime_ready`, `ready` | Starting Reborn | 90 → 100 % |

The positions are ordinal weights, not time or work estimates, and are never
shown as numbers. The bar only moves forward: earlier or unknown names are
ignored, a missing milestone leaves the bar where it is until a later one
arrives, and nothing advances on a timer. The splash redraws only while the bar
is gliding to a new position (33 ms ticks) and is otherwise asleep in `poll`.

The initramfs `rescue` stage, a 60 s timeout and a failed hand-off show
"Could not start / Restart the player" without a bar. A late `READY` still
works.

## Shutdown

Reborn dissolves the UI into the same screen with a full bar and **Saving**,
saves (audio, session, database), then shows **Shutting down** (or
**Restarting**, **Battery empty**) while the bar drains right to left and the
screen dims. It presents a black frame, waits one refresh, turns the backlight
off and acknowledges. It does not invent progress: the two labels are the two
real phases. If Reborn never acknowledges (hang, crash, deadline), the platform
turns the backlight off *before* stopping Reborn, so restoring the fbdev buffer
cannot show on a lit panel.

## Checks

- `cargo test -p reborn-ui` (phase table, bar geometry, no accent colour, fade
  and shutdown schedules) and `cargo test -p reborn --test boot_handoff_guard`
  (first frame is the real UI under the cover, readiness after presentation).
- Y2Linux `tests/test_reborn_splash.py` (milestone mapping, no regression,
  missing/failed milestones, pixel identity with Reborn, hand-off timing, idle
  without wake-ups) and `tests/test_platform_power_contract.py`.
- Previews: `docs/ui/previews/boot/contact-sheet.png`, made by Y2Linux
  `tools/graphics/boot-previews.py`.
