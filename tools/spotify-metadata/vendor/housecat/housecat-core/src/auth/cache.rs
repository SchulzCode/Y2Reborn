//! The on-disk cache: device id, credentials, client token (spec sections 5.1, 5.4, 5.5).

use std::path::{Path, PathBuf};

use crate::auth::credentials::Credentials;
use crate::random;
use crate::Result;

pub const CREDENTIALS_FILE: &str = "credentials.json";
pub const DEVICE_ID_FILE: &str = "device_id";
pub const CLIENT_TOKEN_FILE: &str = "client_token.json";

#[derive(Debug, Clone)]
pub struct CacheDir {
    root: PathBuf,
}

impl CacheDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        CacheDir { root: root.into() }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Forty lowercase hex characters, generated once per cache directory.
    pub fn device_id(&self) -> Result<String> {
        if let Some(bytes) = self.read(DEVICE_ID_FILE)? {
            let existing = String::from_utf8_lossy(&bytes).trim().to_string();
            if existing.len() == 40 && existing.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(existing.to_ascii_lowercase());
            }
        }
        let fresh = hex::encode(random::random_bytes::<20>());
        self.write(DEVICE_ID_FILE, fresh.as_bytes())?;
        Ok(fresh)
    }

    pub fn load_credentials(&self) -> Result<Credentials> {
        Credentials::load(&self.root.join(CREDENTIALS_FILE))
    }

    pub fn save_credentials(&self, credentials: &Credentials) -> Result<()> {
        credentials.save(&self.root.join(CREDENTIALS_FILE))
    }

    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.root.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.root.join(name), bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_created_once_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(dir.path().join("nested").join("cache"));
        let first = cache.device_id().unwrap();
        assert_eq!(first.len(), 40);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let second = cache.device_id().unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(cache.path().join("device_id")).unwrap(), first);
    }

    #[test]
    fn different_caches_get_different_device_ids() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(CacheDir::new(a.path()).device_id().unwrap(), CacheDir::new(b.path()).device_id().unwrap());
    }

    #[test]
    fn credentials_round_trip_through_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(dir.path());
        assert!(matches!(cache.load_credentials(), Err(crate::Error::AuthRequired(_))));
        let creds = Credentials::stored("alice", vec![1, 2]);
        cache.save_credentials(&creds).unwrap();
        assert_eq!(cache.load_credentials().unwrap(), creds);
        assert!(cache.path().join("credentials.json").exists());
    }

    #[test]
    fn raw_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(dir.path());
        assert_eq!(cache.read("client_token.json").unwrap(), None);
        cache.write("client_token.json", b"{}").unwrap();
        assert_eq!(cache.read("client_token.json").unwrap(), Some(b"{}".to_vec()));
    }
}
