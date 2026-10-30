# Y2Reborn

The native music player for the **Innioasis Y2** running
[Y2Linux](https://github.com/SchulzCode/Y2Linux). It is written in Rust, decodes
with FFmpeg 9.0.2, draws with OpenGL ES on the Y2's GPU (DRM + Lima), plays
through ALSA (wired) or BlueALSA (Bluetooth), and keeps its library in SQLite.

> **Status: 1.0.0-beta.8.43, part of Y2Linux 1.0 Beta 8.43 (Baseline 164, 2026-10-28), public beta.**
> Git tag `baseline-164` in both repositories. Reborn has no installer of its own: it is built
> into the Y2Linux images. Install them from the
> [Y2Linux releases](https://github.com/SchulzCode/Y2Linux/releases) with Y2Linux's
> [INSTALL](https://github.com/SchulzCode/Y2Linux/blob/main/docs/INSTALL.md) guide; Reborn then
> updates with the system over Wi-Fi (Settings → About & Updates).

## What it does

- Library of artists, albums and songs from Internal Storage and the SD card ([how storage
  works](https://github.com/SchulzCode/Y2Linux/blob/main/docs/STORAGE.md)),
  with cover art from the album folder first and the embedded picture otherwise (thumbnails
  cached in `/data/reborn/cache`); Albums as Cover Flow or a grid, and Genres.
- Wired playback at 44.1 kHz S16 stereo, as on stock; 48 kHz and other rates are decoded and converted.
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
- **Last.fm scrobbling** (Settings → Last.fm): connect by scanning a QR code with your phone
  (password sign-in as a fallback). A library song counts once heard for half its length or
  4 minutes (pauses and skipping ahead don't count; radio and untagged files never do), Now
  Playing shows on your profile, plays are kept on the player while offline and sent in batches
  when Wi-Fi is back, and an optional Rockbox-format `.scrobbler.log` works without an account.
  Nothing is recorded until you opt in; Disconnect and Reset Settings forget the sign-in and its
  waiting plays. Full guide, privacy and security:
  [Last.fm scrobbling](https://github.com/SchulzCode/Y2Linux/blob/main/docs/LASTFM.md).
- **Spotify** (Home → Spotify, Premium account; ported from
  [PiPod-OS](https://github.com/daavid-codes/PiPod-OS)): connect by scanning a QR code with your
  phone (Spotify's device sign-in; no password on the player), then Playlists (including ones you
  only follow), Liked Songs, Albums and Artists (Top Songs, Albums, All Songs). A song plays within
  the playlist or album it was chosen from, so Previous/Next stay in it. `spotifyd` streams and
  Reborn plays its sound through its own output, so the equalizer, volume, Bluetooth and the jack
  apply as to any song; Now Playing shows the song, its cover and position. The player is also a
  Spotify Connect device of your account: the phone app can start, pause and skip it. Needs
  `spotifyd` and `reborn-spotify-metadata` in the Y2Linux image ([how](tools/spotify/README.md));
  without them Spotify isn't on Home. Not yet tried on a Y2.
- Sound and playback: five-band equalizer with presets, headphone profiles (47 common
  headphones and IEMs from AutoEQ built in, more as `ParametricEQ.txt` files in an `EQ` folder; up to 10
  filters with a clip-safe preamp),
  headphone output level (Low/Medium/High) and DAC filter, Normalize Volume (ReplayGain), gapless
  playback, crossfade (up to 12 s), shuffle and repeat.
- Search (Home → Search) across artists, albums and songs, with results shown live while typing:
  every typed word must begin a word of the name (case, accents and a leading "The" ignored).
- Storage (Settings → Storage): free space of Internal Storage and the SD card, Scan for Music, and
  the setup of Internal Storage when Y2Linux found a large old Innioasis library (it asks once
  and keeps the old files readable until you choose *Set Up Internal Storage*). A damaged SD card
  says *Needs repair · Check it on a computer*.
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
- A full-screen keyboard for search, station search, Wi-Fi passwords and the Last.fm password
  sign-in, driven by the buttons:
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
  restyle every screen (colours, fonts, pictures, layouts). Sixteen skins ship with it (Classic, Nano Glass, Walkman, Terminal, Winamp, Paper, Big Type,
  Cassette, Neon Night, Zune, Bunny Bow and five Mini colours);
  more are added by copying a folder into `Skins` on the player. See [skins](docs/skins.md).
- Error reports (Settings → About & Updates → Error Reports), opt-in after a one-time question:
  sent over Wi-Fi when something goes wrong or with Send Report Now, or saved to Internal Storage
  or the SD card (Save Report) to attach to a GitHub issue. The player's logs and technical
  details with personal data removed, in about ten seconds, with the progress on screen
  ([Error reports](https://github.com/SchulzCode/Y2Linux/blob/main/docs/ERROR-REPORTS.md)).
- Works with the platform: sleep/wake preparation, workload hints to the CPU governor,
  `rebornctl` and a control socket at `/run/reborn/control.sock`.

Known limits (details in [current state](docs/CURRENT_REBORN_STATE.md)): native
high-rate output is gated; AAC is negotiated with AirPods Pro but not yet listened to; saved
Bluetooth headphones are not connected on play; the player cannot tell whether wired headphones are
plugged in (no jack detection yet), so "jack" means "not Bluetooth". Crossfade and EQ have not been checked on hardware.
Last.fm scrobbling works on the player (QR sign-in and real scrobbles confirmed); sending plays
that waited offline across sleep is still to be checked.

## How scrobbling stays safe

- **Your password:** with the QR sign-in it is typed only into Last.fm's own page on your phone;
  the fallback sends it once over HTTPS and never stores, logs or displays it.
- **Your sign-in:** the player keeps only Last.fm's session key, in one root-only file in
  `/data/reborn/state` (not reachable over USB, never in logs, diagnostics or update images). It
  can scrobble but cannot read or change your password or e-mail; revoke it any time on last.fm
  (Settings → Applications).
- **The connection:** HTTPS with certificate checks only; every request is signed as Last.fm
  requires; keys and data travel in the request body on curl's standard input, never on a command
  line.
- **The app key:** Y2Reborn's Last.fm API key and secret are not in this repository. Y2Linux's
  release build installs them as a root-only file (`/usr/share/reborn/lastfm-api.json`); a build
  without that file simply has no Last.fm. Like every client app, a released image necessarily
  contains the key; it identifies the app but gives no access to any account, and it can be
  replaced with an update if ever misused. Forks: register your own key.

Code: `crates/reborn-core/src/scrobble.rs` (what counts), `crates/reborn-platform/src/scrobble.rs`
(offline queue, log, clock), `crates/reborn-platform/src/lastfm.rs` (Last.fm API, signing),
`app/reborn/src/scrobbling.rs` (the player's side) and `crates/reborn-ui/src/qr.rs` (QR codes).

## Spotify

- **Sign-in:** OAuth device authorization with the client id librespot uses; the player keeps only
  the refresh token (`/data/reborn/state/spotify/account.json`, root-only) and spotifyd's own
  reusable credentials. Disconnect removes both. No token is ever logged.
- **Pieces:** `crates/reborn-platform/src/spotify.rs` (sign-in, spotifyd as a child process, its
  sound through a FIFO, MPRIS control on the system bus, the library helper),
  `app/reborn/src/spotify.rs` (the player's side), `crates/reborn-ui` (`SpotifyView`, the pages),
  `tools/spotify` (spotifyd patch, D-Bus policy, build notes) and `tools/spotify-metadata` (the
  library helper, its own workspace).

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

On the device the player uses `/data/reborn` for its state (including the Last.fm session and
waiting plays in `state/`), plays music from
`/media/internal/music` (or `/data/music` before the internal music partition is
set up) and `/media/sd`.

## Licence

MIT (see [LICENSE](LICENSE)). The Open Sans glyph data is under the SIL Open Font
Licence (`assets/fonts/OpenSans-OFL.txt`). The spotifyd patch in `tools/spotify` is under spotifyd's GPL-3.0;
the vendored housecat crates in `tools/spotify-metadata` are MIT. No firmware, keys (including the Last.fm API key),
Bluetooth bonds or private device data belong in this repository.
