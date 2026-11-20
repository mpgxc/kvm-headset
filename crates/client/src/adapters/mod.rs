//! One adapter per OS, picked at compile time.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::Linux as Platform;
#[cfg(target_os = "macos")]
pub use macos::MacOs as Platform;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("adapter ainda não implementado para este sistema; só macOS e Linux por enquanto");
