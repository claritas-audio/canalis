//! lilv, opened at run time: a chain process starts without liblilv, and LV2 is there
//! once the library is installed. Only what `lv2`, `lv2ui` and the scan use, shaped like
//! the `lilv` crate it replaces. lilv is not thread-safe; everything but `ActiveInstance`
//! stays on the loop thread.

use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::ptr::NonNull;
use std::sync::{Arc, OnceLock};

use lv2_raw::core::{LV2Descriptor, LV2Feature, LV2Handle};
use lv2_raw::urid::LV2UridMap;

/// What to tell a user whose system lacks lilv.
pub const MISSING: &str = "LV2 plugins need lilv: on Debian or Ubuntu `sudo apt install \
     liblilv-0-0 libsuil-0-0`, on Fedora `sudo dnf install lilv-libs suil`; then rescan";

#[repr(C)]
pub(crate) struct LilvWorld {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvNode {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvPlugin {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvPlugins {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvPort {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvNodes {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvUi {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvUis {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvIter {
    _private: [u8; 0],
}
#[repr(C)]
pub(crate) struct LilvState {
    _private: [u8; 0],
}

/// `LilvInstanceImpl` from lilv.h: the instance functions are inline there, so they are
/// reimplemented here on these fields.
#[repr(C)]
pub(crate) struct InstanceImpl {
    descriptor: *const LV2Descriptor,
    handle: LV2Handle,
    pimpl: *mut c_void,
}

pub(crate) type UiSupportedFn = unsafe extern "C" fn(*const c_char, *const c_char) -> c_uint;
type SetPortValueFn = unsafe extern "C" fn(*const c_char, *mut c_void, *const c_void, u32, u32);

/// A struct of C functions from the first of `sonames` that opens, all or nothing.
macro_rules! dynamic_library {
    ($lib:ident [$($soname:literal),*] { $($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)* }) => {
        struct $lib {
            _library: libloading::Library,
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl $lib {
            fn open() -> Result<$lib, String> {
                let mut errors = Vec::new();
                for soname in [$($soname),*] {
                    // SAFETY: the audio libraries opened this way only set up their own
                    // statics when loaded.
                    match unsafe { libloading::Library::new(soname) } {
                        // SAFETY: each symbol is looked up by its C name and given the
                        // signature lilv.h declares for it.
                        Ok(library) => return unsafe {
                            Ok($lib {
                                $($name: *library
                                    .get(concat!(stringify!($name), "\0").as_bytes())
                                    .map_err(|e| e.to_string())?,)*
                                _library: library,
                            })
                        },
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                Err(errors.join("; "))
            }
        }
    };
}
pub(crate) use dynamic_library;

dynamic_library! { Lib ["liblilv-0.so.0", "liblilv-0.so"] {
    lilv_world_new: fn() -> *mut LilvWorld;
    lilv_world_free: fn(*mut LilvWorld);
    lilv_world_load_all: fn(*mut LilvWorld);
    lilv_world_set_option: fn(*mut LilvWorld, *const c_char, *const LilvNode);
    lilv_world_get_all_plugins: fn(*const LilvWorld) -> *const LilvPlugins;
    lilv_world_load_resource: fn(*mut LilvWorld, *const LilvNode) -> i32;
    lilv_world_get: fn(*mut LilvWorld, *const LilvNode, *const LilvNode, *const LilvNode) -> *mut LilvNode;
    lilv_new_uri: fn(*mut LilvWorld, *const c_char) -> *mut LilvNode;
    lilv_new_string: fn(*mut LilvWorld, *const c_char) -> *mut LilvNode;
    lilv_node_free: fn(*mut LilvNode);
    lilv_node_is_uri: fn(*const LilvNode) -> bool;
    lilv_node_as_uri: fn(*const LilvNode) -> *const c_char;
    lilv_node_as_string: fn(*const LilvNode) -> *const c_char;
    lilv_plugins_size: fn(*const LilvPlugins) -> c_uint;
    lilv_plugins_begin: fn(*const LilvPlugins) -> *mut LilvIter;
    lilv_plugins_next: fn(*const LilvPlugins, *mut LilvIter) -> *mut LilvIter;
    lilv_plugins_is_end: fn(*const LilvPlugins, *mut LilvIter) -> bool;
    lilv_plugins_get: fn(*const LilvPlugins, *mut LilvIter) -> *const LilvPlugin;
    lilv_plugins_get_by_uri: fn(*const LilvPlugins, *const LilvNode) -> *const LilvPlugin;
    lilv_plugin_get_uri: fn(*const LilvPlugin) -> *const LilvNode;
    lilv_plugin_get_name: fn(*const LilvPlugin) -> *mut LilvNode;
    lilv_plugin_get_author_name: fn(*const LilvPlugin) -> *mut LilvNode;
    lilv_plugin_get_bundle_uri: fn(*const LilvPlugin) -> *const LilvNode;
    lilv_plugin_get_num_ports: fn(*const LilvPlugin) -> u32;
    lilv_plugin_get_port_ranges_float: fn(*const LilvPlugin, *mut f32, *mut f32, *mut f32);
    lilv_plugin_get_port_by_index: fn(*const LilvPlugin, u32) -> *const LilvPort;
    lilv_plugin_has_latency: fn(*const LilvPlugin) -> bool;
    lilv_plugin_get_latency_port_index: fn(*const LilvPlugin) -> u32;
    lilv_plugin_instantiate: fn(*const LilvPlugin, f64, *const *const LV2Feature) -> *mut InstanceImpl;
    lilv_plugin_get_related: fn(*const LilvPlugin, *const LilvNode) -> *mut LilvNodes;
    lilv_plugin_get_uis: fn(*const LilvPlugin) -> *mut LilvUis;
    lilv_port_is_a: fn(*const LilvPlugin, *const LilvPort, *const LilvNode) -> bool;
    lilv_port_has_property: fn(*const LilvPlugin, *const LilvPort, *const LilvNode) -> bool;
    lilv_port_get_name: fn(*const LilvPlugin, *const LilvPort) -> *mut LilvNode;
    lilv_port_get_symbol: fn(*const LilvPlugin, *const LilvPort) -> *const LilvNode;
    lilv_nodes_begin: fn(*const LilvNodes) -> *mut LilvIter;
    lilv_nodes_next: fn(*const LilvNodes, *mut LilvIter) -> *mut LilvIter;
    lilv_nodes_is_end: fn(*const LilvNodes, *mut LilvIter) -> bool;
    lilv_nodes_get: fn(*const LilvNodes, *mut LilvIter) -> *const LilvNode;
    lilv_nodes_free: fn(*mut LilvNodes);
    lilv_uis_begin: fn(*const LilvUis) -> *mut LilvIter;
    lilv_uis_next: fn(*const LilvUis, *mut LilvIter) -> *mut LilvIter;
    lilv_uis_is_end: fn(*const LilvUis, *mut LilvIter) -> bool;
    lilv_uis_get: fn(*const LilvUis, *mut LilvIter) -> *const LilvUi;
    lilv_uis_free: fn(*mut LilvUis);
    lilv_ui_get_uri: fn(*const LilvUi) -> *const LilvNode;
    lilv_ui_get_bundle_uri: fn(*const LilvUi) -> *const LilvNode;
    lilv_ui_get_binary_uri: fn(*const LilvUi) -> *const LilvNode;
    lilv_ui_is_supported: fn(*const LilvUi, Option<UiSupportedFn>, *const LilvNode, *mut *const LilvNode) -> c_uint;
    lilv_instance_free: fn(*mut InstanceImpl);
    lilv_state_new_from_world: fn(*mut LilvWorld, *mut LV2UridMap, *const LilvNode) -> *mut LilvState;
    lilv_state_restore: fn(*const LilvState, *mut InstanceImpl, Option<SetPortValueFn>, *mut c_void, u32, *const *const LV2Feature);
    lilv_state_free: fn(*mut LilvState);
}}

fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| Lib::open().inspect_err(|e| log::warn!("lilv not loaded ({e}); LV2 is off")))
        .as_ref()
        .map_err(|_| MISSING.to_owned())
}

/// Whether lilv could be opened, so LV2 is available in this process.
pub fn available() -> bool {
    lib().is_ok()
}

/// A null-terminated array of pointers into `features`, as lilv and LV2 take them.
fn feature_pointers(features: &[LV2Feature]) -> Vec<*const LV2Feature> {
    features
        .iter()
        .map(std::ptr::from_ref)
        .chain(std::iter::once(std::ptr::null()))
        .collect()
}

struct WorldOwner {
    lib: &'static Lib,
    ptr: NonNull<LilvWorld>,
}

// SAFETY: lilv objects are only touched by one thread at a time (the chain's loop
// thread); the handles move with the chain's control state, never used concurrently.
unsafe impl Send for WorldOwner {}
// SAFETY: as above; shared references never reach two threads at once.
unsafe impl Sync for WorldOwner {}

impl Drop for WorldOwner {
    fn drop(&mut self) {
        // SAFETY: the world was made by `lilv_world_new` and nothing borrowed from it
        // outlives this owner (every borrow holds the `Arc`).
        unsafe { (self.lib.lilv_world_free)(self.ptr.as_ptr()) };
    }
}

/// Every plugin description lilv found.
#[derive(Clone)]
pub struct World {
    owner: Arc<WorldOwner>,
}

impl World {
    pub fn new() -> Result<World, String> {
        let lib = lib()?;
        // SAFETY: no arguments; a null world is an allocation failure.
        let ptr = NonNull::new(unsafe { (lib.lilv_world_new)() }).ok_or("lilv_world_new failed")?;
        Ok(World {
            owner: Arc::new(WorldOwner { lib, ptr }),
        })
    }

    pub fn with_load_all() -> Result<World, String> {
        let world = World::new()?;
        world.load_all();
        Ok(world)
    }

    fn lib(&self) -> &'static Lib {
        self.owner.lib
    }

    fn ptr(&self) -> *mut LilvWorld {
        self.owner.ptr.as_ptr()
    }

    pub fn load_all(&self) {
        // SAFETY: a live world.
        unsafe { (self.lib().lilv_world_load_all)(self.ptr()) }
    }

    pub fn set_option(&self, uri: &str, value: &Node) {
        let Ok(uri) = CString::new(uri) else { return };
        // SAFETY: a live world, a NUL-terminated URI and a live node; lilv copies both.
        unsafe { (self.lib().lilv_world_set_option)(self.ptr(), uri.as_ptr(), value.ptr) }
    }

    fn owned(&self, ptr: *mut LilvNode) -> Option<Node> {
        (!ptr.is_null()).then(|| Node {
            ptr,
            owned: true,
            world: Arc::clone(&self.owner),
        })
    }

    fn borrowed(&self, ptr: *const LilvNode) -> Option<Node> {
        (!ptr.is_null()).then(|| Node {
            ptr,
            owned: false,
            world: Arc::clone(&self.owner),
        })
    }

    /// A URI node; an empty one when `uri` has a NUL byte, which no URI has.
    pub fn new_uri(&self, uri: &str) -> Node {
        let uri = CString::new(uri).unwrap_or_default();
        // SAFETY: a live world and a NUL-terminated string lilv copies.
        let ptr = unsafe { (self.lib().lilv_new_uri)(self.ptr(), uri.as_ptr()) };
        self.owned(ptr).unwrap_or_else(|| self.null())
    }

    pub fn new_string(&self, text: &str) -> Node {
        let text = CString::new(text).unwrap_or_default();
        // SAFETY: as in `new_uri`.
        let ptr = unsafe { (self.lib().lilv_new_string)(self.ptr(), text.as_ptr()) };
        self.owned(ptr).unwrap_or_else(|| self.null())
    }

    /// A node lilv treats as a wildcard and that matches nothing.
    fn null(&self) -> Node {
        Node {
            ptr: std::ptr::null(),
            owned: false,
            world: Arc::clone(&self.owner),
        }
    }

    pub fn plugins(&self) -> Plugins {
        Plugins {
            // SAFETY: a live world; the collection is owned by it.
            ptr: unsafe { (self.lib().lilv_world_get_all_plugins)(self.ptr()) },
            world: self.clone(),
        }
    }

    pub fn load_resource(&self, node: &Node) -> i32 {
        // SAFETY: a live world and node.
        unsafe { (self.lib().lilv_world_load_resource)(self.ptr(), node.ptr) }
    }

    /// The first object of a matching statement; `None` parts are wildcards.
    pub fn get(
        &self,
        subject: Option<&Node>,
        predicate: Option<&Node>,
        object: Option<&Node>,
    ) -> Option<Node> {
        let p = |n: Option<&Node>| n.map_or(std::ptr::null(), |n| n.ptr);
        // SAFETY: a live world; null nodes are wildcards; the result is ours to free.
        let ptr =
            unsafe { (self.lib().lilv_world_get)(self.ptr(), p(subject), p(predicate), p(object)) };
        self.owned(ptr)
    }

    /// A state (a preset) described in the loaded data under `node`.
    pub fn new_state(&self, map: &mut LV2UridMap, node: &Node) -> Option<State> {
        // SAFETY: a live world, a map that outlives the call and a live node.
        let ptr = unsafe { (self.lib().lilv_state_new_from_world)(self.ptr(), map, node.ptr) };
        NonNull::new(ptr).map(|ptr| State {
            ptr,
            lib: self.lib(),
        })
    }
}

/// A node lilv made for us (freed on drop) or one it owns (borrowed).
pub struct Node {
    ptr: *const LilvNode,
    owned: bool,
    world: Arc<WorldOwner>,
}

impl Drop for Node {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: lilv handed this node to us to free.
            unsafe { (self.world.lib.lilv_node_free)(self.ptr.cast_mut()) };
        }
    }
}

impl Node {
    fn text(&self, f: unsafe extern "C" fn(*const LilvNode) -> *const c_char) -> Option<&str> {
        if self.ptr.is_null() {
            return None;
        }
        // SAFETY: a live node; lilv returns a string owned by the node, or null.
        let text = unsafe { f(self.ptr) };
        // SAFETY: non-null strings from lilv are NUL-terminated and live as the node does.
        (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_str().ok())?
    }

