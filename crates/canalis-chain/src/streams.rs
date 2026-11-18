//! The capture + playback stream pair of one chain process. The capture stream only
//! triggers the playback stream; the playback stream's process callback dequeues both
//! buffers, runs the chain and queues them back, as `module-filter-chain` does.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use assert_no_alloc::assert_no_alloc;
use log::{info, warn};
use pipewire::core::CoreRc;
use pipewire::properties::PropertiesBox;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use pipewire::spa::param::format::{MediaSubtype, MediaType};
use pipewire::spa::param::format_utils;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Property, PropertyFlags, Value};
use pipewire::spa::sys;
use pipewire::spa::utils::Direction;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use rtrb::{Consumer, Producer};

use canalis_chain::{
    Chain, ChainKind, INSERT_CHANNELS, MAX_QUANTUM, MAX_SENDS, Message, Meters, SlotRunner,
};

/// Failed playback triggers: counted on the audio thread, logged by the main loop.
pub static TRIGGER_FAILURES: AtomicU32 = AtomicU32::new(0);

const CHANNELS: u32 = 2;

/// Keeps the audio callbacks off a stream the loop thread reconnects: a trigger or a
/// dequeue on a stream inside `pw_stream_connect` segfaults in libpipewire.
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    busy: AtomicU32,
}

impl Gate {
    /// Audio thread: runs `f` only while the gate is open. Two atomic stores, no wait.
    fn pass(&self, f: impl FnOnce()) {
        // SeqCst on both sides: either `shut` sees this callback busy, or this callback
        // sees the gate shut.
        self.busy.fetch_add(1, Ordering::SeqCst);
        if self.open.load(Ordering::SeqCst) {
            f();
        }
        self.busy.fetch_sub(1, Ordering::SeqCst);
    }

    /// Loop thread: runs `f` with no callback inside the gate, then opens it.
    fn shut<T>(&self, f: impl FnOnce() -> T) -> T {
        self.open.store(false, Ordering::SeqCst);
        while self.busy.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
        let out = f();
        self.open.store(true, Ordering::SeqCst);
        out
    }
}

/// Both streams and their listeners; drop it to leave the graph.
pub struct Pair {
    capture: StreamRc,
    playback: StreamRc,
    /// The key input; a stream object from the start, a node only while a key is set.
    key: StreamRc,
    key_connected: std::cell::Cell<bool>,
    /// The hardware insert's return, the same way; its send is a pair on the playback end.
    insert_return: StreamRc,
    /// Wanted by the control plane; the callback follows it once the format says so.
    insert: Arc<AtomicBool>,
    /// Destination pairs the playback end carries right now.
    sends: std::cell::Cell<usize>,
    /// The last published processing latency, kept across a reconnect.
    latency: std::cell::Cell<u32>,
    gate: Arc<Gate>,
    _capture_listener: StreamListener<StreamRc>,
    _playback_listener: StreamListener<Playback>,
    /// The null source apps record from, for mic and cable chains; dies with us.
    _device: Option<pipewire::node::Node>,
}

/// User data of the playback stream: everything the process callback touches.
struct Playback {
    capture: StreamRc,
    key: StreamRc,
    insert_return: StreamRc,
    insert_wanted: Arc<AtomicBool>,
    /// The playback end carries the insert pair after the send pairs; set with the
    /// format of the reconnected stream, never ahead of it.
    insert: bool,
    chain: Chain,
    channels: usize,
    messages: Consumer<Message>,
    garbage: Producer<Box<SlotRunner>>,
}

/// What the callback needs from the control side, handed over at connect time.
pub struct Wiring {
    pub meters: Arc<Meters>,
    pub messages: Consumer<Message>,
    pub garbage: Producer<Box<SlotRunner>>,
    /// Runs on the loop thread when the format is negotiated: `(rate, channels)`.
    pub on_format: Box<dyn FnMut(u32, u32)>,
}

impl Pair {
    /// Reconnects the playback end with one stereo pair per destination: the main pair
    /// as FL/FR, the others as `SEND_POSITIONS` in order. Links are re-made by the
    /// control plane; the capture end stays as it is.
    pub fn set_sends(&self, sends: usize) -> Result<(), pipewire::Error> {
        let sends = sends.clamp(1, MAX_SENDS);
        if sends == self.sends.get() {
            return Ok(());
        }
        let before = self.sends.replace(sends);
        self.gate
            .shut(|| self.reconnect_playback())
            .inspect_err(|_| self.sends.set(before))
    }

