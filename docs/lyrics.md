# Synchronized lyrics

Open the Now Playing options menu and select **Lyrics**. This source candidate adds a native
480×360 viewer; it has not been tested on a physical Y2 and is proposed as a draft contribution.

Place a UTF-8 `.lrc` file beside the local song, with the same base name:

```text
Music/Homeward.mp3
Music/Homeward.lrc
```

The reader checks `.lrc`, then `.LRC`, in that directory on Internal Storage or an available
SD-card source. It does not search the internet or read embedded lyric tags. Radio retains its
existing menu. This example uses original demonstration lyrics:

```lrc
[ar:Reborn original examples]
[ti:Homeward]
[offset:+250]
[00:02.00]A quiet street begins to glow
[00:05.000]The harbour wakes below
[00:08.00][00:29.50]We bring the long way home
[00:20.00]
```

Positive offsets advance all entries: `+250` makes the first line appear at 1.750 seconds.
Negative offsets delay them. The final valid offset applies to the entire file; adjusted times
below zero clamp to zero. Multiple timestamps repeat a line. Valid entries are sorted by time,
preserving file order for equal times; simultaneous lines share the accent. Before the first
timestamp no line is highlighted. The final line remains until the track changes. Empty timed
lines mark instrumental breaks.

The parser accepts integer minutes, two-digit seconds from 00–59, and optional one-, two- or
three-digit fractional seconds. UTF-8 BOM, LF/CRLF and Unicode text are supported. Metadata such
as `[ar:]`, `[ti:]` and `[al:]` is ignored; the header uses the playing song's title. Malformed
timestamps and untimed lines are ignored. Invalid encoding or limits produce a readable error
without changing playback. Enhanced word timing, translation selection, embedded lyrics and
automatic downloads are outside this implementation. Missing glyphs use the existing font fallback.

## Controls

| Input in Lyrics | Action |
| --- | --- |
| Wheel | Browse the text without seeking audio |
| Centre | Return to following the current line |
| Back | Return to Now Playing |
| Play/Pause, Previous/Next, held seeks, volume | Existing global playback actions |

There is no new global gesture. Track changes and seeking return the viewer to the audio
position. Paused lyrics remain at the paused position. Reduced UI motion skips scroll easing.
Close and reopen Lyrics to reload a sidecar edited while it was open.

Missing files show **No lyrics found**, followed by
“Add an .lrc file beside your song to display synced lyrics.”

## Architecture and resource bounds

`reborn-core::lyrics` parses bytes and finds the active entry by binary search. A lazy application
worker uses the existing descriptor-based storage guard to open only regular sibling files.
It reads, parses and prepares wrapped rows in the background. One replaceable pending job and
one result bound the queue. Revision tokens reject results after navigation, track changes,
source replacement or cancellation.

The UI receives an immutable prepared document. Drawing performs no filesystem access, parsing
or paragraph layout. It selects visible rows by binary search and uses `AppModel.position_ms`,
the existing playback position, as the only lyric clock. Existing motion helpers animate the
presentation without advancing lyric timing. Changed lines, input, results and unsettled motion
request redraws. Normal playback never starts the reader until Lyrics is opened.

The cache covers only the displayed track and is cleared when Lyrics closes or its track/source
revision changes. Storage removal clears stale content. Sleep preparation cancels pending work
and waits for the active reader to release storage. FFmpeg and audio playback are unchanged.

Limits are 256 KiB of file bytes, 8,192 input lines, 4,096 bytes per input line, 4,096 timed
entries, 32 timestamps per input line and 256 KiB of expanded lyric text. Repeated timestamps
share parsed strings. Rows and entry centers are bounded by these limits; only visible rows are
drawn. These are resource bounds, not on-device memory or latency qualification.

The screen uses existing native fonts, skin colors and opaque clipping bands. It does not add
`{lyrics}`, change the skin compiler, or edit any of the seventeen shipped skins. Custom skins
use the ordinary native fallback in their existing colors.

## Verification and previews

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
../Y2Linux/tools/development/platform-gate.sh
cargo run --bin reborn-lyrics-preview --locked --offline -- /tmp/lyrics-preview skins/classic
python3 tools/preview/render_previews.py /tmp/lyrics-preview
```

The [original fixture](../assets/fixtures/lyrics-original.lrc) supplies the production UI previews.
[Host validation](validation/synced-lyrics-host.md) records actual results. Before merge, verify
real playback, seeks, repeat/shuffle, Bluetooth position reporting, SD removal and repeated
sleep/wake on a physical Y2.