    pub fn as_uri(&self) -> Option<&str> {
        // SAFETY: a live node or null, checked first.
        if self.ptr.is_null() || !unsafe { (self.world.lib.lilv_node_is_uri)(self.ptr) } {
            return None;
        }
        self.text(self.world.lib.lilv_node_as_uri)
    }

    pub fn as_str(&self) -> Option<&str> {
        self.text(self.world.lib.lilv_node_as_string)
    }
}

/// The plugins of a world.
pub struct Plugins {
    ptr: *const LilvPlugins,
    world: World,
}

impl Plugins {
    pub fn count(&self) -> usize {
        // SAFETY: a collection owned by the live world.
        unsafe { (self.world.lib().lilv_plugins_size)(self.ptr) as usize }
    }

    pub fn iter(&self) -> impl Iterator<Item = Plugin> + '_ {
        let lib = self.world.lib();
        // SAFETY: iterating a collection owned by the live world.
        let mut iter = unsafe { (lib.lilv_plugins_begin)(self.ptr) };
        std::iter::from_fn(move || {
            // SAFETY: as above; `iter` came from this collection.
            unsafe {
                if (lib.lilv_plugins_is_end)(self.ptr, iter) {
                    return None;
                }
                let plugin = (lib.lilv_plugins_get)(self.ptr, iter);
                iter = (lib.lilv_plugins_next)(self.ptr, iter);
                Some(self.wrap(plugin))
            }
        })
    }

