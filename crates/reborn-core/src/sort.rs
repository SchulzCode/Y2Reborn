//! A to Z order for library names.
//!
//! Names are folded before they are compared: lower case, accents taken off
//! (é is e, ß is ss), apostrophes and dots dropped ("Don't" is "dont",
//! "R.E.M." is "rem") and other punctuation read as a space. A leading
//! article is skipped (`SortAs`). The name shown is never changed.

/// Longest folded name kept, in bytes; longer names are cut there.
const NAME_MAX: usize = 240;

/// `s` folded for comparison: lower case, without accents, apostrophes or
/// dots, other punctuation as single spaces, trimmed.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // A space is owed before the next kept character.
    let mut gap = false;
    for ch in s.chars().flat_map(char::to_lowercase) {
        if out.len() >= NAME_MAX {
            break;
        }
        match ch {
            'a'..='z' | '0'..='9' => {
                push(&mut out, &mut gap, ch);
            }
            // Apostrophes, dots and combining accents join what is around them.
            '\'' | '\u{2018}' | '\u{2019}' | '`' | '\u{b4}' | '.' | '\u{300}'..='\u{36f}' => {}
            c => match fold_letter(c) {
                Some(s) => s.chars().for_each(|c| push(&mut out, &mut gap, c)),
                None if c.is_alphanumeric() => push(&mut out, &mut gap, c),
                None => gap = !out.is_empty(),
            },
        }
    }
    out
}

fn push(out: &mut String, gap: &mut bool, ch: char) {
    if std::mem::take(gap) {
        out.push(' ');
    }
    out.push(ch);
}

/// Latin letters with accents (and the ligatures) as plain letters.
fn fold_letter(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// What a name is, for its place in A to Z order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortAs {
    /// An artist (or album artist): a leading "The " is skipped, so The
    /// Postal Service sorts under P, but "A" and "An" stay, since they are
    /// part of names like A Tribe Called Quest or A-ha.
    Artist,
    /// An album, song or genre: a leading "The ", "A " or "An " is skipped
    /// (A Night at the Opera sorts under N), as players do.
    Title,
}

/// The key every A to Z list sorts a name by: the name folded (case,
/// accents and punctuation ignored, `fold`) without its leading article
/// (`SortAs`). Digits come before letters; other scripts after them.
pub fn sort_key(name: &str, kind: SortAs) -> String {
    sort_key_folded(fold(name), kind)
}

/// `sort_key` of a name already folded (`fold`), without folding it again.
pub fn sort_key_folded(mut folded: String, kind: SortAs) -> String {
    let articles: &[&str] = match kind {
        SortAs::Artist => &["the "],
        SortAs::Title => &["the ", "a ", "an "],
    };
    if let Some(a) = articles
        .iter()
        .find(|a| folded.len() > a.len() && folded.starts_with(*a))
    {
        folded.drain(..a.len());
    }
    folded
}

/// Digits a number in `natural_key` is padded to; longer runs stay as they are.
const NUMBER_WIDTH: usize = 10;

/// The key a file or folder name sorts by, as file managers order them: the
/// name folded (`fold`, no article skipped) with its numbers compared as
/// numbers, so "2 - Intro" comes before "10 - Outro" (Folders, Beta 8.39).
pub fn natural_key(name: &str) -> String {
    let folded = fold(name);
    let mut out = String::with_capacity(folded.len() + 8);
    let mut digits = String::new();
    let flush = |out: &mut String, digits: &mut String| {
        if digits.is_empty() {
            return;
        }
        let number = digits.trim_start_matches('0');
        let number = if number.is_empty() { "0" } else { number };
        for _ in number.len()..NUMBER_WIDTH {
            out.push('0');
        }
        out.push_str(number);
        digits.clear();
    };
    for ch in folded.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            flush(&mut out, &mut digits);
            out.push(ch);
        }
    }
    flush(&mut out, &mut digits);
    out
}

