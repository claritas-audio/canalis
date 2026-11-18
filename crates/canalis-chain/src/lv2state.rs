//! LV2 state through the plugin's `state:interface`, as an opaque blob the control plane
//! keeps in the slot like a CLAP state: the POD, portable properties the plugin stores
//! plus its control port values, so a restored slot sounds as it did. Properties travel
//! by URI, not URID, since URIDs are numbered per process.

use std::ffi::{CStr, CString, c_void};
use std::ptr;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use lv2_raw::core::{LV2Descriptor, LV2Feature};
use lv2_sys::{LV2_State_Interface, LV2_State_Status};

use crate::lv2::{map, unmap};

const URI_STATE_INTERFACE: &CStr = c"http://lv2plug.in/ns/ext/state#interface";
const MAGIC: &[u8; 4] = b"CLV2";
const VERSION: u8 = 1;
const STATUS_SUCCESS: LV2_State_Status = lv2_sys::LV2_State_Status_LV2_STATE_SUCCESS;
const STATUS_UNKNOWN: LV2_State_Status = lv2_sys::LV2_State_Status_LV2_STATE_ERR_UNKNOWN;
const STATUS_BAD_FLAGS: LV2_State_Status = lv2_sys::LV2_State_Status_LV2_STATE_ERR_BAD_FLAGS;
const IS_POD: u32 = lv2_sys::LV2_State_Flags::LV2_STATE_IS_POD.0;
const IS_PORTABLE: u32 = lv2_sys::LV2_State_Flags::LV2_STATE_IS_PORTABLE.0;

/// One stored property: a value of some atom type under a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub key: CString,
    pub type_: CString,
    pub flags: u32,
    pub value: Vec<u8>,
}

/// What a slot keeps of a plugin: control inputs by symbol, then the properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Blob {
    pub ports: Vec<(String, f32)>,
    pub properties: Vec<Property>,
}

