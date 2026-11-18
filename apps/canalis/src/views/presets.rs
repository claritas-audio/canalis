//! Ready-made chains for an empty Chains view: one click creates the chain, its slots
//! and its ends. The Claritas CLAPs ship with Canalis (ADR 0007); a preset whose other
//! plugins are missing says what to install.

use canalis_chain::{SlotConfig, SlotSpec};
use canalis_pw::claritas;
use canalis_pw::{ChainKind, Command, Graph, MediaClass, virtual_node_name};

use crate::i18n::t;

pub struct Preset {
    pub label: &'static str,
    pub kind: ChainKind,
    pub description: &'static str,
    slots: fn(&Graph) -> Vec<SlotSpec>,
    /// What the keyed slots hear: nothing, the default mic, or the calls.
    key: Key,
    /// Plugin ids the preset cannot do without; the rest are added when installed.
    needs: &'static [&'static str],
    /// A cable that also plays to the default output, so you hear what it carries.
    listen: bool,
}

/// The sidechain a preset's chain is keyed by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    None,
    /// The default microphone: duck under your own voice.
    DefaultMic,
    /// The Calls cable the preset sets up: duck under the other side of a call.
    Calls,
}

pub const PRESETS: [Preset; 5] = [
    Preset {
        label: "Desktop EQ",
        kind: ChainKind::Output,
        description: "Claritas EQ → limiter on everything you hear; becomes the default output",
        slots: desktop_eq,
        key: Key::None,
        needs: &[claritas::EQ_ID],
        listen: false,
    },
    Preset {
        label: "Voice for calls",
        kind: ChainKind::Mic,
        description: "gate → EQ → Comp → limiter on the default mic; calls record from it",
        slots: voice_for_calls,
        key: Key::None,
        needs: &[claritas::EQ_ID, claritas::COMP_ID],
        listen: false,
    },
    Preset {
        label: "Duck music when I talk",
        kind: ChainKind::Output,
        description: "LSP sidechain compressor keyed by the default mic; send music into it",
        slots: ducking,
        key: Key::DefaultMic,
        needs: &[LSP_SC_COMPRESSOR_LV2],
        listen: false,
    },
    Preset {
        label: "Duck music under calls",
        kind: ChainKind::Output,
        description: "music lowers while someone speaks in a call: call apps go to a Calls \
                      cable you hear, which keys an LSP sidechain compressor; send music into it",
        slots: ducking,
        key: Key::Calls,
        needs: &[LSP_SC_COMPRESSOR_LV2],
        listen: false,
    },
    Preset {
        label: "Music into calls",
        kind: ChainKind::Cable,
        description: "play music into it, pick it as the mic in Telegram or Discord; \
                      you hear it too, a limiter keeps it from blasting",
        slots: music_into_calls,
        key: Key::None,
        needs: &[],
        listen: true,
    },
];

const LSP_GATE: &str = "in.lsp-plug.gate_stereo";
const LSP_LIMITER: &str = "in.lsp-plug.limiter_stereo";
const LSP_SC_COMPRESSOR_LV2: &str = "http://lsp-plug.in/plugins/lv2/sc_compressor_stereo";

/// Why a preset cannot be created yet.
#[derive(Debug, PartialEq, Eq)]
pub struct Missing {
    pub text: &'static str,
    /// Settings › Plugins can install what is missing.
    pub installable: bool,
}

impl Preset {
    /// `label` in the interface language; `label` itself names the chain it creates.
    pub fn title(&self) -> &'static str {
        match self.label {
            "Desktop EQ" => t("Desktop EQ"),
            "Voice for calls" => t("Voice for calls"),
            "Duck music when I talk" => t("Duck music when I talk"),
            "Duck music under calls" => t("Duck music under calls"),
            "Music into calls" => t("Music into calls"),
            other => other,
        }
    }

    /// `description` in the interface language.
    pub fn about(&self) -> &'static str {
        match self.label {
            "Desktop EQ" => {
                t("Claritas EQ → limiter on everything you hear; becomes the default output")
            }
            "Voice for calls" => {
                t("gate → EQ → Comp → limiter on the default mic; calls record from it")
            }
            "Duck music when I talk" => {
                t("LSP sidechain compressor keyed by the default mic; send music into it")
            }
            "Duck music under calls" => t(
                "music lowers while someone speaks in a call: call apps go to a Calls \
                 cable you hear, which keys an LSP sidechain compressor; send music into it",
            ),
            "Music into calls" => t(
                "play music into it, pick it as the mic in Telegram or Discord; \
                 you hear it too, a limiter keeps it from blasting",
            ),
            _ => self.description,
        }
    }

    /// What has to be installed before the preset can be created, if anything.
    pub fn missing(&self, graph: &Graph) -> Option<Missing> {
        let absent = *self
            .needs
            .iter()
            .find(|id| !graph.plugins.iter().any(|p| p.id == **id))?;
        Some(if absent == LSP_SC_COMPRESSOR_LV2 {
            Missing {
                text: if graph.lv2_off.is_some() {
                    t("needs LV2 support and LSP Plugins (LV2), not installed yet")
                } else {
                    t("needs LSP Plugins (LV2), not installed yet")
                },
                installable: true,
            }
        } else {
            Missing {
                text: t(
                    "the Claritas plugins that ship with Canalis are missing; reinstall Canalis",
                ),
                installable: false,
            }
        })
    }
}

