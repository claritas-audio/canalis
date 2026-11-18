//! LV2 plugins in a slot, hosted with lilv. The world and the plugin descriptions stay on
//! the loop thread; the callback gets an activated instance with its port buffers wired
//! once. Control ports are plain `f32`s the callback reads, so a parameter change is one
//! store. One URID map serves every plugin of the process.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::{info, warn};
use lv2_raw::core::{LV2Descriptor, LV2Feature};
use lv2_raw::urid::LV2UridMap;
use serde::{Deserialize, Serialize};

use crate::MAX_QUANTUM;
use crate::clap::{ParamInfo, PresetInfo, SlotInfo};
use crate::lilv::{self, ActiveInstance, Instance, Plugin, SetPortValue, World};
use crate::lv2state::{self, Blob};
use crate::x11::{WindowEvent, X11};

const URI_AUDIO_PORT: &str = "http://lv2plug.in/ns/lv2core#AudioPort";
const URI_CV_PORT: &str = "http://lv2plug.in/ns/lv2core#CVPort";
const URI_CONTROL_PORT: &str = "http://lv2plug.in/ns/lv2core#ControlPort";
const URI_INPUT_PORT: &str = "http://lv2plug.in/ns/lv2core#InputPort";
const URI_ATOM_PORT: &str = "http://lv2plug.in/ns/ext/atom#AtomPort";
const URI_TOGGLED: &str = "http://lv2plug.in/ns/lv2core#toggled";
const URI_INTEGER: &str = "http://lv2plug.in/ns/lv2core#integer";
const URI_ENUMERATION: &str = "http://lv2plug.in/ns/lv2core#enumeration";
const URI_SIDECHAIN: &str = "http://lv2plug.in/ns/lv2core#isSideChain";
const URI_ATOM_SEQUENCE: &CStr = c"http://lv2plug.in/ns/ext/atom#Sequence";
const URI_ATOM_CHUNK: &CStr = c"http://lv2plug.in/ns/ext/atom#Chunk";
const URI_URID_MAP: &CStr = c"http://lv2plug.in/ns/ext/urid#map";
const URI_URID_UNMAP: &CStr = c"http://lv2plug.in/ns/ext/urid#unmap";
const URI_OPTIONS: &CStr = c"http://lv2plug.in/ns/ext/options#options";
const URI_BOUNDED_BLOCK_LENGTH: &CStr = c"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
const URI_MIN_BLOCK_LENGTH: &CStr = c"http://lv2plug.in/ns/ext/buf-size#minBlockLength";
const URI_MAX_BLOCK_LENGTH: &CStr = c"http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
const URI_ATOM_INT: &CStr = c"http://lv2plug.in/ns/ext/atom#Int";
const URI_PRESET: &str = "http://lv2plug.in/ns/ext/presets#Preset";
const URI_RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// The state is saved this long after the last change, so a drag is one save.
const STATE_SETTLE: Duration = Duration::from_millis(400);
/// Bytes of every atom port buffer; LSP asks for 8 K minimum on its UI ports.
const ATOM_CAPACITY: usize = 16_384;

/// One LV2 plugin, as `canalis-chain --scan-lv2` prints it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lv2Descriptor {
    pub uri: String,
    pub name: String,
    pub vendor: String,
    pub bundle: String,
}

/// Every plugin lilv finds on `LV2_PATH` and the standard directories; parses TTL only.
/// An error when lilv is not installed.
pub fn scan() -> Result<Vec<Lv2Descriptor>, String> {
    let world = World::with_load_all()?;
    Ok(world
        .plugins()
        .iter()
        .map(|p| Lv2Descriptor {
            uri: p.uri().as_uri().unwrap_or("").to_owned(),
            name: p.name().as_str().unwrap_or("").to_owned(),
            vendor: p
                .author_name()
                .and_then(|n| n.as_str().map(str::to_owned))
                .unwrap_or_default(),
            bundle: p.bundle_uri().as_uri().map(file_path).unwrap_or_default(),
        })
        .filter(|d| !d.uri.is_empty())
        .collect())
}

