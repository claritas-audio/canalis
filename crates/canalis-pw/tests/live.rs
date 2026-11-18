//! Drives every command against the running PipeWire session. Ignored by default:
//! `cargo test -p canalis-pw --test live -- --ignored --test-threads 1`. Needs pactl,
//! pw-cat and a default sink; refuses to run if any `canalis.*` node already exists.

use std::path::PathBuf;
use std::process::{Child, Command as Process, Stdio};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use canalis_pw::{
    ChainKind, ChainStatus, Command, Direction, Graph, Handle, MediaClass, VIRTUAL_PREFIX,
    VirtualKind,
};

const TIMEOUT: Duration = Duration::from_secs(6);

struct Live {
    handle: Handle,
    graph: Arc<Graph>,
    null_sink_module: String,
    config: PathBuf,
}

impl Live {
    fn start() -> Live {
        let _ = env_logger::builder().is_test(false).try_init();
        let config = std::env::temp_dir().join(format!("canalis-live-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&config);
        let handle = canalis_pw::spawn_with_config(|| {}, config.clone()).expect("thread");
        let out = Process::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                "sink_name=canalis_live_test",
                "sink_properties=node.description=CanalisLiveTest",
            ])
            .output()
            .expect("pactl runs");
        let null_sink_module = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        assert!(!null_sink_module.is_empty(), "pactl load-module failed");
        let mut live = Live {
            handle,
            graph: Arc::default(),
            null_sink_module,
            config,
        };
        live.wait("connected with the test sink and its ports", |g| {
            g.connected
                && g.node_by_name("canalis_live_test")
                    .is_some_and(|n| g.ports_of(n.id).count() == 4)
        });
        assert!(
            !live.graph.nodes.iter().any(
                |n| n.name.starts_with(VIRTUAL_PREFIX) && n.name != canalis_pw::MIDI_INPUT_NAME
            ),
            "a canalis.* node exists; not running against a live config"
        );
        live
    }

    /// Polls snapshots until `ok` holds, panicking after `TIMEOUT`.
    fn wait(&mut self, what: &str, ok: impl Fn(&Graph) -> bool) {
        let start = Instant::now();
        loop {
            if let Some(g) = self.handle.latest() {
                self.graph = g;
            }
            if ok(&self.graph) {
                return;
            }
            assert!(
                start.elapsed() < TIMEOUT,
                "timed out waiting for {what}; last graph: connected {} nodes {} ports {} links {} test sink {:?}",
                self.graph.connected,
                self.graph.nodes.len(),
                self.graph.ports.len(),
                self.graph.links.len(),
                self.graph
                    .nodes
                    .iter()
                    .map(|n| n.name.as_str())
                    .collect::<Vec<_>>()
            );
            sleep(Duration::from_millis(50));
        }
    }

    fn port(&self, node: &str, direction: Direction, channel: &str) -> canalis_pw::PortId {
        let node = self.graph.node_by_name(node).expect("node present");
        self.graph
            .ports_of(node.id)
            .find(|p| p.direction == direction && p.channel.as_deref() == Some(channel))
            .expect("port present")
            .id
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = Process::new("pactl")
            .args(["unload-module", &self.null_sink_module])
            .status();
        let _ = std::fs::remove_file(&self.config);
    }
}

