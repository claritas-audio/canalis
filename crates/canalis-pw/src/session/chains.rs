//! Supervising `canalis-chain` processes: one per configured chain, restarted bypassed
//! when it dies, talked to over its stdin/stdout JSON lines.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use pipewire::channel::Sender;

use crate::config::ChainConfig;
use crate::model::{ChainMeters, ChainState, ChainStatus, SlotInfo};

/// A process that lived shorter than this before dying is "crashing"; its restarts back off.
const SHORT_LIFE: Duration = Duration::from_secs(5);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(10);
/// Quick crashes in a row after which the chain is left stopped instead of restarted.
const GIVE_UP_AFTER: u32 = 3;

/// What a chain process tells the loop thread (meters go straight into atomics instead).
pub(super) enum ChainEvent {
    Up {
        name: String,
    },
    Latency {
        name: String,
        samples: u32,
        slots: Vec<u32>,
    },
    SlotInfo {
        name: String,
        slot: usize,
        info: SlotInfo,
    },
    Gui {
        name: String,
        slot: usize,
        open: bool,
    },
    /// The process died of a fatal signal, in this slot or outside the slots.
    Crash {
        name: String,
        slot: Option<usize>,
    },
    /// The plugin's state to keep in the config, base64.
    State {
        name: String,
        slot: usize,
        state: String,
    },
    /// Control values a plugin's own UI changed; replace the slot's params map.
    Params {
        name: String,
        slot: usize,
        params: toml::Value,
    },
    /// The hardware insert's measured round trip, to keep in the slot; `None` when the
    /// click never came back.
    Measured {
        name: String,
        slot: usize,
        samples: Option<u32>,
    },
    /// The chain bypassed this top-level slot itself: it kept overrunning the cycle.
    Overload {
        name: String,
        slot: usize,
        slot_us: u64,
        cycle_us: u64,
    },
    /// The chain's peak load, % of the cycle.
    Load {
        name: String,
        percent: f32,
    },
    /// A recording's progress or end, as the chain reports it.
    Recording {
        name: String,
        recording: crate::model::Recording,
    },
    /// Not from a chain: the plugin scan thread is done (`session::plugins`).
    PluginsScanned(Vec<crate::config::PluginEntry>, Option<String>),
    /// `pid` tells a restarted chain's old process from the one now running.
    Exited {
        name: String,
        pid: u32,
    },
}

pub(super) struct Supervisor {
    binary: PathBuf,
    events: Sender<ChainEvent>,
    processes: HashMap<String, Process_>,
}

struct Process_ {
    child: Child,
    stdin: ChildStdin,
    started: Instant,
    status: ChainStatus,
    latency_samples: u32,
    slot_latencies: Vec<u32>,
    restarts: u32,
    /// Set when the process died; the next spawn sends every slot bypassed.
    degraded: bool,
    restart_delay: Duration,
    restart_at: Option<Instant>,
    error: Option<String>,
    meters: Arc<ChainMeters>,
    slot_infos: HashMap<usize, SlotInfo>,
    gui_open: HashMap<usize, bool>,
    /// The process died of a fatal signal (a plugin's doing, most likely).
    crashed: bool,
    /// The slot active at the last crash; kept across the bypassed restart, which does
    /// not load it at all.
    crashed_slot: Option<usize>,
    /// Crashes in a row that came within `SHORT_LIFE` of a start.
    quick_crashes: u32,
    /// By top-level slot: why the chain bypassed it on its own.
    overloads: HashMap<usize, String>,
    recording: Option<crate::model::Recording>,
    load: Option<f32>,
}

impl Supervisor {
    pub(super) fn new(events: Sender<ChainEvent>) -> Self {
        Supervisor {
            binary: locate_binary(),
            events,
            processes: HashMap::new(),
        }
    }

    /// Spawns the chain unless it runs or waits for its restart; `true` when state changed.
    pub(super) fn ensure(&mut self, config: &ChainConfig) -> bool {
        if self.processes.contains_key(&config.name) {
            return false;
        }
        self.spawn(config, false, 0, Arc::default(), None);
        true
    }

