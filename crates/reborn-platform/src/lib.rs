pub mod avrcp;
pub mod bluetooth;
pub mod codecs;
pub mod contract;
pub mod input;
mod native;
pub mod power;
pub mod storage;
pub mod wifi;
pub mod workload;
pub use native::{filesystem_uuid, free_bytes, install_signals, stop_requested};

pub mod dashboard;
