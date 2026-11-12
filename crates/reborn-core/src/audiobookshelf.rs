//! Requests from the Audiobookshelf interface.

/// Requests from the Audiobookshelf screens. Credentials are never debugged.
#[derive(Clone, PartialEq, Eq)]
pub enum AudiobookshelfRequest {
    Status,
    SignIn {
        server: String,
        username: String,
        password: String,
    },
    Libraries,
    Books {
        library: String,
        page: u32,
    },
    Search {
        library: String,
        query: String,
    },
    OpenBook {
        item: String,
    },
    Download {
        item: String,
    },
    Forget,
}

impl std::fmt::Debug for AudiobookshelfRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudiobookshelfRequest { .. }")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_debug_does_not_expose_account_details() {
        let request = AudiobookshelfRequest::SignIn {
            server: "private-server".into(),
            username: "private-user".into(),
            password: "private-password".into(),
        };
        let output = format!("{request:?}");
        assert!(!output.contains("private-"));
    }
}
