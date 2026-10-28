// Non-Windows workspace builds exercise portable test doubles. Private fields that are consumed
// only by the real Windows backends are intentionally inactive on those targets.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
pub mod account;
pub mod audio;
pub mod capture;
#[cfg(windows)]
pub mod cloud_diagnostics;
#[cfg(windows)]
pub mod configuration;
#[cfg(windows)]
pub mod controller_settings;
#[cfg(windows)]
pub mod diagnostic_sharing;
pub mod diagnostics;
pub mod encoder;
#[cfg(windows)]
pub mod fixed_access;
#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod identity;
pub mod input;
mod input_restriction;
#[cfg(windows)]
pub mod recent_access;
pub mod runtime;
mod session_control;
#[cfg(windows)]
pub mod startup;
mod storage;
pub mod transfer;
pub mod transfer_checkpoint;
pub mod transfer_directory;
#[cfg(windows)]
pub mod transfer_recovery;
pub mod transfer_task;
pub mod transfer_task_catalog;
#[cfg(windows)]
pub mod transfer_task_dialog;
pub mod transfer_task_native;
pub mod transfer_task_recovery;
#[cfg(test)]
pub(crate) mod transfer_task_tests;
pub mod tray;
#[cfg(windows)]
pub mod trusted;
pub mod window;
