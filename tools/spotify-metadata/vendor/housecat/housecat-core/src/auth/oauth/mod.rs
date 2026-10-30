//! OAuth flows that produce a first-login access token (spec section 5.4).

pub mod device;
pub mod pkce;

pub use device::{device, DeviceFlow};
pub use pkce::{pkce, PkceFlow};
