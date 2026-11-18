//! The control side of the chain process: owns the configured slots, builds runners off
//! the audio thread, feeds the message ring and frees what the callback hands back.
//! CLAP instances live here too; only their processors cross into the callback.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use rtrb::{Consumer, Producer};

use canalis_chain::clap::{self, ClapInstance, SlotInfo};
use canalis_chain::ipc::{FromChain, ToChain};
use canalis_chain::lv2::{self, Lv2Host, Lv2Instance};
use canalis_chain::recorder::Recorder;
use canalis_chain::spectrum::Analyzer;
use canalis_chain::x11::{Fd, X11};
use canalis_chain::{
    LaneRunner, MAX_LANES, MAX_SENDS, MAX_SLOTS, MEASURE_NONE, Message, SlotConfig, SlotPath,
    SlotRunner, SlotSpec, lane_slot_id, slot_config, slot_config_mut, slot_ids,
};

/// How often a running recording reports its length.
const RECORDING_REPORT: Duration = Duration::from_millis(500);
/// How often the chain's peak load is reported.
const LOAD_REPORT: Duration = Duration::from_millis(500);

pub struct Control {
    /// Window titles start with the chain's label.
    label: String,
    /// The X11 connection plugin windows go on; `None` without a display.
    x11: Option<X11>,
    rate: u32,
    channels: u32,
    slots: Vec<SlotConfig>,
    /// Loaded plugins, active ones and those waiting for their processor to come back.
    instances: Vec<ClapInstance>,
    lv2_instances: Vec<Lv2Instance>,
    /// The lilv world, loaded the first time an LV2 slot is built.
    lv2: Option<Lv2Host>,
    /// Per slot id, what the runner was built with; reported after every rebuild.
    infos: HashMap<usize, SlotInfo>,
    /// Per slot id; a group's own is the longest of its lanes' sums.
    latencies: HashMap<usize, u32>,
    messages: Producer<Message>,
    garbage: Consumer<Box<SlotRunner>>,
    /// Reports for the control plane, drained by `main` after every call in here.
    reports: Vec<FromChain>,
    /// Destination pairs the playback end carries; a change is picked up by `main`.
    sends: usize,
    sends_changed: bool,
    key: bool,
    key_changed: bool,
    insert: bool,
    insert_changed: bool,
    /// A rebuild set slot latencies the timer has not published yet.
    latency_dirty: bool,
    /// The hardware insert slots' measurement cells, by slot.
    measures: Vec<(usize, Arc<AtomicU32>)>,
    /// The output being recorded, and when it was last reported.
    recorder: Option<(Recorder, Instant)>,
    /// When the peak load was last reported.
    load_reported: Instant,
    /// The spectrum analyzer, while a window shows the spectrum.
    analyzer: Option<Analyzer>,
}

impl Control {
    pub fn new(
        label: &str,
        messages: Producer<Message>,
        garbage: Consumer<Box<SlotRunner>>,
    ) -> Self {
        let x11 = match X11::connect() {
            Ok(x11) => Some(x11),
            Err(e) => {
                warn!("plugin windows unavailable: {e}");
                None
            }
        };
        Control {
            label: label.to_owned(),
            x11,
            rate: 0,
            channels: 0,
            slots: Vec::new(),
            instances: Vec::new(),
            lv2_instances: Vec::new(),
            lv2: None,
            infos: HashMap::new(),
            latencies: HashMap::new(),
            messages,
            garbage,
            reports: Vec::new(),
            sends: 1,
            sends_changed: false,
            key: false,
            key_changed: false,
            insert: false,
            insert_changed: false,
            latency_dirty: false,
            measures: Vec::new(),
            recorder: None,
            load_reported: Instant::now(),
            analyzer: None,
        }
    }

    /// Whether the insert send pair and return input should exist, once per change.
    pub fn take_insert_change(&mut self) -> Option<bool> {
        self.insert_changed.then(|| {
            self.insert_changed = false;
            self.insert
        })
    }

    /// Whether the key input should exist, once per change.
    pub fn take_key_change(&mut self) -> Option<bool> {
        self.key_changed.then(|| {
            self.key_changed = false;
            self.key
        })
    }

