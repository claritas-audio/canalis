//! The soundboard in the service: `canalis-pads` mixes the pads in a real-time callback of
//! one stream, `canalis.soundboard`, a source like a mic. The reconciler links it to the
//! board's outputs (and the default output when monitoring); a chain that wants effects on
//! the pads takes it as an input. Files are decoded on threads of their own and kept.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use canalis_pads::{Command as PadCommand, Controller, Engine, Mode, RATE, Sound};
use log::{info, warn};
use pipewire::channel;
use pipewire::core::CoreRc;
use pipewire::spa::pod::Pod;
use pipewire::spa::utils::Direction as SpaDirection;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};

use super::Session;
use super::playback::format_pod;
use crate::SOUNDBOARD_NODE;
use crate::config::{ControlTarget, Pad, Soundboard};
use crate::model::{Direction, Graph, NodeId, PadState, PortId};

/// The most frames one callback asks for.
const MAX_QUANTUM: usize = 8192;
/// How soon after a command the loop looks at what the engine reported.
const REPORT_DELAY: Duration = Duration::from_millis(30);

pub(super) struct Board {
    /// By file: the decoded sound, or why it cannot play.
    sounds: HashMap<PathBuf, Result<Arc<Sound>, String>>,
    decoding: HashSet<PathBuf>,
    /// Pads pressed while their file was still decoding.
    waiting: Vec<String>,
    live: Option<Live>,
    decoded: channel::Sender<(PathBuf, Result<Sound, String>)>,
    wake: channel::Sender<()>,
}

/// The stream and the engine's controller, while connected and enabled.
struct Live {
    controller: Controller,
    _listener: StreamListener<Feed>,
    _stream: StreamRc,
}

/// What the callback owns.
struct Feed {
    engine: Engine,
    scratch: Vec<[f32; 2]>,
}

impl Board {
    pub(super) fn new(
        decoded: channel::Sender<(PathBuf, Result<Sound, String>)>,
        wake: channel::Sender<()>,
    ) -> Board {
        Board {
            sounds: HashMap::new(),
            decoding: HashSet::new(),
            waiting: Vec::new(),
            live: None,
            decoded,
            wake,
        }
    }

    /// The stream belonged to a connection that is gone.
    pub(super) fn disconnected(&mut self) {
        self.live = None;
    }

    pub(super) fn states(&self, board: &Soundboard) -> Vec<PadState> {
        board
            .pads
            .iter()
            .map(|pad| {
                let sound = self.sounds.get(&pad.path);
                PadState {
                    name: pad.name.clone(),
                    length: sound.and_then(|s| s.as_ref().ok()).map(|s| s.seconds()),
                    error: sound.and_then(|s| s.as_ref().err()).cloned(),
                    playing: self
                        .live
                        .as_ref()
                        .and_then(|l| l.controller.elapsed(pad.slot))
                        .map(|d| d.as_secs_f32()),
                }
            })
            .collect()
    }

    /// Decodes every file on the board not yet known and forgets the ones it dropped.
    fn load(&mut self, board: &Soundboard) {
        self.sounds
            .retain(|path, _| board.pads.iter().any(|p| p.path == *path));
        for pad in &board.pads {
            if self.sounds.contains_key(&pad.path) || !self.decoding.insert(pad.path.clone()) {
                continue;
            }
            let path = pad.path.clone();
            let decoded = self.decoded.clone();
            std::thread::spawn(move || {
                let sound = canalis_pads::decode(&path);
                let _ = decoded.send((path, sound));
            });
        }
    }

    fn send(&mut self, command: PadCommand) -> bool {
        let Some(live) = &mut self.live else {
            warn!("the soundboard is not running");
            return false;
        };
        if !live.controller.send(command) {
            warn!("the soundboard's queue is full; command dropped");
            return false;
        }
        true
    }

    fn wake_in(&self, delay: Duration) {
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = wake.send(());
        });
    }
}

impl Session {
    pub(super) fn load_pads(&mut self) {
        self.board.load(&self.config.soundboard);
    }

    pub(super) fn set_soundboard(&mut self, board: Soundboard) {
        let level = board.level_db;
        self.config.soundboard = board;
        self.save_config();
        if !self.config.soundboard.enabled {
            self.board.live = None;
        } else if self.board.live.is_some() {
            self.board.send(PadCommand::Level {
                gain: db_to_gain(level),
            });
        }
        self.load_pads();
        self.changed();
    }

    pub(super) fn rename_pad(&mut self, name: &str, to: String) {
        if self.config.soundboard.pad(&to).is_some() {
            warn!("a pad named {to} exists; not renaming {name}");
            return;
        }
        let Some(pad) = self
            .config
            .soundboard
            .pads
            .iter_mut()
            .find(|p| p.name == name)
        else {
            return;
        };
        pad.name.clone_from(&to);
        for control in &mut self.config.controls {
            if let ControlTarget::Pad { name: pad } = &mut control.target
                && pad == name
            {
                pad.clone_from(&to);
            }
        }
        self.save_config();
        self.changed();
    }

