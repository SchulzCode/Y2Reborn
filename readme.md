# Y2Reborn

The native music player for the **Innioasis Y2** running
[Y2Linux](https://github.com/SchulzCode/Y2Linux). It is written in Rust, decodes
with FFmpeg 9.0.2, draws with OpenGL ES on the Y2's GPU (DRM + Lima), plays
through ALSA (wired) or BlueALSA (Bluetooth), and keeps its library in SQLite.

> **Status: 1.0.0-beta.7.6, part of Y2Linux 1.0 Beta 7.6 (Baseline 67, 2026-10-16), public beta.**
> Git tag `baseline-67` in both repositories. Reborn has no installer of its own: it is built
> into the Y2Linux images. Install them from the
> [Y2Linux releases](https://github.com/SchulzCode/Y2Linux/releases) with Y2Linux's
> [INSTALL](https://github.com/SchulzCode/Y2Linux/blob/main/docs/INSTALL.md) guide; Reborn then
> updates with the system over Wi-Fi (Settings → About & Updates).

## What it does

- Library of artists, albums and songs from the internal music storage and the SD card,
  with cover art from the album folder first and the embedded picture otherwise (thumbnails
  cached in `/data/reborn/cache`); Albums as Cover Flow or a grid, and Genres.
- Wired playback at 44.1/48 kHz S16 stereo; higher-rate files are decoded and converted.
  Bluetooth playback; the codec is chosen automatically (SBC, AAC and LDAC in the public images;
  aptX/aptX HD only in owner-private builds). The output follows the headphones: Bluetooth
  headphones take over from the jack as soon as they connect (playback continues there), the
  jack takes over again when they disconnect (playback pauses). Choosing the jack by hand sticks
  until the headphones reconnect. AAC headphones get the encoder's high-quality mode at the highest
  rate they accept (AirPods: 256 kbit/s), and the rate steps down by itself on a weak link
  (service menu: AAC Quality, AAC Auto Rate; SBC XQ for SBC-only headphones).
- Internet radio (Home → Radio): Favorites, Popular, Countries, Genres and Search from the free
  community directory radio-browser.info, plus your own .m3u/.pls lists in a `Radio` folder on
  Internal Storage or the SD card. MP3, AAC and Ogg streams over Wi-Fi with the station's song
  title on Now Playing and automatic reconnect; HLS-only stations are not supported yet. Station
  logos, Station Info, lists A to Z with each country's ten most listened stations on top, and
  Previous/Next between stations. FFmpeg
  stays built without networking: `curl` receives the stream and hands Reborn plain audio.
- Search (Home → Search) across artists, albums and songs, with results shown live while typing:
  every typed word must begin a word of the name (case, accents and a leading "The" ignored).
- Playlists (Home → Playlists): .m3u/.m3u8 files from Internal Storage and the SD card, made and
  changed on the player (add from any song, album or Now Playing; remove; move with the wheel;
  rename, duplicate, delete), a Favorites playlist with a heart, and playlists exported by a
  computer (Windows, macOS or Linux paths) matched to your library. The file stays the playlist:
  edits are written back into it, songs the player cannot find are kept.
- Pause and Play continue at once: a paused song keeps its file and audio output open for up to
  three minutes (a station stops; it is live). Holding Previous/Next seeks, and Previous/Next work
  even while a song is still starting.
- A status-bar clock in the player's time zone: found automatically over Wi-Fi (once per network)
  or chosen under Settings → Display → Time Zone (country first; built-in zone rules with
  daylight saving; shown once the player's clock is set).
- A full-screen keyboard for search, station search and Wi-Fi passwords, driven by the buttons:
  Back, Play/Pause, Previous and Next move up, down, left and right; Select types; hold Select
  when done, hold Back to cancel. Pages for capitals, digits, symbols and accented letters.
- Click-wheel UI (Product UI v4, Open Sans) with motion at 60 frames per second: gliding focus,
  smooth scrolling, sliding screens, and the playing song as a wave on Now Playing. A status bar
  with the screen's name, clock, Wi-Fi, Bluetooth, a USB symbol while a computer is connected
  and the battery; short messages (a computer connected, files copying, songs added, problems)
  appear in it. Home with Now Playing, Music, Playlists, Radio, Search and Settings; Bluetooth
  pairing, low battery, hold-to-turn-off, startup and shutdown screens; Software Update in one
  step (Update Now: Download · Check · Install with progress, speed and time left; Cancel during
  the download). A to Z lists ignore case, accents and a leading "The".
- Skins: Settings → Skin switches between Reborn's own look and skins that
  restyle every screen (colours, fonts, pictures, layouts). Seventeen skins ship with it (Classic, Nano Glass, Walkman, Terminal, Winamp, Paper, Big Type,
  Cassette, Neon Night, Zune, Bunny Bow, Gallery and five Mini colours);
  more are added by copying a folder into `Skins` on the player. See [skins](docs/skins.md).
- Works with the platform: sleep/wake preparation, workload hints to the CPU governor,
  `rebornctl` and a control socket at `/run/reborn/control.sock`.

Known limits (details in [current state](docs/CURRENT_REBORN_STATE.md)): native
high-rate output is gated; AAC is negotiated with AirPods Pro but not yet listened to; saved
Bluetooth headphones are not connected on play; the player cannot tell whether wired headphones are
plugged in (no jack detection yet), so "jack" means "not Bluetooth". Crossfade and EQ have not been checked on hardware.

## Development

Use Rust **1.99.0**. `rust-toolchain.toml` pins it together with the
`armv7-unknown-linux-gnueabihf` target. All crates are vendored in `vendor/`,
so builds work offline. Host builds link the system's development libraries
through `pkg-config`: FFmpeg (`libavformat`, `libavcodec`, `libavutil`,
`libavfilter`, `libswresample`, `libswscale`), ALSA, D-Bus, SQLite, libdrm, GBM,
EGL and GLES v2.

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

UI previews render the production drawing code on the host
(`tools/preview/render_previews.py`, output in `docs/ui/previews/v3/`).

The ARM binary is built only through Y2Linux's Buildroot, which provides the
cross toolchain, sysroot, FFmpeg, SQLite, ALSA, D-Bus and Mesa. Check out this
repository **next to** Y2Linux (`../Y2Reborn`). `tools/build/cross.sh` is
what the Buildroot recipe calls. Host tests don't prove anything about the real Y2:
display, audio and power behaviour need the device.

| Path | Contents |
| --- | --- |
| `app/reborn` | The player binary, `rebornctl` and `reborn-bench` |
| `crates/` | `reborn-core` (state), `reborn-ui`, `reborn-graphics`, `reborn-audio`, `reborn-media` (FFmpeg membrane), `reborn-library` (SQLite), `reborn-platform` (Y2Linux API), `reborn-control`, `reborn-observability` |
| `assets/` | Fonts, UI art and media test fixtures |
| `tools/` | Cross build, previews, asset generation, qualification helpers |
| `vendor/` | Vendored, checksummed crate sources |
| `docs/` | Documentation; start at [docs/README.md](docs/README.md) |

On the device the player uses `/data/reborn` for its state, plays music from
`/media/internal/music` (or `/data/music` before the internal music partition is
set up) and `/media/sd`.

## Licence

MIT (see [LICENSE](LICENSE)). The Open Sans glyph data is under the SIL Open Font
Licence (`assets/fonts/OpenSans-OFL.txt`). No firmware, keys, Bluetooth bonds or
private device data belong in this repository.
