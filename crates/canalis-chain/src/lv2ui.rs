//! LV2 plugin UIs through suil, embedded in the same X11 windows CLAP GUIs use. The UI
//! runs on the loop thread (its idle interface is called from the tick); what it writes
//! to control ports is queued and applied by the control side.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;

use std::sync::OnceLock;

use lv2_raw::core::LV2Feature;

use crate::lilv::{Plugin, Ui, World, dynamic_library};

use crate::lv2::features;

const URI_X11UI: &CStr = c"http://lv2plug.in/ns/extensions/ui#X11UI";
const URI_PARENT: &CStr = c"http://lv2plug.in/ns/extensions/ui#parent";
const URI_RESIZE: &CStr = c"http://lv2plug.in/ns/extensions/ui#resize";
const URI_IDLE: &CStr = c"http://lv2plug.in/ns/extensions/ui#idleInterface";
const URI_INSTANCE_ACCESS: &CStr = c"http://lv2plug.in/ns/ext/instance-access";
/// Control port values travel as plain floats: protocol 0.
const PROTOCOL_FLOAT: u32 = 0;

#[repr(C)]
struct SuilHost {
    _private: [u8; 0],
}

#[repr(C)]
struct SuilInstance {
    _private: [u8; 0],
}

type PortWrite = extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);
type PortIndex = extern "C" fn(*mut c_void, *const c_char) -> u32;
type PortSubscribe = extern "C" fn(*mut c_void, u32, u32, *const *const LV2Feature) -> u32;

dynamic_library! { Suil ["libsuil-0.so.0", "libsuil-0.so"] {
    suil_ui_supported: fn(*const c_char, *const c_char) -> u32;
    suil_host_new: fn(PortWrite, PortIndex, PortSubscribe, PortSubscribe) -> *mut SuilHost;
    suil_host_free: fn(*mut SuilHost);
    suil_instance_new: fn(*mut SuilHost, *mut c_void, *const c_char, *const c_char, *const c_char, *const c_char, *const c_char, *const c_char, *const *const LV2Feature) -> *mut SuilInstance;
    suil_instance_free: fn(*mut SuilInstance);
    suil_instance_port_event: fn(*mut SuilInstance, u32, u32, u32, *const c_void);
    suil_instance_extension_data: fn(*mut SuilInstance, *const c_char) -> *const c_void;
    suil_instance_get_handle: fn(*mut SuilInstance) -> *mut c_void;
}}

/// suil, opened on first use: without it LV2 plugins run but show no UI.
fn suil() -> Option<&'static Suil> {
    static SUIL: OnceLock<Option<Suil>> = OnceLock::new();
    SUIL.get_or_init(|| {
        Suil::open()
            .inspect_err(|e| log::warn!("suil not loaded ({e}); LV2 plugin windows are off"))
            .ok()
    })
    .as_ref()
}

#[repr(C)]
struct Lv2UiResize {
    handle: *mut c_void,
    ui_resize: extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
}

#[repr(C)]
struct Lv2UiIdleInterface {
    idle: Option<extern "C" fn(*mut c_void) -> c_int>,
}

/// What the UI hands back to us, through C callbacks that only get this pointer.
struct Controller {
    writes: Mutex<Vec<(u32, f32)>>,
    resize: Mutex<Option<(u32, u32)>>,
    symbols: HashMap<String, u32>,
}

extern "C" fn write_port(
    controller: *mut c_void,
    port_index: u32,
    buffer_size: u32,
    protocol: u32,
    buffer: *const c_void,
) {
    if protocol != PROTOCOL_FLOAT || buffer_size < 4 || buffer.is_null() || controller.is_null() {
        return;
    }
    // SAFETY: suil passes back the `Controller` we gave it, which outlives the instance;
    // a float protocol buffer holds one f32.
    let (controller, value) =
        unsafe { (&*(controller as *const Controller), *(buffer as *const f32)) };
    if let Ok(mut writes) = controller.writes.lock() {
        writes.push((port_index, value));
    }
}

