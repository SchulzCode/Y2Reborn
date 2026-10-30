//! Resolve a track or episode to its audio files (spec section 6.2, `track_audio`).

use bytes::Bytes;
use prost::Message;

use housecat_core::spclient::SpClient;
use housecat_core::{Error, FileId, IdKind, Result, Session, SpotifyId};
use housecat_protocol::spotify::extendedmetadata as ext;
use housecat_protocol::spotify::metadata as md;

/// Spotify's audio file formats. `Other` keeps values the schema does not name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioFormat {
    OggVorbis96,
    OggVorbis160,
    OggVorbis320,
    Mp3256,
    Mp3320,
    Mp3160,
    Mp396,
    Mp3160Enc,
    Aac24,
    Aac48,
    Flac,
    XheAac24,
    XheAac16,
    XheAac12,
    Flac24Bit,
    Other(i32),
}

impl AudioFormat {
    pub(crate) fn from_wire(value: i32) -> Self {
        use md::audio_file::Format;
        match Format::try_from(value) {
            Ok(Format::OggVorbis96) => AudioFormat::OggVorbis96,
            Ok(Format::OggVorbis160) => AudioFormat::OggVorbis160,
            Ok(Format::OggVorbis320) => AudioFormat::OggVorbis320,
            Ok(Format::Mp3256) => AudioFormat::Mp3256,
            Ok(Format::Mp3320) => AudioFormat::Mp3320,
            Ok(Format::Mp3160) => AudioFormat::Mp3160,
            Ok(Format::Mp396) => AudioFormat::Mp396,
            Ok(Format::Mp3160Enc) => AudioFormat::Mp3160Enc,
            Ok(Format::Aac24) => AudioFormat::Aac24,
            Ok(Format::Aac48) => AudioFormat::Aac48,
            Ok(Format::FlacFlac) => AudioFormat::Flac,
            Ok(Format::XheAac24) => AudioFormat::XheAac24,
            Ok(Format::XheAac16) => AudioFormat::XheAac16,
            Ok(Format::XheAac12) => AudioFormat::XheAac12,
            Ok(Format::FlacFlac24bit) => AudioFormat::Flac24Bit,
            Err(_) => AudioFormat::Other(value),
        }
    }

    pub fn is_ogg_vorbis(self) -> bool {
        matches!(self, AudioFormat::OggVorbis96 | AudioFormat::OggVorbis160 | AudioFormat::OggVorbis320)
    }