    pub fn plugin(&self, uri: &Node) -> Option<Plugin> {
        // SAFETY: a collection owned by the live world and a live node.
        let plugin = unsafe { (self.world.lib().lilv_plugins_get_by_uri)(self.ptr, uri.ptr) };
        (!plugin.is_null()).then(|| self.wrap(plugin))
    }

    fn wrap(&self, ptr: *const LilvPlugin) -> Plugin {
        Plugin {
            ptr,
            world: self.world.clone(),
        }
    }
}

/// One plugin description, owned by the world.
#[derive(Clone)]
pub struct Plugin {
    ptr: *const LilvPlugin,
    world: World,
}

// SAFETY: as for `WorldOwner`.
unsafe impl Send for Plugin {}

/// A control port's range.
#[derive(Clone, Copy, Debug)]
pub struct FloatRanges {
    pub default: f32,
    pub min: f32,
    pub max: f32,
}

impl Plugin {
    fn lib(&self) -> &'static Lib {
        self.world.lib()
    }

    pub fn uri(&self) -> Node {
        // SAFETY: a plugin of the live world; the node is the plugin's.
        let ptr = unsafe { (self.lib().lilv_plugin_get_uri)(self.ptr) };
        self.world
            .borrowed(ptr)
            .unwrap_or_else(|| self.world.null())
    }