    /// The playback end needs this many pairs now, once per change.
    pub fn take_sends_change(&mut self) -> Option<usize> {
        self.sends_changed.then(|| {
            self.sends_changed = false;
            self.sends
        })
    }

    /// A new format: every runner is rebuilt for the new rate.
    pub fn set_format(&mut self, rate: u32, channels: u32) {
        debug!(
            "format {rate} Hz x {channels}; {} slots to rebuild",
            self.slots.len()
        );
        self.rate = rate;
        self.channels = channels;
        self.send(Message::Rate { rate });
        self.rebuild();
    }

    /// Applies a command from the control plane; `true` when the chain's latency may have changed.
    pub fn apply(&mut self, command: ToChain) -> bool {
        match command {
            ToChain::Slots { slots } => {
                if slots.len() > MAX_SLOTS {
                    warn!("{} slots asked, {MAX_SLOTS} kept", slots.len());
                }
                self.slots = slots;
                self.slots.truncate(MAX_SLOTS);
                debug!("{} slots configured at {} Hz", self.slots.len(), self.rate);
                let insert = self
                    .slots
                    .iter()
                    .any(|s| matches!(s.spec, SlotSpec::Hardware { .. }));
                if insert != self.insert {
                    self.insert = insert;
                    self.insert_changed = true;
                }
                self.rebuild();
                true
            }
            ToChain::Bypass { slot, bypass } => {
                if let Some(config) = slot_config_mut(&mut self.slots, slot) {
                    config.bypass = bypass;
                    self.send(Message::Bypass {
                        index: slot,
                        bypass,
                    });
                }
                false
            }
            ToChain::Mix { slot, mix } => {
                if let Some(config) = slot_config_mut(&mut self.slots, slot) {
                    config.mix = mix;
                    self.send(Message::Mix { index: slot, mix });
                }
                false
            }
            ToChain::Param { slot, id, value } => {
                if let Some(SlotSpec::Clap { params, .. } | SlotSpec::Lv2 { params, .. }) =
                    slot_config_mut(&mut self.slots, slot).map(|c| &mut c.spec)
                {
                    params.insert(id.to_string(), value);
                    self.send(Message::Param {
                        index: slot,
                        id,
                        value,
                    });
                    if let Some(instance) = self.instances.iter_mut().rev().find(|i| i.slot == slot)
                    {
                        instance.touch();
                    }
                    if let Some(instance) =
                        self.lv2_instances.iter_mut().rev().find(|i| i.slot == slot)
                    {
                        instance.note_control(id, value);
                    }
                }
                false
            }
            ToChain::Gui { slot, open } => {
                self.set_gui(slot, open);
                false
            }
            ToChain::Key { enabled } => {
                if enabled != self.key {
                    self.key = enabled;
                    self.key_changed = true;
                }
                false
            }
            ToChain::Measure { slot } => {
                self.send(Message::Measure { index: slot });
                false
            }
            ToChain::LaneGain {
                slot,
                lane,
                gain_db,
            } => {
                if let Some(SlotSpec::Group { lanes }) =
                    slot_config_mut(&mut self.slots, slot).map(|c| &mut c.spec)
                    && let Some(l) = lanes.get_mut(lane)
                {
                    l.gain_db = gain_db;
                    self.send(Message::LaneGain {
                        index: slot,
                        lane,
                        gain: linear(gain_db),
                    });
                }
                false
            }
            ToChain::LanePan { slot, lane, pan } => {
                if let Some(SlotSpec::Group { lanes }) =
                    slot_config_mut(&mut self.slots, slot).map(|c| &mut c.spec)
                    && let Some(l) = lanes.get_mut(lane)
                {
                    let pan = pan.clamp(-1.0, 1.0);
                    l.pan = pan;
                    self.send(Message::LanePan {
                        index: slot,
                        lane,
                        pan,
                    });
                }
                false
            }
            ToChain::LaneMute { slot, lane, mute } => {
                if let Some(SlotSpec::Group { lanes }) =
                    slot_config_mut(&mut self.slots, slot).map(|c| &mut c.spec)
                    && let Some(l) = lanes.get_mut(lane)
                {
                    l.mute = mute;
                    self.send(Message::LaneMute {
                        index: slot,
                        lane,
                        mute,
                    });
                }
                false
            }
            ToChain::Preset { slot, uri } => {
                if let Some(config) = slot_config_mut(&mut self.slots, slot)
                    && let SlotSpec::Lv2 {
                        params,
                        state,
                        preset,
                        ..
                    } = &mut config.spec
                {
                    *preset = Some(uri);
                    *state = None;
                    params.clear();
                    let config = config.clone();
                    let runner = self.build(slot, &config);
                    self.replace(slot, Some(runner));
                    if let Some(info) = self.infos.get(&slot) {
                        self.reports.push(FromChain::SlotInfo {
                            slot,
                            info: info.clone(),
                        });
                    }
                }
                true
            }
            ToChain::Mute { mute } => {
                self.send(Message::Mute { mute });
                false
            }
            ToChain::ResetLoudness => {
                self.send(Message::ResetLoudness);
                false
            }
            ToChain::Spectrum { on } => {
                self.spectrum(on);
                false
            }
            ToChain::AutoLevel { target } => {
                self.send(Message::AutoLevel { target });
                false
            }
            ToChain::Record { path } => {
                self.record(path.map(PathBuf::from));
                false
            }
            ToChain::Sends { count, gains_db } => {
                let count = count.clamp(1, MAX_SENDS);
                if count != self.sends {
                    self.sends = count;
                    self.sends_changed = true;
                }
                let mut gains = [1.0f32; MAX_SENDS];
                for (gain, db) in gains.iter_mut().zip(gains_db) {
                    *gain = linear(db);
                }
                self.send(Message::Sends { gains });
                false
            }
            ToChain::Quit => false,
        }
    }

