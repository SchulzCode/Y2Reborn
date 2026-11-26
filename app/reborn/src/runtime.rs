//! `Runtime`'s methods, by area (the struct and its small helpers are in main.rs).
mod art_owner;
pub(crate) use art_owner::{art_owner_of, keeps_art, ArtOwner};
mod audio;
mod battery_history;
#[cfg(test)]
pub(crate) use audio::keep_live_parts;
pub(crate) use battery_history::Writer as BatteryWriter;
mod effects;
mod idle;
mod platform;
mod queue;
mod radio;
mod saver;
mod sleep;
mod themes;
mod transport;
mod whats_new;