/// The ends a new chain of `kind` starts with: the default hardware device on the side
/// the user would pick, nothing on the side apps connect to.
pub fn default_ends(graph: &Graph, kind: ChainKind) -> (Vec<String>, Vec<String>) {
    let device = |class: MediaClass, default: Option<&str>| -> Vec<String> {
        default
            .and_then(|d| graph.node_by_name(d))
            .filter(|n| n.class == class && !n.is_virtual_device())
            .map(|n| vec![n.name.clone()])
            .unwrap_or_default()
    };
    match kind {
        ChainKind::Output => (
            Vec::new(),
            device(MediaClass::Sink, graph.defaults.sink.as_deref()),
        ),
        ChainKind::Mic => (
            device(MediaClass::Source, graph.defaults.source.as_deref()),
            Vec::new(),
        ),
        ChainKind::Cable | ChainKind::Insert => (Vec::new(), Vec::new()),
    }
}

/// A Claritas EQ slot, flat, when the catalogue has the plugin.
pub fn claritas_eq(graph: &Graph) -> Option<SlotSpec> {
    plugin(graph, claritas::EQ_ID)
}

/// A Claritas EQ slot drawing `bands` with `output_db` of gain (an AutoEQ curve).
pub fn claritas_eq_curve(
    graph: &Graph,
    bands: &[claritas::EqBand],
    output_db: f32,
) -> Option<SlotSpec> {
    let mut spec = claritas_eq(graph)?;
    if let SlotSpec::Clap { params, .. } = &mut spec {
        *params = claritas::curve_params(bands, output_db);
    }
    Some(spec)
}

/// A Claritas Comp slot, when the catalogue has the plugin.
pub fn claritas_comp(graph: &Graph) -> Option<SlotSpec> {
    plugin(graph, claritas::COMP_ID)
}

/// `label`, numbered when a chain of that name exists (or is about to, in `also`), so a
/// second copy is its own chain and the commands that follow never rewrite the first.
pub fn free_label(label: &str, graph: &Graph, also: &[String]) -> String {
    let taken = |candidate: &str| {
        let name = virtual_node_name(candidate);
        graph.chains.iter().any(|c| c.name == name)
            || graph.nodes.iter().any(|n| n.name == name)
            || also.iter().any(|l| virtual_node_name(l) == name)
    };
    (1..)
        .map(|n| match n {
            1 => label.to_owned(),
            n => format!("{label} {n}"),
        })
        .find(|label| !taken(label))
        .expect("a free number")
}

/// The commands that create the preset's chain: the chain, its slots, its ends.
pub fn apply(preset: &Preset, graph: &Graph) -> Vec<Command> {
    let label = free_label(preset.label, graph, &[]);
    let name = virtual_node_name(&label);
    let slots = (preset.slots)(graph)
        .into_iter()
        .filter_map(|spec| toml::Value::try_from(SlotConfig::new(spec)).ok())
        .collect();
    let (inputs, mut outputs) = default_ends(graph, preset.kind);
    if preset.listen {
        outputs = default_ends(graph, ChainKind::Output).1;
    }
    let mut commands = vec![
        Command::CreateChain {
            label,
            kind: preset.kind,
        },
        Command::SetChainSlots {
            name: name.clone(),
            slots,
        },
        Command::SetChainEndpoints {
            name: name.clone(),
            inputs,
            outputs,
        },
    ];
    let key = match preset.key {
        Key::None => None,
        Key::DefaultMic => default_ends(graph, ChainKind::Mic).0.into_iter().next(),
        Key::Calls => {
            let (setup, source) = calls_cable(graph);
            commands.splice(0..0, setup);
            Some(source)
        }
    };
    if let Some(key) = key {
        commands.push(Command::SetChainKey {
            name,
            key: Some(key),
        });
    }
    commands
}

