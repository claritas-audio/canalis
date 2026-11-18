//! Undo for routing and chain edits: every link, target, default, endpoint, mute and gain
//! change, and every slot change (added, removed, moved, bypassed, mixed, a parameter
//! set), remembers how to put the previous state back. Inverses are computed from the
//! snapshot the command was issued against, except a new link's id, which is looked up
//! when the undo runs. Moving boxes in the Graph is undone too: those entries carry the
//! layout before and after.

use std::collections::HashMap;

use canalis_chain::{SlotConfig, SlotSpec, slot_config};
use canalis_pw::{Command, Graph, PortId, Scene};

/// What undoing a command sends.
#[derive(Clone, Debug, PartialEq)]
pub enum Inverse {
    Command(Command),
    /// Remove the link between these ports, whatever id PipeWire gave it.
    UnlinkBetween {
        output: PortId,
        input: PortId,
    },
}

impl Inverse {
    fn resolve(&self, graph: &Graph) -> Option<Command> {
        match self {
            Inverse::Command(command) => Some(command.clone()),
            Inverse::UnlinkBetween { output, input } => graph
                .links
                .iter()
                .find(|l| l.output == *output && l.input == *input)
                .map(|l| Command::Unlink { link: l.id }),
        }
    }
}

/// Where each Graph box sits, by key.
pub type Layout = HashMap<String, egui::Pos2>;

/// A change and its way back.
#[derive(Clone, Debug, PartialEq)]
enum Entry {
    Routing {
        undo: Box<Inverse>,
        redo: Box<Inverse>,
    },
    Layout {
        before: Layout,
        after: Layout,
    },
}

/// What an undo or redo hands back: a command for the service or a layout for the Graph.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Command(Command),
    Layout(Layout),
}

#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

/// How many steps are kept.
const DEPTH: usize = 100;

impl UndoStack {
    /// Remembers `command` if it is a routing change; `graph` is the state it changes.
    pub fn record(&mut self, graph: &Graph, command: &Command) {
        let Some(undo) = inverse(graph, command) else {
            return;
        };
        self.push(Entry::Routing {
            undo: Box::new(undo),
            redo: Box::new(Inverse::Command(command.clone())),
        });
    }

    /// Remembers boxes moved in the Graph.
    pub fn record_layout(&mut self, before: Layout, after: Layout) {
        if before != after {
            self.push(Entry::Layout { before, after });
        }
    }

