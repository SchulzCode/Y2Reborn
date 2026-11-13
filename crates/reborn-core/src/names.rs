//! The names in a tag: the artists of an artist tag and the genres of a genre tag, one by one, and
//! a joined tag as people read it.

/// The names in an artist tag (#38, #40): a multi-value tag comes joined by
/// ";" ("Calvin Harris;Kelis"), and a guest follows "feat.", "ft." or
/// "featuring". "&" and "," stay, as in "Simon & Garfunkel". An empty tag
/// is one unknown artist.
pub fn split_artists(tag: &str) -> Vec<&str> {
    const GUEST: [&str; 5] = [" featuring ", " feat. ", " feat ", " ft. ", " ft "];
    let mut names: Vec<&str> = Vec::new();
    for mut rest in tag.split(';') {
        loop {
            // ASCII markers, so every match starts and ends on a char boundary.
            let guest = GUEST
                .iter()
                .filter_map(|guest| {
                    rest.as_bytes()
                        .windows(guest.len())
                        .position(|w| w.eq_ignore_ascii_case(guest.as_bytes()))
                        .map(|at| (at, guest.len()))
                })
                .min();
            let (name, next) = match guest {
                Some((at, len)) => (&rest[..at], Some(&rest[at + len..])),
                None => (rest, None),
            };
            let name = name.trim();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
            match next {
                Some(next) => rest = next,
                None => break,
            }
        }
    }
    if names.is_empty() {
        names.push(tag.trim());
    }
    names
}

/// Genre names that hold a "/" of their own and stay whole.
const SLASHED_GENRES: [&str; 1] = ["Singer/Songwriter"];

/// The length of the slashed genre name `text` starts with, when the name
/// ends there.
fn slashed_genre(text: &str) -> Option<usize> {
    SLASHED_GENRES.iter().find_map(|kept| {
        let len = kept.len();
        let lead = text.as_bytes().get(..len)?;
        // ASCII names, so a match ends on a char boundary.
        (lead.eq_ignore_ascii_case(kept.as_bytes())
            && text[len..]
                .trim_start()
                .chars()
                .next()
                .is_none_or(|next| next == '/'))
        .then_some(len)
    })
}

/// The genres in a genre tag (#40, #124): a multi-value tag comes joined by
/// ";", as artists do, or by "/" ("Pop/Rock", as MusicBrainz Picard writes
/// ID3v2.3). An empty tag is one "No Genre".
pub fn split_genres(tag: &str) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for part in tag.split(';') {
        let mut rest = part.trim_start();
        while !rest.is_empty() {
            let from = slashed_genre(rest).unwrap_or(0);
            let end = rest[from..].find('/').map_or(rest.len(), |at| from + at);
            let name = rest[..end].trim();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
            rest = rest.get(end + 1..).unwrap_or("").trim_start();
        }
    }
    if names.is_empty() {
        names.push("");
    }
    names
}

/// A tag of names joined by ";" as people read it (#40):
/// "Lauryn Hill;Mary J. Blige" shows as "Lauryn Hill, Mary J. Blige".
pub fn joined_names(tag: &str) -> std::borrow::Cow<'_, str> {
    if !tag.contains(';') {
        return tag.into();
    }
    let names: Vec<&str> = tag
        .split(';')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    names.join(", ").into()
}

#[cfg(test)]
mod tests {
    use super::{joined_names, split_artists, split_genres};

    #[test]
    fn joined_genres_split_and_joined_names_read_with_commas() {
        assert_eq!(split_genres("Jazz;R&B"), ["Jazz", "R&B"]);
        assert_eq!(split_genres(" Jazz ; Soul;Jazz"), ["Jazz", "Soul"]);
        // #124: Picard joins the genres of an ID3v2.3 tag with "/".
        assert_eq!(split_genres("Rock/Pop"), ["Rock", "Pop"]);
        assert_eq!(
            split_genres("Pop / Rock;Jazz/pop/"),
            ["Pop", "Rock", "Jazz", "pop"]
        );
        assert_eq!(split_genres("Singer/Songwriter"), ["Singer/Songwriter"]);
        assert_eq!(
            split_genres("Folk/singer/songwriter / Pop"),
            ["Folk", "singer/songwriter", "Pop"]
        );
        assert_eq!(
            split_genres("Singer/Songwriters"),
            ["Singer", "Songwriters"]
        );
        assert_eq!(split_genres("ロック/ポップ"), ["ロック", "ポップ"]);
        assert_eq!(split_genres("/"), [""]);
        assert_eq!(split_genres(""), [""], "one No Genre");
        assert_eq!(split_genres(";"), [""]);
        assert_eq!(
            joined_names("Lauryn Hill;Mary J. Blige"),
            "Lauryn Hill, Mary J. Blige"
        );
        assert_eq!(joined_names("A; B ;"), "A, B");
        assert_eq!(joined_names("Simon & Garfunkel"), "Simon & Garfunkel");
        assert!(matches!(
            joined_names("Kelis"),
            std::borrow::Cow::Borrowed("Kelis")
        ));
    }

    #[test]
    fn joined_tags_and_guests_split_but_band_names_stay() {
        assert_eq!(
            split_artists("Calvin Harris;Kelis"),
            ["Calvin Harris", "Kelis"]
        );
        assert_eq!(
            split_artists("Calvin Harris; Dillon Francis ;Dizzee Rascal"),
            ["Calvin Harris", "Dillon Francis", "Dizzee Rascal"]
        );
        assert_eq!(
            split_artists("Calvin Harris Feat. Kelis"),
            ["Calvin Harris", "Kelis"]
        );
        assert_eq!(
            split_artists("Daft Punk ft. Pharrell Williams featuring Nile Rodgers"),
            ["Daft Punk", "Pharrell Williams", "Nile Rodgers"]
        );
        assert_eq!(split_artists("Simon & Garfunkel"), ["Simon & Garfunkel"]);
        assert_eq!(split_artists("Earth, Wind & Fire"), ["Earth, Wind & Fire"]);
        assert_eq!(
            split_artists("Soft Cell"),
            ["Soft Cell"],
            "\"ft\" only as a word"
        );
        assert_eq!(split_artists("Kelis;Kelis"), ["Kelis"]);
        assert_eq!(split_artists(""), [""], "one unknown artist");
        assert_eq!(
            split_artists("宇多田ヒカル feat. 椎名林檎"),
            ["宇多田ヒカル", "椎名林檎"]
        );
    }
}
