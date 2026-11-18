//! CLAP plugins in a slot, hosted with clack. The plugin instance stays on the loop
//! thread; only the audio processor with its preallocated port and event buffers goes
//! into the callback, and comes back here to be deactivated.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window,
};
use clack_extensions::latency::{HostLatency, HostLatencyImpl, PluginLatency};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamInfoFlags, ParamRescanFlags, PluginParams,
};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, InputChannel};
use clack_host::utils::ClapId;
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::MAX_QUANTUM;
use crate::x11::{WindowEvent, X11};

/// A plugin's state is saved this long after its last change, so a drag is one save.
const STATE_SETTLE: Duration = Duration::from_millis(400);

/// Parameter changes queued between two process calls; more are dropped.
const MAX_PENDING_EVENTS: usize = 256;

/// What a loaded plugin tells the control plane about itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotInfo {
    pub name: String,
    #[serde(default)]
    pub params: Vec<ParamInfo>,
    #[serde(default)]
    pub error: Option<String>,
    /// The plugin can show its own window (CLAP gui extension, X11, embedded).
    #[serde(default)]
    pub has_gui: bool,
    /// Factory presets the slot can recall (LV2 `pset:Preset` resources today).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<PresetInfo>,
    /// The plugin has a sidechain input the chain's key feeds.
    #[serde(default)]
    pub sidechain: bool,
}

/// A preset the plugin ships, by URI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    pub uri: String,
    pub label: String,
}

/// One plugin inside a bundle, as `canalis-chain --scan` prints it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub id: String,
    pub name: String,
    pub vendor: String,
}

