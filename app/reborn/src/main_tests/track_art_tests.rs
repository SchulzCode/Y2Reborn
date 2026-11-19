use super::*;
use reborn_core::RADIO_SOURCE;

fn restored() -> AppModel {
    // What `AppModel::restore` leaves after a restart: a song queued, paused, no session yet.
    AppModel {
        queue: vec![Track {
            id: 7,
            album: "Album".into(),
            artist: "Artist".into(),
            ..Default::default()
        }],
        queue_entry_ids: vec![QueueEntryId(1)],
        playback: PlaybackState::Paused,
        ..Default::default()
    }
}

fn ask(model: &AppModel, art: bool, asked: &mut Option<u64>) -> Option<artwork::Job> {
    restored_job(model, art, asked, Path::new("/cache"))
}

#[test]
fn the_restored_song_asks_for_its_picture_once() {
    let model = restored();
    let mut asked = None;

    let job = ask(&model, false, &mut asked).expect("a restored song needs its picture");
    assert_eq!(job.thumb, restored_key(&model.queue[0]));
    assert_eq!(asked, Some(job.thumb));
    // A song without a cover, or a decode that failed, is not asked for again.
    assert!(ask(&model, false, &mut asked).is_none());
}

#[test]
fn the_picture_is_not_asked_for_when_something_else_shows_or_will_show_it() {
    let mut asked = None;
    assert!(
        ask(&restored(), true, &mut asked).is_none(),
        "already shown"
    );

    for state in [
        PlaybackState::Playing,
        PlaybackState::Buffering,
        PlaybackState::Stopped,
        PlaybackState::Error,
    ] {
        let model = AppModel {
            playback: state,
            ..restored()
        };
        assert!(ask(&model, false, &mut asked).is_none(), "{state:?}");
    }
    let started = AppModel {
        generation: 1,
        ..restored()
    };
    assert!(
        ask(&started, false, &mut asked).is_none(),
        "playback has begun"
    );

    let empty = AppModel::default();
    assert!(ask(&empty, false, &mut asked).is_none(), "no song");

    let mut station = restored();
    station.queue[0].source_id = RADIO_SOURCE.into();
    assert!(
        ask(&station, false, &mut asked).is_none(),
        "a station has its logo"
    );
    assert_eq!(asked, None, "nothing was asked for");
}

#[test]
fn the_restored_key_is_not_cover_flows() {
    let track = &restored().queue[0];
    assert_ne!(restored_key(track), reborn_ui::art_key(track));
    assert_eq!(restored_key(track), restored_key(&track.clone()));
}

#[test]
fn a_late_picture_is_shown_only_for_the_song_still_waiting_for_it() {
    let model = restored();
    let key = restored_key(&model.queue[0]);
    let picture = || Some(vec![1, 2, 3, 4]);

    assert_eq!(restored_picture(&model, false, key, picture()), picture());
    // A song without a cover shows the placeholder.
    assert_eq!(restored_picture(&model, false, key, None), None);
    // Play was pressed meanwhile: the decoder publishes the picture itself.
    let started = AppModel {
        generation: 1,
        ..restored()
    };
    assert_eq!(restored_picture(&started, false, key, picture()), None);
    // A picture was shown already.
    assert_eq!(restored_picture(&model, true, key, picture()), None);
    // The queue moved to another song.
    let mut other = restored();
    other.queue[0].album = "Other".into();
    assert_eq!(restored_picture(&other, false, key, picture()), None);
}
