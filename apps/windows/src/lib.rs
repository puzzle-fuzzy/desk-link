// Non-Windows workspace builds exercise portable test doubles. Private fields that are consumed
// only by the real Windows backends are intentionally inactive on those targets.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod capture;
#[cfg(windows)]
pub mod configuration;
#[cfg(windows)]
pub mod controller_settings;
pub mod diagnostics;
pub mod encoder;
#[cfg(windows)]
pub mod fixed_access;
#[cfg(windows)]
pub mod identity;
pub mod input;
#[cfg(windows)]
pub mod recent_access;
pub mod runtime;
mod storage;
pub mod tray;
#[cfg(windows)]
pub mod trusted;
pub mod window;
