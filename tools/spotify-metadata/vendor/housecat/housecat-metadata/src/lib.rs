//! housecat-metadata: typed access to Spotify's catalog (spec section 6).

pub mod audio_info;
pub mod catalog;
pub mod client;
pub(crate) mod convert;
pub(crate) mod entities;
pub(crate) mod lyrics;
pub mod model;
pub(crate) mod playlist;

pub use audio_info::{
    episode_audio, episode_audio_with, track_audio, track_audio_with, AudioFile, AudioFormat, TrackAudio,
};
pub use catalog::Catalog;
pub use client::Metadata;
pub use model::*;