impl Blob {
    pub fn encode(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        push_u32(&mut bytes, self.ports.len() as u32);
        for (symbol, value) in &self.ports {
            push_bytes(&mut bytes, symbol.as_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        push_u32(&mut bytes, self.properties.len() as u32);
        for p in &self.properties {
            push_bytes(&mut bytes, p.key.as_bytes());
            push_bytes(&mut bytes, p.type_.as_bytes());
            push_u32(&mut bytes, p.flags);
            push_bytes(&mut bytes, &p.value);
        }
        BASE64.encode(bytes)
    }

    pub fn decode(text: &str) -> Option<Blob> {
        let bytes = BASE64.decode(text).ok()?;
        let mut cursor = Cursor {
            bytes: &bytes,
            at: 0,
        };
        if cursor.take(4)? != MAGIC || cursor.take(1)? != [VERSION] {
            return None;
        }
        let mut ports = Vec::new();
        for _ in 0..cursor.u32()? {
            let symbol = String::from_utf8(cursor.bytes()?.to_vec()).ok()?;
            let value = f32::from_le_bytes(cursor.take(4)?.try_into().ok()?);
            ports.push((symbol, value));
        }
        let mut properties = Vec::new();
        for _ in 0..cursor.u32()? {
            let key = CString::new(cursor.bytes()?).ok()?;
            let type_ = CString::new(cursor.bytes()?).ok()?;
            let flags = cursor.u32()?;
            let value = cursor.bytes()?.to_vec();
            properties.push(Property {
                key,
                type_,
                flags,
                value,
            });
        }
        Some(Blob { ports, properties })
    }
}

fn push_u32(bytes: &mut Vec<u8>, n: u32) {
    bytes.extend_from_slice(&n.to_le_bytes());
}

fn push_bytes(bytes: &mut Vec<u8>, data: &[u8]) {
    push_u32(bytes, data.len() as u32);
    bytes.extend_from_slice(data);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&[u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

fn state_interface(descriptor: &LV2Descriptor) -> Option<&LV2_State_Interface> {
    let extension_data = descriptor.extension_data?;
    let interface = extension_data(URI_STATE_INTERFACE.as_ptr()).cast::<LV2_State_Interface>();
    // SAFETY: the plugin returns its static interface struct or NULL for this URI.
    unsafe { interface.as_ref() }
}

/// Whether the plugin implements the state interface at all.
pub fn has_state(descriptor: &LV2Descriptor) -> bool {
    state_interface(descriptor).is_some()
}

/// The plugin's properties right now. The state extension lets `save` run while the
/// plugin processes audio, so this is loop-thread work.
///
/// # Safety
/// `handle` must be the live instance behind `descriptor`.
pub unsafe fn save(
    descriptor: &LV2Descriptor,
    handle: *mut c_void,
    features: &[LV2Feature],
) -> Option<Vec<Property>> {
    let save = state_interface(descriptor)?.save?;
    let mut properties: Vec<Property> = Vec::new();
    let feature_ptrs = feature_pointers(features);
    // SAFETY: `store` writes only into `properties` through the handle, which outlives the
    // call; the plugin's own code runs in this chain's process.
    let status = unsafe {
        save(
            handle,
            Some(store),
            ptr::from_mut(&mut properties).cast(),
            IS_POD | IS_PORTABLE,
            feature_ptrs.as_ptr(),
        )
    };
    (status == STATUS_SUCCESS).then_some(properties)
}

unsafe extern "C" fn store(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    type_: u32,
    flags: u32,
) -> LV2_State_Status {
    if flags & IS_POD == 0 || value.is_null() {
        return STATUS_BAD_FLAGS;
    }
    let (Some(key), Some(type_)) = (unmap(key), unmap(type_)) else {
        return STATUS_UNKNOWN;
    };
    // SAFETY: `save` passed our Vec as the handle; the plugin hands `size` bytes at `value`.
    let (properties, bytes) = unsafe {
        (
            &mut *handle.cast::<Vec<Property>>(),
            std::slice::from_raw_parts(value.cast::<u8>(), size),
        )
    };
    properties.push(Property {
        key,
        type_,
        flags,
        value: bytes.to_vec(),
    });
    STATUS_SUCCESS
}

struct Mapped {
    key: u32,
    type_: u32,
    flags: u32,
    value: *const u8,
    size: usize,
}

/// Hands `properties` to the plugin's `restore`. Instantiation class: only before the
/// instance is activated.
///
/// # Safety
/// `handle` must be the live, not yet activated instance behind `descriptor`.
pub unsafe fn restore(
    descriptor: &LV2Descriptor,
    handle: *mut c_void,
    properties: &[Property],
    features: &[LV2Feature],
) -> bool {
    let Some(restore) = state_interface(descriptor).and_then(|i| i.restore) else {
        return false;
    };
    let mapped: Vec<Mapped> = properties
        .iter()
        .map(|p| Mapped {
            key: map(&p.key),
            type_: map(&p.type_),
            flags: p.flags,
            value: p.value.as_ptr(),
            size: p.value.len(),
        })
        .collect();
    let feature_ptrs = feature_pointers(features);
    // SAFETY: `retrieve` reads only `mapped`, whose values borrow `properties`, both alive
    // for the call; the plugin's own code runs in this chain's process.
    let status = unsafe {
        restore(
            handle,
            Some(retrieve),
            ptr::from_ref(&mapped).cast_mut().cast(),
            0,
            feature_ptrs.as_ptr(),
        )
    };
    status == STATUS_SUCCESS
}

unsafe extern "C" fn retrieve(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    type_: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    // SAFETY: `restore` passed our Vec as the handle.
    let mapped = unsafe { &*handle.cast::<Vec<Mapped>>() };
    let Some(found) = mapped.iter().find(|m| m.key == key) else {
        return ptr::null();
    };
    // SAFETY: the plugin passes valid out-pointers or NULL for each.
    unsafe {
        if let Some(size) = size.as_mut() {
            *size = found.size;
        }
        if let Some(type_) = type_.as_mut() {
            *type_ = found.type_;
        }
        if let Some(flags) = flags.as_mut() {
            *flags = found.flags;
        }
    }
    found.value.cast()
}

/// `lv2_raw` and `lv2-sys` spell the same C struct differently.
fn feature_pointers(features: &[LV2Feature]) -> Vec<*const lv2_sys::LV2_Feature> {
    features
        .iter()
        .map(|f| ptr::from_ref(f).cast::<lv2_sys::LV2_Feature>())
        .chain(std::iter::once(ptr::null()))
        .collect()
}

/// A control port value as lilv hands it back from a preset, as `f32`.
///
/// # Safety
/// `value` must point at `size` readable bytes.
pub unsafe fn port_value(value: *const c_void, size: u32, type_: u32) -> Option<f32> {
    let type_uri = unmap(type_)?;
    let size = size as usize;
    // SAFETY: lilv passes `size` bytes of the atom's body at `value`.
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), size) };
    match type_uri.to_bytes() {
        b"http://lv2plug.in/ns/ext/atom#Float" => {
            Some(f32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?))
        }
        b"http://lv2plug.in/ns/ext/atom#Double" => {
            Some(f64::from_ne_bytes(bytes.get(..8)?.try_into().ok()?) as f32)
        }
        b"http://lv2plug.in/ns/ext/atom#Int" | b"http://lv2plug.in/ns/ext/atom#Bool" => {
            Some(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?) as f32)
        }
        b"http://lv2plug.in/ns/ext/atom#Long" => {
            Some(i64::from_ne_bytes(bytes.get(..8)?.try_into().ok()?) as f32)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_round_trips_through_base64() {
        let blob = Blob {
            ports: vec![("g_in".into(), 0.5), ("enabled".into(), 1.0)],
            properties: vec![Property {
                key: c"urn:test:key".to_owned(),
                type_: c"http://lv2plug.in/ns/ext/atom#Chunk".to_owned(),
                flags: IS_POD | IS_PORTABLE,
                value: vec![1, 2, 3, 0, 255],
            }],
        };
        let text = blob.encode();
        assert_eq!(Blob::decode(&text), Some(blob));
        assert_eq!(Blob::decode("not base64 at all!"), None);
        assert_eq!(Blob::decode(&BASE64.encode(b"CLV2")), None);
    }
}
