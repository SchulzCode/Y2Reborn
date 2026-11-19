use super::*;

#[test]
fn a_version_names_its_release() {
    assert_eq!(
        release_title("1.0.0-beta.9.3.35").as_deref(),
        Some("Y2Linux 1.0 Beta 9.3.35")
    );
    assert_eq!(
        release_title("1.0.0-beta.8.4").as_deref(),
        Some("Y2Linux 1.0 Beta 8.4")
    );
    assert_eq!(
        release_title("1.0.0-beta.9.3.24-product-ui-v3").as_deref(),
        Some("Y2Linux 1.0 Beta 9.3.24")
    );
    assert_eq!(release_title("1.0.0-beta.").as_deref(), None);
    assert_eq!(release_title("2.0.0").as_deref(), None);
    assert_eq!(release_title("dev").as_deref(), None);
}

#[test]
fn only_the_players_part_in_plain_words() {
    let body = "**Y2Linux 1.0 Beta 9.3.33**: lyrics and more.\n\n\
        ## What's new\n\
        - **Lyrics** ([Y2Reborn #126](https://github.com/x/pull/126), thanks to Lord-Funion): Now Playing → hold Select → **Lyrics**.\n\
        - A `.lrc` file next to the song.\n\n\
        ## Known issues\n\
        * AirPods can stay silent ([Y2Reborn #132](https://x/132)).\n\n\
        More details in plain words: **[Beta 9.3.33 report](https://x)**.\n\n\
        <!-- github-only -->\n\n## Install with SP Flash Tool\n- Unpack both.\n<!-- /github-only -->";
    assert_eq!(
        lines(body),
        vec![
            Line::Text("Y2Linux 1.0 Beta 9.3.33: lyrics and more.".into()),
            Line::Heading("What's new".into()),
            Line::Bullet(
                "Lyrics (Y2Reborn #126, thanks to Lord-Funion): Now Playing › hold Select › Lyrics."
                    .into()
            ),
            Line::Bullet("A .lrc file next to the song.".into()),
            Line::Heading("Known issues".into()),
            Line::Bullet("AirPods can stay silent (Y2Reborn #132).".into()),
        ]
    );
}

#[test]
fn a_bracket_that_is_no_link_stays() {
    assert_eq!(
        lines("- Volume [0–100] stays"),
        vec![Line::Bullet("Volume [0–100] stays".into())]
    );
    assert!(lines("").is_empty());
    assert!(lines("<!-- github-only -->\n- hidden").is_empty());
}
