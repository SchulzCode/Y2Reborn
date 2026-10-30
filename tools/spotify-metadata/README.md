# reborn-spotify-metadata

The helper Reborn runs to list a Spotify library (playlists, Liked Songs, saved
albums, followed artists and their songs): `crates/reborn-platform/src/spotify.rs`
(`browse`) is its only caller. Ported unchanged in substance from PiPod-OS's
`pipod-spotify-metadata`, over the vendored [housecat](https://github.com/ambb205/housecat)
crates (`vendor/housecat`, MIT).

Its own Cargo workspace, outside Reborn's: see `../spotify/README.md` for why and
how to build and install it. To try it against a signed-in player's state:

```sh
REBORN_SPOTIFY_CACHE_DIR=/data/reborn/state/spotify/cache/oauth \
    reborn-spotify-metadata playlists
```