    pub fn name(&self) -> Node {
        // SAFETY: as above; the name node is ours to free.
        let ptr = unsafe { (self.lib().lilv_plugin_get_name)(self.ptr) };
        self.world.owned(ptr).unwrap_or_else(|| self.world.null())
    }

    pub fn author_name(&self) -> Option<Node> {
        // SAFETY: as above.
        let ptr = unsafe { (self.lib().lilv_plugin_get_author_name)(self.ptr) };
        self.world.owned(ptr)
    }

    pub fn bundle_uri(&self) -> Node {
        // SAFETY: as in `uri`.
        let ptr = unsafe { (self.lib().lilv_plugin_get_bundle_uri)(self.ptr) };
        self.world
            .borrowed(ptr)
            .unwrap_or_else(|| self.world.null())
    }

    pub fn ports_count(&self) -> usize {
        // SAFETY: a plugin of the live world.
        unsafe { (self.lib().lilv_plugin_get_num_ports)(self.ptr) as usize }
    }

    pub fn port_ranges_float(&self) -> Vec<FloatRanges> {
        let n = self.ports_count();
        let (mut min, mut max, mut default) = (vec![0.0f32; n], vec![1.0f32; n], vec![0.0f32; n]);
        // SAFETY: three arrays of `num_ports` floats, as lilv writes.
        unsafe {
            (self.lib().lilv_plugin_get_port_ranges_float)(
                self.ptr,
                min.as_mut_ptr(),
                max.as_mut_ptr(),
                default.as_mut_ptr(),
            );
        }
        // lilv writes NaN where the plugin gives no value.
        (0..n)
            .map(|i| FloatRanges {
                min: if min[i].is_nan() { 0.0 } else { min[i] },
                max: if max[i].is_nan() { 1.0 } else { max[i] },
                default: if default[i].is_nan() { 0.0 } else { default[i] },
            })
            .collect()
    }