    /// The X11 socket, for the loop to watch; `pump_windows` drains it.
    pub fn x11_fd(&self) -> Option<Fd> {
        self.x11.as_ref().map(X11::fd)
    }

    /// Delivers window-manager events to the plugins whose windows they concern.
    pub fn pump_windows(&mut self) {
        let Some(x11) = &self.x11 else {
            return;
        };
        for event in x11.poll_events() {
            for instance in &mut self.instances {
                if instance.on_window_event(&event, x11) {
                    self.reports.push(FromChain::Gui {
                        slot: instance.slot,
                        open: false,
                    });
                }
            }
            for instance in &mut self.lv2_instances {
                if instance.on_window_event(&event, x11) {
                    self.reports.push(FromChain::Gui {
                        slot: instance.slot,
                        open: false,
                    });
                }
            }
        }
    }

    fn set_gui(&mut self, slot: usize, open: bool) {
        let Some(x11) = &self.x11 else {
            warn!("slot {slot}: no X11 display for a plugin window");
            return;
        };
        let title = format!("{} · slot {}", self.label, slot + 1);
        if let Some(instance) = self.lv2_instances.iter_mut().rev().find(|i| i.slot == slot) {
            if open {
                let opened =
                    lv2_host(&mut self.lv2).and_then(|host| instance.open_ui(host, x11, &title));
                if let Err(e) = opened {
                    warn!("slot {slot}: {e}");
                }
            } else {
                instance.close_ui(x11);
            }
            self.reports.push(FromChain::Gui {
                slot,
                open: instance.gui_open(),
            });
            return;
        }
        let Some(instance) = self.instances.iter_mut().rev().find(|i| i.slot == slot) else {
            return;
        };
        if open {
            if let Err(e) = instance.open_gui(x11, &title) {
                warn!("slot {slot}: {e}");
            }
        } else {
            instance.close_gui(x11);
        }
        self.reports.push(FromChain::Gui {
            slot,
            open: instance.gui_open(),
        });
    }

    /// The chain's latency: every top-level slot's, a group counting as its slowest lane.
    pub fn latency_samples(&self) -> u32 {
        self.slot_latencies().iter().sum()
    }

    /// Each top-level slot's latency, in order.
    pub fn slot_latencies(&self) -> Vec<u32> {
        self.slots
            .iter()
            .enumerate()
            .map(|(top, slot)| self.top_latency(top, slot))
            .collect()
    }

    fn top_latency(&self, top: usize, slot: &SlotConfig) -> u32 {
        match &slot.spec {
            SlotSpec::Group { lanes } => lanes
                .iter()
                .enumerate()
                .map(|(lane, l)| {
                    (0..l.slots.len())
                        .map(|sub| self.latency_of(lane_slot_id(top, lane, sub)))
                        .sum()
                })
                .max()
                .unwrap_or(0),
            _ => self.latency_of(top),
        }
    }

