//! WASAPI endpoint volume (`IAudioEndpointVolume`) on capture endpoints.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use anyhow::{Context, Result};
use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STGM_READ,
};
use windows::Win32::System::Variant::VT_LPWSTR;

use super::{InputVolume, best_match};

struct WinVolume {
    name: String,
    endpoint: IAudioEndpointVolume,
}

// SAFETY: COM is initialized in the multithreaded apartment before the
// interface is created, so it may be used from any thread.
unsafe impl Send for WinVolume {}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: standard property-store access; the PROPVARIANT is cleared.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let key = &DEVPKEY_Device_FriendlyName as *const _ as *const PROPERTYKEY;
        let mut value = store.GetValue(key).ok()?;
        let inner = &value.Anonymous.Anonymous;
        let result = if inner.vt == VT_LPWSTR {
            let ptr = *(&inner.Anonymous as *const _ as *const *const u16);
            let mut len = 0;
            while len < 32_768 && *ptr.add(len) != 0 {
                len += 1;
            }
            Some(
                OsString::from_wide(std::slice::from_raw_parts(ptr, len))
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        };
        let _ = PropVariantClear(&mut value);
        result
    }
}

pub fn open(device: Option<&str>) -> Result<Box<dyn InputVolume>> {
    // SAFETY: COM calls on this thread after initialization.
    unsafe {
        // Fails harmlessly if COM is already initialized on this thread.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("creating the audio device enumerator")?;

        let device = match device {
            None => enumerator
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .context("there is no default microphone")?,
            Some(query) => {
                let collection = enumerator
                    .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
                    .context("listing microphones")?;
                let mut devices = Vec::new();
                for i in 0..collection.GetCount()? {
                    let d = collection.Item(i)?;
                    if let Some(name) = friendly_name(&d) {
                        devices.push((name, d));
                    }
                }
                best_match(&devices, query)
                    .map(|(_, d)| d.clone())
                    .with_context(|| format!("no microphone matches \"{query}\""))?
            }
        };
        let name = friendly_name(&device).unwrap_or_else(|| "microphone".into());
        let endpoint: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .with_context(|| format!("opening the volume control of \"{name}\""))?;
        Ok(Box::new(WinVolume { name, endpoint }))
    }
}

impl InputVolume for WinVolume {
    fn device(&self) -> &str {
        &self.name
    }

    fn volume(&self) -> Result<f32> {
        // SAFETY: valid COM interface.
        Ok(unsafe { self.endpoint.GetMasterVolumeLevelScalar()? })
    }

    fn set_volume(&self, volume: f32) -> Result<()> {
        // SAFETY: valid COM interface; null event context is allowed.
        unsafe {
            self.endpoint
                .SetMasterVolumeLevelScalar(volume.clamp(0.0, 1.0), std::ptr::null())?
        };
        Ok(())
    }

    fn muted(&self) -> Result<Option<bool>> {
        // SAFETY: valid COM interface.
        Ok(Some(unsafe { self.endpoint.GetMute()? }.as_bool()))
    }

    fn set_muted(&self, muted: bool) -> Result<()> {
        // SAFETY: valid COM interface; null event context is allowed.
        unsafe { self.endpoint.SetMute(muted, std::ptr::null())? };
        Ok(())
    }
}