    /// Kills the chain's process and forgets it; `true` when there was one.
    pub(super) fn stop(&mut self, name: &str) -> bool {
        let Some(mut process) = self.processes.remove(name) else {
            return false;
        };
        let _ = process.child.kill();
        let _ = process.child.wait();
        info!("chain {name} stopped");
        true
    }

    /// Starts the chain afresh with every slot loaded, the crashed one too: what Start
    /// normally does after a crash.
    pub(super) fn restart(&mut self, config: &ChainConfig) {
        let meters = self
            .processes
            .get(&config.name)
            .map_or_else(Arc::default, |p| Arc::clone(&p.meters));
        self.stop(&config.name);
        info!("chain {} restarted normally", config.name);
        self.spawn(config, false, 0, meters, None);
    }

    /// The slot list is about to change: what the chain reported about slots is stale.
    pub(super) fn forget_slot_infos(&mut self, name: &str) {
        if let Some(process) = self.processes.get_mut(name) {
            process.slot_infos.clear();
            process.gui_open.clear();
        }
    }

    pub(super) fn send(&mut self, name: &str, line: &str) {
        let Some(process) = self.processes.get_mut(name) else {
            warn!("chain {name} is not running; dropped {line}");
            return;
        };
        debug!("chain {name} <- {line}");
        if writeln!(process.stdin, "{line}").is_err() {
            warn!("chain {name}: cannot write to its stdin");
        }
    }

