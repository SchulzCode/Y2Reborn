//! Customize > Buttons fixtures of the preview renderer (Beta 9.3.29).
use super::Setup;
use reborn_core::{AppModel, Screen};

/// Beta 9.3.29: Customize > Buttons, the player drawn per layer, a button's page, a picker.
pub fn cases(at: fn(Screen, &str) -> AppModel) -> Vec<(&'static str, AppModel, Setup)> {
    let home = at(Screen::Home, "");
    let at = |filter: &str| at(Screen::Customize, filter);
    let focus = |n: usize, active: &'static str| -> Setup {
        Box::new(move |_, m| {
            if !active.is_empty() {
                m.settings.buttons.active = active.into();
            }
            m.navigation.focus = n;
        })
    };
    vec![
        // A session saved on the older Wheel & Buttons page, and its Haptics picker.
        ("51g-customize-wheel", at("wheel"), focus(0, "")),
        ("51h-customize-picker", at("choose:haptics"), focus(1, "")),
        ("51k-buttons", at("buttons"), focus(3, "one_hand")),
        ("51l-buttons-menus", at("buttons:menus"), focus(1, "")),
        (
            "51m-buttons-screen-off",
            at("buttons:screen_off"),
            focus(4, "one_hand"),
        ),
        (
            "51n-buttons-wheel",
            at("buttons:now_playing"),
            focus(5, "wheel_seeks"),
        ),
        (
            "51o-buttons-control",
            at("buttons:menus:next"),
            focus(0, ""),
        ),
        (
            "51p-buttons-pick",
            at("buttons:now_playing:select:hold"),
            focus(3, ""),
        ),
        (
            "51q-buttons-layouts",
            at("buttons:layouts"),
            focus(1, "pocket"),
        ),
        // The Quick Menu while another layout is in use: Buttons, Use Default.
        (
            "51r-quick-buttons",
            home,
            Box::new(|_, m| {
                m.settings.buttons.active = "pocket".into();
                m.navigation.modal = Some(reborn_core::Modal::QuickMenu);
                m.navigation.modal_focus = 4;
            }),
        ),
    ]
}