/// The filesystem path of a `file://` URI from lilv, `%XX` escapes decoded: a bundle
/// named `Alt Denoiser.lv2` comes back as `Alt%20Denoiser.lv2`.
pub(crate) fn file_path(uri: &str) -> String {
    let encoded = uri.trim_start_matches("file://").as_bytes();
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut i = 0;
    while i < encoded.len() {
        let hex = encoded
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (encoded[i], hex) {
            (b'%', Some(byte)) => {
                bytes.push(byte);
                i += 3;
            }
            (byte, _) => {
                bytes.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

struct Urids {
    by_uri: HashMap<CString, u32>,
    /// URID 1 first; a URI's bytes never move once mapped, so `unmap_uri` can hand
    /// out pointers into them.
    by_id: Vec<CString>,
}

static URIDS: Mutex<Option<Urids>> = Mutex::new(None);

pub(crate) fn map(uri: &CStr) -> u32 {
    let mut guard = URIDS.lock().unwrap_or_else(|e| e.into_inner());
    let urids = guard.get_or_insert_with(|| Urids {
        by_uri: HashMap::new(),
        by_id: Vec::new(),
    });
    let next = urids.by_uri.len() as u32 + 1;
    *urids.by_uri.entry(uri.to_owned()).or_insert_with(|| {
        urids.by_id.push(uri.to_owned());
        next
    })
}

pub(crate) fn unmap(urid: u32) -> Option<CString> {
    let guard = URIDS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()?
        .by_id
        .get(urid.checked_sub(1)? as usize)
        .cloned()
}

extern "C" fn map_uri(_handle: *mut c_void, uri: *const c_char) -> u32 {
    if uri.is_null() {
        return 0;
    }
    // SAFETY: the plugin passes a NUL-terminated URI that lives for the call.
    map(unsafe { CStr::from_ptr(uri) })
}

unsafe extern "C" fn unmap_uri(_handle: *mut c_void, urid: u32) -> *const c_char {
    let guard = URIDS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .and_then(|u| u.by_id.get(urid.checked_sub(1)? as usize))
        .map_or(std::ptr::null(), |uri| uri.as_ptr())
}

/// Block lengths the options feature reports: `run` gets 1 to `MAX_QUANTUM` frames.
static MIN_BLOCK: i32 = 1;
static MAX_BLOCK: i32 = MAX_QUANTUM as i32;

/// The host features every instance gets; `Sync` because the pointers are to statics.
pub(crate) struct Features {
    _map: Box<LV2UridMap>,
    _unmap: Box<lv2_sys::LV2_URID_Unmap>,
    _options: Box<[lv2_sys::LV2_Options_Option; 3]>,
    pub(crate) list: Vec<LV2Feature>,
}

// SAFETY: the feature list points at boxed map and options that live as long as the process
// and at statics; nothing behind the pointers is mutated.
unsafe impl Sync for Features {}
// SAFETY: as above.
unsafe impl Send for Features {}

pub(crate) fn features() -> &'static Features {
    static FEATURES: OnceLock<Features> = OnceLock::new();
    FEATURES.get_or_init(|| {
        let map = Box::new(LV2UridMap {
            handle: std::ptr::null_mut(),
            map: map_uri,
        });
        let unmap = Box::new(lv2_sys::LV2_URID_Unmap {
            handle: std::ptr::null_mut(),
            unmap: Some(unmap_uri),
        });
        let int = self::map(URI_ATOM_INT);
        let block = |key: &CStr, value: &'static i32| lv2_sys::LV2_Options_Option {
            context: lv2_sys::LV2_Options_Context_LV2_OPTIONS_INSTANCE,
            subject: 0,
            key: self::map(key),
            size: size_of::<i32>() as u32,
            type_: int,
            value: value as *const i32 as *const c_void,
        };
        // Plugins built with JUCE or DPF refuse to instantiate without these.
        let options = Box::new([
            block(URI_MIN_BLOCK_LENGTH, &MIN_BLOCK),
            block(URI_MAX_BLOCK_LENGTH, &MAX_BLOCK),
            lv2_sys::LV2_Options_Option {
                context: lv2_sys::LV2_Options_Context_LV2_OPTIONS_INSTANCE,
                subject: 0,
                key: 0,
                size: 0,
                type_: 0,
                value: std::ptr::null(),
            },
        ]);
        let list = vec![
            LV2Feature {
                uri: URI_URID_MAP.as_ptr(),
                data: &*map as *const LV2UridMap as *mut c_void,
            },
            LV2Feature {
                uri: URI_URID_UNMAP.as_ptr(),
                data: &*unmap as *const lv2_sys::LV2_URID_Unmap as *mut c_void,
            },
            LV2Feature {
                uri: URI_OPTIONS.as_ptr(),
                data: options.as_ptr() as *mut c_void,
            },
            LV2Feature {
                uri: URI_BOUNDED_BLOCK_LENGTH.as_ptr(),
                data: std::ptr::null_mut(),
            },
        ];
        Features {
            _map: map,
            _unmap: unmap,
            _options: options,
            list,
        }
    })
}

/// The lilv world: every plugin description on the system, parsed once per process.
pub struct Lv2Host {
    world: World,
    /// Factory presets by plugin URI, listed once: loading them parses their bundles.
    presets: std::cell::RefCell<HashMap<String, Vec<PresetInfo>>>,
}

impl Lv2Host {
    /// The world over every installed bundle; an error when lilv is not installed.
    pub fn new() -> Result<Self, String> {
        let started = std::time::Instant::now();
        let world = World::with_load_all()?;
        info!(
            "LV2 world loaded: {} plugins in {:.0?}",
            world.plugins().count(),
            started.elapsed()
        );
        Ok(Lv2Host {
            world,
            presets: Default::default(),
        })
    }

    /// Whether this world has `uri`; a bundle installed after it was loaded is not in it.
    pub fn has(&self, uri: &str) -> bool {
        self.world
            .plugins()
            .plugin(&self.world.new_uri(uri))
            .is_some()
    }
}

#[cfg(test)]
impl Lv2Host {
    /// A world over `path` (colon-separated bundle directories) instead of `LV2_PATH`.
    fn with_path(path: &str) -> Self {
        let world = World::new().expect("lilv is installed where tests run");
        world.set_option(
            "http://drobilla.net/ns/lilv#lv2-path",
            &world.new_string(path),
        );
        world.load_all();
        Lv2Host {
            world,
            presets: Default::default(),
        }
    }
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

/// The loop-thread side of a loaded plugin.
pub struct Lv2Instance {
    plugin: Plugin,
    serial: u64,
    /// Written by the callback from the plugin's latency port, in samples.
    latency: Arc<AtomicU32>,
    /// The LV2 instance handle, for a UI's instance access and the state interface.
    handle: *mut c_void,
    descriptor: *const LV2Descriptor,
    has_state: bool,
    dirty_since: Option<Instant>,
    symbols: HashMap<String, u32>,
    control_values: Vec<f32>,
    control_inputs: Vec<bool>,
    ui: Option<crate::lv2ui::Lv2Ui>,
    window: Option<u32>,
    pub uri: String,
    pub slot: usize,
    pub info: SlotInfo,
    pub latency_samples: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PortKind {
    AudioIn,
    /// An audio input marked `lv2:isSideChain`: hears the chain's key.
    SideChainIn,
    AudioOut,
    ControlIn,
    ControlOut,
    AtomIn,
    AtomOut,
    Other,
}

/// The audio-thread side: an activated instance and the buffers its ports point into.
pub struct Lv2Processor {
    active: ActiveInstance,
    serial: u64,
    audio_in: Vec<Vec<f32>>,
    sidechain_in: Vec<Vec<f32>>,
    audio_out: Vec<Vec<f32>>,
    /// One value per plugin port; only control ports are read or written.
    controls: Vec<f32>,
    control_inputs: Vec<bool>,
    atoms: Vec<AtomBuffer>,
    latency_port: Option<usize>,
    latency: Arc<AtomicU32>,
    sequence_urid: u32,
    chunk_urid: u32,
}

struct AtomBuffer {
    data: Vec<u64>,
    input: bool,
}

/// `uri` from the world, activated for `rate`. Before activation the `preset` is
/// restored, then the `state` blob, then `saved` control values go on top.
pub fn load(
    host: &Lv2Host,
    uri: &str,
    saved: &BTreeMap<String, f64>,
    state: Option<&str>,
    preset: Option<&str>,
    rate: u32,
) -> Result<(Lv2Instance, Lv2Processor), String> {
    let world = &host.world;
    let plugin = world
        .plugins()
        .plugin(&world.new_uri(uri))
        .ok_or_else(|| format!("no LV2 plugin {uri} installed"))?;
    let name = plugin.name().as_str().unwrap_or(uri).to_owned();
    let count = plugin.ports_count();
    let ranges = plugin.port_ranges_float();
    let is = |uri: &str| world.new_uri(uri);
    let (audio, cv, control, input, atom) = (
        is(URI_AUDIO_PORT),
        is(URI_CV_PORT),
        is(URI_CONTROL_PORT),
        is(URI_INPUT_PORT),
        is(URI_ATOM_PORT),
    );
    let (toggled, integer, enumeration) = (is(URI_TOGGLED), is(URI_INTEGER), is(URI_ENUMERATION));
    let sidechain = is(URI_SIDECHAIN);

    let mut kinds = Vec::with_capacity(count);
    let mut params = Vec::new();
    let mut controls = vec![0.0f32; count];
    for (index, value_slot) in controls.iter_mut().enumerate() {
        let port = plugin
            .port_by_index(index)
            .ok_or_else(|| format!("{uri}: port {index} missing"))?;
        let is_input = port.is_a(&input);
        let kind = if port.is_a(&audio) || port.is_a(&cv) {
            if is_input && port.has_property(&sidechain) {
                PortKind::SideChainIn
            } else if is_input {
                PortKind::AudioIn
            } else {
                PortKind::AudioOut
            }
        } else if port.is_a(&control) {
            if is_input {
                PortKind::ControlIn
            } else {
                PortKind::ControlOut
            }
        } else if port.is_a(&atom) {
            if is_input {
                PortKind::AtomIn
            } else {
                PortKind::AtomOut
            }
        } else {
            PortKind::Other
        };
        kinds.push(kind);
        if matches!(kind, PortKind::ControlIn | PortKind::ControlOut) {
            let range = ranges.get(index).copied().unwrap_or(lilv::FloatRanges {
                default: 0.0,
                min: 0.0,
                max: 1.0,
            });
            *value_slot = range.default;
            if kind == PortKind::ControlIn {
                let value = saved
                    .get(&index.to_string())
                    .map_or(range.default, |v| *v as f32);
                *value_slot = value;
                params.push(ParamInfo {
                    id: index as u32,
                    name: port
                        .name()
                        .and_then(|n| n.as_str().map(str::to_owned))
                        .unwrap_or_else(|| format!("port {index}")),
                    module: String::new(),
                    min: f64::from(range.min),
                    max: f64::from(range.max),
                    default: f64::from(range.default),
                    value: f64::from(value),
                    stepped: port.has_property(&toggled)
                        || port.has_property(&integer)
                        || port.has_property(&enumeration),
                });
            }
        }
    }
    if kinds.contains(&PortKind::Other) {
        return Err(format!("{uri}: a port type this host does not provide"));
    }

    // SAFETY: instantiating runs the plugin's code; this process exists so that a
    // misbehaving plugin takes down only its chain.
    let mut instance = unsafe { plugin.instantiate(f64::from(rate.max(1)), &features().list) }
        .ok_or_else(|| format!("{uri}: instantiate failed"))?;
    let sequence_urid = map(URI_ATOM_SEQUENCE);
    let chunk_urid = map(URI_ATOM_CHUNK);
    let mut audio_in = Vec::new();
    let mut sidechain_in = Vec::new();
    let mut audio_out = Vec::new();
    let mut atoms = Vec::new();
    for (index, kind) in kinds.iter().enumerate() {
        match kind {
            PortKind::AudioIn => {
                audio_in.push(vec![0.0f32; MAX_QUANTUM]);
                let buffer = audio_in.last_mut().expect("just pushed");
                // SAFETY: the buffer is owned by the processor for the instance's whole life
                // and never reallocated.
                unsafe { instance.connect_port_mut(index, buffer.as_mut_ptr()) };
            }
            PortKind::SideChainIn => {
                sidechain_in.push(vec![0.0f32; MAX_QUANTUM]);
                let buffer = sidechain_in.last_mut().expect("just pushed");
                // SAFETY: as above.
                unsafe { instance.connect_port_mut(index, buffer.as_mut_ptr()) };
            }
            PortKind::AudioOut => {
                audio_out.push(vec![0.0f32; MAX_QUANTUM]);
                let buffer = audio_out.last_mut().expect("just pushed");
                // SAFETY: as above.
                unsafe { instance.connect_port_mut(index, buffer.as_mut_ptr()) };
            }
            PortKind::ControlIn | PortKind::ControlOut => {
                // SAFETY: `controls` keeps its allocation for the processor's life.
                unsafe { instance.connect_port_mut(index, controls.as_mut_ptr().add(index)) };
            }
            PortKind::AtomIn | PortKind::AtomOut => {
                let mut atom = AtomBuffer {
                    data: vec![0u64; ATOM_CAPACITY / 8],
                    input: *kind == PortKind::AtomIn,
                };
                atom.reset(sequence_urid, chunk_urid);
                atoms.push(atom);
                let buffer = atoms.last_mut().expect("just pushed");
                // SAFETY: as above; the buffer is 8-byte aligned as atoms require.
                unsafe { instance.connect_port_mut(index, buffer.data.as_mut_ptr()) };
            }
            PortKind::Other => {}
        }
    }
    let control_inputs: Vec<bool> = kinds.iter().map(|k| *k == PortKind::ControlIn).collect();
    let latency_port = plugin.latency_port_index();
    let symbols: HashMap<String, u32> = (0..count)
        .filter_map(|index| {
            let symbol = plugin.port_by_index(index)?.symbol()?;
            Some((symbol.as_str()?.to_owned(), index as u32))
        })
        .collect();
    let has_gui = crate::lv2ui::choose_ui(world, &plugin).is_some();
    let handle = instance.handle();
    let descriptor: *const LV2Descriptor = instance
        .descriptor()
        .map_or(std::ptr::null(), std::ptr::from_ref);
    // SAFETY: lilv keeps the descriptor alive with the instance.
    let has_state = unsafe { descriptor.as_ref() }.is_some_and(lv2state::has_state);
    let mut restored = false;
    if let Some(preset) = preset {
        let mut ports = PortValues {
            symbols: &symbols,
            control_inputs: &control_inputs,
            controls: &mut controls,
        };
        match apply_preset(world, &instance, preset, &mut ports) {
            Ok(()) => restored = true,
            Err(e) => warn!("{uri}: preset {preset}: {e}"),
        }
    }
    if let Some(state) = state {
        match Blob::decode(state) {
            Some(blob) => {
                for (symbol, value) in &blob.ports {
                    if let Some(&index) = symbols.get(symbol)
                        && control_inputs.get(index as usize).copied().unwrap_or(false)
                    {
                        controls[index as usize] = *value;
                    }
                }
                // SAFETY: as above, and `handle` is this instance; restore runs before
                // activation, as the extension asks.
                let taken = unsafe {
                    descriptor.as_ref().is_none_or(|descriptor| {
                        blob.properties.is_empty()
                            || lv2state::restore(
                                descriptor,
                                handle,
                                &blob.properties,
                                &features().list,
                            )
                    })
                };
                if !taken {
                    warn!("{uri}: the plugin did not take its saved state");
                }
            }
            None => warn!("{uri}: saved state is not a state blob"),
        }
    }
    for (key, value) in saved {
        if let Ok(index) = key.parse::<usize>()
            && control_inputs.get(index).copied().unwrap_or(false)
        {
            controls[index] = *value as f32;
        }
    }
    for param in &mut params {
        param.value = f64::from(controls[param.id as usize]);
    }
    let presets = host
        .presets
        .borrow_mut()
        .entry(uri.to_owned())
        .or_insert_with(|| list_presets(world, &plugin))
        .clone();
    // SAFETY: every port is connected above; activation is the plugin's own code.
    let active = unsafe { instance.activate() };
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let latency = Arc::new(AtomicU32::new(0));
    info!(
        "{name} ({uri}) loaded: {} controls, {} in, {} sidechain, {} out",
        params.len(),
        audio_in.len(),
        sidechain_in.len(),
        audio_out.len()
    );
    let has_sidechain = !sidechain_in.is_empty();
    Ok((
        Lv2Instance {
            plugin,
            serial,
            latency: Arc::clone(&latency),
            handle,
            descriptor,
            has_state,
            dirty_since: restored.then(Instant::now),
            symbols,
            control_values: controls.clone(),
            control_inputs: control_inputs.clone(),
            ui: None,
            window: None,
            uri: uri.to_owned(),
            slot: 0,
            info: SlotInfo {
                name,
                params,
                error: None,
                has_gui,
                presets,
                sidechain: has_sidechain,
            },
            latency_samples: 0,
        },
        Lv2Processor {
            active,
            serial,
            audio_in,
            sidechain_in,
            audio_out,
            controls,
            control_inputs,
            atoms,
            latency_port,
            latency,
            sequence_urid,
            chunk_urid,
        },
    ))
}

/// Writes the control port values a lilv state carries into the instance's controls.
struct PortValues<'a> {
    symbols: &'a HashMap<String, u32>,
    control_inputs: &'a [bool],
    controls: &'a mut [f32],
}

impl SetPortValue for PortValues<'_> {
    fn set_port_value(&mut self, symbol: &str, value: *const c_void, size: u32, type_: u32) {
        let Some(&index) = self.symbols.get(symbol) else {
            return;
        };
        if !self
            .control_inputs
            .get(index as usize)
            .copied()
            .unwrap_or(false)
        {
            return;
        }
        // SAFETY: lilv passes `size` bytes of the port's atom body.
        if let Some(value) = unsafe { lv2state::port_value(value, size, type_) } {
            self.controls[index as usize] = value;
        }
    }
}

/// Restores a `pset:Preset` resource into the not yet activated instance.
fn apply_preset(
    world: &World,
    instance: &Instance,
    preset: &str,
    ports: &mut PortValues<'_>,
) -> Result<(), String> {
    let node = world.new_uri(preset);
    let _ = world.load_resource(&node);
    let mut map = LV2UridMap {
        handle: std::ptr::null_mut(),
        map: map_uri,
    };
    let state = world
        .new_state(&mut map, &node)
        .ok_or("no such preset in the installed bundles")?;
    state.restore(instance, Some(ports), 0, &features().list);
    Ok(())
}

/// The factory presets a plugin ships, by label.
fn list_presets(world: &World, plugin: &Plugin) -> Vec<PresetInfo> {
    let class = world.new_uri(URI_PRESET);
    let label = world.new_uri(URI_RDFS_LABEL);
    let Some(related) = plugin.related(Some(&class)) else {
        return Vec::new();
    };
    let mut presets: Vec<PresetInfo> = related
        .iter()
        .filter_map(|node| {
            let _ = world.load_resource(&node);
            let uri = node.as_uri()?.to_owned();
            let label = world
                .get(Some(&node), Some(&label), None)
                .and_then(|n| n.as_str().map(str::to_owned))
                .unwrap_or_else(|| uri.rsplit(['/', '#']).next().unwrap_or(&uri).to_owned());
            Some(PresetInfo { uri, label })
        })
        .collect();
    presets.sort_by(|a, b| a.label.cmp(&b.label));
    presets
}

impl AtomBuffer {
    /// An empty sequence for an input, a chunk of the whole capacity for an output.
    fn reset(&mut self, sequence: u32, chunk: u32) {
        let header: &mut [u32] = bytemuck_cast(&mut self.data);
        if self.input {
            header[0] = 8;
            header[1] = sequence;
            header[2] = 0;
            header[3] = 0;
        } else {
            header[0] = (ATOM_CAPACITY - 8) as u32;
            header[1] = chunk;
        }
    }
}

/// The first four `u32`s of an 8-byte aligned buffer: the atom header and sequence body.
fn bytemuck_cast(data: &mut [u64]) -> &mut [u32] {
    // SAFETY: u64 and u32 are plain integers; the slice is at least two u64s long and
    // the view covers only its first 16 bytes.
    unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr().cast::<u32>(), 4) }
}