    /// Applies one event; `true` when the published state changed.
    pub(super) fn on_event(&mut self, event: ChainEvent, configs: &[ChainConfig]) -> bool {
        match event {
            ChainEvent::Up { name } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                let degraded = process.degraded;
                process.status = if degraded {
                    ChainStatus::Degraded
                } else {
                    ChainStatus::Running
                };
                process.error = None;
                let off = process.crashed_slot.filter(|_| degraded);
                if let Some(config) = configs.iter().find(|c| c.name == name) {
                    let line = slots_line(&config.slots, degraded, off);
                    self.send(&name, &line);
                    if config.mute {
                        self.send(&name, &mute_line(true));
                    }
                    if let Some(line) = sends_line(config) {
                        self.send(&name, &line);
                    }
                    if config.key.is_some() {
                        self.send(&name, &key_line(true));
                    }
                    if config.auto_level {
                        self.send(&name, &auto_level_line(config));
                    }
                }
                true
            }
            ChainEvent::Latency {
                name,
                samples,
                slots,
            } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                let changed = process.latency_samples != samples || process.slot_latencies != slots;
                process.latency_samples = samples;
                process.slot_latencies = slots;
                changed
            }
            ChainEvent::SlotInfo { name, slot, info } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                debug!(
                    "chain {name} slot {slot}: {} ({} params{})",
                    info.name,
                    info.params.len(),
                    info.error
                        .as_ref()
                        .map_or(String::new(), |e| format!(", error: {e}"))
                );
                let previous = process.slot_infos.insert(slot, info);
                previous.as_ref() != process.slot_infos.get(&slot)
            }
            ChainEvent::Gui { name, slot, open } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                process.gui_open.insert(slot, open) != Some(open)
            }
            ChainEvent::Crash { name, slot } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                process.crashed = true;
                process.crashed_slot = slot;
                true
            }
            ChainEvent::Overload {
                name,
                slot,
                slot_us,
                cycle_us,
            } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                process.overloads.insert(
                    slot,
                    format!(
                        "{:.1}/{:.1} ms",
                        slot_us as f64 / 1000.0,
                        cycle_us as f64 / 1000.0
                    ),
                );
                true
            }
            ChainEvent::Load { name, percent } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                // The strip shows whole percents: a change below that is not worth a snapshot.
                let changed = process.load.is_none_or(|old| (old - percent).abs() >= 1.0);
                process.load = Some(percent);
                changed
            }
            ChainEvent::Recording { name, recording } => {
                let Some(process) = self.processes.get_mut(&name) else {
                    return false;
                };
                process.recording = Some(recording);
                true
            }
            ChainEvent::PluginsScanned(..)
            | ChainEvent::State { .. }
            | ChainEvent::Measured { .. }
            | ChainEvent::Params { .. } => false,
            ChainEvent::Exited { name, pid } => {
                // Start normally kills the old process and spawns a new one at once; the
                // old one's exit must not be taken for the new one's (waiting on it hung
                // the control plane).
                let Some(process) = self
                    .processes
                    .get_mut(&name)
                    .filter(|p| p.child.id() == pid)
                else {
                    return false;
                };
                let code = process.child.wait().ok().and_then(|s| s.code());
                let lived = process.started.elapsed();
                process.degraded = true;
                process.status = ChainStatus::Stopped;
                process.error = Some(match (process.crashed, process.crashed_slot, code) {
                    (true, Some(slot), _) => {
                        format!("crashed in slot {} after {:.0?}", slot + 1, lived)
                    }
                    (true, None, _) => format!("crashed outside the slots after {:.0?}", lived),
                    (false, _, Some(code)) => format!("exited with {code} after {:.0?}", lived),
                    (false, _, None) => format!("killed after {:.0?}", lived),
                });
                if lived < SHORT_LIFE {
                    process.restart_delay = (process.restart_delay * 2).min(MAX_RESTART_DELAY);
                    process.quick_crashes += 1;
                } else {
                    process.restart_delay = Duration::from_secs(1);
                    process.quick_crashes = 0;
                }
                if process.quick_crashes >= GIVE_UP_AFTER {
                    process.restart_at = None;
                    let why = process.error.take().unwrap_or_default();
                    process.error = Some(format!(
                        "{why}; stopped after {} quick crashes: Start normally to try again",
                        process.quick_crashes
                    ));
                    warn!("chain {name} keeps crashing; not restarting it");
                    return true;
                }
                process.restart_at = Some(Instant::now() + process.restart_delay);
                warn!(
                    "chain {name} {}; restarting bypassed in {:?}",
                    process.error.as_deref().unwrap_or(""),
                    process.restart_delay
                );
                true
            }
        }
    }

    /// Restarts whatever is due; `true` when something was spawned.
    pub(super) fn tick(&mut self, configs: &[ChainConfig]) -> bool {
        let due: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| p.restart_at.is_some_and(|at| at <= Instant::now()))
            .map(|(name, _)| name.clone())
            .collect();
        let mut changed = false;
        for name in due {
            let Some(old) = self.processes.remove(&name) else {
                continue;
            };
            let Some(config) = configs.iter().find(|c| c.name == name) else {
                continue;
            };
            self.spawn(
                config,
                true,
                old.restarts + 1,
                old.meters,
                Some(old.restart_delay),
            );
            if let Some(process) = self.processes.get_mut(&name) {
                process.crashed = old.crashed;
                process.crashed_slot = old.crashed_slot;
                process.quick_crashes = old.quick_crashes;
            }
            changed = true;
        }
        changed
    }

    pub(super) fn states(&self, configs: &[ChainConfig]) -> Vec<ChainState> {
        configs
            .iter()
            .map(|config| {
                let process = self.processes.get(&config.name);
                ChainState {
                    name: config.name.clone(),
                    label: config.label.clone(),
                    kind: config.kind,
                    slots: config.slots.clone(),
                    inputs: config.inputs.clone(),
                    outputs: config.outputs.clone(),
                    status: process.map_or(ChainStatus::Stopped, |p| p.status),
                    pid: process.map(|p| p.child.id()),
                    latency_samples: process.map_or(0, |p| p.latency_samples),
                    slot_latencies: process
                        .map(|p| p.slot_latencies.clone())
                        .unwrap_or_default(),
                    restarts: process.map_or(0, |p| p.restarts),
                    error: process.and_then(|p| p.error.clone()),
                    meters: process.map_or_else(Arc::default, |p| Arc::clone(&p.meters)),
                    slot_infos: process
                        .map(|p| p.slot_infos.iter().map(|(k, v)| (*k, v.clone())).collect())
                        .unwrap_or_default(),
                    gui_open: process
                        .map(|p| p.gui_open.iter().map(|(k, v)| (*k, *v)).collect())
                        .unwrap_or_default(),
                    crashed_slot: process.and_then(|p| p.crashed_slot),
                    restarting: process.is_some_and(|p| p.restart_at.is_some()),
                    overloads: process
                        .map(|p| p.overloads.iter().map(|(k, v)| (*k, v.clone())).collect())
                        .unwrap_or_default(),
                    recording: process.and_then(|p| p.recording.clone()),
                    load: process.and_then(|p| p.load),
                    auto_level: config.auto_level,
                    compare: config.compare.as_ref().map(|c| c.playing),
                    mute: config.mute,
                    color: config.color.clone(),
                    icon: config.icon.clone(),
                    collapsed: config.collapsed,
                    loudness_target: config.loudness_target,
                    gains: config.gains.clone(),
                    key: config.key.clone(),
                }
            })
            .collect()
    }

    pub(super) fn binary(&self) -> &Path {
        &self.binary
    }

    pub(super) fn running(&self) -> impl Iterator<Item = &str> {
        self.processes.keys().map(String::as_str)
    }

    fn spawn(
        &mut self,
        config: &ChainConfig,
        degraded: bool,
        restarts: u32,
        meters: Arc<ChainMeters>,
        restart_delay: Option<Duration>,
    ) {
        let spawned = Process::new(&self.binary)
            .args(["--node-name", &config.name])
            .args(["--label", &config.label])
            .args(["--kind", config.kind.as_str()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                warn!("cannot start {}: {e}", self.binary.display());
                return;
            }
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            warn!("chain {}: no pipes", config.name);
            let _ = child.kill();
            return;
        };
        let name = config.name.clone();
        let events = self.events.clone();
        let reader_meters = Arc::clone(&meters);
        let pid = child.id();
        let thread = std::thread::Builder::new()
            .name(format!("chain-{name}"))
            .spawn(move || read_reports(name, pid, stdout, events, reader_meters));
        if thread.is_err() {
            warn!("chain {}: no reader thread", config.name);
        }
        info!(
            "chain {} spawned as pid {}{}",
            config.name,
            child.id(),
            if degraded { " (bypassed)" } else { "" }
        );
        self.processes.insert(
            config.name.clone(),
            Process_ {
                child,
                stdin,
                started: Instant::now(),
                status: ChainStatus::Starting,
                latency_samples: 0,
                slot_latencies: Vec::new(),
                restarts,
                degraded,
                restart_delay: restart_delay.unwrap_or(Duration::from_secs(1)),
                restart_at: None,
                error: None,
                meters,
                slot_infos: HashMap::new(),
                gui_open: HashMap::new(),
                crashed: false,
                crashed_slot: None,
                quick_crashes: 0,
                overloads: HashMap::new(),
                recording: None,
                load: None,
            },
        );
    }
}