    pub fn port_by_index(&self, index: usize) -> Option<Port<'_>> {
        let index = u32::try_from(index).ok()?;
        // SAFETY: a plugin of the live world; out of range gives null.
        let ptr = unsafe { (self.lib().lilv_plugin_get_port_by_index)(self.ptr, index) };
        (!ptr.is_null()).then_some(Port { plugin: self, ptr })
    }

    pub fn latency_port_index(&self) -> Option<usize> {
        // SAFETY: a plugin of the live world; the index is only asked for when one exists.
        unsafe {
            (self.lib().lilv_plugin_has_latency)(self.ptr)
                .then(|| (self.lib().lilv_plugin_get_latency_port_index)(self.ptr) as usize)
        }
    }

    /// # Safety
    /// Runs the plugin's own code.
    pub unsafe fn instantiate(&self, rate: f64, features: &[LV2Feature]) -> Option<Instance> {
        let features = feature_pointers(features);
        // SAFETY: a plugin of the live world and a null-terminated feature array that
        // lives for the call; the plugin's code is the caller's risk.
        let ptr =
            unsafe { (self.lib().lilv_plugin_instantiate)(self.ptr, rate, features.as_ptr()) };
        NonNull::new(ptr).map(|ptr| Instance {
            ptr,
            lib: self.lib(),
        })
    }