    /// Adds the insert pair to the playback end and brings up the return input, or
    /// takes both away. The control plane links them to the gear.
    pub fn set_insert(&self, enabled: bool) -> Result<(), pipewire::Error> {
        if enabled == self.insert.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.insert.store(enabled, Ordering::Relaxed);
        self.gate.shut(|| {
            if let Err(e) = self.reconnect_playback() {
                self.insert.store(!enabled, Ordering::Relaxed);
                return Err(e);
            }
            if enabled {
                let format = format_pod(1, false);
                let latency = process_latency_pod(0);
                let mut params = [
                    Pod::from_bytes(&format).expect("a valid format pod"),
                    Pod::from_bytes(&latency).expect("a valid latency pod"),
                ];
                self.insert_return.connect(
                    Direction::Input,
                    None,
                    StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | async_flag(),
                    &mut params,
                )?;
            } else {
                self.insert_return.disconnect()?;
            }
            Ok(())
        })?;
        info!("hardware insert {}", if enabled { "up" } else { "gone" });
        Ok(())
    }

    fn reconnect_playback(&self) -> Result<(), pipewire::Error> {
        let sends = self.sends.get();
        let insert = self.insert.load(Ordering::Relaxed);
        self.playback.disconnect()?;
        let format = format_pod(sends, insert);
        let latency = process_latency_pod(self.latency.get());
        let mut params = [
            Pod::from_bytes(&format).expect("a valid format pod"),
            Pod::from_bytes(&latency).expect("a valid latency pod"),
        ];
        self.playback.connect(
            Direction::Output,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | StreamFlags::TRIGGER,
            &mut params,
        )?;
        info!(
            "playback end reconnected with {sends} pair(s){}",
            if insert { " and the insert pair" } else { "" }
        );
        Ok(())
    }

    /// Connects or disconnects the key input, `<name>.key`: a capture stream the callback
    /// reads like the main one and hands to the slots with a sidechain.
    pub fn set_key(&self, enabled: bool) -> Result<(), pipewire::Error> {
        if enabled == self.key_connected.get() {
            return Ok(());
        }
        self.gate.shut(|| {
            if enabled {
                let format = format_pod(1, false);
                let latency = process_latency_pod(0);
                let mut params = [
                    Pod::from_bytes(&format).expect("a valid format pod"),
                    Pod::from_bytes(&latency).expect("a valid latency pod"),
                ];
                self.key.connect(
                    Direction::Input,
                    None,
                    StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | async_flag(),
                    &mut params,
                )
            } else {
                self.key.disconnect()
            }
        })?;
        self.key_connected.set(enabled);
        info!("key input {}", if enabled { "up" } else { "gone" });
        Ok(())
    }

    /// Publishes the chain's processing latency on both ends.
    pub fn set_process_latency(&self, samples: u32) -> Result<(), pipewire::Error> {
        self.latency.set(samples);
        let bytes = process_latency_pod(samples);
        for stream in [&self.capture, &self.playback] {
            let mut params = [Pod::from_bytes(&bytes).expect("a valid latency pod")];
            stream.update_params(&mut params)?;
        }
        Ok(())
    }
}