    pub fn is_mp3(self) -> bool {
        matches!(
            self,
            AudioFormat::Mp3256 | AudioFormat::Mp3320 | AudioFormat::Mp3160 | AudioFormat::Mp396 | AudioFormat::Mp3160Enc
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFile {
    pub id: FileId,
    pub format: AudioFormat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackAudio {
    pub id: SpotifyId,
    pub name: String,
    pub files: Vec<AudioFile>,
    /// Other track IDs Spotify offers when this one is unavailable in the account's country.
    pub alternatives: Vec<SpotifyId>,
}

pub async fn track_audio(session: &Session, id: SpotifyId) -> Result<TrackAudio> {
    track_audio_with(session.spclient(), id).await
}

pub async fn track_audio_with(spclient: &SpClient, id: SpotifyId) -> Result<TrackAudio> {
    if id.kind != IdKind::Track {
        return Err(Error::protocol("track_audio", format!("{id} is not a track")));
    }
    let bytes = fetch_entity(spclient, id, ext::ExtensionKind::TrackV4).await?;
    let track = md::Track::decode(bytes).map_err(|e| Error::protocol("track metadata", format!("bad protobuf: {e}")))?;
    Ok(track_audio_from_proto(id, &track))
}

pub async fn episode_audio(session: &Session, id: SpotifyId) -> Result<TrackAudio> {
    episode_audio_with(session.spclient(), id).await
}

pub async fn episode_audio_with(spclient: &SpClient, id: SpotifyId) -> Result<TrackAudio> {
    if id.kind != IdKind::Episode {
        return Err(Error::protocol("episode_audio", format!("{id} is not an episode")));
    }
    let bytes = fetch_entity(spclient, id, ext::ExtensionKind::EpisodeV4).await?;
    let episode =
        md::Episode::decode(bytes).map_err(|e| Error::protocol("episode metadata", format!("bad protobuf: {e}")))?;
    Ok(TrackAudio {
        id,
        name: episode.name.clone().unwrap_or_default(),
        files: files_from_proto(&episode.audio),
        alternatives: Vec::new(),
    })
}

async fn fetch_entity(spclient: &SpClient, id: SpotifyId, kind: ext::ExtensionKind) -> Result<Bytes> {
    let mut payloads = crate::entities::fetch(spclient, &[id], kind).await?;
    payloads.pop().flatten().map(Bytes::from).ok_or_else(|| Error::NotFound(id.to_uri()))
}

pub(crate) fn track_audio_from_proto(id: SpotifyId, track: &md::Track) -> TrackAudio {
    tracing::debug!(
        %id,
        name = track.name.as_deref().unwrap_or(""),
        files = track.file.len(),
        alternatives = track.alternative.len(),
        restrictions = track.restriction.len(),
        "track metadata received"
    );
    let alternatives = track
        .alternative
        .iter()
        .filter_map(|alt| alt.gid.as_deref())
        .filter_map(|gid| SpotifyId::from_raw(IdKind::Track, gid).ok())
        .collect();
    TrackAudio {
        id,
        name: track.name.clone().unwrap_or_default(),
        files: files_from_proto(&track.file),
        alternatives,
    }
}

fn files_from_proto(files: &[md::AudioFile]) -> Vec<AudioFile> {
    files
        .iter()
        .filter_map(|file| {
            let raw = file.file_id.as_deref()?;
            let id = match FileId::from_slice(raw) {
                Ok(id) => id,
                Err(_) if !raw.is_empty() && raw.len() < 20 => {
                    // Some files arrive with 16-byte ids; librespot zero-pads them (its issue #1188).
                    let mut padded = [0u8; 20];
                    padded[..raw.len()].copy_from_slice(raw);
                    FileId(padded)
                }
                Err(error) => {
                    tracing::debug!(%error, "dropping an audio file with an unusable id");
                    return None;
                }
            };
            let Some(format) = file.format else {
                tracing::debug!(id = %id.to_hex(), "dropping an audio file without a format");
                return None;
            };
            tracing::debug!(id = %id.to_hex(), format, "audio file");
            Some(AudioFile { id, format: AudioFormat::from_wire(format) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use housecat_core::auth::credentials::Credentials;
    use housecat_core::client_token::ClientTokenProvider;
    use housecat_core::login5::Login5;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn track_id() -> SpotifyId {
        SpotifyId::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap()
    }

    fn sample_track() -> md::Track {
        md::Track {
            gid: Some(track_id().to_raw().to_vec()),
            name: Some("Never Gonna Give You Up".to_string()),
            file: vec![
                md::AudioFile { file_id: Some(vec![1; 20]), format: Some(md::audio_file::Format::OggVorbis320 as i32) },
                md::AudioFile { file_id: Some(vec![2; 20]), format: Some(md::audio_file::Format::Mp3160 as i32) },
                md::AudioFile { file_id: Some(vec![3; 20]), format: None },
                md::AudioFile { file_id: Some(vec![4; 20]), format: Some(99) },
            ],
            alternative: vec![md::Track { gid: Some(vec![9; 16]), ..Default::default() }],
            ..Default::default()
        }
    }

    fn response_for(uri: &str, status: i32, payload: Option<Vec<u8>>) -> ext::BatchedExtensionResponse {
        ext::BatchedExtensionResponse {
            header: None,
            extended_metadata: vec![ext::EntityExtensionDataArray {
                header: None,
                extension_kind: ext::ExtensionKind::TrackV4 as i32,
                extension_data: vec![ext::EntityExtensionData {
                    header: Some(ext::EntityExtensionDataHeader {
                        status_code: status,
                        etag: String::new(),
                        locale: String::new(),
                        cache_ttl_in_seconds: 0,
                        offline_ttl_in_seconds: 0,
                    }),
                    entity_uri: uri.to_string(),
                    extension_data: payload.map(|value| prost_types::Any {
                        type_url: "type.googleapis.com/spotify.metadata.Track".to_string(),
                        value,
                    }),
                }],
            }],
        }
    }

    #[test]
    fn proto_track_maps_to_track_audio() {
        let audio = track_audio_from_proto(track_id(), &sample_track());
        assert_eq!(audio.name, "Never Gonna Give You Up");
        assert_eq!(audio.files.len(), 3, "the file without a format is dropped, the unknown format is kept");
        assert_eq!(audio.files[0], AudioFile { id: FileId([1; 20]), format: AudioFormat::OggVorbis320 });
        assert_eq!(audio.files[1].format, AudioFormat::Mp3160);
        assert_eq!(audio.files[2].format, AudioFormat::Other(99));
        assert!(audio.files[0].format.is_ogg_vorbis());
        assert!(audio.files[1].format.is_mp3());
        assert_eq!(audio.alternatives, vec![SpotifyId::from_raw(IdKind::Track, &[9; 16]).unwrap()]);
    }

    #[test]
    fn short_file_ids_are_zero_padded_like_librespot() {
        let track = md::Track {
            file: vec![md::AudioFile { file_id: Some(vec![5; 16]), format: Some(md::audio_file::Format::OggVorbis160 as i32) }],
            ..Default::default()
        };
        let audio = track_audio_from_proto(track_id(), &track);
        assert_eq!(audio.files.len(), 1);
        let mut expected = [0u8; 20];
        expected[..16].copy_from_slice(&[5; 16]);
        assert_eq!(audio.files[0].id, FileId(expected));
    }

    #[tokio::test]
    async fn rejects_non_track_ids_before_any_request() {
        let album = SpotifyId::from_uri("spotify:album:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let server = MockServer::start().await;
        let err = track_audio_with(&spclient(&server), album).await.unwrap_err();
        assert!(matches!(err, Error::Protocol { what: "track_audio", .. }), "got {err:?}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn track_audio_with_fetches_and_decodes_through_spclient() {
        let server = server_with_auth().await;
        let uri = track_id().to_uri();
        let body = response_for(&uri, 200, Some(sample_track().encode_to_vec())).encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/extended-metadata/v0/extended-metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(&server)
            .await;

        let audio = track_audio_with(&spclient(&server), track_id()).await.unwrap();
        assert_eq!(audio.id, track_id());
        assert_eq!(audio.files.len(), 3);

        let requests = server.received_requests().await.unwrap();
        let post = requests.iter().find(|r| r.url.path().starts_with("/extended-metadata")).unwrap();
        let request = ext::BatchedEntityRequest::decode(&post.body[..]).unwrap();
        assert_eq!(request.entity_request[0].entity_uri, uri);
    }

    #[tokio::test]
    async fn a_missing_track_is_not_found() {
        let server = server_with_auth().await;
        let body = response_for(&track_id().to_uri(), 404, None).encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/extended-metadata/v0/extended-metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let err = track_audio_with(&spclient(&server), track_id()).await.unwrap_err();
        assert!(matches!(&err, Error::NotFound(uri) if *uri == track_id().to_uri()), "got {err:?}");
    }

    // --- test doubles for the HTTP side; the same shape core uses in its own tests ---

    async fn server_with_auth() -> MockServer {
        use housecat_protocol::spotify::clienttoken::http::v0 as ct;
        use housecat_protocol::spotify::login5::v3 as l5;

        let server = MockServer::start().await;
        let ct_body = ct::ClientTokenResponse {
            response_type: ct::ClientTokenResponseType::ResponseGrantedTokenResponse as i32,
            response: Some(ct::client_token_response::Response::GrantedToken(ct::GrantedTokenResponse {
                token: "ct-1".to_string(),
                expires_after_seconds: 1_209_600,
                refresh_after_seconds: 7200,
                domains: vec![],
            })),
        }
        .encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/v1/clienttoken"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ct_body))
            .mount(&server)
            .await;

        let l5_body = l5::LoginResponse {
            warnings: vec![],
            login_context: vec![],
            identifier_token: String::new(),
            user_info: None,
            response: Some(l5::login_response::Response::Ok(l5::LoginOk {
                username: "alice".to_string(),
                access_token: "bearer-1".to_string(),
                stored_credential: vec![],
                access_token_expires_in: 3600,
            })),
        }
        .encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(l5_body))
            .mount(&server)
            .await;
        server
    }

    fn spclient(server: &MockServer) -> SpClient {
        let http = reqwest::Client::new();
        let client_token = Arc::new(
            ClientTokenProvider::new(http.clone(), "device-1", None)
                .with_url(format!("{}/v1/clienttoken", server.uri())),
        );
        let login5 = Arc::new(
            Login5::new(http.clone(), "device-1", client_token.clone(), Credentials::stored("alice", vec![1]))
                .unwrap()
                .with_url(format!("{}/v3/login", server.uri())),
        );
        SpClient::new(http, vec![server.uri()], "SE", login5, client_token)
    }
}
