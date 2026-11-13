use super::*;

#[test]
fn every_built_in_layout_passes_the_check_and_reborn_default_is_first() {
    let presets = built_in();
    assert_eq!(presets[0].0, REBORN_DEFAULT);
    assert_eq!(presets[0].3, Layout::default());
    for (id, _, _, layout) in presets {
        assert_eq!(layout.check(), Ok(()), "{id}");
    }
}

#[test]
fn now_playing_falls_back_to_the_menus_and_the_dark_does_not() {
    let layout = Layout::default();
    use Control::*;
    assert_eq!(
        layout.resolve(Layer::NowPlaying, Next, Gesture::Press),
        Command::Next
    );
    assert_eq!(
        layout.resolve(Layer::NowPlaying, WheelForward, Gesture::Press),
        Command::VolumeUp
    );
    assert_eq!(
        layout.resolve(Layer::ScreenOff, Select, Gesture::Press),
        Command::Nothing
    );
    assert_eq!(
        layout.resolve(Layer::ScreenOff, WheelForward, Gesture::Hold),
        Command::Nothing
    );
}

#[test]
fn the_check_refuses_layouts_that_lock_people_out() {
    use Command as C;
    use Control::*;
    let mut layout = Layout::default();
    layout.set(Layer::Menus, Select, Gesture::Press, Some(C::PlayPause));
    assert_eq!(layout.check(), Err(Problem::Missing(C::Select)));
    // Select on another button is fine.
    layout.set(Layer::Menus, PlayPause, Gesture::Press, Some(C::Select));
    assert_eq!(layout.check(), Ok(()));
    // The wheel turned into volume: the menus can no longer scroll, unless buttons do it.
    layout.set(
        Layer::Menus,
        WheelForward,
        Gesture::Press,
        Some(C::VolumeUp),
    );
    assert_eq!(layout.check(), Err(Problem::Missing(C::ScrollForward)));
    layout.set(Layer::Menus, Next, Gesture::Press, Some(C::ScrollForward));
    assert_eq!(layout.check(), Ok(()));
    // A hold of the wheel is not a gesture: it cannot carry Back.
    let mut wheel = Layout::default();
    wheel.set(Layer::Menus, Back, Gesture::Press, Some(C::Nothing));
    wheel.set(Layer::Menus, Back, Gesture::Hold, Some(C::Nothing));
    wheel.set(Layer::Menus, WheelForward, Gesture::Hold, Some(C::Back));
    assert_eq!(wheel.check(), Err(Problem::Missing(C::Back)));
    // Nothing leaves Now Playing.
    let mut stuck = Layout::default();
    for (control, gesture) in [(Back, Gesture::Press), (Back, Gesture::Hold)] {
        stuck.set(Layer::NowPlaying, control, gesture, Some(C::Nothing));
    }
    assert_eq!(stuck.check(), Err(Problem::NoWayOut));
    // Opening Settings in a pocket.
    let mut dark = Layout::default();
    dark.set(
        Layer::ScreenOff,
        Back,
        Gesture::Press,
        Some(C::OpenSettings),
    );
    assert_eq!(dark.check(), Err(Problem::NotInTheDark(Back)));
    for problem in [
        Problem::Missing(C::Back),
        Problem::NoWayOut,
        Problem::NotInTheDark(Back),
    ] {
        assert!(problem.message().ends_with('.'), "{}", problem.message());
    }
}

#[test]
fn the_older_settings_become_a_preset_once_and_nothing_changes_for_defaults() {
    let mut fresh = ButtonSettings::default();
    fresh.migrate(NowPlayingWheel::Volume, ScreenOffButtons::Buttons);
    assert_eq!(
        (fresh.active.as_str(), fresh.own.len(), fresh.migrated),
        (REBORN_DEFAULT, 0, true)
    );

    let mut seek = ButtonSettings::default();
    seek.migrate(NowPlayingWheel::Seek, ScreenOffButtons::Buttons);
    assert_eq!(seek.active, "wheel_seeks", "a built-in that matches");

    let mut both = ButtonSettings::default();
    both.migrate(NowPlayingWheel::Seek, ScreenOffButtons::WheelAndButtons);
    assert_eq!(both.active, "own:0");
    assert_eq!(both.active_name(), "My Buttons");
    let layout = both.layout();
    use Control::*;
    assert_eq!(
        layout.resolve(Layer::ScreenOff, Select, Gesture::Press),
        Command::PlayPause
    );
    assert_eq!(
        layout.resolve(Layer::NowPlaying, WheelBack, Gesture::Press),
        Command::SeekBack
    );
    // Only once: a later change of the old settings is not taken over again.
    both.migrate(NowPlayingWheel::Volume, ScreenOffButtons::Locked);
    assert_eq!(both.own.len(), 1);
}