    /// Resources related to the plugin (presets, with `class`).
    pub fn related(&self, class: Option<&Node>) -> Option<Nodes> {
        let class = class.map_or(std::ptr::null(), |c| c.ptr);
        // SAFETY: a plugin of the live world; the collection is ours to free.
        let ptr = unsafe { (self.lib().lilv_plugin_get_related)(self.ptr, class) };
        NonNull::new(ptr).map(|ptr| Nodes {
            ptr,
            world: self.world.clone(),
        })
    }

    pub fn uis(&self) -> Option<Uis> {
        // SAFETY: as in `related`.
        let ptr = unsafe { (self.lib().lilv_plugin_get_uis)(self.ptr) };
        NonNull::new(ptr).map(|ptr| Uis {
            ptr,
            world: self.world.clone(),
        })
    }
}

/// One port of a plugin.
pub struct Port<'a> {
    plugin: &'a Plugin,
    ptr: *const LilvPort,
}

impl Port<'_> {
    pub fn is_a(&self, class: &Node) -> bool {
        // SAFETY: a port of a plugin of the live world and a live node.
        unsafe { (self.plugin.lib().lilv_port_is_a)(self.plugin.ptr, self.ptr, class.ptr) }
    }

    pub fn has_property(&self, property: &Node) -> bool {
        // SAFETY: as in `is_a`.
        unsafe {
            (self.plugin.lib().lilv_port_has_property)(self.plugin.ptr, self.ptr, property.ptr)
        }
    }

    pub fn name(&self) -> Option<Node> {
        // SAFETY: as in `is_a`; the name node is ours to free.
        let ptr = unsafe { (self.plugin.lib().lilv_port_get_name)(self.plugin.ptr, self.ptr) };
        self.plugin.world.owned(ptr)
    }

    pub fn symbol(&self) -> Option<Node> {
        // SAFETY: as in `is_a`; the symbol node is the port's.
        let ptr = unsafe { (self.plugin.lib().lilv_port_get_symbol)(self.plugin.ptr, self.ptr) };
        self.plugin.world.borrowed(ptr)
    }
}

/// A collection of nodes lilv gave us.
pub struct Nodes {
    ptr: NonNull<LilvNodes>,
    world: World,
}

impl Drop for Nodes {
    fn drop(&mut self) {
        // SAFETY: the collection was handed to us to free.
        unsafe { (self.world.lib().lilv_nodes_free)(self.ptr.as_ptr()) };
    }
}

impl Nodes {
    /// The nodes, borrowed from the collection.
    pub fn iter(&self) -> impl Iterator<Item = Node> + '_ {
        let lib = self.world.lib();
        let nodes = self.ptr.as_ptr();
        // SAFETY: iterating a live collection.
        let mut iter = unsafe { (lib.lilv_nodes_begin)(nodes) };
        std::iter::from_fn(move || {
            // SAFETY: as above; `iter` came from this collection.
            unsafe {
                if (lib.lilv_nodes_is_end)(nodes, iter) {
                    return None;
                }
                let node = (lib.lilv_nodes_get)(nodes, iter);
                iter = (lib.lilv_nodes_next)(nodes, iter);
                self.world.borrowed(node)
            }
        })
    }
}