impl Lv2Instance {
    pub fn matches(&self, processor: &Lv2Processor) -> bool {
        self.serial == processor.serial
    }

    /// Deactivates the instance that came back from the callback.
    pub fn retire(mut self, processor: Lv2Processor, x11: Option<&X11>) {
        if let Some(x11) = x11 {
            self.close_ui(x11);
        }
        // SAFETY: the callback no longer touches the processor; deactivation is the plugin's code.
        unsafe { processor.active.deactivate() };
    }

    pub fn gui_open(&self) -> bool {
        self.window.is_some()
    }

    /// Our record of a control value, so a UI opened later starts from it.
    pub fn note_control(&mut self, id: u32, value: f64) {
        if let Some(slot) = self.control_values.get_mut(id as usize) {
            *slot = value as f32;
        }
        if let Some(ui) = &self.ui {
            ui.send_control(id, value as f32);
        }
        self.touch();
    }

    /// Something changed the plugin; its state is saved once the changes settle.
    pub fn touch(&mut self) {
        self.dirty_since = Some(Instant::now());
    }

    /// The state blob to persist once changes have settled; also refreshes the reported
    /// control values. Plugins without the state interface keep their `params` instead.
    pub fn tick_state(&mut self) -> Option<String> {
        if self.dirty_since.is_some_and(|t| t.elapsed() < STATE_SETTLE) {
            return None;
        }
        self.dirty_since.take()?;
        for param in &mut self.info.params {
            if let Some(value) = self.control_values.get(param.id as usize) {
                param.value = f64::from(*value);
            }
        }
        if !self.has_state {
            return None;
        }
        // SAFETY: lilv keeps the descriptor alive with the instance; the state
        // extension allows `save` while the plugin runs.
        let descriptor = unsafe { self.descriptor.as_ref() }?;
        // SAFETY: `handle` is the instance behind `descriptor`, alive until `retire`.
        let saved = unsafe { lv2state::save(descriptor, self.handle, &features().list) };
        let Some(properties) = saved else {
            warn!("{}: the plugin could not save its state", self.uri);
            return None;
        };
        let ports = self
            .symbols
            .iter()
            .filter(|(_, index)| self.control_inputs.get(**index as usize) == Some(&true))
            .filter_map(|(symbol, index)| {
                Some((symbol.clone(), *self.control_values.get(*index as usize)?))
            })
            .collect();
        Some(Blob { ports, properties }.encode())
    }