/// The chain's stdout: meters into the atomics, the rest to the loop thread.
fn read_reports(
    name: String,
    pid: u32,
    stdout: std::process::ChildStdout,
    events: Sender<ChainEvent>,
    meters: Arc<ChainMeters>,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(report) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match report.get("type").and_then(|t| t.as_str()) {
            Some("meters") => {
                let pair = |key: &str| -> [f32; 2] {
                    let values = report.get(key).and_then(|v| v.as_array());
                    std::array::from_fn(|c| {
                        values
                            .and_then(|v| v.get(c))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0) as f32
                    })
                };
                meters.set(pair("input"), pair("output"));
                if let Some(values) = report.get("loudness").and_then(|v| v.as_array()) {
                    let loudness: [f32; 4] = std::array::from_fn(|i| {
                        values.get(i).and_then(|v| v.as_f64()).unwrap_or(-200.0) as f32
                    });
                    meters.set_loudness(loudness);
                }
                let number = |key: &str| report.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
                meters.set_balance(number("balance") as f32);
                meters.set_auto_gain_db(number("auto_gain_db") as f32);
                meters.set_spectrum(report.get("spectrum").and_then(|v| v.as_array()).map(
                    |bands| {
                        bands
                            .iter()
                            .map(|b| b.as_f64().unwrap_or(-100.0) as f32)
                            .collect()
                    },
                ));
            }
            Some("up") => {
                let _ = events.send(ChainEvent::Up { name: name.clone() });
            }
            Some("latency") => {
                let samples = report.get("samples").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let slots = report
                    .get("slots")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().map(|v| v.as_u64().unwrap_or(0) as u32).collect())
                    .unwrap_or_default();
                let _ = events.send(ChainEvent::Latency {
                    name: name.clone(),
                    samples,
                    slots,
                });
            }
            Some("crash") => {
                let slot = report
                    .get("slot")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as usize);
                let _ = events.send(ChainEvent::Crash {
                    name: name.clone(),
                    slot,
                });
            }
            Some("params") => {
                let slot = report
                    .get("slot")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as usize);
                let params = report
                    .get("params")
                    .cloned()
                    .and_then(|v| toml::Value::try_from(v).ok());
                if let (Some(slot), Some(params)) = (slot, params) {
                    let _ = events.send(ChainEvent::Params {
                        name: name.clone(),
                        slot,
                        params,
                    });
                }
            }
            Some("overload") => {
                let number = |key: &str| report.get(key).and_then(|v| v.as_u64());
                if let (Some(slot), Some(slot_us), Some(cycle_us)) =
                    (number("slot"), number("slot_us"), number("cycle_us"))
                {
                    warn!(
                        "chain {name} bypassed slot {slot}: {slot_us} µs of a {cycle_us} µs cycle"
                    );
                    let _ = events.send(ChainEvent::Overload {
                        name: name.clone(),
                        slot: slot as usize,
                        slot_us,
                        cycle_us,
                    });
                }
            }
            Some("load") => {
                if let Some(percent) = report.get("percent").and_then(|v| v.as_f64()) {
                    let _ = events.send(ChainEvent::Load {
                        name: name.clone(),
                        percent: percent as f32,
                    });
                }
            }
            Some("recording") => {
                match serde_json::from_value::<crate::model::Recording>(report.clone()) {
                    Ok(recording) => {
                        if recording.done {
                            match &recording.error {
                                Some(e) => warn!("chain {name} recording {}: {e}", recording.path),
                                None => info!("chain {name} recorded {}", recording.path),
                            }
                        }
                        let _ = events.send(ChainEvent::Recording {
                            name: name.clone(),
                            recording,
                        });
                    }
                    Err(e) => warn!("chain {name}: bad recording report: {e}"),
                }
            }
            Some("measured") => {
                let slot = report
                    .get("slot")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as usize);
                let samples = report
                    .get("samples")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as u32);
                if let Some(slot) = slot {
                    let _ = events.send(ChainEvent::Measured {
                        name: name.clone(),
                        slot,
                        samples,
                    });
                }
            }
            Some("state") => {
                let slot = report
                    .get("slot")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as usize);
                let state = report.get("state").and_then(|v| v.as_str());
                if let (Some(slot), Some(state)) = (slot, state) {
                    let _ = events.send(ChainEvent::State {
                        name: name.clone(),
                        slot,
                        state: state.to_owned(),
                    });
                }
            }
            Some("gui") => {
                let slot = report
                    .get("slot")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as usize);
                let open = report.get("open").and_then(|v| v.as_bool());
                if let (Some(slot), Some(open)) = (slot, open) {
                    let _ = events.send(ChainEvent::Gui {
                        name: name.clone(),
                        slot,
                        open,
                    });
                }
            }
            Some("slot_info") => {
                let slot = report
                    .get("slot")
                    .and_then(|v| v.as_u64())
                    .map(|s| s as usize);
                let info = serde_json::from_value::<SlotInfo>(report.clone()).ok();
                if let (Some(slot), Some(info)) = (slot, info) {
                    let _ = events.send(ChainEvent::SlotInfo {
                        name: name.clone(),
                        slot,
                        info,
                    });
                }
            }
            _ => {}
        }
    }
    let _ = events.send(ChainEvent::Exited { name, pid });
}