extern "C" fn port_index(controller: *mut c_void, symbol: *const c_char) -> u32 {
    if controller.is_null() || symbol.is_null() {
        return u32::MAX;
    }
    // SAFETY: as in `write_port`; the symbol is a NUL-terminated string for the call.
    let (controller, symbol) =
        unsafe { (&*(controller as *const Controller), CStr::from_ptr(symbol)) };
    symbol
        .to_str()
        .ok()
        .and_then(|s| controller.symbols.get(s).copied())
        .unwrap_or(u32::MAX)
}

extern "C" fn port_subscribe(
    _controller: *mut c_void,
    _port_index: u32,
    _protocol: u32,
    _features: *const *const LV2Feature,
) -> u32 {
    1
}

extern "C" fn ui_resize(handle: *mut c_void, width: c_int, height: c_int) -> c_int {
    if handle.is_null() {
        return 1;
    }
    // SAFETY: the resize feature's handle is our `Controller`.
    let controller = unsafe { &*(handle as *const Controller) };
    if let Ok(mut resize) = controller.resize.lock() {
        *resize = Some((width.max(1) as u32, height.max(1) as u32));
    }
    0
}

/// A UI the plugin ships that suil can show in an X11 window, with how well it fits.
pub struct UiChoice {
    uri: String,
    ui_type: String,
    bundle: String,
    binary: String,
}

/// The best X11-capable UI of `plugin`, if any.
pub fn choose_ui(world: &World, plugin: &Plugin) -> Option<UiChoice> {
    let suil = suil()?;
    let container = world.new_uri(URI_X11UI.to_str().ok()?);
    let uis = plugin.uis()?;
    let mut best: Option<(u32, UiChoice)> = None;
    for ui in uis.iter() {
        let (quality, ui_type) = ui.is_supported(suil.suil_ui_supported, &container);
        if quality == 0 {
            continue;
        }
        let Some(choice) = describe(&ui, ui_type) else {
            continue;
        };
        if best.as_ref().is_none_or(|(q, _)| quality > *q) {
            best = Some((quality, choice));
        }
    }
    best.map(|(_, c)| c)
}

fn describe(ui: &Ui<'_>, ui_type: Option<String>) -> Option<UiChoice> {
    let path = |node: Option<crate::lilv::Node>| -> Option<String> {
        node?.as_uri().map(crate::lv2::file_path)
    };
    Some(UiChoice {
        uri: ui.uri()?.as_uri()?.to_owned(),
        ui_type: ui_type.unwrap_or_else(|| URI_X11UI.to_str().unwrap_or_default().to_owned()),
        bundle: path(ui.bundle_uri())?,
        binary: path(ui.binary_uri())?,
    })
}

/// A UI instance living in our X11 window.
pub struct Lv2Ui {
    suil: &'static Suil,
    host: *mut SuilHost,
    instance: *mut SuilInstance,
    controller: Box<Controller>,
    _resize: Box<Lv2UiResize>,
    _features: Vec<LV2Feature>,
    _strings: Vec<CString>,
}