    /// The plugin's own UI, through suil, in a new top-level window titled `title`.
    pub fn open_ui(&mut self, host: &Lv2Host, x11: &X11, title: &str) -> Result<(), String> {
        if self.window.is_some() {
            return Ok(());
        }
        let choice =
            crate::lv2ui::choose_ui(&host.world, &self.plugin).ok_or("the plugin has no X11 UI")?;
        let window = x11.create_window(title, 600, 400)?;
        let ui = match crate::lv2ui::Lv2Ui::open(
            &choice,
            &self.uri,
            self.handle,
            window,
            self.symbols.clone(),
        ) {
            Ok(ui) => ui,
            Err(e) => {
                x11.close_window(window);
                return Err(e);
            }
        };
        for (index, value) in self.control_values.iter().enumerate() {
            if self.control_inputs.get(index).copied().unwrap_or(false) {
                ui.send_control(index as u32, *value);
            }
        }
        self.ui = Some(ui);
        self.window = Some(window);
        Ok(())
    }

    pub fn close_ui(&mut self, x11: &X11) {
        if self.ui.take().is_some() {
            // A UI can change state through atoms we never see; a close is the last chance.
            self.touch();
        }
        if let Some(window) = self.window.take() {
            x11.close_window(window);
        }
    }

    /// Runs the UI's idle work, applies its resize, and returns the controls it changed;
    /// `closed` is set when the UI went away.
    pub fn tick_ui(&mut self, x11: &X11) -> (Vec<(u32, f32)>, bool) {
        let Some(ui) = &self.ui else {
            return (Vec::new(), false);
        };
        if ui.idle() {
            self.close_ui(x11);
            return (Vec::new(), true);
        }
        if let (Some((w, h)), Some(window)) = (ui.take_resize(), self.window) {
            x11.resize(window, w, h);
        }
        let writes = ui.take_writes();
        for (id, value) in &writes {
            if let Some(slot) = self.control_values.get_mut(*id as usize) {
                *slot = *value;
            }
        }
        if !writes.is_empty() {
            self.touch();
        }
        (writes, false)
    }