fn silent_stream() -> Child {
    Process::new("pw-cat")
        .args([
            "-p",
            "--raw",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
            "/dev/zero",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("pw-cat runs")
}

#[test]
#[ignore]
fn links_are_created_and_destroyed_on_request() {
    let mut live = Live::start();
    let default_sink = live.graph.defaults.sink.clone().expect("a default sink");
    let output = live.port("canalis_live_test", Direction::Output, "FL");
    let input = live.port(&default_sink, Direction::Input, "FL");

    live.handle.send(Command::Link { output, input });
    live.wait("the link", |g| g.link_between(output, input).is_some());
    let link = live.graph.link_between(output, input).expect("link").id;
    let saved = std::fs::read_to_string(&live.config).expect("config written");
    assert!(
        saved.contains("canalis_live_test"),
        "link persisted by name"
    );

    live.handle.send(Command::Unlink { link });
    live.wait("the link to go", |g| {
        g.link_between(output, input).is_none()
    });
    let saved = std::fs::read_to_string(&live.config).expect("config written");
    assert!(
        !saved.contains("canalis_live_test"),
        "link removed from config"
    );
}

#[test]
#[ignore]
fn virtual_devices_come_and_go_and_can_be_the_default() {
    let mut live = Live::start();
    let original = live.graph.defaults.sink.clone().expect("a default sink");

    live.handle.send(Command::CreateVirtual {
        kind: VirtualKind::Sink,
        label: "Live Test".into(),
    });
    live.wait("the virtual sink", |g| {
        g.node_by_name("canalis.live-test")
            .is_some_and(|n| n.class == MediaClass::Sink && g.ports_of(n.id).count() == 4)
    });

    live.handle.send(Command::SetDefault {
        kind: VirtualKind::Sink,
        name: "canalis.live-test".into(),
    });
    live.wait("the default to move", |g| {
        g.defaults.sink.as_deref() == Some("canalis.live-test")
    });
    live.handle.send(Command::SetDefault {
        kind: VirtualKind::Sink,
        name: original.clone(),
    });
    live.wait("the default to move back", |g| {
        g.defaults.sink.as_deref() == Some(original.as_str())
    });

    live.handle.send(Command::DeleteVirtual {
        name: "canalis.live-test".into(),
    });
    live.wait("the virtual sink to go", |g| {
        g.node_by_name("canalis.live-test").is_none()
    });
    let saved = std::fs::read_to_string(&live.config).expect("config written");
    assert!(!saved.contains("live-test"));
}

#[test]
#[ignore]
fn volume_is_read_and_set_through_props() {
    let mut live = Live::start();
    live.wait("the test sink volume", |g| {
        g.node_by_name("canalis_live_test")
            .is_some_and(|n| n.volume.is_some())
    });
    let node = live
        .graph
        .node_by_name("canalis_live_test")
        .expect("node")
        .id;
    live.handle.send(Command::SetVolume { node, cubic: 0.5 });
    live.wait("volume 0.5", |g| {
        g.node_by_name("canalis_live_test")
            .and_then(|n| n.volume.as_ref())
            .is_some_and(|v| (v.cubic() - 0.5).abs() < 0.01)
    });
}

#[test]
#[ignore]
fn streams_move_to_a_target_and_back() {
    let mut live = Live::start();
    let mut player = silent_stream();
    live.wait("the pw-cat stream", |g| {
        g.nodes
            .iter()
            .any(|n| n.app.as_deref() == Some("pw-cat") && n.class == MediaClass::PlaybackStream)
    });
    let stream = live
        .graph
        .nodes
        .iter()
        .find(|n| n.app.as_deref() == Some("pw-cat"))
        .expect("stream")
        .id;
    let sink = live
        .graph
        .node_by_name("canalis_live_test")
        .expect("sink")
        .id;

    live.handle.send(Command::SetTarget {
        stream,
        target: Some("canalis_live_test".into()),
    });
    live.wait("the stream on the test sink", |g| {
        g.node(stream)
            .is_some_and(|n| n.target.as_deref() == Some("canalis_live_test"))
            && g.downstream_of(stream) == vec![sink]
    });
    let saved = std::fs::read_to_string(&live.config).expect("config written");
    assert!(
        saved.contains("pw-cat"),
        "target persisted by application.name"
    );

    live.handle.send(Command::SetTarget {
        stream,
        target: None,
    });
    live.wait("the stream back on the default", |g| {
        g.node(stream).is_some_and(|n| n.target.is_none()) && g.downstream_of(stream) != vec![sink]
    });
    let _ = player.kill();
    let _ = player.wait();
}

/// Destroys a global from outside, as a crash or another tool would.
fn destroy_from_outside(id: u32) {
    let status = Process::new("pw-cli")
        .args(["destroy", &id.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("pw-cli runs");
    assert!(status.success(), "pw-cli destroy {id}");
}

#[test]
#[ignore]
fn config_is_reapplied_when_objects_vanish_or_the_app_restarts() {
    let mut live = Live::start();
    let default_sink = live.graph.defaults.sink.clone().expect("a default sink");

    live.handle.send(Command::CreateVirtual {
        kind: VirtualKind::Sink,
        label: "Live Test".into(),
    });
    live.wait("the virtual sink with ports", |g| {
        g.node_by_name("canalis.live-test")
            .is_some_and(|n| g.ports_of(n.id).count() == 4)
    });
    let output = live.port("canalis.live-test", Direction::Output, "FL");
    let input = live.port(&default_sink, Direction::Input, "FL");
    live.handle.send(Command::Link { output, input });
    live.wait("the link", |g| g.link_between(output, input).is_some());

    let link = live.graph.link_between(output, input).expect("link").id;
    destroy_from_outside(link);
    live.wait("the link to be re-created", |g| {
        g.link_between(output, input).is_some_and(|l| l.id != link)
    });

    let node = live
        .graph
        .node_by_name("canalis.live-test")
        .expect("node")
        .id;
    destroy_from_outside(node.0);
    live.wait("the sink and its link to be re-created", |g| {
        g.node_by_name("canalis.live-test").is_some_and(|n| {
            n.id != node
                && g.ports_of(n.id).count() == 4
                && g.ports_of(n.id)
                    .filter(|p| p.direction == Direction::Output)
                    .any(|p| g.links.iter().any(|l| l.output == p.id))
        })
    });

    let config = live.config.clone();
    let restarted = canalis_pw::spawn_with_config(|| {}, config).expect("second thread");
    let first = std::mem::replace(&mut live.handle, restarted);
    drop(first);
    live.wait("the restarted control plane", |g| {
        g.connected && g.node_by_name("canalis.live-test").is_some()
    });

    live.handle.send(Command::DeleteVirtual {
        name: "canalis.live-test".into(),
    });
    live.wait("the virtual sink to go", |g| {
        g.node_by_name("canalis.live-test").is_none()
    });
}

#[test]
#[ignore]
fn chains_are_spawned_linked_restarted_bypassed_and_deleted() {
    let mut live = Live::start();
    let default_sink = live.graph.defaults.sink.clone().expect("a default sink");
    let name = "canalis.live-chain";
    live.handle.send(Command::CreateChain {
        label: "Live Chain".into(),
        kind: ChainKind::Output,
    });
    live.wait("the chain running with both ends in the graph", |g| {
        g.chain(name)
            .is_some_and(|c| c.status == ChainStatus::Running && c.pid.is_some())
            && g.node_by_name(name)
                .is_some_and(|n| n.class == MediaClass::Sink && g.ports_of(n.id).count() == 4)
            && g.node_by_name("canalis.live-chain.out")
                .is_some_and(|n| n.class == MediaClass::PlaybackStream)
    });

    let slot: toml::Value = toml::from_str(
        "[spec]\nkind = \"eq\"\n[[spec.bands]]\nshape = \"high-cut\"\nfreq_hz = 200.0\nslope_db = 48\n",
    )
    .expect("a slot table");
    live.handle.send(Command::SetChainSlots {
        name: name.into(),
        slots: vec![slot],
    });
    live.wait("the slot stored", |g| {
        g.chain(name).is_some_and(|c| c.slots.len() == 1)
    });

    live.handle.send(Command::SetChainEndpoints {
        name: name.into(),
        inputs: vec![],
        outputs: vec![default_sink.clone()],
    });
    let feeds_default = |g: &Graph| {
        let out = g.node_by_name("canalis.live-chain.out");
        let sink = g.node_by_name(&default_sink);
        matches!((out, sink), (Some(out), Some(sink)) if g.downstream_of(out.id) == vec![sink.id])
    };
    live.wait("the out end linked to the default sink", feeds_default);

    let pid = live.graph.chain(name).and_then(|c| c.pid).expect("a pid");
    let killed = Process::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill runs");
    assert!(killed.success());
    live.wait("a bypassed restart with the links back", |g| {
        g.chain(name).is_some_and(|c| {
            c.status == ChainStatus::Degraded && c.restarts == 1 && c.pid != Some(pid)
        }) && feeds_default(g)
    });
    let saved = std::fs::read_to_string(&live.config).expect("config written");
    assert!(saved.contains("[[chain]]") && saved.contains("high-cut"));

    live.handle.send(Command::DeleteChain { name: name.into() });
    live.wait("the chain and its nodes gone", |g| {
        g.chain(name).is_none() && g.node_by_name(name).is_none()
    });
}

#[test]
#[ignore]
fn a_second_output_gets_its_own_send_pair() {
    let mut live = Live::start();
    let default_sink = live.graph.defaults.sink.clone().expect("a default sink");
    let name = "canalis.live-sends";
    live.handle.send(Command::CreateVirtual {
        kind: VirtualKind::Sink,
        label: "Live Send".into(),
    });
    live.handle.send(Command::CreateChain {
        label: "Live Sends".into(),
        kind: ChainKind::Output,
    });
    live.wait("the chain running and the extra sink up", |g| {
        g.chain(name)
            .is_some_and(|c| c.status == ChainStatus::Running)
            && g.node_by_name("canalis.live-send").is_some()
    });
    live.handle.send(Command::SetChainEndpoints {
        name: name.into(),
        inputs: vec![],
        outputs: vec![default_sink.clone(), "canalis.live-send".into()],
    });
    live.handle.send(Command::SetChainGain {
        name: name.into(),
        target: "canalis.live-send".into(),
        gain_db: -12.0,
    });
    let out_name = "canalis.live-sends.out";
    live.wait(
        "four ports on the out end, FL/FR to the default, RL/RR to the sink",
        |g| {
            let Some(out) = g.node_by_name(out_name) else {
                return false;
            };
            let linked_to = |channel: &str| -> Option<String> {
                let port = g
                    .ports_of(out.id)
                    .find(|p| p.channel.as_deref() == Some(channel))?;
                let link = g.links.iter().find(|l| l.output == port.id)?;
                let node = g.port(link.input)?.node;
                g.node(node).map(|n| n.name.clone())
            };
            g.ports_of(out.id).count() == 4
                && linked_to("FL").as_deref() == Some(default_sink.as_str())
                && linked_to("FR").as_deref() == Some(default_sink.as_str())
                && linked_to("RL").as_deref() == Some("canalis.live-send")
                && linked_to("RR").as_deref() == Some("canalis.live-send")
                && g.chain(name)
                    .is_some_and(|c| c.gains.get("canalis.live-send") == Some(&-12.0))
        },
    );
    let saved = std::fs::read_to_string(&live.config).expect("config written");
    assert!(saved.contains(r#""canalis.live-send" = -12.0"#), "{saved}");

    live.handle.send(Command::DeleteChain { name: name.into() });
    live.handle.send(Command::DeleteVirtual {
        name: "canalis.live-send".into(),
    });
    live.wait("the chain and the sink gone", |g| {
        g.chain(name).is_none() && g.node_by_name("canalis.live-send").is_none()
    });
}

#[test]
#[ignore]
fn a_key_brings_up_the_key_input_linked_from_its_source() {
    let mut live = Live::start();
    let default_source = live
        .graph
        .defaults
        .source
        .clone()
        .expect("a default source");
    let name = "canalis.live-key";
    live.handle.send(Command::CreateChain {
        label: "Live Key".into(),
        kind: ChainKind::Output,
    });
    live.wait("the chain running", |g| {
        g.chain(name)
            .is_some_and(|c| c.status == ChainStatus::Running)
    });
    assert!(live.graph.node_by_name("canalis.live-key.key").is_none());
    live.handle.send(Command::SetChainKey {
        name: name.into(),
        key: Some(default_source.clone()),
    });
    live.wait("the key input up and fed by the default source", |g| {
        let Some(key) = g.node_by_name("canalis.live-key.key") else {
            return false;
        };
        let source = g.node_by_name(&default_source);
        g.chain(name)
            .is_some_and(|c| c.key.as_deref() == Some(default_source.as_str()))
            && source.is_some_and(|s| g.upstream_of(key.id) == vec![s.id])
    });
    live.handle.send(Command::SetChainKey {
        name: name.into(),
        key: None,
    });
    live.wait("the key input gone", |g| {
        g.node_by_name("canalis.live-key.key").is_none()
            && g.chain(name).is_some_and(|c| c.key.is_none())
    });
    live.handle.send(Command::DeleteChain { name: name.into() });
    live.wait("the chain gone", |g| g.chain(name).is_none());
}

/// A standard MIDI file, format 0, with CC 20 alternating 127 and 0 every `step` ticks.
fn midi_file(dir: &std::path::Path, events: u32, step: u32) -> std::path::PathBuf {
    let mut track = Vec::new();
    for i in 0..events {
        let delta: u32 = if i == 0 { 0 } else { step };
        // Variable-length quantity, two bytes are enough for `step` < 16384.
        if delta >= 0x80 {
            track.push(0x80 | ((delta >> 7) & 0x7f) as u8);
        }
        track.push((delta & 0x7f) as u8);
        track.extend_from_slice(&[0xB0, 20, if i % 2 == 0 { 127 } else { 0 }]);
    }
    track.extend_from_slice(&[0, 0xFF, 0x2F, 0]);
    let mut bytes = b"MThd".to_vec();
    bytes.extend_from_slice(&6u32.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&96u16.to_be_bytes());
    bytes.extend_from_slice(b"MTrk");
    bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&track);
    let path = dir.join("control.mid");
    std::fs::write(&path, bytes).expect("midi file written");
    path
}

#[test]
#[ignore]
fn a_midi_control_is_learned_and_toggles_mute() {
    let mut live = Live::start();
    let name = "canalis.live-midi";
    live.handle.send(Command::CreateChain {
        label: "Live Midi".into(),
        kind: ChainKind::Output,
    });
    live.wait("the chain running and the MIDI input node", |g| {
        g.chain(name)
            .is_some_and(|c| c.status == ChainStatus::Running)
            && g.node_by_name(canalis_pw::MIDI_INPUT_NAME).is_some()
    });
    let dir = std::env::temp_dir().join(format!("canalis-midi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    // 120 bpm, 96 ticks a beat: one event a second for twenty seconds.
    let file = midi_file(&dir, 20, 192);
    let mut player = Process::new("pw-cat")
        .args([
            "--midi",
            "--playback",
            "-P",
            "{ node.name = midi.test, node.autoconnect = false }",
        ])
        .arg(&file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("pw-cat starts");
    live.wait("the test MIDI node", |g| {
        g.node_by_name("midi.test").is_some()
    });
    live.handle.send(Command::SetMidiInput {
        node: Some("midi.test".into()),
    });
    live.handle.send(Command::Learn {
        target: Some(canalis_pw::ControlTarget::Mute { chain: name.into() }),
    });
    live.wait("the control learned from CC 20", |g| {
        g.learning.is_none() && g.controls.iter().any(|c| {
            matches!(c.trigger, canalis_pw::ControlTrigger::Cc { cc: 20, .. })
                && matches!(&c.target, canalis_pw::ControlTarget::Mute { chain } if chain == name)
        })
    });
    live.wait("the next CC muting the chain", |g| {
        g.chain(name).is_some_and(|c| c.mute)
    });
    live.wait("the one after unmuting it", |g| {
        g.chain(name).is_some_and(|c| !c.mute)
    });
    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_dir_all(&dir);
    live.handle.send(Command::ForgetControl { index: 0 });
    live.handle.send(Command::SetMidiInput { node: None });
    live.handle.send(Command::DeleteChain { name: name.into() });
    live.wait("the chain and the mapping gone", |g| {
        g.chain(name).is_none() && g.controls.is_empty() && g.midi_input.is_none()
    });
}

#[test]
#[ignore]
fn a_scene_brings_the_default_back() {
    let mut live = Live::start();
    let original = live.graph.defaults.sink.clone().expect("a default sink");
    let before = canalis_pw::Scene::capture("before", &live.graph);
    assert_eq!(before.sink.as_deref(), Some(original.as_str()));

    live.handle.send(Command::CreateVirtual {
        kind: VirtualKind::Sink,
        label: "Scene Test".into(),
    });
    live.wait("the virtual sink", |g| {
        g.node_by_name("canalis.scene-test").is_some()
    });
    live.handle.send(Command::SetDefault {
        kind: VirtualKind::Sink,
        name: "canalis.scene-test".into(),
    });
    live.wait("the default to move", |g| {
        g.defaults.sink.as_deref() == Some("canalis.scene-test")
    });

    live.handle.send(Command::SaveScene {
        scene: before.clone(),
    });
    live.wait("the scene to be listed", |g| {
        g.scenes.iter().any(|s| s.name == "before")
    });
    live.handle.send(Command::ApplyScene { scene: before });
    live.wait("the default to come back with the scene", |g| {
        g.defaults.sink.as_deref() == Some(original.as_str())
    });

    live.handle.send(Command::DeleteScene {
        name: "before".into(),
    });
    live.wait("the scene to go", |g| g.scenes.is_empty());
    live.handle.send(Command::DeleteVirtual {
        name: "canalis.scene-test".into(),
    });
    live.wait("the virtual sink to go", |g| {
        g.node_by_name("canalis.scene-test").is_none()
    });
}
