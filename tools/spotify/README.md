# Spotify on the Y2

Reborn's Spotify (Home → Spotify) is ported from
[PiPod-OS](https://github.com/daavid-codes/PiPod-OS), a Rockbox fork whose Spotify
client is written in C. The parts are the same; how they meet the player is not:

| Part | PiPod-OS | Y2Reborn |
| --- | --- | --- |
| Sign-in | Device Authorization Grant, QR code (`spotify_streaming_auth-pipod.c`) | The same, in Rust (`crates/reborn-platform/src/spotify.rs`), QR drawn by `reborn-ui` |
| Playback | `spotifyd` writes to ALSA itself | `spotifyd` writes PCM into a FIFO; Reborn decodes it as a streamed WAV, so the equalizer, volume, Bluetooth and the jack apply |
| Control | `busctl --user` (session bus) | the `dbus` crate on the **system** bus |
| spotifyd's life | a `systemd --user` unit | a child of Reborn, started when Spotify is first used |
| Library | `pipod-spotify-metadata` | the same helper, renamed `reborn-spotify-metadata` (`tools/spotify-metadata`) |

Nothing here is built by Reborn's own Cargo workspace: both programs have
dependencies that are not in `vendor/`. A build without them simply has no
Spotify (the page says so, and Home doesn't list it).

## What Y2Linux has to install

1. **spotifyd 0.4.2** at `/usr/bin/spotifyd`, built with the `dbus_mpris`
   feature (the `pipe` backend needs no feature), with
   `0001-add-playcontexttrack-dbus-method.patch` applied:

   ```sh
   git clone --branch v0.4.2 --depth 1 https://github.com/Spotifyd/spotifyd.git
   cd spotifyd
   patch -p1 < ../Y2Reborn/tools/spotify/0001-add-playcontexttrack-dbus-method.patch
   cargo build --release --locked --no-default-features --features dbus_mpris \
       --target armv7-unknown-linux-gnueabihf
   ```

   The patch adds `rs.spotifyd.Controls.PlayContextTrack(context, track)`, so a
   song chosen in a playlist plays *in that playlist* (Next and Previous stay in
   it) and is picked by its URI, never by its number on an album. Without the
   patch Reborn falls back to `OpenUri` with the song alone (its own album). The
   patch's reasoning is in PiPod-OS's `tools/spotifyd-patches/README.md`; it is
   unchanged here.

2. **reborn-spotify-metadata** at `/usr/bin/reborn-spotify-metadata`. Its
   workspace is `tools/spotify-metadata` (housecat, vendored; tokio, reqwest and
   prost from crates.io). Run Cargo from outside this repository, because the
   repository's `.cargo/config.toml` keeps Reborn's own builds offline:

   ```sh
   cd /
   cargo build --release --locked --target armv7-unknown-linux-gnueabihf \
       --manifest-path /path/to/Y2Reborn/tools/spotify-metadata/Cargo.toml
   ```

   For an offline Buildroot build, run `cargo vendor` in that directory and let
   the recipe use the result.

3. **`rs.spotifyd.conf`** in `/usr/share/dbus-1/system.d/`: lets spotifyd (root)
   own its name on the system bus and Reborn (root) call it.

`REBORN_SPOTIFYD` and `REBORN_SPOTIFY_METADATA` override both paths (host
testing).

## On the player

- **State:** `/data/reborn/state/spotify/` (0700): `account.json` (refresh
  token, 0600), `cache/` (spotifyd's reusable credentials; `no_audio_cache`, so
  no music is stored), `spotifyd.conf` (written by Reborn each start) and the FIFO
  `audio.pcm`. Disconnect removes all of it.
- **spotifyd's configuration:** `backend = "pipe"` into the FIFO, 320 kbit/s,
  `volume_controller = "none"` (Reborn's volume applies), `use_mpris` on the
  system bus, `disable_discovery` (no Zeroconf; the player is still a Spotify
  Connect device of the signed-in account, so the phone app can control it).
- **Sound path:** spotifyd → FIFO → `spotify-pump` thread → a pipe that starts
  with a WAV header (sizes unknown) → `reborn_media::Decoder::open_stream`. With
  no decoder attached the pump stops reading, so spotifyd waits instead of
  racing through the song. A new song chosen on the player drops what was still
  buffered from the one before.
- **Following spotifyd:** about once a second the control thread reads
  `PlaybackStatus`, `Position` and `Metadata`; Now Playing shows the song, its
  length, position and cover (fetched like a station logo). Pausing or skipping
  from the phone is followed after it has lasted two seconds.
- **Logs:** subsystem `spotify` (sign-in, spotifyd starting, stopping and
  refusing commands). No token is ever logged.

## Licences

`0001-add-playcontexttrack-dbus-method.patch` changes spotifyd's source and is
under spotifyd's licence (GPL-3.0); spotifyd runs as its own program, not linked
into Reborn. housecat (`tools/spotify-metadata/vendor/housecat`) is MIT.

## Not yet verified on a Y2

This port compiles and its host tests pass; the real device has not run it.
Check, in this order: spotifyd 0.4.2 accepts `dbus_type = "system"` and the
policy above lets it own `rs.spotifyd.instance<pid>`; the sign-in writes
`cache/oauth/credentials.json` and spotifyd connects with it; sound reaches
the FIFO (`backend = "pipe"`, `device = <fifo>`); the helper lists playlists.