    pub(super) fn press_pad(&mut self, name: &str) {
        let Some(pad) = self.config.soundboard.pad(name).cloned() else {
            warn!("no pad named {name}");
            return;
        };
        match self.board.sounds.get(&pad.path) {
            Some(Ok(sound)) => {
                let sound = Arc::clone(sound);
                self.start_pad(&pad, sound);
            }
            Some(Err(e)) => warn!("pad {name} cannot play: {e}"),
            None => {
                self.board.waiting.push(pad.name.clone());
                self.load_pads();
            }
        }
    }

    pub(super) fn release_pad(&mut self, name: &str) {
        self.board.waiting.retain(|w| w != name);
        if let Some(pad) = self.config.soundboard.pad(name)
            && pad.mode == Mode::Hold
        {
            let id = pad.slot;
            self.stop_command(PadCommand::Release { id });
        }
    }

    pub(super) fn stop_pad(&mut self, name: &str) {
        self.board.waiting.retain(|w| w != name);
        if let Some(id) = self.config.soundboard.pad(name).map(|p| p.slot) {
            self.stop_command(PadCommand::Stop { id });
        }
    }

    pub(super) fn stop_pads(&mut self) {
        self.board.waiting.clear();
        self.stop_command(PadCommand::StopAll);
    }

    /// Takes in what the engine reported; runs shortly after every command, at a pad's
    /// expected end, and on the tick.
    pub(super) fn pads_report(&mut self) {
        if let Some(live) = &mut self.board.live
            && live.controller.drain()
        {
            self.changed();
        }
    }

    pub(super) fn pad_decoded(&mut self, path: PathBuf, sound: Result<Sound, String>) {
        self.board.decoding.remove(&path);
        if !self.config.soundboard.pads.iter().any(|p| p.path == path) {
            return;
        }
        if let Err(e) = &sound {
            warn!("{}: {e}", path.display());
        }
        let sound = sound.map(Arc::new);
        self.board.sounds.insert(path.clone(), sound.clone());
        let waiting = std::mem::take(&mut self.board.waiting);
        let (now, later): (Vec<String>, Vec<String>) = waiting.into_iter().partition(|name| {
            self.config
                .soundboard
                .pad(name)
                .is_some_and(|p| p.path == path)
        });
        self.board.waiting = later;
        if let Ok(sound) = sound {
            for name in now {
                if let Some(pad) = self.config.soundboard.pad(&name).cloned() {
                    self.start_pad(&pad, Arc::clone(&sound));
                }
            }
        }
        self.changed();
    }

    fn start_pad(&mut self, pad: &Pad, sound: Arc<Sound>) {
        let length = Duration::from_secs_f32(sound.seconds());
        let press = PadCommand::Press(canalis_pads::Pad {
            id: pad.slot,
            sound,
            gain: db_to_gain(pad.gain_db),
            mode: pad.mode,
            looped: pad.looped,
            choke: pad.choke,
        });
        if self.board.send(press) {
            info!("pad {} pressed", pad.name);
            self.board.wake_in(REPORT_DELAY);
            if !pad.looped {
                self.board.wake_in(length + REPORT_DELAY);
            }
        }
    }

    fn stop_command(&mut self, command: PadCommand) {
        if self.board.send(command) {
            self.board.wake_in(REPORT_DELAY);
        }
    }

    /// The stream while the board is enabled, and its links: the board's outputs and,
    /// when it monitors, the default output; any other link from it goes.
    pub(super) fn reconcile_pads(&mut self) {
        let board = &self.config.soundboard;
        if !board.enabled {
            return;
        }
        if self.board.live.is_none()
            && let Some(connection) = &self.connection
        {
            match open(&connection.core, db_to_gain(board.level_db)) {
                Ok(live) => {
                    info!("soundboard up");
                    self.board.live = Some(live);
                }
                Err(e) => warn!("cannot open the soundboard: {e}"),
            }
        }
        let graph = self.mirror.snapshot(true);
        let Some(from) = graph.node_by_name(SOUNDBOARD_NODE) else {
            return;
        };
        let mut targets = board.outputs.clone();
        if board.monitor
            && let Some(sink) = graph.defaults.sink.clone()
            && !targets.contains(&sink)
        {
            targets.push(sink);
        }
        let wanted: Vec<(PortId, PortId)> = targets
            .iter()
            .filter_map(|t| graph.node_by_name(t))
            .flat_map(|to| stereo_pairs(&graph, from.id, to.id))
            .collect();
        for (output, input) in &wanted {
            if graph.link_between(*output, *input).is_none()
                && self.attempt(format!("soundboard link {} -> {}", output.0, input.0))
            {
                self.create_link(*output, *input);
            }
        }
        let stale: Vec<u32> = graph
            .links
            .iter()
            .filter(|l| graph.port(l.output).is_some_and(|p| p.node == from.id))
            .filter(|l| !wanted.contains(&(l.output, l.input)))
            .map(|l| l.id)
            .collect();
        for id in stale {
            self.destroy_global(id);
        }
    }

