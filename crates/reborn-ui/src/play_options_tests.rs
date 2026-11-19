use super::*;

fn keys(items: &[Item]) -> Vec<&str> {
    items.iter().map(|i| i.key.as_str()).collect()
}

#[test]
fn one_menu_for_every_group_with_its_own_keys() {
    assert_eq!(
        keys(&menu(Family::Group, true)),
        [
            "group_play",
            "group_shuffle",
            "group_shuffle_albums",
            "group_next",
            "group_queue",
            "group_playlist"
        ]
    );
    // One album: nothing to mix as albums.
    assert!(!keys(&menu(Family::Page, false)).contains(&"collection_shuffle_albums"));
    // A playlist is not added to a playlist; its menu goes on with its own rows.
    assert_eq!(
        keys(&menu(Family::Playlist, true)),
        [
            "pl_play",
            "pl_shuffle",
            "pl_shuffle_albums",
            "pl_next",
            "pl_queue"
        ]
    );
}

#[test]
fn every_menu_key_reads_back_as_its_choice() {
    for family in [Family::Group, Family::Page, Family::Playlist] {
        for (item, (choice, label, _)) in menu(family, true).iter().zip(CHOICES) {
            assert_eq!(choice_of(family, &item.key), Some(choice), "{}", item.key);
            assert_eq!(item.label, label);
        }
    }
    assert_eq!(choice_of(Family::Playlist, "pl_sort"), None);
    assert_eq!(choice_of(Family::Group, "collection_play"), None);
    assert_eq!(choice_of(Family::Page, "collection_playful"), None);
}

fn choice_of(family: Family, key: &str) -> Option<Choice> {
    choice(family, key)
}

#[test]
fn header_rows_per_page() {
    let labels = |s| {
        header_rows(s)
            .into_iter()
            .map(|i| i.label)
            .collect::<Vec<_>>()
    };
    assert_eq!(labels(Screen::Tracks), ["Play All", "Shuffle All"], "#163");
    assert_eq!(labels(Screen::Album), ["Play All", "Shuffle All"]);
    assert_eq!(labels(Screen::Artist), ["Play All"]);
    assert_eq!(labels(Screen::Folders), ["Shuffle All"]);
    assert!(labels(Screen::Settings).is_empty());
    assert_eq!(header_choice(PLAY_ALL), Some(Choice::Play));
    assert_eq!(header_choice(SHUFFLE_ALL), Some(Choice::Shuffle));
    assert_eq!(header_choice("track:1"), None);
}

#[test]
fn music_lists_have_titles_and_play_what_they_show() {
    assert_eq!(list_title(RECENTLY_ADDED), Some("Recently Added"));
    assert_eq!(list_title(MOST_PLAYED), Some("Most Played"));
    assert_eq!(list_title("genre:Jazz"), None);
    let song = |name: &str, mtime| Track {
        path: format!("/m/{name}.flac").into(),
        title: name.into(),
        duration_ms: 200_000,
        mtime,
        online: true,
        ..Default::default()
    };
    let tracks = vec![song("old", 1), song("new", 2)];
    let mut m = AppModel::default();
    assert_eq!(list_songs(&m, &tracks, RECENTLY_ADDED), Some(vec![1, 0]));
    assert_eq!(list_songs(&m, &tracks, RECENTLY_PLAYED), Some(vec![]));
    m.play_stats.record(&tracks[0]);
    assert_eq!(list_songs(&m, &tracks, MOST_PLAYED), Some(vec![0]));
    assert_eq!(list_songs(&m, &tracks, ""), None);
}

#[test]
fn shuffle_all_on_songs_plays_every_song_shuffled() {
    let mut ui = Ui::default();
    let tracks: Vec<Track> = ["b", "a", "c"]
        .iter()
        .map(|n| Track {
            path: format!("/m/{n}.flac").into(),
            title: (*n).into(),
            online: true,
            ..Default::default()
        })
        .collect();
    let mut m = AppModel {
        screen: Screen::Tracks,
        ..Default::default()
    };
    let rows = ui.catalog_rows(&m, &tracks);
    assert_eq!(keys(&rows[..2]), [PLAY_ALL, SHUFFLE_ALL]);
    match ui.select_row(&mut m, &tracks, SHUFFLE_ALL) {
        Effect::PlayCollection {
            members, shuffle, ..
        } => {
            assert_eq!(members, [1, 0, 2], "Songs' A to Z order");
            assert_eq!(shuffle, ShuffleMode::Songs);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(m.screen, Screen::NowPlaying);
}
