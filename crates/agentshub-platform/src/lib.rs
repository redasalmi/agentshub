//! Linux adapters for the app: the session bus and clean quitting.

pub mod instance;
#[cfg(target_os = "linux")]
pub mod scope;
pub mod sigterm;
