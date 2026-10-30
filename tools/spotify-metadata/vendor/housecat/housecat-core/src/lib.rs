//! housecat-core: an authenticated Spotify client session.
//!
//! See `docs/superpowers/specs/2026-09-22-housecat-design.md`, section 5.

pub mod ap;
pub mod apresolve;
pub mod auth;
pub mod cdn;
pub mod client_token;
pub mod consts;
pub mod error;
pub mod hashcash;
pub mod http;
pub mod id;
pub mod login5;
pub mod random;
pub mod ratelimit;
pub mod session;
pub mod spclient;

pub use auth::credentials::{CredentialKind, Credentials};
pub use error::{Error, Result};
pub use id::{FileId, IdKind, SpotifyId};
pub use session::{Session, SessionBuilder, SessionState};
