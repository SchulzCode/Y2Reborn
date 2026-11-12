# Y2Reborn

**The music player for the Innioasis Y2 running [Y2Linux](https://github.com/SchulzCode/Y2Linux).** Made for the
click wheel: smooth at 60 frames per second, quick with large libraries, and it looks the way you want with seventeen
skins. Written in Rust, it decodes with FFmpeg, draws with OpenGL ES on the Y2's GPU and keeps its library in SQLite.

<p align="center">
  <img src="docs/images/reborn-hero.png" alt="Reborn's Home screen, Now Playing with its progress wave, and Cover Flow" width="100%">
</p>

> **Reborn comes with Y2Linux.** It has no installer of its own: install Y2Linux from its
> **[latest release](https://github.com/SchulzCode/Y2Linux/releases/latest)** with the
> [install guide](https://github.com/SchulzCode/Y2Linux/blob/main/docs/INSTALL.md), and Reborn updates together with
> the system over Wi-Fi (Settings → System → Software Update). Reborn and Y2Linux share version numbers and the
> `baseline-N` tags.

## What it does

**Finding your music**
- Home with Now Playing, Music, Playlists, Radio, Search and Settings (choose which, and in which order).
- Artists, albums, songs, genres and folders from Internal Storage and the SD card; albums as **Cover Flow** or a
  grid; covers from the album folder (`cover.jpg`, `folder.jpg`, …) or from the file.
- Fast scrolling: turn the wheel quickly and long lists move several rows at a time; spin it and they jump letter
  by letter.
- **Search** with results while you type, across artists, albums, songs and playlists, with See All per section.
- **Up Next**: Play Next and Add to Queue for songs, albums, artists, genres and folders; move or remove songs; the
  songs you heard before stay in the list.
- **Playlists** (`.m3u`/`.m3u8`) made and changed on the player or copied from a computer, and Favorites with a
  heart. Changes are written back into the playlist file.

**Listening**
- Gapless playback, crossfade up to 12 s, shuffle by song or by album, repeat and a sleep timer.
- A five-band equalizer with presets, headphone profiles for 47 headphones and IEMs (AutoEq) plus your own AutoEq
  `ParametricEQ.txt` files, Normalize Volume (ReplayGain), balance, headphone level and DAC filter.
- **Bluetooth headphones** with SBC, AAC and LDAC, picked automatically; the music follows them when they connect.
- **Internet radio** from radio-browser.info: popular stations, countries, genres, search, favorites and your own
  `.m3u`/`.pls` lists, with station logos and the song that is playing.
- **Audiobooks** with chapters, your place in every book, a speed per book (0.75–2×) and a sleep timer
  ([guide](https://github.com/SchulzCode/Y2Linux/blob/main/docs/AUDIOBOOKS.md)).
- **Audiobookshelf**: browse a server and download books to listen offline ([guide](docs/audiobookshelf.md)).
- **Last.fm scrobbling**: scan a QR code with your phone to sign in; plays wait on the player while offline
  ([guide](https://github.com/SchulzCode/Y2Linux/blob/main/docs/LASTFM.md)).

**Making it yours**
- **Seventeen skins** (Classic, Nano Glass, Walkman, Terminal, Winamp, Paper, Big Type, Cassette, Neon Night, Zune,
  Bunny Bow, Luna XP and five Mini colours), and more by copying a skin folder into `Skins` on the player
  ([how to make one](docs/skins.md)).
- Customize: Cover Flow on or off, how albums are sorted, lists that wrap around, the wheel on Now Playing (volume or
  seeking), a wheel click, haptic ticks and what the buttons do while the screen is off.
- A status bar with the clock, Wi-Fi, Bluetooth, a computer connection and the battery, and short messages when
  something happens.

<p align="center">
  <img src="docs/images/reborn-browse.png" alt="Music, an album page and live search results" width="100%">
  <br><em>Music, an album and live search</em>
</p>

<p align="center">
  <img src="docs/images/reborn-listen.png" alt="Internet radio, an audiobook playing and the Up Next queue" width="100%">
  <br><em>Internet radio, an audiobook and Up Next</em>
</p>

<p align="center">
  <img src="docs/images/reborn-settings.png" alt="Settings, the sound output page and a software update downloading" width="100%">
  <br><em>Settings, where the sound goes, and an update over Wi-Fi</em>
</p>

<p align="center">
  <img src="docs/images/reborn-skins.png" alt="Now Playing in eight of the seventeen skins" width="100%">
  <br><em>Now Playing in eight of the seventeen skins</em>
</p>

*The pictures come from Reborn's own drawing code with sample music (`reborn-preview`, 1.0 Beta 9.2.13).*

What is known not to work yet is listed in Y2Linux's
[known issues](https://github.com/SchulzCode/Y2Linux/blob/main/readme.md#known-issues) and in
[Reborn's current state](docs/CURRENT_REBORN_STATE.md). Found a problem? Please
[open an issue](https://github.com/SchulzCode/Y2Reborn/issues) and send an error report from the player
(Settings → System → Error Reports).

## How scrobbling stays safe

- **Your password:** with the QR sign-in it is typed only into Last.fm's own page on your phone; the fallback sends
  it once over HTTPS and never stores, logs or displays it.
- **Your sign-in:** the player keeps only Last.fm's session key, in one root-only file in `/data/reborn/state` (not
  reachable over USB, never in logs, diagnostics or update images). It can scrobble but cannot read or change your
  password or e-mail; revoke it any time on last.fm (Settings → Applications).
- **The connection:** HTTPS with certificate checks only; every request is signed as Last.fm requires; keys and data
  travel in the request body on curl's standard input, never on a command line.
- **The app key** is not in this repository. Y2Linux's release build installs it as a root-only file
  (`/usr/share/reborn/lastfm-api.json`); a build without it simply has no Last.fm. Forks: register your own key.

Code: `crates/reborn-core/src/scrobble.rs` (what counts), `crates/reborn-platform/src/scrobble.rs` (offline queue,
log, clock), `crates/reborn-platform/src/lastfm.rs` (Last.fm API, signing), `app/reborn/src/scrobbling.rs` (the
player's side) and `crates/reborn-ui/src/qr.rs` (QR codes).

## Development

Use Rust **1.99.0**; `rust-toolchain.toml` pins it together with the `armv7-unknown-linux-gnueabihf` target. All
crates are vendored in `vendor/`, so builds work offline. Host builds link the system's development libraries through
`pkg-config`: FFmpeg (`libavformat`, `libavcodec`, `libavutil`, `libavfilter`, `libswresample`, `libswscale`), ALSA,
D-Bus, SQLite, libdrm, GBM, EGL and GLES v2. Code follows Y2Linux's
[coding standard](https://github.com/SchulzCode/Y2Linux/blob/main/docs/CODING-STANDARD.md).

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

UI previews render the production drawing code on the host with sample data:

```sh
cargo run --release --bin reborn-preview -- OUT [--skin skins/classic]
python3 tools/preview/render_previews.py OUT --output PNG_DIR
```

The ARM binary is built only through Y2Linux's Buildroot, which provides the cross toolchain, sysroot, FFmpeg,
SQLite, ALSA, D-Bus and Mesa. Check out this repository **next to** Y2Linux (`../Y2Reborn`);
`tools/build/cross.sh` is what the Buildroot recipe calls. Host tests don't prove anything about the real Y2:
display, audio and power behaviour need the device.

| Path | Contents |
| --- | --- |
| `app/reborn` | The player binary, `rebornctl`, `reborn-bench` and `reborn-preview` |
| `crates/` | `reborn-core` (state), `reborn-ui`, `reborn-graphics`, `reborn-audio`, `reborn-media` (FFmpeg membrane), `reborn-library` (SQLite), `reborn-platform` (Y2Linux API), `reborn-control`, `reborn-observability` |
| `skins/` | The seventeen shipped skins |
| `assets/` | Fonts, UI art and media test fixtures |
| `tools/` | Cross build, previews, asset generation, qualification helpers |
| `vendor/` | Vendored, checksummed crate sources |
| `docs/` | Documentation; start at [docs/README.md](docs/README.md) |

On the device the player keeps its state in `/data/reborn` (including the Last.fm session and waiting plays in
`state/`) and plays music from `/media/internal/music` and `/media/sd`.

## Licence

MIT (see [LICENSE](LICENSE)). The Open Sans glyph data is under the SIL Open Font Licence
(`assets/fonts/OpenSans-OFL.txt`). No firmware, keys (including the Last.fm API key), Bluetooth bonds or private
device data belong in this repository.
