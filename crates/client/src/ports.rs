//! Ports: what the client needs from the operating system.
//!
//! Each OS gets one adapter in [`crate::adapters`] implementing both traits.

use std::path::Path;

use anyhow::Result;

/// Virtual devices that stand in for the headset, named as the agent opens them.
#[derive(Debug, Clone)]
pub struct Cable {
    /// Capture device carrying what apps play; the agent streams it to the hub.
    pub speaker: String,
    /// Playback device feeding the virtual mic apps record from; the agent plays the headset
    /// mic into it.
    pub mic: String,
}

/// Provides the virtual speaker and mic (RF6).
pub trait VirtualCable {
    /// Makes sure both virtual devices exist, creating them where the OS allows it.
    ///
    /// # Errors
    /// Fails when they are missing and cannot be created; the message says how to install them.
    fn prepare(&self) -> Result<Cable>;
}

/// Starts the agent at login and restarts it if it dies (RF9).
pub trait Autostart {
    /// Registers `exe run` as a login service and starts it.
    ///
    /// # Errors
    /// Fails when the service manager rejects the registration.
    fn install(&self, exe: &Path) -> Result<()>;

    /// Stops and removes the login service; succeeds when it was not installed.
    ///
    /// # Errors
    /// Fails when the service file cannot be removed.
    fn uninstall(&self) -> Result<()>;
}