    /// A link drawn from the soundboard (in the Graph) is one more output of the board.
    pub(super) fn soundboard_link(&mut self, input_node: &str) {
        let board = &mut self.config.soundboard;
        if !board.outputs.iter().any(|o| o == input_node) {
            board.outputs.push(input_node.to_owned());
            self.save_config();
            self.changed();
        }
    }

    /// A link from the soundboard removed: that output goes, or the monitor if it was it.
    pub(super) fn soundboard_unlink(&mut self, input_node: &str) {
        let default_sink = self.mirror.snapshot(false).defaults.sink;
        let board = &mut self.config.soundboard;
        if board.outputs.iter().any(|o| o == input_node) {
            board.outputs.retain(|o| o != input_node);
        } else if default_sink.as_deref() == Some(input_node) {
            board.monitor = false;
        }
        self.save_config();
        self.changed();
    }
}

/// The soundboard's stream: a source apps and chains see like a mic, mixed by the engine
/// in the real-time callback.
fn open(core: &CoreRc, level: f32) -> Result<Live, String> {
    let (mut controller, engine) = canalis_pads::pair();
    controller.send(PadCommand::Level { gain: level });
    let props = pipewire::properties::properties! {
        "node.name" => SOUNDBOARD_NODE,
        "node.description" => "Soundboard",
        "media.class" => "Audio/Source",
        "media.type" => "Audio",
        "node.autoconnect" => "false",
        // Runs with nothing linked too, so a press is played (to no one) now rather than
        // queued until an output appears.
        "node.always-process" => "true",
        // Never the default input WirePlumber falls back to.
        "priority.session" => "1",
    };
    let stream = StreamRc::new(core.clone(), SOUNDBOARD_NODE, props).map_err(|e| e.to_string())?;
    let feed = Feed {
        engine,
        scratch: vec![[0.0; 2]; MAX_QUANTUM],
    };
    let listener = stream
        .add_local_listener_with_user_data(feed)
        .process(|stream, feed| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested() as usize;
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let stride = 2 * size_of::<f32>();
            let room = data.as_raw().maxsize as usize / stride;
            let frames = if requested == 0 {
                room
            } else {
                requested.min(room)
            }
            .min(feed.scratch.len());
            let mixed = &mut feed.scratch[..frames];
            feed.engine.process(mixed);
            if let Some(bytes) = data.data() {
                for (dst, frame) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(mixed.iter()) {
                    dst[..4].copy_from_slice(&frame[0].to_le_bytes());
                    dst[4..].copy_from_slice(&frame[1].to_le_bytes());
                }
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (frames * stride) as u32;
        })
        .register()
        .map_err(|e| e.to_string())?;
    let format = format_pod(2, RATE);
    let mut params = [Pod::from_bytes(&format).ok_or("a bad format pod")?];
    stream
        .connect(
            SpaDirection::Output,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;
    Ok(Live {
        controller,
        _listener: listener,
        _stream: stream,
    })
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The soundboard's FL/FR into `to`'s inputs of the same channel, or its first two inputs
/// (a mono input takes both).
fn stereo_pairs(graph: &Graph, from: NodeId, to: NodeId) -> Vec<(PortId, PortId)> {
    let outs: Vec<_> = graph
        .ports_of(from)
        .filter(|p| p.direction == Direction::Output)
        .collect();
    let ins: Vec<_> = graph
        .ports_of(to)
        .filter(|p| p.direction == Direction::Input)
        .collect();
    outs.iter()
        .enumerate()
        .filter_map(|(i, out)| {
            let same = ins
                .iter()
                .find(|p| p.channel.is_some() && p.channel == out.channel);
            let input = same.or_else(|| ins.get(i.min(ins.len().saturating_sub(1))))?;
            Some((out.id, input.id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pad_reports_its_length_or_why_it_cannot_play() {
        let (decoded, _) = channel::channel();
        let (wake, _) = channel::channel();
        let mut board = Board::new(decoded, wake);
        let pad = |name: &str, path: &str, slot| Pad {
            name: name.into(),
            path: path.into(),
            slot,
            gain_db: 0.0,
            color: None,
            mode: Mode::OneShot,
            looped: false,
            choke: 0,
        };
        let config = Soundboard {
            enabled: true,
            pads: vec![pad("ding", "/ding.wav", 0), pad("gone", "/gone.wav", 1)],
            ..Soundboard::default()
        };
        board.sounds.insert(
            "/ding.wav".into(),
            Ok(Arc::new(Sound {
                frames: vec![[0.0; 2]; RATE as usize / 2],
            })),
        );
        board
            .sounds
            .insert("/gone.wav".into(), Err("No such file".into()));
        let states = board.states(&config);
        assert_eq!(states[0].length, Some(0.5));
        assert_eq!(states[0].playing, None);
        assert_eq!(states[1].error.as_deref(), Some("No such file"));
    }
}
