//! A version's release text from the project's GitHub releases (Settings → System → Software Update
//! → What's New, Y2Linux #128). One small request, the same bounded fetch as the radio directory.
use serde::Deserialize;

const RELEASES: &str = "https://api.github.com/repos/SchulzCode/Y2Linux/releases?per_page=30";

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
}

/// The text of the release called `title` ("Y2Linux 1.0 Beta 9.3.35").
pub fn fetch(title: &str) -> Result<String, String> {
    let bytes = crate::radio::fetch(RELEASES)?;
    find(&bytes, title)
}

fn find(bytes: &[u8], title: &str) -> Result<String, String> {
    let releases: Vec<Release> = serde_json::from_slice(bytes)
        .map_err(|_| "GitHub sent something unreadable.".to_owned())?;
    releases
        .into_iter()
        .find(|r| r.name == title)
        .map(|r| r.body)
        .ok_or_else(|| "These notes aren't online yet. Try again later.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_with_this_title() {
        let list = br#"[{"name":"Y2Linux 1.0 Beta 9.3.35","body":"new"},
                        {"name":"Y2Linux 1.0 Beta 9.3.34","body":"old","draft":false}]"#;
        assert_eq!(find(list, "Y2Linux 1.0 Beta 9.3.34").as_deref(), Ok("old"));
        assert!(
            find(list, "Y2Linux 1.0 Beta 9.3.3").is_err(),
            "the whole title only"
        );
        assert!(find(b"<html>", "x").is_err());
    }
}