/// The `mute` command for a chain.
pub(super) fn mute_line(mute: bool) -> String {
    serde_json::json!({ "type": "mute", "mute": mute }).to_string()
}

/// The `key` command: whether the chain's key input exists.
/// The `auto_level` command: the chain's target while the switch is on, else off.
pub(super) fn auto_level_line(config: &ChainConfig) -> String {
    let target = config.auto_level.then(|| {
        config
            .loudness_target
            .unwrap_or(crate::model::LOUDNESS_TARGET)
    });
    serde_json::json!({ "type": "auto_level", "target": target }).to_string()
}

pub(super) fn key_line(enabled: bool) -> String {
    serde_json::json!({ "type": "key", "enabled": enabled }).to_string()
}

/// The `sends` command: one channel pair per destination with its gain; a chain that
/// feeds nothing keeps the single pair it starts with.
pub(super) fn sends_line(config: &ChainConfig) -> Option<String> {
    let destinations = config.destinations();
    let mut gains_db: Vec<f32> = destinations.iter().map(|d| config.gain_db(d)).collect();
    if gains_db.is_empty() {
        gains_db.push(0.0);
    }
    Some(
        serde_json::json!({ "type": "sends", "count": gains_db.len(), "gains_db": gains_db })
            .to_string(),
    )
}