/// The letter a sort key files under (fast scrolling's overlay): A to Z, or
/// # for digits, symbols and other scripts.
pub fn sort_initial(key: &str) -> char {
    key.chars()
        .next()
        .filter(char::is_ascii_alphabetic)
        .map_or('#', |c| c.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ignores_case_accents_and_punctuation() {
        assert_eq!(fold("Lumière à l'horizon"), "lumiere a lhorizon");
        assert_eq!(
            fold("Émilie & the Northern Lights"),
            "emilie the northern lights"
        );
        assert_eq!(fold("Don't Stop Me Now!"), "dont stop me now");
        assert_eq!(fold("R.E.M."), "rem");
        assert_eq!(fold("  AC/DC -- Live  "), "ac dc live");
        assert_eq!(fold("Straße Œuvre"), "strasse oeuvre");
        // Decomposed accents (e and a combining acute) fold the same.
        assert_eq!(fold("Beyonce\u{301}"), "beyonce");
        // Other scripts stay, in lower case.
        assert_eq!(fold("Северный Свет"), "северный свет");
        assert_eq!(fold("!!!"), "");
    }

    #[test]
    fn a_to_z_skips_the_article_and_files_under_the_next_word() {
        fn artists(names: &[&'static str]) -> Vec<&'static str> {
            let mut v = names.to_vec();
            v.sort_by_cached_key(|n| (sort_key(n, SortAs::Artist), n.to_string()));
            v
        }
        assert_eq!(
            artists(&[
                "The Postal Service",
                "Radiohead",
                "the National",
                "A Tribe Called Quest",
                "Émilie Simon",
                "2Pac",
                "Nina Simone",
                "Theory of a Deadman",
                "The The",
            ]),
            [
                "2Pac",
                "A Tribe Called Quest",
                "Émilie Simon",
                "the National",
                "Nina Simone",
                "The Postal Service",
                "Radiohead",
                "The The",
                "Theory of a Deadman",
            ]
        );
        // Titles skip "A" and "An" too; a lone article is kept.
        assert_eq!(
            sort_key("A Night at the Opera", SortAs::Title),
            "night at the opera"
        );
        assert_eq!(sort_key("An Awesome Wave", SortAs::Title), "awesome wave");
        assert_eq!(sort_key("The Wall", SortAs::Title), "wall");
        assert_eq!(sort_key("The", SortAs::Title), "the");
        assert_eq!(sort_key("A-ha", SortAs::Artist), "a ha");
        assert_eq!(sort_key("Anthem", SortAs::Title), "anthem");
        // The overlay letter follows the key.
        assert_eq!(
            sort_initial(&sort_key("The Postal Service", SortAs::Artist)),
            'P'
        );
        assert_eq!(sort_initial(&sort_key("Émilie", SortAs::Artist)), 'E');
        assert_eq!(sort_initial(&sort_key("10cc", SortAs::Artist)), '#');
        assert_eq!(sort_initial(&sort_key("Северный", SortAs::Title)), '#');
        assert_eq!(sort_initial(""), '#');
    }

    #[test]
    fn file_names_sort_as_file_managers_order_them() {
        let mut names = vec![
            "10 - Outro.mp3",
            "2 - Intro.mp3",
            "01 - Opening.mp3",
            "the Zebra.mp3",
            "Skydivin' - Darren Styles.mp3",
            "apple.mp3",
            "Électrique.mp3",
            "Zulu.mp3",
        ];
        names.sort_by_cached_key(|n| (natural_key(n), n.to_string()));
        assert_eq!(
            names,
            [
                "01 - Opening.mp3",
                "2 - Intro.mp3",
                "10 - Outro.mp3",
                "apple.mp3",
                "Électrique.mp3",
                "Skydivin' - Darren Styles.mp3",
                "the Zebra.mp3",
                "Zulu.mp3",
            ]
        );
        // "01" and "1" are the same number; the name itself breaks the tie.
        assert_eq!(natural_key("CD01"), natural_key("CD1"));
        assert_eq!(natural_key("Vol. 007"), "vol 0000000007");
    }
}
