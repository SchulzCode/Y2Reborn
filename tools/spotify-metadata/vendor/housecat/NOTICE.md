# Vendored: housecat

The crates in this directory (`housecat-core`, `housecat-metadata`,
`housecat-protocol`, and the `housecat` facade) are vendored from
[ambb205/housecat](https://github.com/ambb205/housecat), MIT licensed
(see `LICENSE` in this directory).

`housecat-audio` is deliberately **not** vendored here — this workspace
uses housecat only for Spotify metadata/library access (playlists,
liked songs, saved albums, followed artists). Audio playback stays on
`spotifyd`/`librespot` (`crates/reborn-platform/src/spotify.rs`).

Copied as PiPod-OS vendored it (`firmware/target/hosted/pipod/spotify-metadata`),
so a build doesn't depend on GitHub being reachable. Update by re-copying
`crates/{housecat-core,housecat-metadata,housecat-protocol,housecat}`
from a fresh checkout of the upstream repo (dropping each crate's
`tests/`/`examples/` directories, which this build never runs) and
diffing before committing.
