//! Linux adapters for the app: the tray, systemd scopes, the desktop's motion
//! preference, the session bus and clean quitting.

pub mod instance;
pub mod motion;
pub mod notifications;
#[cfg(target_os = "linux")]
pub mod scope;
pub mod sigterm;
#[cfg(target_os = "linux")]
pub mod tray;
