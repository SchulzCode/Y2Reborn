//! What's New after an update (Y2Linux #128): which GitHub release belongs to this version, and
//! the part of its text written for the player, as plain lines. The release text keeps install
//! steps, file lists and tests between `<!-- github-only -->` markers; only what comes before the
//! first marker is shown. Pure text work: the player fetches and caches it.
use serde::{Deserialize, Serialize};

/// Where the GitHub-only part of a release text starts.
const GITHUB_ONLY: &str = "<!-- github-only -->";

/// One line of the notes as the player shows them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Line {
    Heading(String),
    Bullet(String),
    Text(String),
}

/// The release title for a version: "1.0.0-beta.9.3.35" (also as Reborn's build label,
/// "1.0.0-beta.9.3.35-product-ui-v3") is "Y2Linux 1.0 Beta 9.3.35".
pub fn release_title(version: &str) -> Option<String> {
    let beta = version
        .strip_prefix("1.0.0-beta.")?
        .split(['-', '+'])
        .next()?;
    (!beta.is_empty()).then(|| format!("Y2Linux 1.0 Beta {beta}"))
}

/// The player's part of a release text, as lines: headings, bullets and paragraphs, with Markdown
/// marks and link addresses removed. The pointer to the report on GitHub is left out (it can't be
/// opened on the player).
pub fn lines(body: &str) -> Vec<Line> {
    let shown = body.split(GITHUB_ONLY).next().unwrap_or("");
    shown
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("<!--") && !l.starts_with("More details"))
        .map(|l| {
            if let Some(h) = l.strip_prefix('#') {
                Line::Heading(plain(h.trim_start_matches('#')))
            } else if let Some(b) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                Line::Bullet(plain(b))
            } else {
                Line::Text(plain(l))
            }
        })
        .filter(
            |l| !matches!(l, Line::Heading(t) | Line::Bullet(t) | Line::Text(t) if t.is_empty()),
        )
        .collect()
}

/// Markdown to plain text: `[label](address)` keeps the label, bold, italic and code marks go,
/// arrows become "›".
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find("](").and_then(|close| {
            after[close + 2..]
                .find(')')
                .map(|end| (close, close + 2 + end))
        }) {
            Some((close, end)) => {
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // The player's fonts have no arrow: menu paths read "Settings › Sound" there.
    out.replace("**", "")
        .replace('→', "›")
        .replace('`', "")
        .replace("__", "")
        .trim()
        .to_owned()
}

#[cfg(test)]
#[path = "whats_new_tests.rs"]
mod tests;