pub fn connect(
    core: &CoreRc,
    name: &str,
    label: &str,
    kind: ChainKind,
    wiring: Wiring,
) -> Result<Pair, pipewire::Error> {
    let Wiring {
        meters,
        messages,
        garbage,
        mut on_format,
    } = wiring;
    let group = format!("canalis-chain-{}", std::process::id());
    let device = if kind.source_device() {
        let mut props = PropertiesBox::new();
        props.insert("factory.name", "support.null-audio-sink");
        props.insert("node.name", kind.source_node_name(name));
        props.insert("node.description", label);
        props.insert("node.nick", label);
        props.insert("media.class", "Audio/Source/Virtual");
        props.insert("audio.position", "[ FL FR ]");
        props.insert("monitor.channel-volumes", "true");
        // Not in the streams' link group: nodes of one group are scheduled as one, which
        // starves the null source of what the playback end writes into it.
        Some(core.create_object::<pipewire::node::Node>("adapter", &props)?)
    } else {
        None
    };
    let capture = StreamRc::new(
        core.clone(),
        "capture",
        end_properties(
            &group,
            &kind.capture_node_name(name),
            label,
            kind.capture_class(),
            "Capture",
        ),
    )?;
    let playback = StreamRc::new(
        core.clone(),
        "playback",
        end_properties(
            &group,
            &kind.playback_node_name(name),
            &format!("{label} (out)"),
            kind.playback_class(),
            "Playback",
        ),
    )?;
    let key = StreamRc::new(
        core.clone(),
        "key",
        end_properties(
            &group,
            &kind.key_node_name(name),
            &format!("{label} (key)"),
            "Stream/Input/Audio",
            "Capture",
        ),
    )?;
    let insert_return = StreamRc::new(
        core.clone(),
        "insert-return",
        end_properties(
            &group,
            &kind.insert_return_node_name(name),
            &format!("{label} (insert return)"),
            "Stream/Input/Audio",
            "Capture",
        ),
    )?;
    let insert = Arc::new(AtomicBool::new(false));
    // Shut until the playback end is connected below.
    let gate = Arc::new(Gate::default());

    let capture_gate = Arc::clone(&gate);
    let capture_listener = capture
        .add_local_listener_with_user_data(playback.clone())
        .process(move |_capture, playback| {
            capture_gate.pass(|| {
                if playback.trigger_process().is_err() {
                    TRIGGER_FAILURES.fetch_add(1, Ordering::Relaxed);
                }
            });
        })
        .register()?;

    let playback_listener = playback
        .add_local_listener_with_user_data(Playback {
            capture: capture.clone(),
            key: key.clone(),
            insert_return: insert_return.clone(),
            insert_wanted: Arc::clone(&insert),
            insert: false,
            chain: Chain::new(meters),
            channels: 0,
            messages,
            garbage,
        })
        .param_changed(move |_stream, data, id, param| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                return;
            };
            let Ok((MediaType::Audio, MediaSubtype::Raw)) = format_utils::parse_format(param)
            else {
                return;
            };
            let mut format = AudioInfoRaw::new();
            if format.parse(param).is_err() {
                warn!("unreadable audio format");
                return;
            }
            data.channels = format.channels() as usize;
            data.insert = data.insert_wanted.load(Ordering::Relaxed);
            info!(
                "format: {} Hz, {} channels, planar f32",
                format.rate(),
                format.channels()
            );
            on_format(format.rate(), format.channels());
        })
        .process({
            let gate = Arc::clone(&gate);
            move |stream, data| gate.pass(|| assert_no_alloc(|| process(stream, data)))
        })
        .register()?;

    let format = format_pod(1, false);
    let latency = process_latency_pod(0);
    let mut params = [
        Pod::from_bytes(&format).expect("a valid format pod"),
        Pod::from_bytes(&latency).expect("a valid latency pod"),
    ];
    capture.connect(
        Direction::Input,
        None,
        StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | async_flag(),
        &mut params,
    )?;
    let mut params = [
        Pod::from_bytes(&format).expect("a valid format pod"),
        Pod::from_bytes(&latency).expect("a valid latency pod"),
    ];
    gate.shut(|| {
        playback.connect(
            Direction::Output,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS | StreamFlags::TRIGGER,
            &mut params,
        )
    })?;

    Ok(Pair {
        capture,
        playback,
        key,
        key_connected: std::cell::Cell::new(false),
        insert_return,
        insert,
        sends: std::cell::Cell::new(1),
        latency: std::cell::Cell::new(0),
        gate,
        _capture_listener: capture_listener,
        _playback_listener: playback_listener,
        _device: device,
    })
}

/// `PW_STREAM_FLAG_ASYNC`, which pipewire-rs 0.10 does not name yet.
fn async_flag() -> StreamFlags {
    StreamFlags::from_bits_retain(pipewire::sys::pw_stream_flags_PW_STREAM_FLAG_ASYNC)
}

