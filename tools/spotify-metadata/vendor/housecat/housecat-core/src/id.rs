//! Spotify identifiers: 128-bit entity IDs (base62 / hex / URI) and 160-bit file IDs.

use std::fmt;

const BASE62: &[u8; 62] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdKind {
    Track,
    Album,
    Artist,
    Playlist,
    Show,
    Episode,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpotifyId {
    pub kind: IdKind,
    pub id: u128,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub [u8; 20]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("invalid base62 id: {0}")]
    Base62(String),
    #[error("invalid hex id: {0}")]
    Hex(String),
    #[error("invalid spotify uri: {0}")]
    Uri(String),
    #[error("unsupported id kind: {0}")]
    Kind(String),
    #[error("wrong length: expected {expected}, got {got}")]
    Length { expected: usize, got: usize },
}

impl IdKind {
    pub fn as_str(self) -> &'static str {
        match self {
            IdKind::Track => "track",
            IdKind::Album => "album",
            IdKind::Artist => "artist",
            IdKind::Playlist => "playlist",
            IdKind::Show => "show",
            IdKind::Episode => "episode",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "track" => IdKind::Track,
            "album" => IdKind::Album,
            "artist" => IdKind::Artist,
            "playlist" => IdKind::Playlist,
            "show" => IdKind::Show,
            "episode" => IdKind::Episode,
            _ => return None,
        })
    }
}

impl SpotifyId {
    pub fn from_raw(kind: IdKind, bytes: &[u8]) -> Result<Self, IdError> {
        let raw: [u8; 16] = bytes
            .try_into()
            .map_err(|_| IdError::Length { expected: 16, got: bytes.len() })?;
        Ok(SpotifyId { kind, id: u128::from_be_bytes(raw) })
    }

    pub fn from_base62(kind: IdKind, s: &str) -> Result<Self, IdError> {
        if s.len() != 22 {
            return Err(IdError::Length { expected: 22, got: s.len() });
        }
        let mut id: u128 = 0;
        for c in s.bytes() {
            let digit = BASE62
                .iter()
                .position(|&d| d == c)
                .ok_or_else(|| IdError::Base62(s.to_string()))? as u128;
            id = id
                .checked_mul(62)
                .and_then(|v| v.checked_add(digit))
                .ok_or_else(|| IdError::Base62(s.to_string()))?;
        }
        Ok(SpotifyId { kind, id })
    }

    pub fn from_hex(kind: IdKind, s: &str) -> Result<Self, IdError> {
        let bytes = hex::decode(s).map_err(|_| IdError::Hex(s.to_string()))?;
        Self::from_raw(kind, &bytes)
    }

    pub fn from_uri(uri: &str) -> Result<Self, IdError> {
        let mut parts = uri.split(':');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("spotify"), Some(kind), Some(id), None) => {
                let kind = IdKind::parse(kind).ok_or_else(|| IdError::Kind(kind.to_string()))?;
                Self::from_base62(kind, id)
            }
            _ => Err(IdError::Uri(uri.to_string())),
        }
    }

    pub fn to_raw(&self) -> [u8; 16] {
        self.id.to_be_bytes()
    }

    pub fn to_base62(&self) -> String {
        let mut out = [b'0'; 22];
        let mut n = self.id;
        for slot in out.iter_mut().rev() {
            *slot = BASE62[(n % 62) as usize];
            n /= 62;
        }
        String::from_utf8(out.to_vec()).expect("base62 digits are ascii")
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.to_raw())
    }

    pub fn to_uri(&self) -> String {
        format!("spotify:{}:{}", self.kind.as_str(), self.to_base62())
    }
}

impl fmt::Display for SpotifyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_uri())
    }
}

impl fmt::Debug for SpotifyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_uri())
    }
}

impl FileId {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, IdError> {
        let raw: [u8; 20] = bytes
            .try_into()
            .map_err(|_| IdError::Length { expected: 20, got: bytes.len() })?;
        Ok(FileId(raw))
    }

    pub fn from_hex(s: &str) -> Result<Self, IdError> {
        let bytes = hex::decode(s).map_err(|_| IdError::Hex(s.to_string()))?;
        Self::from_slice(&bytes)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileId({})", self.to_hex())
    }
}

/// Serializes as the `spotify:<kind>:<id>` URI.
impl serde::Serialize for SpotifyId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_uri())
    }
}