impl Lv2Ui {
    /// Instantiates `choice` for `plugin_uri` inside `window`, with the plugin's `handle`
    /// for instance access and the port `symbols` the UI may ask about.
    pub fn open(
        choice: &UiChoice,
        plugin_uri: &str,
        handle: *mut c_void,
        window: u32,
        symbols: HashMap<String, u32>,
    ) -> Result<Self, String> {
        let suil = suil().ok_or("LV2 plugin windows need suil (libsuil-0-0)")?;
        let controller = Box::new(Controller {
            writes: Mutex::new(Vec::new()),
            resize: Mutex::new(None),
            symbols,
        });
        let controller_ptr = &*controller as *const Controller as *mut c_void;
        let resize = Box::new(Lv2UiResize {
            handle: controller_ptr,
            ui_resize,
        });
        let mut features: Vec<LV2Feature> = features()
            .list
            .iter()
            .map(|f| LV2Feature {
                uri: f.uri,
                data: f.data,
            })
            .collect();
        features.push(LV2Feature {
            uri: URI_PARENT.as_ptr(),
            data: window as usize as *mut c_void,
        });
        features.push(LV2Feature {
            uri: URI_RESIZE.as_ptr(),
            data: &*resize as *const Lv2UiResize as *mut c_void,
        });
        features.push(LV2Feature {
            uri: URI_INSTANCE_ACCESS.as_ptr(),
            data: handle,
        });
        features.push(LV2Feature {
            uri: URI_IDLE.as_ptr(),
            data: std::ptr::null_mut(),
        });
        let mut pointers: Vec<*const LV2Feature> = features.iter().map(|f| f as *const _).collect();
        pointers.push(std::ptr::null());
        let strings: Vec<CString> = [
            plugin_uri,
            &choice.uri,
            &choice.ui_type,
            &choice.bundle,
            &choice.binary,
        ]
        .iter()
        .map(|s| CString::new(*s).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
        // SAFETY: the callbacks match suil's signatures and only read our controller.
        let host =
            unsafe { (suil.suil_host_new)(write_port, port_index, port_subscribe, port_subscribe) };
        if host.is_null() {
            return Err("suil host".into());
        }
        // SAFETY: every pointer is to data this struct keeps alive; the feature array is
        // NULL-terminated; instantiating runs the UI's code in this process on purpose.
        let instance = unsafe {
            (suil.suil_instance_new)(
                host,
                controller_ptr,
                URI_X11UI.as_ptr(),
                strings[0].as_ptr(),
                strings[1].as_ptr(),
                strings[2].as_ptr(),
                strings[3].as_ptr(),
                strings[4].as_ptr(),
                pointers.as_ptr(),
            )
        };
        if instance.is_null() {
            // SAFETY: the host was created above and has no instances.
            unsafe { (suil.suil_host_free)(host) };
            return Err(format!("the UI {} did not instantiate", choice.uri));
        }
        Ok(Lv2Ui {
            suil,
            host,
            instance,
            controller,
            _resize: resize,
            _features: features,
            _strings: strings,
        })
    }

    /// Tells the UI a control port's value.
    pub fn send_control(&self, port: u32, value: f32) {
        // SAFETY: a live instance; the buffer is one f32 for the float protocol.
        unsafe {
            (self.suil.suil_instance_port_event)(
                self.instance,
                port,
                4,
                PROTOCOL_FLOAT,
                &value as *const f32 as *const c_void,
            );
        }
    }

    /// Runs the UI's idle work; `true` when the UI says it closed itself.
    pub fn idle(&self) -> bool {
        // SAFETY: a live instance; the returned struct is the UI's static interface.
        let interface =
            unsafe { (self.suil.suil_instance_extension_data)(self.instance, URI_IDLE.as_ptr()) }
                as *const Lv2UiIdleInterface;
        if interface.is_null() {
            return false;
        }
        // SAFETY: as above.
        let Some(idle) = (unsafe { (*interface).idle }) else {
            return false;
        };
        // SAFETY: the handle is the UI's own, from the same instance.
        let handle = unsafe { (self.suil.suil_instance_get_handle)(self.instance) };
        idle(handle) != 0
    }

    /// Control values the UI changed since the last call.
    pub fn take_writes(&self) -> Vec<(u32, f32)> {
        self.controller
            .writes
            .lock()
            .map(|mut w| std::mem::take(&mut *w))
            .unwrap_or_default()
    }

    /// The size the UI asked for, once.
    pub fn take_resize(&self) -> Option<(u32, u32)> {
        self.controller
            .resize
            .lock()
            .ok()
            .and_then(|mut r| r.take())
    }
}

impl Drop for Lv2Ui {
    fn drop(&mut self) {
        // SAFETY: the instance and host were created in `open` and are freed once, here.
        unsafe {
            (self.suil.suil_instance_free)(self.instance);
            (self.suil.suil_host_free)(self.host);
        }
    }
}
