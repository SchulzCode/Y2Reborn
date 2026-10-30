//! Login credentials for the access point (spec section 5.4).
//!
//! The JSON shape (`username`, integer `auth_type`, base64 `auth_data`) is the one
//! librespot writes to `credentials.json`, so an existing cache can be reused.

use std::fmt;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    /// An OAuth access token; only valid for a first login. Wire value 3.
    AccessToken,
    /// Reusable stored credentials returned by the access point. Wire value 1.
    Stored,
}

impl CredentialKind {
    /// The `AuthenticationType` value from `authentication.proto`.
    pub(crate) fn auth_type(self) -> i32 {
        match self {
            CredentialKind::Stored => 1,
            CredentialKind::AccessToken => 3,
        }
    }

    pub(crate) fn from_auth_type(value: i32) -> Option<Self> {
        match value {
            1 => Some(CredentialKind::Stored),
            3 => Some(CredentialKind::AccessToken),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: Option<String>,
    pub kind: CredentialKind,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct CredentialsFile {
    username: Option<String>,
    auth_type: i32,
    auth_data: String,
}

impl Credentials {
    pub fn access_token(token: impl Into<String>) -> Self {
        Credentials { username: None, kind: CredentialKind::AccessToken, data: token.into().into_bytes() }
    }

    pub fn stored(username: impl Into<String>, data: Vec<u8>) -> Self {
        Credentials { username: Some(username.into()), kind: CredentialKind::Stored, data }
    }

    pub fn to_json(&self) -> String {
        let file = CredentialsFile {
            username: self.username.clone(),
            auth_type: self.kind.auth_type(),
            auth_data: BASE64.encode(&self.data),
        };
        serde_json::to_string(&file).expect("credentials serialize")
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let file: CredentialsFile =
            serde_json::from_str(json).map_err(|e| Error::protocol("credentials file", e.to_string()))?;
        let kind = CredentialKind::from_auth_type(file.auth_type)
            .ok_or_else(|| Error::protocol("credentials file", format!("unknown auth_type {}", file.auth_type)))?;
        let data = BASE64
            .decode(&file.auth_data)
            .map_err(|e| Error::protocol("credentials file", format!("auth_data is not base64: {e}")))?;
        Ok(Credentials { username: file.username, kind, data })
    }

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(json) => Self::from_json(&json),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::AuthRequired(format!("no cached credentials at {}", path.display())))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_json())?;
        Ok(())
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("kind", &self.kind)
            .field("data", &format_args!("<redacted {} bytes>", self.data.len()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_uses_librespot_field_names_and_encodings() {
        let creds = Credentials::stored("alice", vec![1, 2, 3]);
        assert_eq!(creds.to_json(), r#"{"username":"alice","auth_type":1,"auth_data":"AQID"}"#);

        let token = Credentials::access_token("tok");
        assert_eq!(token.to_json(), r#"{"username":null,"auth_type":3,"auth_data":"dG9r"}"#);
    }

    #[test]
    fn json_round_trips() {
        let creds = Credentials::stored("alice", vec![0, 255, 7]);
        assert_eq!(Credentials::from_json(&creds.to_json()).unwrap(), creds);
    }

    #[test]
    fn parses_a_librespot_cache_file() {
        // Shape written by librespot's `Cache::save_credentials`.
        let json = r#"{"username":"bob","auth_type":1,"auth_data":"c2VjcmV0"}"#;
        let creds = Credentials::from_json(json).unwrap();
        assert_eq!(creds.username.as_deref(), Some("bob"));
        assert_eq!(creds.kind, CredentialKind::Stored);
        assert_eq!(creds.data, b"secret");
    }

    #[test]
    fn rejects_unknown_auth_type_and_bad_base64() {
        assert!(matches!(
            Credentials::from_json(r#"{"username":"x","auth_type":9,"auth_data":"AQID"}"#),
            Err(crate::Error::Protocol { .. })
        ));
        assert!(matches!(
            Credentials::from_json(r#"{"username":"x","auth_type":1,"auth_data":"!!"}"#),
            Err(crate::Error::Protocol { .. })
        ));
    }

    #[test]
    fn save_and_load_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let creds = Credentials::stored("alice", vec![9, 8, 7]);
        creds.save(&path).unwrap();
        assert_eq!(Credentials::load(&path).unwrap(), creds);
    }

    #[test]
    fn missing_file_means_auth_required() {
        let dir = tempfile::tempdir().unwrap();
        let err = Credentials::load(&dir.path().join("nope.json")).unwrap_err();
        assert!(matches!(err, crate::Error::AuthRequired(_)), "got {err:?}");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let creds = Credentials::stored("alice", b"supersecret".to_vec());
        let debug = format!("{creds:?}");
        assert!(!debug.contains("supersecret"));
        assert!(!debug.contains("c3VwZXJzZWNyZXQ"));
        assert!(debug.contains("alice"));
        assert!(debug.contains("11 bytes"));
    }
}
