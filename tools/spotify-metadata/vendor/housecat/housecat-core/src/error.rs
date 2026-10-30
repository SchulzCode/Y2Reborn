use std::time::Duration;

use crate::id::IdError;

pub type Result<T> = std::result::Result<T, Error>;

/// The one error type of every housecat crate (spec section 8). The variants an app acts on
/// come first; the rest carry context for logs and bug reports.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Credentials were rejected or are missing. Run an OAuth flow again
    /// (`housecat::auth::cached_or_device`) and reconnect.
    #[error("authentication required: {0}")]
    AuthRequired(String),

    /// The account is not Premium; nothing else will work.
    #[error("a Spotify Premium account is required")]
    PremiumRequired,

    /// The requested entity does not exist (or is not visible to this account).
    #[error("not found: {0}")]
    NotFound(String),

    /// The content cannot be played from this account or country.
    #[error("not available: {reason}")]
    NotAvailable { reason: String },

    /// The track has files, but none in a format housecat decodes (Vorbis or MP3).
    #[error("no supported audio format is available; only Ogg Vorbis and MP3 are decoded")]
    UnsupportedFormat,

    /// The access point refused to hand out the audio key. Code 1 is what accounts
    /// created after roughly late 2024 receive (spec section 3); metadata still works.
    #[error(
        "Spotify refused the audio key (code {code}). Accounts created after late 2024 are \
         refused on this legacy path and there is no known workaround; older accounts work."
    )]
    AudioKeyRefused { code: u16 },

    /// Spotify asked us to slow down for longer than we are willing to wait inline.
    /// Retry after `retry_after`.
    #[error("rate limited by Spotify; retry after {retry_after:?}")]
    RateLimited { retry_after: Duration },

    /// The access point connection was lost. The session reconnects on its own; socket
    /// requests (audio keys) fail until `SessionState::Connected` comes back.
    #[error("the access point connection was lost")]
    Disconnected,

    /// Something did not arrive in time: an access-point reply, or audio data during
    /// `AudioTrack::read`. Usually transient.
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),

    /// An HTTP request could not be made or completed; usually transient.
    #[error("transport error at {endpoint}: {source}")]
    Transport {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },

    /// A response was malformed, unexpected, or a known endpoint answered with an
    /// error that looks like a yank. `detail` carries what the server said. Not transient:
    /// if it repeats, Spotify changed something and `housecat_core::consts` needs a look.
    #[error("protocol error in {what}: {detail}")]
    Protocol { what: &'static str, detail: String },

    /// The container or codec data could not be decoded.
    #[error("decode error: {0}")]
    Decode(String),

    /// The access point told us to use a different one. `Session::connect` handles it;
    /// apps never see it.
    #[error("the access point asked us to try another access point")]
    TryAnotherAccessPoint,

    /// A file or socket operation failed (the credential cache, the audio buffer).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A Spotify id or URI could not be parsed.
    #[error(transparent)]
    Id(#[from] IdError),
}

impl Error {
    pub fn protocol(what: &'static str, detail: impl Into<String>) -> Self {
        Error::Protocol { what, detail: detail.into() }
    }

    pub fn transport(endpoint: impl Into<String>, source: reqwest::Error) -> Self {
        Error::Transport { endpoint: endpoint.into(), source }
    }

    /// True for failures that may pass on retry: transport errors, timeouts, rate limits,
    /// and a lost connection.
    pub fn is_transient(&self) -> bool {
        matches!(self, Error::Transport { .. } | Error::Timeout(_) | Error::RateLimited { .. } | Error::Disconnected)
    }

    /// True when the app has to run the OAuth flow again.
    pub fn needs_login(&self) -> bool {
        matches!(self, Error::AuthRequired(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_helpers() {
        assert!(Error::Timeout("x").is_transient());
        assert!(Error::Disconnected.is_transient());
        assert!(Error::RateLimited { retry_after: Duration::from_secs(1) }.is_transient());
        assert!(!Error::AuthRequired("no".into()).is_transient());
        assert!(!Error::NotFound("spotify:track:x".into()).is_transient());
        assert!(Error::AuthRequired("no".into()).needs_login());
        assert!(!Error::PremiumRequired.needs_login());
    }
}