    fn latency_of(&self, id: usize) -> u32 {
        self.latencies.get(&id).copied().unwrap_or(0)
    }

    /// Swaps the runner a slot id addresses, top-level or inside a lane.
    fn replace(&mut self, id: usize, slot: Option<Box<SlotRunner>>) {
        let message = match SlotPath::from_id(id) {
            SlotPath::Top(index) => Message::Replace { index, slot },
            SlotPath::Lane { top, lane, sub } => Message::ReplaceLane {
                top,
                lane,
                sub,
                slot,
            },
        };
        self.send(message);
    }

    pub fn take_reports(&mut self) -> Vec<FromChain> {
        std::mem::take(&mut self.reports)
    }

    /// Timer duty: frees what the callback replaced, deactivating plugins on this thread,
    /// and serves plugins' main-thread requests. `true` when the latency changed.
    /// Starts recording to `path` (not while a recording runs) or, with `None`, stops:
    /// the tap leaves the callback and the writer finishes the file on its own.
    fn record(&mut self, path: Option<PathBuf>) {
        match path {
            Some(path) if self.rate == 0 => self.reports.push(FromChain::Recording {
                path: path.display().to_string(),
                seconds: 0.0,
                dropped: 0,
                done: true,
                error: Some("nothing plays through the chain yet".into()),
            }),
            Some(path) if self.recorder.is_none() => {
                match Recorder::start(path.clone(), self.rate) {
                    Ok((recorder, tap)) => {
                        info!("recording to {}", path.display());
                        self.send(Message::Tap(Some(tap)));
                        self.recorder = Some((recorder, Instant::now()));
                    }
                    Err(e) => self.reports.push(FromChain::Recording {
                        path: path.display().to_string(),
                        seconds: 0.0,
                        dropped: 0,
                        done: true,
                        error: Some(e.to_string()),
                    }),
                }
            }
            Some(_) => warn!("already recording"),
            None => {
                if let Some((recorder, _)) = &self.recorder {
                    recorder.stop();
                }
                self.send(Message::Tap(None));
            }
        }
    }

    /// Reports the recording's progress, and its end once the writer is done.
    fn report_recording(&mut self) {
        let Some((recorder, reported)) = &mut self.recorder else {
            return;
        };
        let finished = recorder.finished();
        if finished.is_none() && reported.elapsed() < RECORDING_REPORT {
            return;
        }
        *reported = Instant::now();
        let done = finished.is_some();
        self.reports.push(FromChain::Recording {
            path: recorder.path.display().to_string(),
            seconds: recorder.seconds(),
            dropped: recorder.dropped(),
            done,
            error: finished.and_then(Result::err).map(|e| e.to_string()),
        });
        if done {
            self.recorder = None;
        }
    }

    /// Starts or stops the analyzer; it starts only once the stream has a rate.
    fn spectrum(&mut self, on: bool) {
        if !on {
            self.send(Message::Spectrum(None));
            self.analyzer = None;
            return;
        }
        if self.analyzer.is_some() || self.rate == 0 {
            return;
        }
        match Analyzer::start(self.rate) {
            Ok((analyzer, tap)) => {
                self.send(Message::Spectrum(Some(tap)));
                self.analyzer = Some(analyzer);
            }
            Err(e) => warn!("no spectrum analyzer: {e}"),
        }
    }

    /// The latest spectra, input then output, while the analyzer runs.
    pub fn spectrum_bands(&self) -> Option<Vec<f32>> {
        let [input, output] = self.analyzer.as_ref()?.bands();
        Some(
            input
                .iter()
                .chain(&output)
                .map(|v| (v * 10.0).round() / 10.0)
                .collect(),
        )
    }

