//! Spotify library browsing for Y2Reborn, without the public Web API.
//!
//! Ported from PiPod-OS's `pipod-spotify-metadata`. One-shot CLI:
//! `reborn-spotify-metadata <command> [arg] [cursor]`. It reads
//! `REBORN_SPOTIFY_CACHE_DIR` for the credentials housecat-core needs to
//! connect (the *same* directory Reborn writes for spotifyd at sign-in, so
//! one device-grant login covers playback and browsing), prints one JSON
//! object to stdout, and exits. `crates/reborn-platform/src/spotify.rs`
//! (`browse`) is the only caller and treats a non-zero exit as a failure;
//! its last stderr line says why.
//!
//! Why not the Web API: Spotify's Development Mode Web API cannot read a
//! playlist the account only follows, and Extended Quota Mode is closed to
//! projects like this one. Spotify's own client protocol, which the official
//! app uses with the same session, has no such restriction.
//!
//! Output shape, the same for every command:
//!   `{"items": [{"label": "...", "uri": "spotify:...", "id": "base62-or-absent"}, ...], "next": "opaque-cursor-or-absent", "context": "spotify:...-or-absent"}`
//! `next`, when present, is passed back verbatim as the `cursor` argument
//! of the following call. `context`, only for "liked-songs", is a context
//! URI for the whole list.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use housecat_core::auth::cache::CacheDir;
use housecat_core::{Error, IdKind, Session, SpotifyId};
use housecat_metadata::{Album, Artist, Catalog, Metadata, Page, Paging, Saved, Track};

#[derive(serde::Serialize)]
struct Row {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(serde::Serialize)]
struct Output {
    items: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    /// A ready-made Spirc context URI covering this *entire* list, not
    /// just one row -- only ever set for "liked-songs" below. Lets Reborn
    /// give PlayContextTrack a real "spotify:user:<id>:collection" context
    /// (the same one Spotify's own official app uses for its Liked
    /// Songs screen) instead of falling back to a tapped track's own
    /// *album* as context, which made Next/Previous walk that album
    /// instead of the rest of Liked Songs. Every row's own `id` field
    /// still carries the track's own album id (main.rs's track_row(),
    /// unchanged) since other views (artist-top-tracks, etc.) still
    /// need it as their own fallback context.
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

/// Rows carry at most 300 items regardless of what Spotify would hand back
/// for one artist's full discography (artist-all-tracks) or one playlist --
/// a browse-and-pick screen doesn't need an artist's entire back catalog
/// resident, and a pathological playlist/discography can't produce an
/// unbounded response.
const MAX_ITEMS: usize = 300;
/// How many of an artist's own albums+singles this expands into full
/// tracklists for "every track by this artist" -- bounds the number of
/// album fetches a single artist-all-tracks call can make.
const MAX_ALBUMS_FOR_ALL_TRACKS: usize = 60;

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("[reborn-spotify-metadata] {msg}");
    ExitCode::FAILURE
}

fn parse_id(kind: IdKind, arg: &str) -> Result<SpotifyId, Error> {
    SpotifyId::from_base62(kind, arg).map_err(|e| Error::protocol("id", e.to_string()))
}

/// `cursor` is either empty (first page) or a decimal offset this same
/// process previously returned as `next` -- see the module doc comment.
fn parse_paging(cursor: &str, limit: u32) -> Paging {
    Paging { offset: cursor.parse().unwrap_or(0), limit }
}

/// `id` carries the track's own containing album's id, not the track's
/// own -- Reborn uses it as the Spirc context for views with no better-known
/// container of their own (artist-top-tracks, artist-all-tracks), pairing it
/// with the track's own URI (not a numeric position) via the
/// PlayContextTrack D-Bus method tools/spotify/ adds to spotifyd (see its
/// README.md for why a numeric position made picking a track unreliable).
/// Unused for playlist-tracks/album-tracks rows, which already have their
/// own real container id (the caller's `arg`).
fn track_row(t: Track) -> Row {
    let artist = t.artists.first().map(|a| a.name.as_str()).unwrap_or("");
    let label = if artist.is_empty() { t.name } else { format!("{} - {}", t.name, artist) };
    Row { label, uri: Some(t.id.to_uri()), id: Some(t.album.id.to_base62()) }
}