/// The Calls cable's label; its chain is `canalis.calls`.
const CALLS: &str = "Calls";

/// Apps whose playback is a call, by a word of their `application.name`. A call in a
/// browser tab plays as the browser and cannot be told apart.
const CALL_APPS: [&str; 11] = [
    "telegram", "discord", "zoom", "teams", "slack", "skype", "signal", "mumble", "element",
    "jitsi", "webrtc",
];

fn is_call_app(app: &str) -> bool {
    let app = app.to_lowercase();
    CALL_APPS.iter().any(|word| app.contains(word))
}

/// The Calls cable, made if missing and played to the default output, with the call apps
/// that are playing now sent into it (the service then keeps sending them by name); and
/// the node a chain keyed by the calls listens to.
fn calls_cable(graph: &Graph) -> (Vec<Command>, String) {
    let name = virtual_node_name(CALLS);
    let mut commands = Vec::new();
    if graph.chain(&name).is_none() {
        commands.push(Command::CreateChain {
            label: CALLS.to_owned(),
            kind: ChainKind::Cable,
        });
        commands.push(Command::SetChainEndpoints {
            name: name.clone(),
            inputs: Vec::new(),
            outputs: default_ends(graph, ChainKind::Output).1,
        });
    }
    for stream in graph.nodes.iter().filter(|n| {
        n.class == MediaClass::PlaybackStream && n.app.as_deref().is_some_and(is_call_app)
    }) {
        commands.push(Command::SetTarget {
            stream: stream.id,
            target: Some(ChainKind::Cable.capture_node_name(&name)),
        });
    }
    (commands, ChainKind::Cable.source_node_name(&name))
}

/// LSP sidechain compressor ports (from its TTL) and the ducking setup: keyed by the
/// external sidechain, in below a speaking voice, out a bit lower so the duck does not
/// flutter between words, 8:1, 20 ms in, 400 ms back.
const SC_TYPE: u32 = 22;
const SC_EXTERNAL: f64 = 2.0;
const ATTACK_THRESHOLD: u32 = 34;
const RELEASE_THRESHOLD: u32 = 36;
const ATTACK_MS: u32 = 35;
const RELEASE_MS: u32 = 37;
const RATIO: u32 = 39;

/// dBFS as the linear gain LSP's threshold ports take.
fn gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The LSP sidechain compressor (LV2, where the key ports are marked as such), ducking
/// music under a voice.
fn ducking(graph: &Graph) -> Vec<SlotSpec> {
    let entry = graph
        .plugins
        .iter()
        .find(|p| p.id == LSP_SC_COMPRESSOR_LV2 && p.is_lv2());
    entry
        .map(|entry| SlotSpec::Lv2 {
            uri: entry.id.clone(),
            params: [
                (SC_TYPE, SC_EXTERNAL),
                (ATTACK_THRESHOLD, gain(-45.0)),
                (RELEASE_THRESHOLD, gain(-55.0)),
                (RATIO, 8.0),
                (ATTACK_MS, 20.0),
                (RELEASE_MS, 400.0),
            ]
            .into_iter()
            .map(|(index, value): (u32, f64)| (index.to_string(), value))
            .collect(),
            state: None,
            preset: None,
        })
        .into_iter()
        .collect()
}

/// A limiter when LSP's is installed; a bare cable otherwise.
fn music_into_calls(graph: &Graph) -> Vec<SlotSpec> {
    plugin(graph, LSP_LIMITER).into_iter().collect()
}

fn desktop_eq(graph: &Graph) -> Vec<SlotSpec> {
    claritas_eq(graph)
        .into_iter()
        .chain(plugin(graph, LSP_LIMITER))
        .collect()
}

fn voice_for_calls(graph: &Graph) -> Vec<SlotSpec> {
    let mut slots = Vec::new();
    slots.extend(plugin(graph, LSP_GATE));
    slots.extend(claritas_eq(graph));
    slots.extend(claritas_comp(graph));
    slots.extend(plugin(graph, LSP_LIMITER));
    slots
}