    /// The chain is quitting: a recording in progress is finished so its file is whole.
    pub fn finish_recording(&mut self) {
        if let Some((recorder, _)) = self.recorder.take() {
            let path = recorder.path.clone();
            match recorder.finish() {
                Ok(()) => info!("recording saved to {}", path.display()),
                Err(e) => warn!("recording to {}: {e}", path.display()),
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        self.report_recording();
        if self.load_reported.elapsed() >= LOAD_REPORT {
            self.load_reported = Instant::now();
            // Idle (no cycle ran) reports 0, so the strip never keeps a stale load.
            let percent = canalis_chain::overload::take_peak_load().unwrap_or(0.0);
            self.reports.push(FromChain::Load { percent });
        }
        let latency_before = self.latency_samples();
        let mut returned: Vec<Box<SlotRunner>> = Vec::new();
        while let Ok(runner) = self.garbage.pop() {
            returned.push(runner);
        }
        while let Some(runner) = returned.pop() {
            // A replaced group comes back whole; its lanes' slots are retired one by one.
            let runner = match runner.into_lane_slots() {
                Ok(slots) => {
                    returned.extend(slots);
                    continue;
                }
                Err(runner) => runner,
            };
            let runner = match runner.into_lv2_or_back() {
                Ok(processor) => {
                    match self
                        .lv2_instances
                        .iter()
                        .position(|i| i.matches(&processor))
                    {
                        Some(at) => {
                            let instance = self.lv2_instances.swap_remove(at);
                            let (slot, was_open) = (instance.slot, instance.gui_open());
                            instance.retire(processor, self.x11.as_ref());
                            if was_open {
                                self.reports.push(FromChain::Gui { slot, open: false });
                            }
                        }
                        None => warn!("an LV2 processor came back without its instance"),
                    }
                    continue;
                }
                Err(runner) => runner,
            };
            if let Some(processor) = runner.into_clap() {
                match self.instances.iter().position(|i| i.matches(&processor)) {
                    Some(at) if self.instances[at].restart_pending => {
                        self.restart(at, processor);
                    }
                    Some(at) => {
                        let instance = self.instances.swap_remove(at);
                        let was_open = instance.gui_open();
                        let slot = instance.slot;
                        instance.retire(processor, self.x11.as_ref());
                        if was_open {
                            self.reports.push(FromChain::Gui { slot, open: false });
                        }
                    }
                    None => warn!("a plugin processor came back without its instance"),
                }
            }
        }
        self.pump_windows();
        let mut restarts = Vec::new();
        for instance in &mut self.instances {
            instance.tick();
            if instance.wants_restart() {
                restarts.push(instance.slot);
                instance.restart_pending = true;
            }
            if let Some(x11) = &self.x11
                && instance.tick_gui(x11)
            {
                self.reports.push(FromChain::Gui {
                    slot: instance.slot,
                    open: false,
                });
            }
            if let Some(state) = instance.tick_state() {
                if let Some(SlotSpec::Clap { state: saved, .. }) =
                    slot_config_mut(&mut self.slots, instance.slot).map(|c| &mut c.spec)
                {
                    *saved = Some(state.clone());
                }
                self.reports.push(FromChain::State {
                    slot: instance.slot,
                    state,
                });
                self.reports.push(FromChain::SlotInfo {
                    slot: instance.slot,
                    info: instance.info.clone(),
                });
            }
        }
        for slot in restarts {
            // The processor comes back through the garbage ring and is reactivated there;
            // the slot passes audio through meanwhile.
            if let Some(config) = slot_config(&self.slots, slot).cloned() {
                let channels = self.channels as usize;
                let runner = Box::new(SlotRunner::passthrough(&config, channels));
                self.replace(slot, Some(runner));
            }
        }
        let measures = self.measures.clone();
        for (slot, measured) in &measures {
            let value = measured.swap(0, Ordering::Relaxed);
            if value == 0 {
                continue;
            }
            let samples = (value != MEASURE_NONE).then(|| value - 1);
            if let Some(samples) = samples {
                self.latencies.insert(*slot, samples);
                if let Some(SlotSpec::Hardware { latency, .. }) =
                    slot_config_mut(&mut self.slots, *slot).map(|c| &mut c.spec)
                {
                    *latency = samples;
                }
            }
            self.reports.push(FromChain::Measured {
                slot: *slot,
                samples,
            });
        }
        let clap_latencies: Vec<(usize, u32)> = self
            .instances
            .iter()
            .map(|i| (i.slot, i.latency_samples))
            .collect();
        for (slot, latency) in clap_latencies {
            self.latencies.insert(slot, latency);
        }
        let mut ui_writes: Vec<(usize, Vec<(u32, f32)>)> = Vec::new();
        for instance in &mut self.lv2_instances {
            instance.tick();
            if let Some(state) = instance.tick_state() {
                if let Some(SlotSpec::Lv2 { state: saved, .. }) =
                    slot_config_mut(&mut self.slots, instance.slot).map(|c| &mut c.spec)
                {
                    *saved = Some(state.clone());
                }
                self.reports.push(FromChain::State {
                    slot: instance.slot,
                    state,
                });
                self.reports.push(FromChain::SlotInfo {
                    slot: instance.slot,
                    info: instance.info.clone(),
                });
            }
            self.latencies
                .insert(instance.slot, instance.latency_samples);
            if let Some(x11) = &self.x11 {
                let (writes, closed) = instance.tick_ui(x11);
                if closed {
                    self.reports.push(FromChain::Gui {
                        slot: instance.slot,
                        open: false,
                    });
                }
                if !writes.is_empty() {
                    ui_writes.push((instance.slot, writes));
                }
            }
        }
        for (slot, writes) in ui_writes {
            for (id, value) in &writes {
                self.send(Message::Param {
                    index: slot,
                    id: *id,
                    value: f64::from(*value),
                });
            }
            if let Some(SlotSpec::Lv2 { params, .. }) =
                slot_config_mut(&mut self.slots, slot).map(|c| &mut c.spec)
            {
                for (id, value) in &writes {
                    params.insert(id.to_string(), f64::from(*value));
                }
                self.reports.push(FromChain::Params {
                    slot,
                    params: params.clone(),
                });
            }
        }
        std::mem::take(&mut self.latency_dirty) || latency_before != self.latency_samples()
    }

    /// A plugin's `request_restart`, once its processor is back: the same instance is
    /// deactivated and activated again and a new callback side goes in.
    fn restart(&mut self, at: usize, processor: clap::ClapProcessor) {
        let channels = self.channels as usize;
        let slot = self.instances[at].slot;
        let Some(config) = slot_config(&self.slots, slot).cloned() else {
            return;
        };
        match self.instances[at].reactivate(processor) {
            Ok(processor) => {
                let latency = self.instances[at].latency_samples;
                self.latencies.insert(slot, latency);
                let runner = Box::new(SlotRunner::clap(&config, processor, channels, latency));
                self.replace(slot, Some(runner));
            }
            Err(error) => {
                warn!("slot {slot}: restart failed: {error}");
                let instance = self.instances.swap_remove(at);
                drop(instance);
                let info = SlotInfo {
                    name: config.spec.name(),
                    params: Vec::new(),
                    error: Some(error),
                    has_gui: false,
                    presets: Vec::new(),
                    sidechain: false,
                };
                self.infos.insert(slot, info.clone());
                self.reports.push(FromChain::SlotInfo { slot, info });
            }
        }
    }

    fn send(&mut self, message: Message) {
        if self.messages.push(message).is_err() {
            warn!("message ring full; a slot change was dropped");
        }
    }

    fn rebuild(&mut self) {
        if self.rate == 0 {
            return;
        }
        self.latencies.clear();
        self.infos.clear();
        self.measures.clear();
        self.latency_dirty = true;
        for index in 0..MAX_SLOTS {
            let slot = self
                .slots
                .get(index)
                .cloned()
                .map(|config| self.build(index, &config));
            self.send(Message::Replace { index, slot });
        }
        for id in slot_ids(&self.slots) {
            if let Some(info) = self.infos.get(&id) {
                self.reports.push(FromChain::SlotInfo {
                    slot: id,
                    info: info.clone(),
                });
            }
        }
    }

    /// The runner for the slot with `index` (a slot id: top-level or inside a lane).
    fn build(&mut self, index: usize, config: &SlotConfig) -> Box<SlotRunner> {
        let channels = self.channels as usize;
        // Every slot reports itself, so a slot list change never shows stale plugin info.
        self.infos.insert(
            index,
            SlotInfo {
                name: config.spec.name(),
                params: Vec::new(),
                error: None,
                has_gui: false,
                presets: Vec::new(),
                sidechain: false,
            },
        );
        if config.disabled {
            if let Some(info) = self.infos.get_mut(&index) {
                info.error = Some("off after a crash: Start normally loads it again".into());
            }
            return Box::new(SlotRunner::passthrough(config, channels));
        }
        if let SlotSpec::Group { lanes } = &config.spec {
            let top = SlotPath::from_id(index);
            if !matches!(top, SlotPath::Top(_)) {
                warn!("slot {index}: a group cannot hold a group; passing through");
                if let Some(info) = self.infos.get_mut(&index) {
                    info.error = Some("a group cannot hold a group".into());
                }
                return Box::new(SlotRunner::passthrough(config, channels));
            }
            let runners: Vec<LaneRunner> = lanes
                .iter()
                .take(MAX_LANES)
                .enumerate()
                .map(|(lane, l)| {
                    let slots = l
                        .slots
                        .iter()
                        .take(MAX_SLOTS)
                        .enumerate()
                        .map(|(sub, c)| self.build(lane_slot_id(top.top(), lane, sub), c))
                        .collect();
                    LaneRunner::new(slots, linear(l.gain_db), l.mute, channels).panned(l.pan)
                })
                .collect();
            return Box::new(SlotRunner::group(config, top.top(), runners, channels));
        }
        if let SlotSpec::Hardware { latency, .. } = &config.spec {
            let measured = Arc::new(AtomicU32::new(0));
            self.measures.push((index, Arc::clone(&measured)));
            self.latencies.insert(index, *latency);
            return Box::new(SlotRunner::hardware(config, *latency, measured));
        }
        if let SlotSpec::Lv2 {
            uri,
            params,
            state,
            preset,
        } = &config.spec
        {
            if self.lv2.as_ref().is_some_and(|host| !host.has(uri)) {
                // Loaded instances keep the old world alive; only new loads use this one.
                info!("{uri} is not in the LV2 world; reading the bundles again");
                self.lv2 = None;
            }
            let loaded = lv2_host(&mut self.lv2).and_then(|host| {
                lv2::load(
                    host,
                    uri,
                    params,
                    state.as_deref(),
                    preset.as_deref(),
                    self.rate,
                )
            });
            return match loaded {
                Ok((mut instance, processor)) => {
                    instance.slot = index;
                    self.infos.insert(index, instance.info.clone());
                    self.lv2_instances.push(instance);
                    Box::new(SlotRunner::lv2(config, processor, channels))
                }
                Err(error) => {
                    warn!("slot {index}: {uri}: {error}");
                    self.infos.insert(
                        index,
                        SlotInfo {
                            name: config.spec.name(),
                            params: Vec::new(),
                            error: Some(error),
                            has_gui: false,
                            presets: Vec::new(),
                            sidechain: false,
                        },
                    );
                    Box::new(SlotRunner::passthrough(config, channels))
                }
            };
        }
        let SlotSpec::Clap {
            path,
            id,
            params,
            state,
        } = &config.spec
        else {
            return Box::new(SlotRunner::passthrough(config, channels));
        };
        match clap::load(path, id, params, state.as_deref(), self.rate) {
            Ok((mut instance, processor)) => {
                instance.slot = index;
                let latency = instance.latency_samples;
                self.latencies.insert(index, latency);
                self.infos.insert(index, instance.info.clone());
                self.instances.push(instance);
                Box::new(SlotRunner::clap(config, processor, channels, latency))
            }
            Err(error) => {
                warn!("slot {index}: {path} {id:?}: {error}");
                self.infos.insert(
                    index,
                    SlotInfo {
                        name: config.spec.name(),
                        params: Vec::new(),
                        error: Some(error),
                        has_gui: false,
                        presets: Vec::new(),
                        sidechain: false,
                    },
                );
                Box::new(SlotRunner::passthrough(config, channels))
            }
        }
    }
}

/// dB to linear; at or below −60 dB the gain is off.
fn linear(db: f32) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// The process's LV2 world, made on first use; an error naming the package to install
/// when lilv is missing.
fn lv2_host(host: &mut Option<Lv2Host>) -> Result<&Lv2Host, String> {
    if host.is_none() {
        *host = Some(Lv2Host::new()?);
    }
    Ok(host.as_ref().expect("just made"))
}