    fn push(&mut self, entry: Entry) {
        self.redo.clear();
        self.undo.push(entry);
        if self.undo.len() > DEPTH {
            self.undo.remove(0);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// What undoes the last change, moved to the redo side.
    pub fn undo(&mut self, graph: &Graph) -> Option<Step> {
        let entry = self.undo.pop()?;
        let step = match &entry {
            Entry::Routing { undo, .. } => undo.resolve(graph).map(Step::Command),
            Entry::Layout { before, .. } => Some(Step::Layout(before.clone())),
        };
        self.redo.push(entry);
        step
    }

    pub fn redo(&mut self, graph: &Graph) -> Option<Step> {
        let entry = self.redo.pop()?;
        let step = match &entry {
            Entry::Routing { redo, .. } => redo.resolve(graph).map(Step::Command),
            Entry::Layout { after, .. } => Some(Step::Layout(after.clone())),
        };
        self.undo.push(entry);
        step
    }
}

/// The inverse of a routing command against `graph`; `None` for everything else.
fn inverse(graph: &Graph, command: &Command) -> Option<Inverse> {
    Some(match command {
        Command::Link { output, input } => Inverse::UnlinkBetween {
            output: *output,
            input: *input,
        },
        Command::Unlink { link } => {
            let link = graph.links.iter().find(|l| l.id == *link)?;
            Inverse::Command(Command::Link {
                output: link.output,
                input: link.input,
            })
        }
        Command::SetTarget { stream, .. } => Inverse::Command(Command::SetTarget {
            stream: *stream,
            target: graph.node(*stream)?.target.clone(),
        }),
        Command::SetDefault { kind, .. } => {
            let previous = match kind {
                canalis_pw::VirtualKind::Sink => graph.defaults.sink.clone(),
                canalis_pw::VirtualKind::Source => graph.defaults.source.clone(),
            }?;
            Inverse::Command(Command::SetDefault {
                kind: *kind,
                name: previous,
            })
        }
        Command::SetChainEndpoints { name, .. } => {
            let chain = graph.chain(name)?;
            Inverse::Command(Command::SetChainEndpoints {
                name: name.clone(),
                inputs: chain.inputs.clone(),
                outputs: chain.outputs.clone(),
            })
        }
        Command::SetChainMute { name, mute } => Inverse::Command(Command::SetChainMute {
            name: name.clone(),
            mute: !mute,
        }),
        Command::SetChainGain { name, target, .. } => {
            let chain = graph.chain(name)?;
            Inverse::Command(Command::SetChainGain {
                name: name.clone(),
                target: target.clone(),
                gain_db: chain.gains.get(target).copied().unwrap_or(0.0),
            })
        }
        Command::ApplyScene { .. } => Inverse::Command(Command::ApplyScene {
            scene: Scene::capture("before", graph),
        }),
        Command::SetChainIcon { name, .. } => Inverse::Command(Command::SetChainIcon {
            name: name.clone(),
            icon: graph.chain(name)?.icon.clone(),
        }),
        Command::SetLoudnessTarget { name, .. } => Inverse::Command(Command::SetLoudnessTarget {
            name: name.clone(),
            target: graph.chain(name)?.loudness_target,
        }),
        Command::SetChainKey { name, .. } => Inverse::Command(Command::SetChainKey {
            name: name.clone(),
            key: graph.chain(name)?.key.clone(),
        }),
        Command::ReorderChains { .. } => Inverse::Command(Command::ReorderChains {
            names: graph.chains.iter().map(|c| c.name.clone()).collect(),
        }),
        Command::SetHidden { key, hidden } => Inverse::Command(Command::SetHidden {
            key: key.clone(),
            hidden: !hidden,
        }),
        Command::SetAppStyle { app, .. } => {
            let style = graph.apps.get(app).cloned().unwrap_or_default();
            Inverse::Command(Command::SetAppStyle {
                app: app.clone(),
                label: style.label,
                color: style.color,
            })
        }
        Command::SetChainSlots { name, .. } => Inverse::Command(Command::SetChainSlots {
            name: name.clone(),
            slots: graph.chain(name)?.slots.clone(),
        }),
        Command::ChainMessage { name, message } => {
            Inverse::Command(slot_message_inverse(graph, name, message)?)
        }
        Command::RenameChain { name, .. } => Inverse::Command(Command::RenameChain {
            name: name.clone(),
            label: graph.chain(name)?.label.clone(),
        }),
        Command::SetChainColor { name, .. } => Inverse::Command(Command::SetChainColor {
            name: name.clone(),
            color: graph.chain(name)?.color.clone(),
        }),
        Command::SetAutoLevel { name, on } => Inverse::Command(Command::SetAutoLevel {
            name: name.clone(),
            on: !on,
        }),
        _ => return None,
    })
}

/// The message that puts a slot's bypass, mix or parameter back as `graph` has it; `None`
/// for messages that change nothing undoable (windows, presets, recording).
fn slot_message_inverse(graph: &Graph, chain: &str, message: &toml::Value) -> Option<Command> {
    let slot = usize::try_from(message.get("slot")?.as_integer()?).ok()?;
    let slots: Vec<SlotConfig> = graph
        .chain(chain)?
        .slots
        .iter()
        .filter_map(|v| v.clone().try_into().ok())
        .collect();
    let before = slot_config(&slots, slot)?;
    let mut back = message.as_table()?.clone();
    match message.get("type")?.as_str()? {
        "bypass" => back.insert("bypass".into(), toml::Value::Boolean(before.bypass)),
        "mix" => back.insert("mix".into(), toml::Value::Float(f64::from(before.mix))),
        "param" => {
            let id = message.get("id")?.as_integer()?;
            let saved = match &before.spec {
                SlotSpec::Clap { params, .. } | SlotSpec::Lv2 { params, .. } => {
                    params.get(&id.to_string()).copied()
                }
                _ => None,
            };
            // Never set: the plugin's own value, as it reported it.
            let value = saved.or_else(|| {
                let info = graph.chain(chain)?.slot_infos.get(&slot)?;
                info.params
                    .iter()
                    .find(|p| i64::from(p.id) == id)
                    .map(|p| p.value)
            })?;
            back.insert("value".into(), toml::Value::Float(value))
        }
        _ => return None,
    };
    Some(Command::ChainMessage {
        name: chain.to_owned(),
        message: toml::Value::Table(back),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(step: Option<Step>) -> Option<Command> {
        match step? {
            Step::Command(command) => Some(command),
            Step::Layout(_) => None,
        }
    }

    #[test]
    fn moving_boxes_undoes_and_redoes_but_not_moving_nothing() {
        let graph = Graph::default();
        let mut stack = UndoStack::default();
        let at = |x: f32| Layout::from([("mic".to_owned(), egui::pos2(x, 0.0))]);
        stack.record_layout(at(0.0), at(0.0));
        assert!(!stack.can_undo(), "nothing moved");
        stack.record_layout(at(0.0), at(50.0));
        assert_eq!(stack.undo(&graph), Some(Step::Layout(at(0.0))));
        assert_eq!(stack.redo(&graph), Some(Step::Layout(at(50.0))));
    }
    use canalis_pw::{Link, VirtualKind};

    /// A graph with one chain holding a bypassed CLAP slot whose parameter 7 is 0.25.
    fn graph_with_a_chain() -> Graph {
        let chain: canalis_pw::ChainState = serde_json::from_value(serde_json::json!({
            "name": "canalis.eq", "label": "EQ", "kind": "output",
            "slots": [{"bypass": true, "mix": 0.5,
                       "spec": {"kind": "clap", "path": "/x.clap", "id": "x", "params": {"7": 0.25}}}],
            "inputs": [], "outputs": [], "status": "Running", "pid": null,
            "latency_samples": 0, "restarts": 0, "error": null, "slot_infos": {},
            "gui_open": {}, "crashed_slot": null, "mute": false, "color": null,
            "icon": null, "collapsed": false, "loudness_target": null, "gains": {}, "key": null
        }))
        .expect("a chain state");
        Graph {
            chains: vec![chain],
            ..Graph::default()
        }
    }

    fn message(text: &str) -> Command {
        Command::ChainMessage {
            name: "canalis.eq".into(),
            message: toml::from_str(text).expect("a message"),
        }
    }

    #[test]
    fn slot_edits_undo_to_what_the_chain_had() {
        let graph = graph_with_a_chain();
        let mut stack = UndoStack::default();
        stack.record(
            &graph,
            &message("type = \"param\"\nslot = 0\nid = 7\nvalue = 0.9"),
        );
        let Some(Command::ChainMessage { message: back, .. }) = command(stack.undo(&graph)) else {
            panic!("a param message back");
        };
        assert_eq!(back["value"].as_float(), Some(0.25));
        stack.record(
            &graph,
            &message("type = \"bypass\"\nslot = 0\nbypass = false"),
        );
        let Some(Command::ChainMessage { message: back, .. }) = command(stack.undo(&graph)) else {
            panic!("a bypass message back");
        };
        assert_eq!(back["bypass"].as_bool(), Some(true));
        stack.record(&graph, &message("type = \"gui\"\nslot = 0\nopen = true"));
        assert!(!stack.can_undo(), "opening a window is not an edit");
        stack.record(
            &graph,
            &Command::SetChainSlots {
                name: "canalis.eq".into(),
                slots: Vec::new(),
            },
        );
        assert_eq!(
            command(stack.undo(&graph)),
            Some(Command::SetChainSlots {
                name: "canalis.eq".into(),
                slots: graph.chains[0].slots.clone(),
            }),
            "a removed slot comes back"
        );
    }

    #[test]
    fn a_default_change_undoes_to_the_previous_default_and_redoes() {
        let mut graph = Graph::default();
        graph.defaults.sink = Some("speakers".into());
        let mut stack = UndoStack::default();
        let change = Command::SetDefault {
            kind: VirtualKind::Sink,
            name: "canalis.desktop-eq".into(),
        };
        stack.record(&graph, &change);
        assert!(stack.can_undo() && !stack.can_redo());
        assert_eq!(
            command(stack.undo(&graph)),
            Some(Command::SetDefault {
                kind: VirtualKind::Sink,
                name: "speakers".into()
            })
        );
        assert_eq!(command(stack.redo(&graph)), Some(change));
    }

    #[test]
    fn a_new_link_is_undone_by_whatever_id_it_got() {
        let mut graph = Graph::default();
        let mut stack = UndoStack::default();
        let link = Command::Link {
            output: PortId(7),
            input: PortId(9),
        };
        stack.record(&graph, &link);
        assert_eq!(command(stack.undo(&graph)), None, "no such link yet");
        command(stack.redo(&graph));
        graph.links.push(Link {
            id: 42,
            output: PortId(7),
            input: PortId(9),
            ours: true,
        });
        assert_eq!(
            command(stack.undo(&graph)),
            Some(Command::Unlink { link: 42 })
        );
    }

    #[test]
    fn volume_changes_are_not_recorded() {
        let mut stack = UndoStack::default();
        stack.record(
            &Graph::default(),
            &Command::SetVolume {
                node: canalis_pw::NodeId(1),
                cubic: 0.5,
            },
        );
        assert!(!stack.can_undo());
    }
}