/// Loads a bundle only to list what is inside; meant for a throwaway process.
pub fn scan(path: &str) -> Result<Vec<PluginDescriptor>, String> {
    // SAFETY: as in `load`; the caller runs this in a process that exists for the scan.
    let entry = unsafe { PluginEntry::load(path) }.map_err(|e| format!("cannot load: {e}"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or("no plugin factory in the bundle")?;
    Ok(factory
        .plugin_descriptors()
        .filter_map(|d| {
            let id = lossy(d.id()?.to_bytes());
            Some(PluginDescriptor {
                name: d.name().map_or_else(|| id.clone(), |n| lossy(n.to_bytes())),
                vendor: d.vendor().map_or_else(String::new, |v| lossy(v.to_bytes())),
                id,
            })
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    /// The plugin's grouping path, e.g. `Band 1`; empty when it has none.
    #[serde(default)]
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub value: f64,
    #[serde(default)]
    pub stepped: bool,
}

#[derive(Default)]
struct Shared {
    restart: AtomicBool,
    callback: AtomicBool,
    /// Packed `GuiSize` the plugin asked its window to take; 0 when none.
    gui_resize: AtomicU64,
    gui_show: AtomicBool,
    gui_hide: AtomicBool,
    gui_closed: AtomicBool,
    /// The plugin sent a parameter value out of `process`, typically from its own window;
    /// CLAP counts that as a dirty state even without `mark_dirty`.
    params_changed: AtomicBool,
}

impl<'a> SharedHandler<'a> for Shared {
    fn request_restart(&self) {
        self.restart.store(true, Ordering::Relaxed);
    }

    fn request_process(&self) {}

    fn request_callback(&self) {
        self.callback.store(true, Ordering::Relaxed);
    }
}

impl HostLogImpl for Shared {
    // A plugin that logs from its audio thread breaks its own real-time budget, not ours to fix.
    fn log(&self, severity: LogSeverity, message: &str) {
        match severity {
            LogSeverity::Debug | LogSeverity::Info => info!("plugin: {message}"),
            _ => warn!("plugin: {message}"),
        }
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {}
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        self.gui_resize
            .store(new_size.pack_to_u64().max(1), Ordering::Relaxed);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        self.gui_show.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        self.gui_hide.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.gui_closed.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct MainThread {
    latency_changed: AtomicBool,
    /// The plugin says its state changed (CLAP `state.mark_dirty`).
    dirty: AtomicBool,
}

impl HostStateImpl for MainThread {
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }
}

impl<'a> MainThreadHandler<'a> for MainThread {}

impl HostLatencyImpl for MainThread {
    fn changed(&self) {
        self.latency_changed.store(true, Ordering::Relaxed);
    }
}

impl HostParamsImplMainThread for MainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {}

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

struct Host;

impl HostHandlers for Host {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostParams>()
            .register::<HostLatency>()
            .register::<HostGui>()
            .register::<HostState>();
    }
}

/// The loop-thread side of a loaded plugin.
pub struct ClapInstance {
    instance: PluginInstance<Host>,
    params: Option<PluginParams>,
    latency: Option<PluginLatency>,
    gui: Option<PluginGui>,
    state: Option<PluginState>,
    /// Since when the state is dirty and not yet saved.
    dirty_since: Option<Instant>,
    in_channels: Vec<usize>,
    out_channels: Vec<usize>,
    rate: u32,
    /// The plugin asked for a restart; the processor is on its way back to be reactivated.
    pub restart_pending: bool,
    last_restart: Option<Instant>,
    /// Our X11 window the plugin's GUI is embedded in, while it is open.
    window: Option<u32>,
    pub path: String,
    pub id: String,
    /// The slot this instance was built for.
    pub slot: usize,
    pub info: SlotInfo,
    pub latency_samples: u32,
}

const GUI_CONFIG: GuiConfiguration<'static> = GuiConfiguration {
    api_type: GuiApiType::X11,
    is_floating: false,
};

/// One port per entry, each `channels * MAX_QUANTUM` samples, planar.
struct PortSet {
    channels: Vec<usize>,
    samples: Vec<Vec<f32>>,
}

impl PortSet {
    fn new(channels: Vec<usize>) -> Self {
        let samples = channels
            .iter()
            .map(|&c| vec![0.0; c * MAX_QUANTUM])
            .collect();
        PortSet { channels, samples }
    }

    fn total_channels(&self) -> usize {
        self.channels.iter().sum()
    }
}

/// The audio-thread side: a started processor and everything `process` needs, allocated once.
pub struct ClapProcessor {
    plugin: PluginAudioProcessor<Host>,
    inputs: PortSet,
    outputs: PortSet,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    events_in: EventBuffer,
    events_out: EventBuffer,
    pending: usize,
    steady_time: u64,
}

fn host_info() -> HostInfo {
    HostInfo::new(
        "Canalis",
        "Claritas Audio",
        "https://github.com/claritas-audio/canalis",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info without NULs")
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Loads `path`, instantiates `id` (or the bundle's first plugin when empty), applies
/// `saved` parameter values, and activates for `rate` and `MAX_QUANTUM` frames.
/// The CLAPs Canalis ships with (the Claritas pair): `<bin>/../lib/clap`, next to this
/// executable's folder, as installed (`/app/lib/clap` in the Flatpak) and as
/// `scripts/bundle-claritas.sh` lays out `target/`.
pub fn bundled_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.join("lib").join("clap"))
}

/// The bundled `.clap` that holds the plugin `id`: what a slot with no path means.
fn bundled(id: &str) -> Result<String, String> {
    bundled_in(
        &bundled_dir().ok_or("no folder for the bundled plugins")?,
        id,
    )
}

fn bundled_in(dir: &std::path::Path, id: &str) -> Result<String, String> {
    let mut bundles: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| {
            format!(
                "{id} is not installed with Canalis ({}: {e})",
                dir.display()
            )
        })?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "clap"))
        .collect();
    bundles.sort();
    bundles
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .find(|p| scan(p).is_ok_and(|found| found.iter().any(|d| d.id == id)))
        .ok_or_else(|| {
            format!(
                "{id} is not installed with Canalis (looked in {})",
                dir.display()
            )
        })
}

pub fn load(
    path: &str,
    id: &str,
    saved: &BTreeMap<String, f64>,
    saved_state: Option<&str>,
    rate: u32,
) -> Result<(ClapInstance, ClapProcessor), String> {
    let found;
    let path = if path.is_empty() {
        found = bundled(id)?;
        found.as_str()
    } else {
        path
    };
    // SAFETY: loading a bundle runs code the user chose to put in a slot; this process exists
    // so that a misbehaving plugin takes down only its chain.
    let entry = unsafe { PluginEntry::load(path) }.map_err(|e| format!("cannot load: {e}"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or("no plugin factory in the bundle")?;
    let descriptor = if id.is_empty() {
        factory.plugin_descriptor(0)
    } else {
        factory
            .plugin_descriptors()
            .find(|d| d.id().is_some_and(|d| d.to_bytes() == id.as_bytes()))
    }
    .ok_or_else(|| format!("no plugin {id:?} in the bundle"))?;
    let plugin_id: &CStr = descriptor.id().ok_or("plugin without an id")?;
    let name = descriptor
        .name()
        .map_or_else(|| lossy(plugin_id.to_bytes()), |n| lossy(n.to_bytes()));

    let mut instance = PluginInstance::<Host>::new(
        |_| Shared::default(),
        |_| MainThread::default(),
        &entry,
        plugin_id,
        &host_info(),
    )
    .map_err(|e| format!("cannot instantiate: {e}"))?;
    let shared = instance.plugin_shared_handle();
    let params: Option<PluginParams> = shared.get_extension();
    let latency: Option<PluginLatency> = shared.get_extension();
    let audio_ports: Option<PluginAudioPorts> = shared.get_extension();
    let gui: Option<PluginGui> = shared.get_extension();
    let state: Option<PluginState> = shared.get_extension();
    let has_gui = gui
        .as_ref()
        .is_some_and(|g| g.is_api_supported(&instance.plugin_handle(), GUI_CONFIG));

    if let (Some(state), Some(saved_state)) = (&state, saved_state) {
        match BASE64.decode(saved_state) {
            Ok(bytes) => {
                let plugin = instance.plugin_handle();
                if state.load(&plugin, &mut Cursor::new(bytes)).is_err() {
                    warn!("{path}: the plugin refused its saved state");
                }
            }
            Err(e) => warn!("{path}: saved state is not base64: {e}"),
        }
    }
    if let Some(params) = &params {
        apply_saved(&mut instance, params, saved)?;
    }
    let param_infos = params
        .as_ref()
        .map_or_else(Vec::new, |p| read_params(&mut instance, p));
    let (in_channels, out_channels) = port_layout(&mut instance, audio_ports.as_ref());
    // CLAP puts the main input at index 0; every other input is a sidechain.
    let sidechain = in_channels.len() > 1;
    let (processor, latency_samples) = activate(
        &mut instance,
        latency.as_ref(),
        &in_channels,
        &out_channels,
        rate,
    )?;
    info!(
        "{name} ({}) loaded from {path}: {} params, {latency_samples} samples latency",
        lossy(plugin_id.to_bytes()),
        param_infos.len()
    );
    Ok((
        ClapInstance {
            instance,
            params,
            latency,
            gui,
            state,
            dirty_since: None,
            window: None,
            in_channels,
            out_channels,
            rate,
            restart_pending: false,
            last_restart: None,
            path: path.to_owned(),
            id: lossy(plugin_id.to_bytes()),
            slot: 0,
            info: SlotInfo {
                name,
                params: param_infos,
                error: None,
                has_gui,
                presets: Vec::new(),
                sidechain,
            },
            latency_samples,
        },
        processor,
    ))
}

/// Activates for `rate` and `MAX_QUANTUM` and builds the callback side with its buffers;
/// also what a plugin's `request_restart` gets, on the same instance.
fn activate(
    instance: &mut PluginInstance<Host>,
    latency: Option<&PluginLatency>,
    in_channels: &[usize],
    out_channels: &[usize],
    rate: u32,
) -> Result<(ClapProcessor, u32), String> {
    let configuration = PluginAudioConfiguration {
        sample_rate: f64::from(rate.max(1)),
        min_frames_count: 1,
        max_frames_count: MAX_QUANTUM as u32,
    };
    let stopped = instance
        .activate(|_, _| (), configuration)
        .map_err(|e| format!("cannot activate: {e}"))?;
    let latency_samples = latency.map_or(0, |l| l.get(&instance.plugin_handle()));
    let inputs = PortSet::new(in_channels.to_vec());
    let outputs = PortSet::new(out_channels.to_vec());
    let processor = ClapProcessor {
        plugin: stopped.into(),
        in_ports: AudioPorts::with_capacity(inputs.total_channels(), inputs.channels.len()),
        out_ports: AudioPorts::with_capacity(outputs.total_channels(), outputs.channels.len()),
        inputs,
        outputs,
        events_in: EventBuffer::with_capacity(MAX_PENDING_EVENTS),
        events_out: EventBuffer::with_capacity(MAX_PENDING_EVENTS),
        pending: 0,
        steady_time: 0,
    };
    Ok((processor, latency_samples))
}

/// Saved values go in through a flush on the inactive plugin, before any audio runs.
fn apply_saved(
    instance: &mut PluginInstance<Host>,
    params: &PluginParams,
    saved: &BTreeMap<String, f64>,
) -> Result<(), String> {
    if saved.is_empty() {
        return Ok(());
    }
    let mut events = EventBuffer::with_capacity(saved.len());
    for (id, value) in saved {
        if let Some(id) = id.parse().ok().and_then(ClapId::from_raw) {
            events.push(&ParamValueEvent::new(0, id, Pckn::match_all(), *value));
        }
    }
    let mut out = EventBuffer::new();
    let mut plugin = instance
        .inactive_plugin_handle()
        .ok_or("plugin already active")?;
    params.flush(
        &mut plugin,
        &InputEvents::from_buffer(&events),
        &mut OutputEvents::from_buffer(&mut out),
    );
    Ok(())
}

fn read_params(instance: &mut PluginInstance<Host>, params: &PluginParams) -> Vec<ParamInfo> {
    let plugin = instance.plugin_handle();
    let mut buffer = ParamInfoBuffer::new();
    (0..params.count(&plugin))
        .filter_map(|index| {
            let info = params.get_info(&plugin, index, &mut buffer)?;
            if info
                .flags
                .intersects(ParamInfoFlags::IS_HIDDEN | ParamInfoFlags::IS_READONLY)
            {
                return None;
            }
            Some(ParamInfo {
                id: info.id.get(),
                name: lossy(info.name),
                module: lossy(info.module),
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                value: params
                    .get_value(&plugin, info.id)
                    .unwrap_or(info.default_value),
                stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            })
        })
        .collect()
}

/// Channel counts of every input and output port; one stereo pair each without the extension.
fn port_layout(
    instance: &mut PluginInstance<Host>,
    audio_ports: Option<&PluginAudioPorts>,
) -> (Vec<usize>, Vec<usize>) {
    let Some(ext) = audio_ports else {
        return (vec![2], vec![2]);
    };
    let plugin = instance.plugin_handle();
    let mut buffer = AudioPortInfoBuffer::new();
    let mut side = |is_input: bool| -> Vec<usize> {
        (0..ext.count(&plugin, is_input))
            .map(|index| {
                ext.get(&plugin, index, is_input, &mut buffer)
                    .map_or(0, |p| p.channel_count as usize)
            })
            .collect()
    };
    (side(true), side(false))
}

impl ClapInstance {
    pub fn matches(&self, processor: &ClapProcessor) -> bool {
        processor.plugin.matches(&self.instance)
    }

    /// The plugin asked to be restarted and no restart ran in the last second.
    pub fn wants_restart(&mut self) -> bool {
        let asked = self
            .instance
            .access_shared_handler(|s| s.restart.swap(false, Ordering::Relaxed));
        asked
            && !self.restart_pending
            && self
                .last_restart
                .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
    }

    /// Deactivates and activates the same instance again (CLAP `request_restart`), with a
    /// fresh callback side; the plugin keeps its state, params and window.
    pub fn reactivate(&mut self, processor: ClapProcessor) -> Result<ClapProcessor, String> {
        let stopped = processor.plugin.into_stopped();
        self.instance.deactivate(stopped);
        let (processor, latency_samples) = activate(
            &mut self.instance,
            self.latency.as_ref(),
            &self.in_channels,
            &self.out_channels,
            self.rate,
        )?;
        self.latency_samples = latency_samples;
        self.restart_pending = false;
        self.last_restart = Some(Instant::now());
        Ok(processor)
    }

    /// Stops and deactivates the processor that came back from the callback.
    pub fn retire(mut self, processor: ClapProcessor, x11: Option<&X11>) {
        if let Some(x11) = x11 {
            self.close_gui(x11);
        }
        let stopped = processor.plugin.into_stopped();
        self.instance.deactivate(stopped);
    }

    pub fn gui_open(&self) -> bool {
        self.window.is_some()
    }

    /// Creates the plugin's GUI embedded in a new top-level window titled `title`.
    pub fn open_gui(&mut self, x11: &X11, title: &str) -> Result<(), String> {
        if self.window.is_some() {
            return Ok(());
        }
        let gui = self.gui.as_ref().ok_or("the plugin has no GUI")?;
        let plugin = self.instance.plugin_handle();
        gui.create(&plugin, GUI_CONFIG)
            .map_err(|e| format!("gui create: {e:?}"))?;
        let size = gui.get_size(&plugin).unwrap_or(GuiSize {
            width: 640,
            height: 400,
        });
        let window = match x11.create_window(title, size.width, size.height) {
            Ok(window) => window,
            Err(e) => {
                gui.destroy(&plugin);
                return Err(e);
            }
        };
        let _ = gui.set_scale(&plugin, 1.0);
        // SAFETY: the handle is a window this process just created and keeps until `close_gui`.
        let parented = unsafe { gui.set_parent(&plugin, Window::from_x11_handle(window.into())) };
        if let Err(e) = parented.and_then(|()| gui.show(&plugin)) {
            gui.destroy(&plugin);
            x11.close_window(window);
            return Err(format!("gui attach: {e:?}"));
        }
        self.window = Some(window);
        Ok(())
    }

    pub fn close_gui(&mut self, x11: &X11) {
        let Some(window) = self.window.take() else {
            return;
        };
        if let Some(gui) = &self.gui {
            let plugin = self.instance.plugin_handle();
            let _ = gui.hide(&plugin);
            gui.destroy(&plugin);
        }
        x11.close_window(window);
    }

    /// Reacts to the window the GUI lives in; `true` when the window was closed.
    pub fn on_window_event(&mut self, event: &WindowEvent, x11: &X11) -> bool {
        match *event {
            WindowEvent::Closed(window) if Some(window) == self.window => {
                self.close_gui(x11);
                true
            }
            WindowEvent::Resized {
                window,
                width,
                height,
            } if Some(window) == self.window => {
                if let Some(gui) = &self.gui {
                    let plugin = self.instance.plugin_handle();
                    if gui.can_resize(&plugin) {
                        let _ = gui.set_size(&plugin, GuiSize { width, height });
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Serves the GUI's requests (resize, show, hide, closed); `true` when it closed itself.
    pub fn tick_gui(&mut self, x11: &X11) -> bool {
        let Some(window) = self.window else {
            return false;
        };
        let packed = self
            .instance
            .access_shared_handler(|s| s.gui_resize.swap(0, Ordering::Relaxed));
        if packed != 0 {
            let size = GuiSize::unpack_from_u64(packed);
            x11.resize(window, size.width, size.height);
        }
        if self
            .instance
            .access_shared_handler(|s| s.gui_closed.swap(false, Ordering::Relaxed))
        {
            self.close_gui(x11);
            return true;
        }
        let (show, hide) = self.instance.access_shared_handler(|s| {
            (
                s.gui_show.swap(false, Ordering::Relaxed),
                s.gui_hide.swap(false, Ordering::Relaxed),
            )
        });
        if let Some(gui) = &self.gui {
            let plugin = self.instance.plugin_handle();
            if show {
                let _ = gui.show(&plugin);
            }
            if hide {
                let _ = gui.hide(&plugin);
            }
        }
        false
    }

    /// Services the plugin's main-thread requests (callbacks and latency changes).
    pub fn tick(&mut self) {
        if self
            .instance
            .access_shared_handler(|s| s.callback.swap(false, Ordering::Relaxed))
        {
            self.instance.call_on_main_thread_callback();
        }
        if self
            .instance
            .access_handler(|m| m.latency_changed.swap(false, Ordering::Relaxed))
            && let Some(latency) = &self.latency
        {
            self.latency_samples = latency.get(&self.instance.plugin_handle());
        }
    }

    /// Something changed the plugin's parameters from our side; the state is saved soon.
    pub fn touch(&mut self) {
        self.dirty_since = Some(Instant::now());
    }

    /// The state to persist, base64, once changes have settled; also refreshes the
    /// reported parameter values so the strip shows what the plugin holds now.
    pub fn tick_state(&mut self) -> Option<String> {
        let dirty = self
            .instance
            .access_handler(|m| m.dirty.swap(false, Ordering::Relaxed));
        let changed = self
            .instance
            .access_shared_handler(|s| s.params_changed.swap(false, Ordering::Relaxed));
        if dirty || changed {
            self.dirty_since = Some(Instant::now());
        }
        if self.dirty_since.is_some_and(|t| t.elapsed() < STATE_SETTLE) {
            return None;
        }
        self.dirty_since.take()?;
        self.refresh_params();
        let state = self.state.as_ref()?;
        let plugin = self.instance.plugin_handle();
        let mut bytes = Vec::new();
        if state.save(&plugin, &mut bytes).is_err() {
            warn!("{}: the plugin could not save its state", self.id);
            return None;
        }
        Some(BASE64.encode(bytes))
    }

    fn refresh_params(&mut self) {
        let Some(params) = self.params else {
            return;
        };
        self.info.params = read_params(&mut self.instance, &params);
    }
}

impl ClapProcessor {
    /// Queues a parameter change for the next process call. Real-time.
    pub fn set_param(&mut self, id: u32, value: f64) {
        if self.pending >= MAX_PENDING_EVENTS {
            return;
        }
        if let Some(id) = ClapId::from_raw(id) {
            self.events_in
                .push(&ParamValueEvent::new(0, id, Pckn::match_all(), value));
            self.pending += 1;
        }
    }

    /// Runs the plugin over `io` in place. Real-time: everything was allocated in `load`.
    /// `key` is the chain's sidechain, one slice per channel, for the inputs after the
    /// main one; empty when the chain has none, and those inputs then hear silence.
    pub fn process(&mut self, io: &mut [&mut [f32]], key: &[&[f32]]) {
        let Some(frames) = io.first().map(|c| c.len().min(MAX_QUANTUM)) else {
            return;
        };
        let ClapProcessor {
            plugin,
            inputs,
            outputs,
            in_ports,
            out_ports,
            events_in,
            events_out,
            pending,
            steady_time,
        } = self;
        for (index, port) in inputs.samples.iter_mut().enumerate() {
            let channels = port.as_chunks_mut::<MAX_QUANTUM>().0.iter_mut();
            if index == 0 && inputs.channels[0] == 1 && io.len() > 1 {
                // A mono plugin hears the mid of a stereo chain, as LV2 ones do.
                if let Some(mono) = channels.into_iter().next() {
                    let scale = 1.0 / io.len() as f32;
                    for (i, sample) in mono[..frames].iter_mut().enumerate() {
                        *sample = io
                            .iter()
                            .map(|c| c.get(i).copied().unwrap_or(0.0))
                            .sum::<f32>()
                            * scale;
                    }
                }
            } else if index == 0 {
                for (channel, source) in channels.zip(io.iter()) {
                    let n = frames.min(source.len());
                    channel[..n].copy_from_slice(&source[..n]);
                }
            } else {
                for (c, channel) in channels.enumerate() {
                    match key.get(c).or(key.last()) {
                        Some(source) => {
                            let n = frames.min(source.len());
                            channel[..n].copy_from_slice(&source[..n]);
                            channel[n..frames].fill(0.0);
                        }
                        None => channel[..frames].fill(0.0),
                    }
                }
            }
        }
        let input_audio = in_ports.with_input_buffers(inputs.samples.iter_mut().map(|port| {
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    port.as_chunks_mut::<MAX_QUANTUM>()
                        .0
                        .iter_mut()
                        .map(|c| InputChannel::variable(&mut c[..frames])),
                ),
            }
        }));
        let mut output_audio =
            out_ports.with_output_buffers(outputs.samples.iter_mut().map(|port| {
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        port.as_chunks_mut::<MAX_QUANTUM>()
                            .0
                            .iter_mut()
                            .map(|c| &mut c[..frames]),
                    ),
                }
            }));
        let input_events = InputEvents::from_buffer(events_in);
        let mut output_events = OutputEvents::from_buffer(events_out);
        let processed = plugin.ensure_processing_started().is_ok_and(|started| {
            started
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    Some(*steady_time),
                    None,
                )
                .is_ok()
        });
        *steady_time += frames as u64;
        if events_out
            .iter()
            .any(|e| e.as_event::<ParamValueEvent>().is_some())
        {
            plugin.access_shared_handler(|s| s.params_changed.store(true, Ordering::Relaxed));
        }
        events_in.clear();
        events_out.clear();
        *pending = 0;
        if !processed {
            return;
        }
        if let Some(port) = outputs.samples.first() {
            let channels = port.as_chunks::<MAX_QUANTUM>().0;
            if let [mono] = channels {
                // A mono output feeds every channel of the chain.
                for target in io.iter_mut() {
                    let n = frames.min(target.len());
                    target[..n].copy_from_slice(&mono[..n]);
                }
            } else {
                for (channel, target) in channels.iter().zip(io.iter_mut()) {
                    let n = frames.min(target.len());
                    target[..n].copy_from_slice(&channel[..n]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claritas EQ's bundle, when this machine has it; the test is a no-op otherwise.
    fn claritas_eq() -> Option<String> {
        let path =
            std::path::PathBuf::from(std::env::var_os("HOME")?).join(".clap/Claritas EQ.clap");
        path.exists().then(|| path.to_string_lossy().into_owned())
    }

    #[test]
    fn a_slot_without_a_path_finds_the_bundled_plugin() {
        let _serial = crate::plugin_test_guard();
        // Laid out by `scripts/bundle-claritas.sh`; a no-op where it has not run.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lib/clap");
        if !dir.join("Claritas Comp.clap").exists() {
            return;
        }
        let found = bundled_in(&dir, "audio.claritas.comp").expect("found");
        assert!(found.ends_with("Claritas Comp.clap"), "{found}");
        assert!(bundled_in(&dir, "no.such.plugin").is_err());
    }

    #[test]
    fn a_flat_plugin_passes_audio_through_and_lists_params() {
        let _serial = crate::plugin_test_guard();
        let Some(path) = claritas_eq() else {
            return;
        };
        let (mut instance, mut processor) =
            load(&path, "", &BTreeMap::new(), None, 48_000).expect("Claritas EQ loads");
        assert_eq!(instance.info.name, "Claritas EQ");
        assert!(!instance.info.params.is_empty(), "no params reported");
        assert!(instance.matches(&processor));

        let mut left: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut right = left.clone();
        let expected = left.clone();
        processor.process(&mut [&mut left, &mut right], &[]);
        for (side, channel) in [("left", &left), ("right", &right)] {
            let worst = channel
                .iter()
                .zip(&expected)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                worst < 1e-3,
                "flat EQ changed the {side} channel by {worst}"
            );
        }

        instance.tick();
        assert!(!instance.wants_restart());
        assert!(instance.info.has_gui, "Claritas EQ should offer an X11 GUI");
        assert!(
            instance.tick_state().is_none(),
            "nothing changed, nothing to save"
        );
        instance.retire(processor, None);
    }

    #[test]
    fn a_mono_plugin_hears_both_channels_and_feeds_both() {
        let _serial = crate::plugin_test_guard();
        let path = "/usr/lib64/clap/lsp-plugins.clap";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let (instance, mut processor) = load(
            path,
            "in.lsp-plug.compressor_mono",
            &BTreeMap::new(),
            None,
            48_000,
        )
        .expect("LSP Compressor Mono loads");
        // The signal is on the right only: a plugin fed the left channel alone hears nothing.
        let mut heard = 0.0f32;
        for _ in 0..40 {
            let mut left = vec![0.0f32; 512];
            let mut right: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
            processor.process(&mut [&mut left, &mut right], &[]);
            assert_eq!(left, right, "the mono output goes to both channels");
            heard = heard.max(left.iter().fold(0.0f32, |m, s| m.max(s.abs())));
        }
        assert!(heard > 0.1, "the right channel reached the plugin: {heard}");
        instance.retire(processor, None);
    }

    #[test]
    fn state_round_trips_through_base64() {
        let _serial = crate::plugin_test_guard();
        let Some(path) = claritas_eq() else {
            return;
        };
        let (mut instance, processor) =
            load(&path, "", &BTreeMap::new(), None, 48_000).expect("Claritas EQ loads");
        instance.touch();
        std::thread::sleep(STATE_SETTLE + Duration::from_millis(50));
        let state = instance
            .tick_state()
            .expect("a state after a change settled");
        assert!(BASE64.decode(&state).is_ok_and(|b| !b.is_empty()));
        instance.retire(processor, None);

        let (again, processor) = load(&path, "", &BTreeMap::new(), Some(&state), 48_000)
            .expect("Claritas EQ loads with its state");
        assert_eq!(again.info.params.len(), 195);
        again.retire(processor, None);
    }

    #[test]
    fn a_scan_lists_the_plugins_in_a_bundle() {
        let _serial = crate::plugin_test_guard();
        let Some(path) = claritas_eq() else {
            return;
        };
        let plugins = scan(&path).expect("scans");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "audio.claritas.eq");
        assert_eq!(plugins[0].name, "Claritas EQ");
        assert!(!plugins[0].vendor.is_empty());
    }

    #[test]
    fn a_missing_bundle_is_an_error_not_a_panic() {
        let err = load(
            "/nonexistent/plugin.clap",
            "",
            &BTreeMap::new(),
            None,
            48_000,
        )
        .err()
        .expect("an error");
        assert!(err.starts_with("cannot load"), "{err}");
    }
}