/// One cycle: the newest capture buffer into the playback buffer through the chain.
/// Real-time: no allocation, no logging, no panics.
fn process(playback: &pipewire::stream::Stream, data: &mut Playback) {
    while let Ok(message) = data.messages.pop() {
        if let Some(old) = data.chain.apply(message)
            && let Err(rtrb::PushError::Full(old)) = data.garbage.push(old)
        {
            // Freeing here would allocate on the audio thread; a leak is the lesser evil.
            std::mem::forget(old);
        }
    }
    let Some(mut out_buffer) = playback.dequeue_buffer() else {
        return;
    };
    let requested = out_buffer.requested() as usize;
    let mut in_buffer = data.capture.dequeue_buffer();
    // Drop every capture buffer but the newest, as filter-chain does, so latency never piles up.
    while let Some(newer) = data.capture.dequeue_buffer() {
        in_buffer = Some(newer);
    }
    let in_datas = in_buffer.as_mut().map_or(&mut [][..], |b| b.datas_mut());
    // The key stream has no node until a key is set; dequeuing then just yields nothing.
    let mut key_buffer = data.key.dequeue_buffer();
    while let Some(newer) = data.key.dequeue_buffer() {
        key_buffer = Some(newer);
    }
    let key_datas = key_buffer.as_mut().map_or(&mut [][..], |b| b.datas_mut());
    let mut ret_buffer = data.insert_return.dequeue_buffer();
    while let Some(newer) = data.insert_return.dequeue_buffer() {
        ret_buffer = Some(newer);
    }
    let ret_datas = ret_buffer.as_mut().map_or(&mut [][..], |b| b.datas_mut());

    let channels = data.channels.min(MAX_CHANNELS);
    let in_frames = in_datas
        .first()
        .map_or(0, |d| d.chunk().size() as usize / size_of::<f32>());
    let frames = requested.max(in_frames).min(MAX_QUANTUM);

    // The main pair is processed in place from the capture buffer; every other pair
    // gets a gained copy of it afterwards.
    let mut io: [&mut [f32]; MAX_CHANNELS] = std::array::from_fn(|_| &mut [][..]);
    let mut io_len = 0;
    for (c, out) in out_buffer.datas_mut().iter_mut().enumerate().take(channels) {
        let max = out.as_raw().maxsize as usize / size_of::<f32>();
        let n = frames.min(max);
        let chunk = out.chunk_mut();
        *chunk.offset_mut() = 0;
        *chunk.stride_mut() = size_of::<f32>() as i32;
        *chunk.size_mut() = (n * size_of::<f32>()) as u32;
        let Some(bytes) = out.data() else {
            continue;
        };
        let Some(samples) = as_f32_mut(bytes, n) else {
            continue;
        };
        let copied = match in_datas.get_mut(c).filter(|_| c < CHANNELS as usize) {
            Some(src) => {
                let in_n = (src.chunk().size() as usize / size_of::<f32>()).min(n);
                src.data()
                    .and_then(|bytes| as_f32_mut(bytes, in_n))
                    .map_or(0, |src| {
                        samples[..in_n].copy_from_slice(src);
                        in_n
                    })
            }
            None => 0,
        };
        samples[copied..].fill(0.0);
        io[io_len] = samples;
        io_len += 1;
    }
    let mut key_io: [&[f32]; CHANNELS as usize] = [&[]; CHANNELS as usize];
    let mut key_len = 0;
    for data in key_datas.iter_mut().take(CHANNELS as usize) {
        let n = (data.chunk().size() as usize / size_of::<f32>()).min(frames);
        if let Some(samples) = data.data().and_then(|bytes| as_f32_mut(bytes, n)) {
            key_io[key_len] = samples;
            key_len += 1;
        }
    }
    let mut ret_io: [&[f32]; INSERT_CHANNELS] = [&[]; INSERT_CHANNELS];
    let mut ret_len = 0;
    for data in ret_datas.iter_mut().take(INSERT_CHANNELS) {
        let n = (data.chunk().size() as usize / size_of::<f32>()).min(frames);
        if let Some(samples) = data.data().and_then(|bytes| as_f32_mut(bytes, n)) {
            ret_io[ret_len] = samples;
            ret_len += 1;
        }
    }
    // With the insert up, the last pair of the playback end is its send.
    let insert_len = if data.insert && io_len >= 2 * INSERT_CHANNELS {
        INSERT_CHANNELS
    } else {
        0
    };
    let main_len = io_len.min(CHANNELS as usize);
    let (rest, insert_pair) = io[..io_len].split_at_mut(io_len - insert_len);
    let (main, extra) = rest.split_at_mut(main_len);
    data.chain
        .process(main, &key_io[..key_len], &ret_io[..ret_len]);
    data.chain.fan_out(main, extra);
    for (c, channel) in insert_pair.iter_mut().enumerate() {
        match data.chain.insert_send() {
            Some(send) => {
                let n = frames.min(channel.len());
                channel[..n].copy_from_slice(&send[c * MAX_QUANTUM..c * MAX_QUANTUM + n]);
            }
            None => channel.fill(0.0),
        }
    }
}

