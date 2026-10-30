//! The shared reqwest client and the user agent Spotify's services expect (spec section 5.6).

use crate::consts;

/// `Spotify/<version> <platform>/0 (<VERSION_STRING>)`, the shape librespot sends.
pub fn user_agent() -> String {
    format!(
        "Spotify/{} {}/0 ({})",
        consts::SPOTIFY_VERSION,
        consts::user_agent_platform(),
        consts::VERSION_STRING
    )
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .timeout(consts::HTTP_TIMEOUT)
        .build()
        .expect("reqwest client with static configuration")
}

#[cfg(test)]
mod tests {
    #[test]
    fn user_agent_has_the_expected_shape() {
        let ua = super::user_agent();
        assert!(ua.starts_with("Spotify/124200290 "), "{ua}");
        assert!(ua.ends_with(&format!("/0 ({})", crate::consts::VERSION_STRING)), "{ua}");
    }
}