/// The UIs a plugin ships.
pub struct Uis {
    ptr: NonNull<LilvUis>,
    world: World,
}

impl Drop for Uis {
    fn drop(&mut self) {
        // SAFETY: the collection was handed to us to free.
        unsafe { (self.world.lib().lilv_uis_free)(self.ptr.as_ptr()) };
    }
}

impl Uis {
    pub fn iter(&self) -> impl Iterator<Item = Ui<'_>> + '_ {
        let lib = self.world.lib();
        let uis = self.ptr.as_ptr();
        // SAFETY: iterating a live collection.
        let mut iter = unsafe { (lib.lilv_uis_begin)(uis) };
        std::iter::from_fn(move || {
            // SAFETY: as above; `iter` came from this collection.
            unsafe {
                if (lib.lilv_uis_is_end)(uis, iter) {
                    return None;
                }
                let ui = (lib.lilv_uis_get)(uis, iter);
                iter = (lib.lilv_uis_next)(uis, iter);
                Some(Ui { ptr: ui, uis: self })
            }
        })
    }
}

/// One UI of a plugin, owned by its collection.
pub struct Ui<'a> {
    ptr: *const LilvUi,
    uis: &'a Uis,
}

impl Ui<'_> {
    fn node(&self, f: unsafe extern "C" fn(*const LilvUi) -> *const LilvNode) -> Option<Node> {
        // SAFETY: a UI of a live collection; the node is the UI's.
        self.uis.world.borrowed(unsafe { f(self.ptr) })
    }

    pub fn uri(&self) -> Option<Node> {
        self.node(self.uis.world.lib().lilv_ui_get_uri)
    }

    pub fn bundle_uri(&self) -> Option<Node> {
        self.node(self.uis.world.lib().lilv_ui_get_bundle_uri)
    }

    pub fn binary_uri(&self) -> Option<Node> {
        self.node(self.uis.world.lib().lilv_ui_get_binary_uri)
    }

    /// How well `supported` says this UI fits `container` (0: not at all), and the UI
    /// type that fits.
    pub fn is_supported(
        &self,
        supported: UiSupportedFn,
        container: &Node,
    ) -> (u32, Option<String>) {
        let mut ui_type: *const LilvNode = std::ptr::null();
        // SAFETY: a UI of a live collection, a C callback and a live node; lilv writes a
        // node it owns into `ui_type`.
        let quality = unsafe {
            (self.uis.world.lib().lilv_ui_is_supported)(
                self.ptr,
                Some(supported),
                container.ptr,
                &mut ui_type,
            )
        };
        let ui_type = self
            .uis
            .world
            .borrowed(ui_type)
            .and_then(|n| n.as_uri().map(str::to_owned));
        (quality, ui_type)
    }
}

/// An instantiated, not yet active plugin.
pub struct Instance {
    ptr: NonNull<InstanceImpl>,
    lib: &'static Lib,
}

// SAFETY: an instance is used by one thread at a time: built on the loop thread, run on
// the audio thread, returned to the loop thread to be freed.
unsafe impl Send for Instance {}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: made by `lilv_plugin_instantiate`, freed once.
        unsafe { (self.lib.lilv_instance_free)(self.ptr.as_ptr()) };
    }
}

impl Instance {
    fn descriptor_ref(&self) -> Option<&LV2Descriptor> {
        // SAFETY: lilv keeps the descriptor alive with the instance.
        unsafe { self.ptr.as_ref().descriptor.as_ref() }
    }

