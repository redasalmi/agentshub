//! Linux adapters for the app: the session bus and clean quitting.

pub mod instance;
pub mod motion;
#[cfg(target_os = "linux")]
pub mod scope;
pub mod sigterm;
#[cfg(target_os = "linux")]
pub mod tray;
