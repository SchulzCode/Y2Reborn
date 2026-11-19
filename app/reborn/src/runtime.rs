//! `Runtime`'s methods, by area (the struct and its small helpers are in main.rs).
mod audio;
#[cfg(test)]
pub(crate) use audio::keep_live_parts;
mod effects;
mod idle;
mod platform;
mod queue;
mod radio;
mod sleep;
mod themes;
mod transport;
mod whats_new;