fn album_row(a: Album) -> Row {
    let artist = a.artists.first().map(|x| x.name.as_str()).unwrap_or("");
    let label = if artist.is_empty() { a.name.clone() } else { format!("{} - {}", a.name, artist) };
    Row { label, uri: Some(a.id.to_uri()), id: Some(a.id.to_base62()) }
}

fn artist_row(a: Artist) -> Row {
    Row { label: a.name, uri: Some(a.id.to_uri()), id: Some(a.id.to_base62()) }
}

fn saved_page<T>(page: Page<Saved<T>>, row: impl Fn(T) -> Row) -> Output {
    let items = page.items.into_iter().map(|s| row(s.item)).collect();
    Output { items, next: page.next.map(|p| p.offset.to_string()), context: None }
}

async fn run(metadata: &Metadata, catalog: &Catalog, me: &str, command: &str, arg: &str, cursor: &str) -> Result<Output, Error> {
    match command {
        // The user's playlists and folders, flattened (folder structure itself
        // isn't surfaced yet -- Reborn's Playlists list is flat).
        // Unlike the public Web API, this covers playlists the account
        // *follows* but didn't create just as fully as its own -- there is
        // no ownership restriction on this internal channel.
        "playlists" => {
            let entries = metadata.rootlist().await?;
            let items = entries
                .into_iter()
                .take(MAX_ITEMS)
                .map(|e| {
                    let label = match e.owner.filter(|o| !o.is_empty() && o != me) {
                        Some(owner) => format!("{} ({})", e.playlist.name, owner),
                        None => e.playlist.name,
                    };
                    Row { label, uri: Some(e.playlist.id.to_uri()), id: Some(e.playlist.id.to_base62()) }
                })
                .collect();
            Ok(Output { items, next: None, context: None })
        }

        "playlist-tracks" => {
            let id = parse_id(IdKind::Playlist, arg)?;
            let playlist = metadata.playlist(id).await?;
            let ids: Vec<SpotifyId> =
                playlist.items.iter().map(|i| i.id).filter(|id| id.kind == IdKind::Track).take(MAX_ITEMS).collect();
            let tracks = metadata.tracks(&ids).await?;
            let items = tracks.into_iter().flatten().map(track_row).collect();
            Ok(Output { items, next: None, context: None })
        }

        "liked-songs" => {
            let mut output = saved_page(catalog.saved_tracks(parse_paging(cursor, 50)).await?, track_row);
            output.context = Some(format!("spotify:user:{me}:collection"));
            Ok(output)
        }
        "saved-albums" => Ok(saved_page(catalog.saved_albums(parse_paging(cursor, 50)).await?, album_row)),
        "followed-artists" => Ok(saved_page(catalog.followed_artists(parse_paging(cursor, 50)).await?, artist_row)),

        // Same thin-reference issue as artist-albums/artist-top-tracks:
        // Album.discs' own TrackRefs turned out to carry ids without
        // populated names too (confirmed: artist-all-tracks re-fetches
        // full tracks from this same per-album discs list and works;
        // this command trusted the TrackRef directly and didn't). Batch-
        // hydrate the real Track entities instead.
        "album-tracks" => {
            let id = parse_id(IdKind::Album, arg)?;
            let album = metadata.album(id).await?;
            let ids: Vec<SpotifyId> = album.discs.into_iter().flatten().map(|t| t.id).take(MAX_ITEMS).collect();
            let tracks = metadata.tracks(&ids).await?;
            let items = tracks.into_iter().flatten().map(track_row).collect();
            Ok(Output { items, next: None, context: None })
        }

        // artist.albums/singles are thin references embedded in the
        // artist's own extended-metadata response -- just enough to carry
        // an id, not a populated name (confirmed empirically: real
        // responses came back with the right item count but an empty
        // `label` on every row). Re-fetch the full, definitive Album
        // entities by id instead of trusting the embedded name, the same
        // "cheap id list, then batch-hydrate" shape catalog.saved_albums()
        // and playlist-tracks already use.
        "artist-albums" => {
            let id = parse_id(IdKind::Artist, arg)?;
            let artist = metadata.artist(id).await?;
            let ids: Vec<SpotifyId> = artist
                .albums
                .into_iter()
                .flatten()
                .chain(artist.singles.into_iter().flatten())
                .map(|a| a.id)
                .take(MAX_ITEMS)
                .collect();
            let albums = metadata.albums(&ids).await?;
            let items = albums.into_iter().flatten().map(album_row).collect();
            Ok(Output { items, next: None, context: None })
        }

        // Same thin-reference issue as artist-albums above: artist.top_tracks
        // carries ids without populated names. Re-fetch the full Track
        // entities instead.
        "artist-top-tracks" => {
            let id = parse_id(IdKind::Artist, arg)?;
            let artist = metadata.artist(id).await?;
            let ids: Vec<SpotifyId> = artist.top_tracks.into_iter().flatten().map(|t| t.id).take(MAX_ITEMS).collect();
            let tracks = metadata.tracks(&ids).await?;
            let items = tracks.into_iter().flatten().map(track_row).collect();
            Ok(Output { items, next: None, context: None })
        }

        // Spotify has no "every track by this artist" endpoint on this
        // channel either (matching the old Web API implementation's own
        // comment) -- aggregate by pulling the artist's albums+singles,
        // then that album's tracks, one call each, entirely inside this one
        // process, and return everything in a single response.
        "artist-all-tracks" => {
            let id = parse_id(IdKind::Artist, arg)?;
            let artist = metadata.artist(id).await?;
            let album_ids: Vec<SpotifyId> = artist
                .albums
                .into_iter()
                .flatten()
                .chain(artist.singles.into_iter().flatten())
                .map(|a| a.id)
                .take(MAX_ALBUMS_FOR_ALL_TRACKS)
                .collect();

            // Collect every track id across all albums first, then hydrate
            // them all in one batch at the end -- same "ids first, then
            // metadata.tracks()" shape as artist-top-tracks/artist-albums
            // above, rather than trusting each album fetch's own TrackRefs
            // individually. Also gives "Title - Artist" labels (track_row())
            // consistent with every other track list, instead of title-only.
            let mut track_ids = Vec::new();
            for album_id in album_ids {
                if track_ids.len() >= MAX_ITEMS {
                    break;
                }
                let album = match metadata.album(album_id).await {
                    Ok(album) => album,
                    Err(error) => {
                        tracing_light_skip(&error, album_id);
                        continue;
                    }
                };
                for t in album.discs.into_iter().flatten() {
                    if track_ids.len() >= MAX_ITEMS {
                        break;
                    }
                    track_ids.push(t.id);
                }
            }
            let tracks = metadata.tracks(&track_ids).await?;
            let items = tracks.into_iter().flatten().map(track_row).collect();
            Ok(Output { items, next: None, context: None })
        }

        other => Err(Error::protocol("command", format!("unknown command '{other}'"))),
    }
}