/// The buffer's bytes as `n` samples, when aligned and large enough.
fn as_f32_mut(bytes: &mut [u8], n: usize) -> Option<&mut [f32]> {
    let ptr = bytes.as_mut_ptr().cast::<f32>();
    if !ptr.is_aligned() || bytes.len() < n * size_of::<f32>() {
        return None;
    }
    // SAFETY: the pointer is aligned for f32 and `n` samples lie inside `bytes`, which we
    // hold exclusively for the lifetime of the returned slice.
    Some(unsafe { std::slice::from_raw_parts_mut(ptr, n) })
}

fn end_properties(
    group: &str,
    node_name: &str,
    description: &str,
    media_class: &str,
    category: &str,
) -> PropertiesBox {
    let mut props = PropertiesBox::new();
    props.insert("node.name", node_name);
    props.insert("node.description", description);
    props.insert("node.nick", description);
    props.insert("media.class", media_class);
    props.insert("media.type", "Audio");
    props.insert("media.category", category);
    props.insert("media.role", "DSP");
    props.insert("node.link-group", group);
    props.insert("node.group", group);
    props.insert("node.virtual", "true");
    props.insert("node.autoconnect", "false");
    props.insert("audio.channels", CHANNELS.to_string());
    props.insert("audio.position", "[ FL FR ]");
    props.insert("resample.prefill", "true");
    props
}

/// `SPA_PARAM_ProcessLatency` with the delay in samples at the graph rate.
fn process_latency_pod(samples: u32) -> Vec<u8> {
    let object = Object {
        type_: sys::SPA_TYPE_OBJECT_ParamProcessLatency,
        id: sys::SPA_PARAM_ProcessLatency,
        properties: vec![Property {
            key: sys::SPA_PARAM_PROCESS_LATENCY_rate,
            flags: PropertyFlags::empty(),
            value: Value::Int(samples as i32),
        }],
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .expect("a latency pod serializes")
        .0
        .into_inner()
}

/// The channel positions of send pairs 1.., mirrored by name in `canalis-pw`'s
/// `send_channels`. They have to be positions PipeWire's channel mixer knows: it folds
/// AUX and anything else past 63 onto the MONO bit and then mutes those channels.
const SEND_POSITIONS: [[u32; 2]; MAX_SENDS - 1] = [
    [sys::SPA_AUDIO_CHANNEL_RL, sys::SPA_AUDIO_CHANNEL_RR],
    [sys::SPA_AUDIO_CHANNEL_SL, sys::SPA_AUDIO_CHANNEL_SR],
    [sys::SPA_AUDIO_CHANNEL_FLC, sys::SPA_AUDIO_CHANNEL_FRC],
    [sys::SPA_AUDIO_CHANNEL_RLC, sys::SPA_AUDIO_CHANNEL_RRC],
    [sys::SPA_AUDIO_CHANNEL_TFL, sys::SPA_AUDIO_CHANNEL_TFR],
    [sys::SPA_AUDIO_CHANNEL_TRL, sys::SPA_AUDIO_CHANNEL_TRR],
];

/// The hardware insert's send pair, after the send pairs; `insert_channels` in `canalis-pw`.
const INSERT_POSITIONS: [u32; 2] = [sys::SPA_AUDIO_CHANNEL_TSL, sys::SPA_AUDIO_CHANNEL_TSR];

/// Planar f32 at whatever rate the graph runs: FL/FR, a named pair per extra send, then
/// the insert pair when `insert`.
fn format_pod(sends: usize, insert: bool) -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32P);
    let sends = sends.clamp(1, MAX_SENDS) as u32;
    let insert_channels = if insert { INSERT_CHANNELS as u32 } else { 0 };
    info.set_channels(CHANNELS * sends + insert_channels);
    let mut position = [0; MAX_CHANNELS];
    position[0] = sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = sys::SPA_AUDIO_CHANNEL_FR;
    for (send, pair) in SEND_POSITIONS.iter().enumerate().take(sends as usize - 1) {
        position[2 + 2 * send] = pair[0];
        position[3 + 2 * send] = pair[1];
    }
    if insert {
        let at = (CHANNELS * sends) as usize;
        position[at] = INSERT_POSITIONS[0];
        position[at + 1] = INSERT_POSITIONS[1];
    }
    info.set_position(position);
    let object = Object {
        type_: sys::SPA_TYPE_OBJECT_Format,
        id: sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .expect("a format pod serializes")
        .0
        .into_inner()
}