    pub fn handle(&self) -> LV2Handle {
        // SAFETY: a live instance.
        unsafe { self.ptr.as_ref().handle }
    }

    pub fn descriptor(&self) -> Option<&LV2Descriptor> {
        self.descriptor_ref()
    }

    /// # Safety
    /// `data` must stay valid, and large enough for the port, while the instance runs.
    pub unsafe fn connect_port_mut<T>(&mut self, index: usize, data: *mut T) {
        let (Some(descriptor), Ok(index)) = (self.descriptor_ref(), u32::try_from(index)) else {
            return;
        };
        // The plugin's own `connect_port` on its own handle; `data` is the caller's promise.
        (descriptor.connect_port)(self.handle(), index, data.cast());
    }

    /// # Safety
    /// Every port must be connected; runs the plugin's own code.
    pub unsafe fn activate(self) -> ActiveInstance {
        if let Some(activate) = self.descriptor_ref().and_then(|d| d.activate) {
            activate(self.handle());
        }
        ActiveInstance { inner: self }
    }
}

/// An activated plugin; the only lilv object the audio thread touches.
pub struct ActiveInstance {
    inner: Instance,
}

impl ActiveInstance {
    /// # Safety
    /// Every port's buffer must hold `frames`; runs the plugin's own code.
    pub unsafe fn run(&mut self, frames: usize) {
        if let Some(descriptor) = self.inner.descriptor_ref() {
            (descriptor.run)(self.inner.handle(), frames.min(u32::MAX as usize) as u32);
        }
    }

    /// Deactivates and frees the instance.
    ///
    /// # Safety
    /// Runs the plugin's own code; the audio thread must be done with the instance.
    pub unsafe fn deactivate(self) {
        if let Some(deactivate) = self.inner.descriptor_ref().and_then(|d| d.deactivate) {
            deactivate(self.inner.handle());
        }
    }
}

/// Receives the port values a state carries.
pub trait SetPortValue {
    fn set_port_value(&mut self, symbol: &str, value: *const c_void, size: u32, type_: u32);
}

unsafe extern "C" fn set_port_value(
    symbol: *const c_char,
    user: *mut c_void,
    value: *const c_void,
    size: u32,
    type_: u32,
) {
    if symbol.is_null() || user.is_null() {
        return;
    }
    // SAFETY: `user` is the `&mut &mut dyn SetPortValue` passed to `restore`, alive for
    // the call; lilv passes a NUL-terminated symbol.
    let (target, symbol) = unsafe {
        (
            &mut *user.cast::<&mut dyn SetPortValue>(),
            CStr::from_ptr(symbol),
        )
    };
    if let Ok(symbol) = symbol.to_str() {
        target.set_port_value(symbol, value, size, type_);
    }
}

/// A state (a preset) loaded from the world.
pub struct State {
    ptr: NonNull<LilvState>,
    lib: &'static Lib,
}

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: made by `lilv_state_new_from_world`, freed once.
        unsafe { (self.lib.lilv_state_free)(self.ptr.as_ptr()) };
    }
}

impl State {
    /// Restores into a not yet activated `instance`; port values go to `ports`.
    pub fn restore(
        &self,
        instance: &Instance,
        ports: Option<&mut dyn SetPortValue>,
        flags: u32,
        features: &[LV2Feature],
    ) {
        let features = feature_pointers(features);
        let callback: Option<SetPortValueFn> = ports.is_some().then_some(set_port_value);
        let mut ports = ports;
        let user = ports.as_mut().map_or(std::ptr::null_mut(), |p| {
            std::ptr::from_mut(p).cast::<c_void>()
        });
        // SAFETY: a live state and instance, a callback whose user data outlives the
        // call, and a null-terminated feature array.
        unsafe {
            (self.lib.lilv_state_restore)(
                self.ptr.as_ptr(),
                instance.ptr.as_ptr(),
                callback,
                user,
                flags,
                features.as_ptr(),
            );
        }
    }
}
