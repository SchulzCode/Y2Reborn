//! Lyrics over the color-lyrics JSON endpoint (spec section 6.2).

use std::time::Duration;

use housecat_core::{Error, Result};
use serde::Deserialize;

use crate::model::{LyricLine, Lyrics};

#[derive(Deserialize)]
struct Response {
    lyrics: Body,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body {
    #[serde(default)]
    sync_type: String,
    #[serde(default)]
    lines: Vec<Line>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(default)]
    start_time_ms: String,
    #[serde(default)]
    words: String,
}

pub(crate) fn parse(json: &[u8]) -> Result<Lyrics> {
    let response: Response =
        serde_json::from_slice(json).map_err(|e| Error::protocol("lyrics", format!("unexpected json: {e}")))?;
    let body = response.lyrics;
    Ok(Lyrics {
        synced: matches!(body.sync_type.as_str(), "LINE_SYNCED" | "SYLLABLE_SYNCED"),
        lines: body
            .lines
            .into_iter()
            .map(|line| LyricLine {
                start: Duration::from_millis(line.start_time_ms.trim().parse().unwrap_or(0)),
                text: line.words,
            })
            .collect(),
        language: body.language.filter(|s| !s.is_empty()),
        provider: body.provider.filter(|s| !s.is_empty()),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The real response's shape with invented lines; real lyric text is never committed.
    pub(crate) const SYNCED: &str = r#"{"lyrics":{"syncType":"LINE_SYNCED","lines":[{"startTimeMs":"1000","words":"first invented line","syllables":[],"endTimeMs":"0"},{"startTimeMs":"2500","words":"second invented line","syllables":[],"endTimeMs":"0"}],"provider":"MusixMatch","providerLyricsId":"1","language":"en","isRtlLanguage":false,"alternatives":[]},"colors":{"background":-1,"text":-1,"highlightText":-1},"hasVocalRemoval":false}"#;

    #[test]
    fn synced_lyrics_parse_into_timed_lines() {
        let lyrics = parse(SYNCED.as_bytes()).unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].start, Duration::from_millis(1000));
        assert_eq!(lyrics.lines[1].text, "second invented line");
        assert_eq!(lyrics.language.as_deref(), Some("en"));
        assert_eq!(lyrics.provider.as_deref(), Some("MusixMatch"));
    }

    #[test]
    fn unsynced_lyrics_have_zero_starts() {
        let json = r#"{"lyrics":{"syncType":"UNSYNCED","lines":[{"startTimeMs":"0","words":"only line","syllables":[],"endTimeMs":"0"}]},"colors":{},"hasVocalRemoval":false}"#;
        let lyrics = parse(json.as_bytes()).unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines[0].start, Duration::ZERO);
        assert!(lyrics.language.is_none());
    }

    #[test]
    fn malformed_json_is_a_protocol_error() {
        assert!(matches!(parse(b"{}"), Err(Error::Protocol { what: "lyrics", .. })));
    }
}