/// A CLAP slot for `id` as the plugin starts, when the catalogue has it.
fn plugin(graph: &Graph, id: &str) -> Option<SlotSpec> {
    let entry = graph.plugins.iter().find(|p| p.id == id && !p.is_lv2())?;
    Some(SlotSpec::Clap {
        path: entry.path.clone(),
        id: entry.id.clone(),
        params: Default::default(),
        state: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use canalis_pw::PluginEntry;

    fn entry(path: &str, id: &str) -> PluginEntry {
        PluginEntry {
            path: path.into(),
            id: id.into(),
            name: id.into(),
            vendor: String::new(),
            modified: 0,
            format: "clap".into(),
        }
    }

    fn with_claritas() -> Graph {
        let mut graph = Graph::default();
        graph
            .plugins
            .push(entry("/app/lib/clap/Claritas EQ.clap", claritas::EQ_ID));
        graph
            .plugins
            .push(entry("/app/lib/clap/Claritas Comp.clap", claritas::COMP_ID));
        graph
    }

    #[test]
    fn a_preset_creates_the_chain_then_fills_it() {
        let mut graph = with_claritas();
        graph
            .plugins
            .push(entry("/usr/lib64/clap/lsp-plugins.clap", LSP_LIMITER));
        let commands = apply(by_label("Desktop EQ"), &graph);
        assert_eq!(commands.len(), 3);
        let Command::CreateChain { label, kind } = &commands[0] else {
            panic!("first command creates the chain");
        };
        assert_eq!((label.as_str(), *kind), ("Desktop EQ", ChainKind::Output));
        let Command::SetChainSlots { name, slots } = &commands[1] else {
            panic!("second command sets the slots");
        };
        assert_eq!(name, "canalis.desktop-eq");
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0]["spec"]["id"].as_str(), Some(claritas::EQ_ID));
        let params = slots[0]["spec"]["params"].as_table();
        assert!(params.is_none_or(|p| p.is_empty()), "the EQ starts flat");
        assert_eq!(slots[1]["spec"]["id"].as_str(), Some(LSP_LIMITER));

        assert_eq!(
            by_label("Duck music when I talk").missing(&graph),
            Some(Missing {
                text: "needs LSP Plugins (LV2), not installed yet",
                installable: true,
            }),
            "the ducking preset says what it lacks"
        );
        let lv2_off = Graph {
            lv2_off: Some("no lilv".into()),
            ..Graph::default()
        };
        assert!(
            by_label("Duck music when I talk")
                .missing(&lv2_off)
                .unwrap()
                .text
                .contains("LV2 support")
        );
        let without_lsp = apply(by_label("Voice for calls"), &with_claritas());
        let Command::SetChainSlots { slots, .. } = &without_lsp[1] else {
            panic!("second command sets the slots");
        };
        assert_eq!(
            slots.len(),
            2,
            "Claritas EQ and Comp; gate and limiter are skipped without LSP"
        );
    }

    #[test]
    fn a_curve_written_as_claritas_params_shapes_the_sound() {
        // The user's build or the one `scripts/bundle-claritas.sh` lays out; a no-op without.
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let candidates = [
            home.map(|h| h.join(".clap/Claritas EQ.clap")),
            Some(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/lib/clap/Claritas EQ.clap"),
            ),
        ];
        let Some(path) = candidates.into_iter().flatten().find(|p| p.exists()) else {
            return;
        };
        let bands = [claritas::EqBand::cut(claritas::EqShape::HighCut, 200.0, 48)];
        let params = claritas::curve_params(&bands, -6.3);
        let (instance, mut processor) = canalis_chain::clap::load(
            &path.to_string_lossy(),
            claritas::EQ_ID,
            &params,
            None,
            48_000,
        )
        .expect("Claritas EQ loads");
        let freq = instance
            .info
            .params
            .iter()
            .find(|p| p.id == claritas::param_id("freq_1"))
            .expect("band 1 frequency");
        assert!(
            (freq.value - params[&freq.id.to_string()]).abs() < 1e-4,
            "the plugin kept {} for band 1",
            freq.value
        );
        let gain_id = claritas::param_id("output_gain");
        let gain = instance
            .info
            .params
            .iter()
            .find(|p| p.id == gain_id)
            .expect("output gain");
        assert!(
            (gain.value - params[&gain_id.to_string()]).abs() < 1e-4,
            "the plugin kept {} for the output gain",
            gain.value
        );
        let tone = |i: usize| (i as f32 * 2.0 * std::f32::consts::PI * 2000.0 / 48_000.0).sin();
        let mut peak = 0.0f32;
        for block in 0..40 {
            let mut left: Vec<f32> = (0..512).map(|i| tone(block * 512 + i)).collect();
            let mut right = left.clone();
            processor.process(&mut [&mut left, &mut right], &[]);
            if block >= 20 {
                peak = left.iter().fold(peak, |m, v| m.max(v.abs()));
            }
        }
        assert!(
            peak < 0.01,
            "2 kHz through a 200 Hz high cut peaked at {peak}"
        );
        instance.retire(processor, None);
    }

    fn by_label(label: &str) -> &'static Preset {
        PRESETS.iter().find(|p| p.label == label).expect("a preset")
    }

    fn node(
        id: u32,
        name: &'static str,
        class: &'static str,
        app: Option<&'static str>,
    ) -> canalis_pw::Node {
        canalis_pw::Node::from_props(id, |key| match key {
            "node.name" => Some(name),
            "media.class" => Some(class),
            "application.name" => app,
            _ => None,
        })
    }

    #[test]
    fn ducking_under_calls_sets_up_a_calls_cable_that_keys_it() {
        let mut graph = Graph::default();
        graph.plugins.push(PluginEntry {
            format: "lv2".into(),
            ..entry("/usr/lib64/lv2/lsp-plugins.lv2/", LSP_SC_COMPRESSOR_LV2)
        });
        graph.nodes.push(node(5, "speakers", "Audio/Sink", None));
        graph.defaults.sink = Some("speakers".into());
        graph.nodes.push(node(
            7,
            "Telegram",
            "Stream/Output/Audio",
            Some("Telegram Desktop"),
        ));
        graph
            .nodes
            .push(node(8, "Firefox", "Stream/Output/Audio", Some("Firefox")));
        let preset = by_label("Duck music under calls");
        assert_eq!(preset.missing(&graph), None);
        let commands = apply(preset, &graph);
        assert!(
            matches!(&commands[0], Command::CreateChain { label, kind: ChainKind::Cable } if label == "Calls")
        );
        let targets: Vec<(u32, Option<String>)> = commands
            .iter()
            .filter_map(|c| match c {
                Command::SetTarget { stream, target } => Some((stream.0, target.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            [(7, Some("canalis.calls".to_owned()))],
            "only the call app moves"
        );
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::SetChainKey { name, key: Some(key) }
                if name == "canalis.duck-music-under-calls" && key == "canalis.calls.source"
        )));
    }

    #[test]
    fn a_second_copy_gets_its_own_name() {
        let mut graph = with_claritas();
        let first = apply(by_label("Voice for calls"), &graph);
        let Command::CreateChain { label, .. } = &first[0] else {
            panic!("creates the chain");
        };
        assert_eq!(label, "Voice for calls");
        graph
            .nodes
            .push(canalis_pw::Node::from_props(9, |key| match key {
                "node.name" => Some("canalis.voice-for-calls"),
                "media.class" => Some("Audio/Source"),
                _ => None,
            }));
        let second = apply(by_label("Voice for calls"), &graph);
        let Command::CreateChain { label, .. } = &second[0] else {
            panic!("creates the chain");
        };
        assert_eq!(label, "Voice for calls 2");
        assert!(
            second
                .iter()
                .skip(1)
                .all(|c| !format!("{c:?}").contains("\"canalis.voice-for-calls\"")),
            "nothing touches the first copy"
        );
    }

    #[test]
    fn without_the_bundled_claritas_a_preset_says_so() {
        let missing = by_label("Desktop EQ")
            .missing(&Graph::default())
            .expect("missing");
        assert!(
            !missing.installable,
            "Get plugins cannot bring Claritas back"
        );
        assert_eq!(by_label("Desktop EQ").missing(&with_claritas()), None);
    }

    #[test]
    fn the_music_cable_listens_through_the_default_output() {
        let mut graph = Graph::default();
        graph.defaults.sink = Some("speakers".into());
        graph
            .nodes
            .push(canalis_pw::Node::from_props(1, |key| match key {
                "node.name" => Some("speakers"),
                "media.class" => Some("Audio/Sink"),
                _ => None,
            }));
        let preset = by_label("Music into calls");
        assert_eq!(
            preset.missing(&graph),
            None,
            "a bare cable is still worth offering"
        );
        let commands = apply(preset, &graph);
        let Command::SetChainEndpoints {
            inputs, outputs, ..
        } = &commands[2]
        else {
            panic!("third command sets the ends");
        };
        assert!(inputs.is_empty(), "apps play into a cable");
        assert_eq!(outputs, &["speakers".to_owned()]);
    }
}