/// The `slots` command for a chain, optionally with every slot bypassed.
pub(super) fn slots_line(slots: &[toml::Value], bypass_all: bool, off: Option<usize>) -> String {
    let mut slots: Vec<serde_json::Value> = slots
        .iter()
        .filter_map(|s| serde_json::to_value(s).ok())
        .collect();
    if bypass_all {
        for slot in slots.iter_mut().filter_map(|s| s.as_object_mut()) {
            slot.insert("bypass".into(), serde_json::Value::Bool(true));
        }
    }
    if let Some(slot) = off
        .and_then(|i| slots.get_mut(i))
        .and_then(|s| s.as_object_mut())
    {
        slot.insert("disabled".into(), serde_json::Value::Bool(true));
    }
    serde_json::json!({ "type": "slots", "slots": slots }).to_string()
}

/// `$CANALIS_CHAIN_BIN`, else `canalis-chain` next to this executable, else `PATH`.
pub(super) fn locate_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CANALIS_CHAIN_BIN") {
        return PathBuf::from(path);
    }
    let sibling = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name("canalis-chain"));
    match sibling {
        Some(path) if path.is_file() => path,
        _ => PathBuf::from("canalis-chain"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slots_line_can_bypass_everything() {
        let slot: toml::Value = toml::from_str("[spec]\nkind = \"comp\"\n").expect("table");
        let plain = slots_line(std::slice::from_ref(&slot), false, None);
        assert_eq!(
            plain,
            r#"{"slots":[{"spec":{"kind":"comp"}}],"type":"slots"}"#
        );
        let bypassed = slots_line(std::slice::from_ref(&slot), true, None);
        assert!(
            slots_line(&[slot], true, Some(0)).contains(r#""disabled":true"#),
            "the crashed slot is not loaded"
        );
        assert!(bypassed.contains(r#""bypass":true"#));
    }

    #[test]
    fn the_auto_level_line_carries_the_target_while_on() {
        let mut config: ChainConfig =
            toml::from_str("name = \"canalis.x\"\nlabel = \"x\"\nkind = \"output\"").unwrap();
        assert_eq!(
            auto_level_line(&config),
            r#"{"target":null,"type":"auto_level"}"#
        );
        config.auto_level = true;
        assert!(
            auto_level_line(&config).contains(r#""target":-16.0"#),
            "the default target"
        );
        config.loudness_target = Some(-23.0);
        assert!(auto_level_line(&config).contains(r#""target":-23.0"#));
    }

    #[test]
    fn sends_line_counts_the_virtual_source_first_and_reads_gains_by_name() {
        let mut config = ChainConfig {
            name: "canalis.mic".into(),
            label: "Mic".into(),
            kind: crate::model::ChainKind::Mic,
            slots: Vec::new(),
            inputs: Vec::new(),
            outputs: vec!["headphones".into()],
            mute: false,
            color: None,
            icon: None,
            collapsed: false,
            loudness_target: None,
            auto_level: false,
            compare: None,
            key: None,
            gains: [("headphones".to_owned(), -12.0)].into_iter().collect(),
        };
        assert_eq!(
            sends_line(&config).as_deref(),
            Some(r#"{"count":2,"gains_db":[0.0,-12.0],"type":"sends"}"#)
        );
        config.kind = crate::model::ChainKind::Output;
        config.outputs.clear();
        assert_eq!(
            sends_line(&config).as_deref(),
            Some(r#"{"count":1,"gains_db":[0.0],"type":"sends"}"#)
        );
    }
}
