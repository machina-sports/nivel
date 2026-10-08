//! CoreAudio: `kAudioDevicePropertyVolumeScalar` / `kAudioDevicePropertyMute`
//! on the input scope. Declared directly against the system frameworks so no
//! bindings generator is needed at build time.

use std::ffi::{c_char, c_void};

use anyhow::{Context, Result, bail};

use super::{InputVolume, best_match};

type ObjectId = u32;
type OsStatus = i32;

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const SYSTEM_OBJECT: ObjectId = 1;
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_INPUT: u32 = fourcc(b"inpt");
const ELEMENT_MAIN: u32 = 0;
const HARDWARE_DEVICES: u32 = fourcc(b"dev#");
const DEFAULT_INPUT_DEVICE: u32 = fourcc(b"dIn ");
const OBJECT_NAME: u32 = fourcc(b"lnam");
const DEVICE_STREAMS: u32 = fourcc(b"stm#");
const VOLUME_SCALAR: u32 = fourcc(b"volm");
const MUTE: u32 = fourcc(b"mute");
const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectHasProperty(id: ObjectId, address: *const Address) -> u8;
    fn AudioObjectIsPropertySettable(
        id: ObjectId,
        address: *const Address,
        settable: *mut u8,
    ) -> OsStatus;
    fn AudioObjectGetPropertyDataSize(
        id: ObjectId,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> OsStatus;
    fn AudioObjectGetPropertyData(
        id: ObjectId,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> OsStatus;
    fn AudioObjectSetPropertyData(
        id: ObjectId,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetLength(s: *const c_void) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(s: *const c_void, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

fn addr(selector: u32, scope: u32, element: u32) -> Address {
    Address {
        selector,
        scope,
        element,
    }
}

fn check(status: OsStatus, what: &str) -> Result<()> {
    if status != 0 {
        bail!("CoreAudio error {status} while {what}");
    }
    Ok(())
}

fn has(id: ObjectId, a: &Address) -> bool {
    // SAFETY: `a` is a valid address struct for the duration of the call.
    unsafe { AudioObjectHasProperty(id, a) != 0 }
}

fn settable(id: ObjectId, a: &Address) -> bool {
    let mut out = 0u8;
    // SAFETY: valid pointers to locals.
    unsafe { AudioObjectIsPropertySettable(id, a, &mut out) == 0 && out != 0 }
}

fn get<T: Copy + Default>(id: ObjectId, a: &Address, what: &str) -> Result<T> {
    let mut value = T::default();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `value` is a plain-old-data local of exactly `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            a,
            0,
            std::ptr::null(),
            &mut size,
            (&raw mut value).cast(),
        )
    };
    check(status, what)?;
    Ok(value)
}

fn set<T: Copy>(id: ObjectId, a: &Address, value: T, what: &str) -> Result<()> {
    // SAFETY: `value` lives for the call and has the size we pass.
    let status = unsafe {
        AudioObjectSetPropertyData(
            id,
            a,
            0,
            std::ptr::null(),
            size_of::<T>() as u32,
            (&raw const value).cast(),
        )
    };
    check(status, what)
}

fn data_size(id: ObjectId, a: &Address) -> u32 {
    let mut size = 0u32;
    // SAFETY: valid pointers to locals.
    let status = unsafe { AudioObjectGetPropertyDataSize(id, a, 0, std::ptr::null(), &mut size) };
    if status == 0 { size } else { 0 }
}

fn device_ids() -> Result<Vec<ObjectId>> {
    let a = addr(HARDWARE_DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN);
    let mut size = data_size(SYSTEM_OBJECT, &a);
    let mut ids = vec![0 as ObjectId; size as usize / size_of::<ObjectId>()];
    // SAFETY: `ids` has room for `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    check(status, "listing audio devices")?;
    ids.truncate(size as usize / size_of::<ObjectId>());
    Ok(ids)
}

fn device_name(id: ObjectId) -> Option<String> {
    let a = addr(OBJECT_NAME, SCOPE_GLOBAL, ELEMENT_MAIN);
    let cf: usize = get(id, &a, "reading a device name").ok()?;
    let cf = cf as *const c_void;
    if cf.is_null() {
        return None;
    }
    // SAFETY: `cf` is a CFStringRef we own (get rule: copy), released below.
    unsafe {
        let len = CFStringGetMaximumSizeForEncoding(CFStringGetLength(cf), UTF8) + 1;
        let mut buf = vec![0u8; len.max(1) as usize];
        let ok = CFStringGetCString(cf, buf.as_mut_ptr().cast(), len, UTF8) != 0;
        CFRelease(cf);
        if !ok {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

fn has_input(id: ObjectId) -> bool {
    data_size(id, &addr(DEVICE_STREAMS, SCOPE_INPUT, ELEMENT_MAIN)) > 0
}

struct MacVolume {
    id: ObjectId,
    name: String,
    /// Elements (0 = main, else channel numbers) that carry a volume.
    elements: Vec<u32>,
}

pub fn open(device: Option<&str>) -> Result<Box<dyn InputVolume>> {
    let id = match device {
        None => {
            let id: ObjectId = get(
                SYSTEM_OBJECT,
                &addr(DEFAULT_INPUT_DEVICE, SCOPE_GLOBAL, ELEMENT_MAIN),
                "finding the default microphone",
            )?;
            if id == 0 {
                bail!("there is no default microphone");
            }
            id
        }
        Some(query) => {
            let inputs: Vec<(String, ObjectId)> = device_ids()?
                .into_iter()
                .filter(|&id| has_input(id))
                .filter_map(|id| device_name(id).map(|n| (n, id)))
                .collect();
            best_match(&inputs, query)
                .map(|(_, id)| *id)
                .with_context(|| format!("no microphone matches \"{query}\""))?
        }
    };
    let name = device_name(id).unwrap_or_else(|| format!("device {id}"));

    let main = addr(VOLUME_SCALAR, SCOPE_INPUT, ELEMENT_MAIN);
    let elements: Vec<u32> = if has(id, &main) && settable(id, &main) {
        vec![ELEMENT_MAIN]
    } else {
        (1..=8)
            .filter(|&ch| {
                let a = addr(VOLUME_SCALAR, SCOPE_INPUT, ch);
                has(id, &a) && settable(id, &a)
            })
            .collect()
    };
    if elements.is_empty() {
        bail!(
            "\"{name}\" has no adjustable input volume in macOS (common for USB mics with a hardware gain knob); use `nivel run` for software leveling"
        );
    }
    Ok(Box::new(MacVolume { id, name, elements }))
}

impl InputVolume for MacVolume {
    fn device(&self) -> &str {
        &self.name
    }

    fn volume(&self) -> Result<f32> {
        let mut sum = 0.0;
        for &e in &self.elements {
            sum += get::<f32>(
                self.id,
                &addr(VOLUME_SCALAR, SCOPE_INPUT, e),
                "reading the input volume",
            )?;
        }
        Ok(sum / self.elements.len() as f32)
    }

    fn set_volume(&self, volume: f32) -> Result<()> {
        let v = volume.clamp(0.0, 1.0);
        for &e in &self.elements {
            set(
                self.id,
                &addr(VOLUME_SCALAR, SCOPE_INPUT, e),
                v,
                "setting the input volume",
            )?;
        }
        Ok(())
    }

    fn muted(&self) -> Result<Option<bool>> {
        let a = addr(MUTE, SCOPE_INPUT, ELEMENT_MAIN);
        if !has(self.id, &a) {
            return Ok(None);
        }
        Ok(Some(
            get::<u32>(self.id, &a, "reading the input mute")? != 0,
        ))
    }

    fn set_muted(&self, muted: bool) -> Result<()> {
        let a = addr(MUTE, SCOPE_INPUT, ELEMENT_MAIN);
        if !has(self.id, &a) {
            bail!("\"{}\" has no mute control", self.name);
        }
        set(self.id, &a, u32::from(muted), "setting the input mute")
    }
}