    /// Reacts to our window: closing it closes the UI. `true` when that happened.
    pub fn on_window_event(&mut self, event: &WindowEvent, x11: &X11) -> bool {
        if let WindowEvent::Closed(window) = *event
            && Some(window) == self.window
        {
            self.close_ui(x11);
            return true;
        }
        false
    }

    /// Reads what the plugin reported as latency; `true` when it changed.
    pub fn tick(&mut self) -> bool {
        let now = self.latency.load(Ordering::Relaxed);
        let changed = now != self.latency_samples;
        self.latency_samples = now;
        changed
    }
}

impl Lv2Processor {
    /// What the plugin reported after its last run. Real-time.
    pub fn latency_samples(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    /// A control value; one store the plugin reads at its next run. Real-time.
    pub fn set_param(&mut self, id: u32, value: f64) {
        let index = id as usize;
        if self.control_inputs.get(index).copied().unwrap_or(false) {
            self.controls[index] = value as f32;
        }
    }

    /// Runs the plugin over `io` in place, with `key` (the chain's sidechain, one slice
    /// per channel, possibly empty) on its sidechain ports. Real-time: buffers were
    /// wired at load.
    pub fn process(&mut self, io: &mut [&mut [f32]], key: &[&[f32]]) {
        let Some(frames) = io.first().map(|c| c.len().min(MAX_QUANTUM)) else {
            return;
        };
        for (buffer, source) in self.audio_in.iter_mut().zip(io.iter()) {
            let n = frames.min(source.len());
            buffer[..n].copy_from_slice(&source[..n]);
        }
        for (c, buffer) in self.sidechain_in.iter_mut().enumerate() {
            match key.get(c).or(key.last()) {
                Some(source) => {
                    let n = frames.min(source.len());
                    buffer[..n].copy_from_slice(&source[..n]);
                    buffer[n..frames].fill(0.0);
                }
                None => buffer[..frames].fill(0.0),
            }
        }
        if self.audio_in.len() == 1 && io.len() > 1 {
            // A mono plugin hears the mid of a stereo chain.
            let (first, rest) = io.split_first().expect("io has channels");
            let n = frames.min(first.len());
            for (i, sample) in self.audio_in[0][..n].iter_mut().enumerate() {
                let mut sum = first[i];
                for channel in rest.iter() {
                    sum += channel.get(i).copied().unwrap_or(0.0);
                }
                *sample = sum / (1 + rest.len()) as f32;
            }
        }
        for atom in &mut self.atoms {
            atom.reset(self.sequence_urid, self.chunk_urid);
        }
        // SAFETY: every port is connected to a buffer of at least MAX_QUANTUM frames.
        unsafe { self.active.run(frames) };
        if let Some(port) = self.latency_port {
            let samples = self.controls.get(port).copied().unwrap_or(0.0);
            self.latency
                .store(samples.max(0.0) as u32, Ordering::Relaxed);
        }
        match self.audio_out.len() {
            0 => {}
            1 => {
                for target in io.iter_mut() {
                    let n = frames.min(target.len());
                    target[..n].copy_from_slice(&self.audio_out[0][..n]);
                }
            }
            _ => {
                for (buffer, target) in self.audio_out.iter().zip(io.iter_mut()) {
                    let n = frames.min(target.len());
                    target[..n].copy_from_slice(&buffer[..n]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSP_LIMITER: &str = "http://lsp-plug.in/plugins/lv2/limiter_stereo";

    #[test]
    fn a_bundle_installed_later_needs_a_new_world() {
        let dir = std::env::temp_dir().join(format!("canalis-late-lv2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let uri = "urn:canalis:test:late";
        let before = Lv2Host::with_path(dir.to_str().unwrap());
        assert!(!before.has(uri));
        std::fs::create_dir_all(dir.join("late.lv2")).unwrap();
        std::fs::write(
            dir.join("late.lv2/manifest.ttl"),
            format!("@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n<{uri}> a lv2:Plugin ; lv2:binary <late.so> .\n"),
        )
        .unwrap();
        assert!(!before.has(uri), "a loaded world does not look again");
        assert!(Lv2Host::with_path(dir.to_str().unwrap()).has(uri));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bundle_uris_become_paths_with_escapes_decoded() {
        assert_eq!(
            file_path("file:///home/u/.lv2/Alt%20Denoiser.lv2/"),
            "/home/u/.lv2/Alt Denoiser.lv2/"
        );
        assert_eq!(
            file_path("file:///usr/lib64/lv2/x42-eq.lv2/"),
            "/usr/lib64/lv2/x42-eq.lv2/"
        );
        assert_eq!(file_path("file:///a/%C3%A8.lv2"), "/a/è.lv2");
        assert_eq!(file_path("file:///a/100%"), "/a/100%", "a stray % stays");
    }

    #[test]
    fn urids_are_stable_and_start_at_one() {
        let a = map(c"urn:canalis:test:a");
        let b = map(c"urn:canalis:test:b");
        assert!(a >= 1 && b >= 1 && a != b);
        assert_eq!(map(c"urn:canalis:test:a"), a);
    }

    /// Runs only where the LSP LV2 bundle is installed.
    #[test]
    fn an_lsp_limiter_loads_and_passes_quiet_audio_through() {
        let _serial = crate::plugin_test_guard();
        let Ok(host) = Lv2Host::new() else { return };
        if host
            .world
            .plugins()
            .plugin(&host.world.new_uri(LSP_LIMITER))
            .is_none()
        {
            return;
        }
        let (mut instance, mut processor) =
            load(&host, LSP_LIMITER, &BTreeMap::new(), None, None, 48_000)
                .expect("the limiter loads");
        assert_eq!(instance.info.name, "LSP Limiter Stereo");
        assert!(instance.info.params.len() > 10);
        assert!(instance.matches(&processor));
        assert!(instance.info.has_gui, "LSP ships an X11 UI");
        let mut left: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
        let mut right = left.clone();
        for _ in 0..8 {
            processor.process(&mut [&mut left, &mut right], &[]);
        }
        assert!(left.iter().all(|s| s.is_finite()));
        instance.tick();
        instance.retire(processor, None);
    }

    /// Runs only where werman's RNNoise bundles are in an LV2 folder: built with JUCE,
    /// they need the options and boundedBlockLength features.
    #[test]
    fn rnnoise_loads_with_the_block_length_options() {
        let _serial = crate::plugin_test_guard();
        let Ok(host) = Lv2Host::new() else { return };
        for (uri, channels) in [
            (
                "https://github.com/werman/noise-suppression-for-voice#mono",
                1,
            ),
            (
                "https://github.com/werman/noise-suppression-for-voice#stereo",
                2,
            ),
        ] {
            if host
                .world
                .plugins()
                .plugin(&host.world.new_uri(uri))
                .is_none()
            {
                continue;
            }
            let (mut instance, mut processor) =
                load(&host, uri, &BTreeMap::new(), None, None, 48_000).expect("RNNoise loads");
            let mut io: Vec<Vec<f32>> = (0..channels)
                .map(|_| (0..480).map(|i| (i as f32 * 0.05).sin() * 0.1).collect())
                .collect();
            for _ in 0..8 {
                let mut refs: Vec<&mut [f32]> = io.iter_mut().map(Vec::as_mut_slice).collect();
                processor.process(&mut refs, &[]);
            }
            assert!(io.iter().flatten().all(|s| s.is_finite()));
            instance.tick();
            instance.retire(processor, None);
        }
    }

    const LSP_SC_COMPRESSOR: &str = "http://lsp-plug.in/plugins/lv2/sc_compressor_stereo";

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Runs only where the LSP LV2 bundle is installed: the sidechain compressor hears
    /// the key on its `lv2:isSideChain` ports and ducks the signal when the key is loud.
    #[test]
    fn a_sidechain_compressor_ducks_on_the_key() {
        let _serial = crate::plugin_test_guard();
        let Ok(host) = Lv2Host::new() else { return };
        if !limiter_installed(&host) {
            return;
        }
        let (instance, mut processor) = load(
            &host,
            LSP_SC_COMPRESSOR,
            &BTreeMap::new(),
            None,
            None,
            48_000,
        )
        .expect("the compressor loads");
        assert!(instance.info.sidechain, "sc_l and sc_r are sidechain ports");
        let tone = |amplitude: f32| -> Vec<f32> {
            (0..512)
                .map(|i| (i as f32 * 0.1).sin() * amplitude)
                .collect()
        };
        let run = |processor: &mut Lv2Processor, key: f32| -> f32 {
            let key_l = tone(key);
            let key_r = key_l.clone();
            let mut level = 0.0;
            for _ in 0..200 {
                let mut left = tone(0.1);
                let mut right = left.clone();
                processor.process(
                    &mut [&mut left, &mut right],
                    &[key_l.as_slice(), key_r.as_slice()],
                );
                level = rms(&left);
            }
            level
        };
        let quiet_key = run(&mut processor, 0.0);
        let loud_key = run(&mut processor, 0.9);
        assert!(
            loud_key < quiet_key * 0.5,
            "a loud key should duck: {quiet_key} -> {loud_key}"
        );
        instance.retire(processor, None);
    }

    /// The "Duck music when I talk" preset's values: a voice at −30 dBFS ducks the
    /// music, room noise at −65 dBFS leaves it alone.
    #[test]
    fn the_ducking_preset_ducks_on_a_voice_not_on_room_noise() {
        let _serial = crate::plugin_test_guard();
        let Ok(host) = Lv2Host::new() else { return };
        if !limiter_installed(&host) {
            return;
        }
        let db = |db: f64| 10f64.powf(db / 20.0);
        let params: BTreeMap<String, f64> = [
            (22, 2.0),
            (34, db(-45.0)),
            (36, db(-55.0)),
            (39, 8.0),
            (35, 20.0),
            (37, 400.0),
        ]
        .into_iter()
        .map(|(index, value): (u32, f64)| (index.to_string(), value))
        .collect();
        let (instance, mut processor) =
            load(&host, LSP_SC_COMPRESSOR, &params, None, None, 48_000).expect("loads");
        let tone = |amplitude: f32| -> Vec<f32> {
            (0..512)
                .map(|i| (i as f32 * 0.1).sin() * amplitude)
                .collect()
        };
        let mut run = |key_db: f32| -> f32 {
            // A sine's peak sits 3 dB above its level.
            let key_l = tone(10f32.powf(key_db / 20.0) * std::f32::consts::SQRT_2);
            let key_r = key_l.clone();
            let mut level = 0.0;
            for _ in 0..200 {
                let mut left = tone(0.1);
                let mut right = left.clone();
                processor.process(
                    &mut [&mut left, &mut right],
                    &[key_l.as_slice(), key_r.as_slice()],
                );
                level = rms(&left);
            }
            level
        };
        let noise = run(-65.0);
        let voice = run(-30.0);
        let open = rms(&tone(0.1));
        assert!(
            noise > open * 0.9,
            "room noise must not duck: {open} -> {noise}"
        );
        assert!(voice < open * 0.5, "a voice should duck: {open} -> {voice}");
        instance.retire(processor, None);
    }

    fn limiter_installed(host: &Lv2Host) -> bool {
        host.world
            .plugins()
            .plugin(&host.world.new_uri(LSP_LIMITER))
            .is_some()
    }

    fn param_value(instance: &Lv2Instance, symbol: &str) -> f64 {
        let id = instance.symbols[symbol];
        instance
            .info
            .params
            .iter()
            .find(|p| p.id == id)
            .expect("a control")
            .value
    }

    /// Runs only where the LSP LV2 bundle is installed.
    #[test]
    fn an_lsp_limiter_saves_its_state_and_takes_it_back() {
        let _serial = crate::plugin_test_guard();
        let Ok(host) = Lv2Host::new() else { return };
        if !limiter_installed(&host) {
            return;
        }
        let (mut instance, processor) =
            load(&host, LSP_LIMITER, &BTreeMap::new(), None, None, 48_000).expect("loads");
        assert!(instance.has_state, "LSP implements state:interface");
        assert_eq!(instance.tick_state(), None, "nothing changed yet");
        let g_in = instance.symbols["g_in"];
        instance.note_control(g_in, 0.25);
        assert_eq!(instance.tick_state(), None, "still settling");
        std::thread::sleep(STATE_SETTLE + Duration::from_millis(50));
        let state = instance.tick_state().expect("a state once settled");
        assert_eq!(param_value(&instance, "g_in"), 0.25);
        let blob = Blob::decode(&state).expect("our own blob");
        assert!(blob.ports.contains(&("g_in".to_owned(), 0.25)));
        instance.retire(processor, None);

        let (instance, processor) = load(
            &host,
            LSP_LIMITER,
            &BTreeMap::new(),
            Some(&state),
            None,
            48_000,
        )
        .expect("loads with its state");
        assert_eq!(param_value(&instance, "g_in"), 0.25);
        instance.retire(processor, None);

        let (instance, processor) = load(
            &host,
            LSP_LIMITER,
            &[(g_in.to_string(), 0.5)].into_iter().collect(),
            Some(&state),
            None,
            48_000,
        )
        .expect("loads with params over the state");
        assert_eq!(
            param_value(&instance, "g_in"),
            0.5,
            "params win over the state"
        );
        instance.retire(processor, None);
    }

    /// Runs only where the LSP LV2 bundle is installed: a preset bundle written next to
    /// the test is listed with its label and restored at load.
    #[test]
    fn a_factory_preset_is_listed_and_restored() {
        let _serial = crate::plugin_test_guard();
        if !Lv2Host::new().is_ok_and(|h| limiter_installed(&h)) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("canalis-preset-test-{}", std::process::id()));
        let bundle = dir.join("quiet.lv2");
        std::fs::create_dir_all(&bundle).expect("a bundle dir");
        let prefixes = "@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n\
            @prefix pset: <http://lv2plug.in/ns/ext/presets#> .\n\
            @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n";
        std::fs::write(
            bundle.join("manifest.ttl"),
            format!(
                "{prefixes}<urn:canalis:test:quiet> a pset:Preset ; lv2:appliesTo <{LSP_LIMITER}> ; \
                 rdfs:seeAlso <quiet.ttl> .\n"
            ),
        )
        .expect("manifest written");
        std::fs::write(
            bundle.join("quiet.ttl"),
            format!(
                "{prefixes}<urn:canalis:test:quiet> a pset:Preset ; lv2:appliesTo <{LSP_LIMITER}> ; \
                 rdfs:label \"Quiet\" ; lv2:port [ lv2:symbol \"g_in\" ; pset:value 0.125 ] .\n"
            ),
        )
        .expect("preset written");
        let path = format!(
            "{}:/usr/lib64/lv2:/usr/lib/lv2:{}/.lv2",
            dir.display(),
            std::env::var("HOME").unwrap_or_default()
        );
        let host = Lv2Host::with_path(&path);
        let (mut instance, processor) = load(
            &host,
            LSP_LIMITER,
            &BTreeMap::new(),
            None,
            Some("urn:canalis:test:quiet"),
            48_000,
        )
        .expect("loads with the preset");
        assert_eq!(param_value(&instance, "g_in"), 0.125);
        assert!(
            instance.info.presets.contains(&PresetInfo {
                uri: "urn:canalis:test:quiet".into(),
                label: "Quiet".into(),
            }),
            "{:?}",
            instance.info.presets
        );
        std::thread::sleep(STATE_SETTLE + Duration::from_millis(50));
        let state = instance
            .tick_state()
            .expect("a preset load is saved as state");
        let blob = Blob::decode(&state).expect("our own blob");
        assert!(blob.ports.contains(&("g_in".to_owned(), 0.125)));
        instance.retire(processor, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