/// One album in a large discography failing (removed, region-locked) isn't
/// worth aborting the whole artist-all-tracks aggregation over -- log to
/// stderr (visible for debugging, only its last line is read) and move on.
fn tracing_light_skip(error: &Error, album_id: SpotifyId) {
    eprintln!("[reborn-spotify-metadata] skipping {album_id}: {error}");
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let Some(command) = args.get(1) else {
        return fail("usage: reborn-spotify-metadata <command> [arg] [cursor]");
    };
    let arg = args.get(2).map(String::as_str).unwrap_or("");
    let cursor = args.get(3).map(String::as_str).unwrap_or("");
    eprintln!("[reborn-spotify-metadata] {command} arg={arg:?} cursor={cursor:?}");

    let cache_dir = match env::var("REBORN_SPOTIFY_CACHE_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => return fail("REBORN_SPOTIFY_CACHE_DIR is not set"),
    };

    let credentials = match CacheDir::new(&cache_dir).load_credentials() {
        Ok(credentials) => credentials,
        Err(error) => return fail(format!("no stored session in {}: {error}", cache_dir.display())),
    };

    let session: Session = match Session::builder().cache_dir(cache_dir).device_name("Y2Reborn").connect(credentials).await
    {
        Ok(session) => session,
        Err(error) => return fail(format!("connect failed: {error}")),
    };

    let metadata = Metadata::new(&session);
    let catalog = Catalog::new(&session);
    let result = run(&metadata, &catalog, session.username(), command, arg, cursor).await;
    session.close();

    match result {
        Ok(output) => match serde_json::to_string(&output) {
            Ok(json) => {
                eprintln!(
                    "[reborn-spotify-metadata] {command} -> {} item(s), next={:?}",
                    output.items.len(),
                    output.next
                );
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => fail(format!("could not serialize the result: {error}")),
        },
        Err(error) => fail(error),
    }
}