#[test]
fn saving_makes_own_presets_and_an_unusable_one_falls_back() {
    let mut settings = ButtonSettings::default();
    let mut layout = Layout::default();
    layout.set(
        Layer::ScreenOff,
        Control::Select,
        Gesture::Press,
        Some(Command::PlayPause),
    );
    settings.save(layout.clone());
    assert_eq!(
        (settings.active.as_str(), settings.active_name()),
        ("own:0", "My Buttons".into())
    );
    // Saved again while it is active: the same preset.
    settings.save(layout.clone());
    assert_eq!(settings.own.len(), 1);
    settings.active = REBORN_DEFAULT.into();
    settings.save(layout.clone());
    assert_eq!(settings.active_name(), "My Buttons 2");
    // Never more than MAX_OWN.
    for _ in 0..6 {
        settings.active = REBORN_DEFAULT.into();
        settings.save(layout.clone());
    }
    assert_eq!(settings.own.len(), MAX_OWN);
    // A stored layout that fails the check (written by hand, or by a bug) is not used.
    settings.own[0].layout.set(
        Layer::Menus,
        Control::Back,
        Gesture::Press,
        Some(Command::Nothing),
    );
    settings.own[0].layout.set(
        Layer::Menus,
        Control::Back,
        Gesture::Hold,
        Some(Command::Nothing),
    );
    settings.active = "own:0".into();
    assert_eq!(settings.layout(), Layout::default());
    settings.delete_active();
    assert_eq!(
        (settings.active.as_str(), settings.own.len()),
        (REBORN_DEFAULT, MAX_OWN - 1)
    );
    // A preset id that is gone.
    settings.active = "own:9".into();
    assert_eq!(
        (settings.layout(), settings.active_name()),
        (Layout::default(), "Reborn Default".into())
    );
}

#[test]
fn a_layout_from_a_newer_reborn_keeps_what_this_one_knows() {
    let json = r#"{"bindings":[
        {"layer":"menus","control":"select","gesture":"press","command":"play_pause"},
        {"layer":"menus","control":"play_pause","gesture":"press","command":"select"},
        {"layer":"menus","control":"next","gesture":"press","command":"teleport"},
        {"layer":"menus","control":"power","gesture":"press","command":"home"},
        {"layer":"menus","control":"back","gesture":"press","command":"reborn_default"}]}"#;
    let layout: Layout = serde_json::from_str(json).unwrap();
    // Laid over Reborn Default: the unknown ones leave their buttons as Reborn made them.
    assert_eq!(layout.bindings.len(), Layout::default().bindings.len());
    assert_eq!(
        layout.resolve(Layer::Menus, Control::Select, Gesture::Press),
        Command::PlayPause
    );
    assert_eq!(
        layout.resolve(Layer::Menus, Control::Next, Gesture::Press),
        Command::Next
    );
    assert_eq!(
        layout.resolve(Layer::Menus, Control::Back, Gesture::Press),
        Command::Back
    );
    // An old session without buttons reads as Reborn Default.
    let settings: ButtonSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings, ButtonSettings::default());
}

#[test]
fn commands_with_an_action_become_that_action() {
    assert_eq!(Command::Next.actions(1), [Action::NextTrack]);
    assert_eq!(
        Command::ScrollForward.actions(3),
        [Action::WheelClockwise(3)]
    );
    assert_eq!(
        Command::ScrollBack.actions(0),
        [Action::WheelCounterClockwise(1)]
    );
    assert_eq!(
        Command::Shuffle.actions(1),
        [Action::Command(Command::Shuffle)]
    );
    assert!(Command::Nothing.actions(1).is_empty());
    for command in Command::OFFERED {
        assert!(
            !command.label().is_empty() && command.hint().ends_with('.'),
            "{command:?}"
        );
    }
    assert!(!Command::OFFERED.contains(&Command::RebornDefault));
    // The keys are the serde names, so a layout file and the row keys agree.
    for command in Command::OFFERED {
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            command.key(),
            "{command:?}"
        );
        assert_eq!(Command::from_key(command.key()), Some(command));
    }
}
