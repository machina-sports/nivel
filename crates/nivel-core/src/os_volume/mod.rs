//! The operating system's own microphone volume and mute.
//!
//! This is the slider in System Settings / Sound settings / pavucontrol. It
//! changes the level every app receives, which is what `nivel ride` steers.

use anyhow::Result;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::pactl;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// One microphone's OS-level input volume.
pub trait InputVolume: Send {
    /// The device being controlled, as the OS names it.
    fn device(&self) -> &str;
    /// Current volume, 0.0 to 1.0.
    fn volume(&self) -> Result<f32>;
    fn set_volume(&self, volume: f32) -> Result<()>;
    /// `None` when the device has no mute control.
    fn muted(&self) -> Result<Option<bool>>;
    fn set_muted(&self, muted: bool) -> Result<()>;
}

/// Open the volume control of a microphone (by name or fragment, as shown by
/// `nivel devices`), or of the default microphone.
pub fn open(device: Option<&str>) -> Result<Box<dyn InputVolume>> {
    #[cfg(target_os = "macos")]
    return macos::open(device);
    #[cfg(windows)]
    return windows::open(device);
    #[cfg(target_os = "linux")]
    return linux::open(device);
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = device;
        anyhow::bail!("controlling the OS input volume is not supported on this platform")
    }
}

/// Exact (case-insensitive) match first, then substring.
#[allow(dead_code)]
fn best_match<'a, T>(items: &'a [(String, T)], query: &str) -> Option<&'a (String, T)> {
    let q = query.to_lowercase();
    items
        .iter()
        .find(|(n, _)| n.to_lowercase() == q)
        .or_else(|| items.iter().find(|(n, _)| n.to_lowercase().contains(&q)))
}