/// Serializes as the 40-character hex string.
impl serde::Serialize for FileId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base62_decodes_to_known_hex() {
        let id = SpotifyId::from_base62(IdKind::Track, "4uLU6hMCjMI75M1A2tKUQC").unwrap();
        assert_eq!(id.to_hex(), "93bc414a606747b2b612491ef83d5a3e");
        let id = SpotifyId::from_base62(IdKind::Album, "7GhIk7Il098yCjg4BQjzvb").unwrap();
        assert_eq!(id.to_hex(), "fc68436ee98747279e840390baaf0821");
        let zero = SpotifyId::from_base62(IdKind::Track, "0000000000000000000000").unwrap();
        assert_eq!(zero.id, 0);
    }

    #[test]
    fn matches_librespot_test_vectors() {
        // From librespot's core/src/spotify_id.rs tests at commit 939dc5ee.
        let a = SpotifyId::from_base62(IdKind::Track, "5sWHDYs0csV6RS48xBl0tH").unwrap();
        assert_eq!(a.to_hex(), "b39fe8081e1f4c54be38e8d6f9f12bb9");
        assert_eq!(a.id, 238762092608182713602505436543891614649);
        let b = SpotifyId::from_hex(IdKind::Track, "9a1b1cfbc6f244569ae0356c77bbe9d8").unwrap();
        assert_eq!(b.to_base62(), "4GNcXTGWmnZ3ySrqvol3o4");
        assert_eq!(b.id, 204841891221366092811751085145916697048);
    }

    #[test]
    fn base62_round_trips() {
        for s in ["4uLU6hMCjMI75M1A2tKUQC", "7GhIk7Il098yCjg4BQjzvb", "0000000000000000000000"] {
            let id = SpotifyId::from_base62(IdKind::Track, s).unwrap();
            assert_eq!(id.to_base62(), s);
        }
    }

    #[test]
    fn hex_and_raw_round_trip() {
        let id = SpotifyId::from_hex(IdKind::Track, "93bc414a606747b2b612491ef83d5a3e").unwrap();
        let raw = id.to_raw();
        assert_eq!(raw.len(), 16);
        assert_eq!(SpotifyId::from_raw(IdKind::Track, &raw).unwrap(), id);
        assert_eq!(id.to_base62(), "4uLU6hMCjMI75M1A2tKUQC");
    }

    #[test]
    fn uri_parse_and_format() {
        let id = SpotifyId::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        assert_eq!(id.kind, IdKind::Track);
        assert_eq!(id.to_uri(), "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
        let ep = SpotifyId::from_uri("spotify:episode:7GhIk7Il098yCjg4BQjzvb").unwrap();
        assert_eq!(ep.kind, IdKind::Episode);
        assert!(matches!(SpotifyId::from_uri("spotify:user:alice"), Err(IdError::Kind(_))));
        assert!(matches!(SpotifyId::from_uri("http://x"), Err(IdError::Uri(_))));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(SpotifyId::from_base62(IdKind::Track, "short"), Err(IdError::Length { .. })));
        assert!(matches!(SpotifyId::from_base62(IdKind::Track, "4uLU6hMCjMI75M1A2tKUQ!"), Err(IdError::Base62(_))));
        assert!(matches!(SpotifyId::from_hex(IdKind::Track, "zz"), Err(IdError::Hex(_))));
        assert!(matches!(SpotifyId::from_raw(IdKind::Track, &[0u8; 15]), Err(IdError::Length { .. })));
    }

    #[test]
    fn file_id_hex_round_trip() {
        let hex_str = "0102030405060708090a0b0c0d0e0f1011121314";
        let f = FileId::from_hex(hex_str).unwrap();
        assert_eq!(f.0[0], 1);
        assert_eq!(f.0[19], 0x14);
        assert_eq!(f.to_hex(), hex_str);
        assert!(matches!(FileId::from_slice(&[0u8; 19]), Err(IdError::Length { .. })));
    }

    #[test]
    fn display_is_the_uri_and_debug_matches() {
        let id = SpotifyId::from_uri("spotify:artist:4uLU6hMCjMI75M1A2tKUQC").unwrap();
        assert_eq!(id.to_string(), "spotify:artist:4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(format!("{id:?}"), "spotify:artist:4uLU6hMCjMI75M1A2tKUQC");
    }
}
