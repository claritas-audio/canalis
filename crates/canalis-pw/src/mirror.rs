//! The registry as plain values, keyed by global id; `snapshot` turns it into a `Graph`.

use std::collections::{BTreeMap, HashMap};

use log::debug;
use pipewire::registry::GlobalObject;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;

use crate::model::{
    ClockSettings, Defaults, Direction, Graph, Latency, Link, MediaClass, Node, Port, Volume,
};

#[derive(Default)]
pub(crate) struct Mirror {
    nodes: BTreeMap<u32, Node>,
    ports: BTreeMap<u32, Port>,
    links: BTreeMap<u32, Link>,
    pub(crate) defaults: Defaults,
    pub(crate) clock: ClockSettings,
    /// `target.object` by subject node id; kept apart because it can arrive before the node.
    targets: HashMap<u32, String>,
}

impl Mirror {
    /// Records a registry global; returns `true` when the graph changed.
    pub(crate) fn add(&mut self, object: &GlobalObject<&DictRef>) -> bool {
        let Some(props) = object.props else {
            return false;
        };
        let get = |key: &str| props.get(key);
        let id = object.id;
        match object.type_ {
            ObjectType::Node => {
                let node = Node::from_props(id, get);
                debug!("+node {id} {:?} {} ({})", node.class, node.name, node.label);
                self.nodes.insert(id, node);
            }
            ObjectType::Port => {
                let Some(port) = Port::from_props(id, get) else {
                    return false;
                };
                debug!(
                    "+port {id} node {} {:?} {}",
                    port.node.0, port.direction, port.name
                );
                self.ports.insert(id, port);
            }
            ObjectType::Link => {
                let Some(link) = Link::from_props(id, get) else {
                    return false;
                };
                debug!("+link {id} {} -> {}", link.output.0, link.input.0);
                self.links.insert(id, link);
            }
            _ => return false,
        }
        true
    }

    /// Forgets a global; returns `true` when it was a mirrored object.
    pub(crate) fn remove(&mut self, id: u32) -> bool {
        let removed = self.nodes.remove(&id).is_some()
            || self.ports.remove(&id).is_some()
            || self.links.remove(&id).is_some();
        if removed {
            debug!("-object {id}");
        }
        removed
    }

    pub(crate) fn clear(&mut self) {
        *self = Mirror::default();
    }

    /// Merges what a node's `info` event carries; keys it lacks keep the registry value
    /// (a null sink's first info has no `node.name`).
    pub(crate) fn update_node<'a>(
        &mut self,
        id: u32,
        get: impl Fn(&str) -> Option<&'a str>,
    ) -> bool {
        let Some(node) = self.nodes.get_mut(&id) else {
            return false;
        };
        let mut updated = node.clone();
        if let Some(name) = get("node.name") {
            updated.name = name.to_owned();
        }
        if let Some(label) = get("node.description").or_else(|| get("node.nick")) {
            updated.label = label.to_owned();
        }
        if let Some(nick) = get("node.nick") {
            updated.short_label = nick.to_owned();
        }
        if let Some(class) = get("media.class") {
            updated.class = MediaClass::from_media_class(Some(class));
        }
        if let Some(app) = get("application.name") {
            updated.app = Some(app.to_owned());
        }
        if let Some(media) = get("media.name") {
            updated.media_name = Some(media.to_owned());
        }
        updated.hardware |= get("device.api").is_some() || get("device.id").is_some();
        if *node == updated {
            return false;
        }
        *node = updated;
        true
    }

    /// A port's `Latency` param for one direction (`Input` is upstream).
    pub(crate) fn set_port_latency(
        &mut self,
        id: u32,
        direction: Direction,
        latency: Latency,
    ) -> bool {
        let Some(port) = self.ports.get_mut(&id) else {
            return false;
        };
        let slot = match direction {
            Direction::Input => &mut port.upstream,
            Direction::Output => &mut port.downstream,
        };
        if *slot == Some(latency) {
            return false;
        }
        *slot = Some(latency);
        true
    }

    pub(crate) fn set_volume(&mut self, id: u32, volume: Volume) -> bool {
        let Some(node) = self.nodes.get_mut(&id) else {
            return false;
        };
        if node.volume.as_ref() == Some(&volume) {
            return false;
        }
        node.volume = Some(volume);
        true
    }

    pub(crate) fn set_target(&mut self, subject: u32, target: Option<&str>) -> bool {
        let before = self.targets.get(&subject).cloned();
        match target {
            Some(t) => self.targets.insert(subject, t.to_owned()),
            None => self.targets.remove(&subject),
        };
        before.as_deref() != target
    }

    pub(crate) fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub(crate) fn node_by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.values().find(|n| n.name == name)
    }

    pub(crate) fn port(&self, id: u32) -> Option<&Port> {
        self.ports.get(&id)
    }

    pub(crate) fn link(&self, id: u32) -> Option<&Link> {
        self.links.get(&id)
    }

    pub(crate) fn snapshot(&self, connected: bool) -> Graph {
        let nodes = self
            .nodes
            .values()
            .map(|n| Node {
                target: self.targets.get(&n.id.0).cloned(),
                ..n.clone()
            })
            .collect();
        Graph {
            connected,
            nodes,
            ports: self.ports.values().cloned().collect(),
            links: self.links.values().copied().collect(),
            defaults: self.defaults.clone(),
            clock: self.clock.clock(),
            chains: Vec::new(),
            plugins: Vec::new(),
            scanning: false,
            scenes: Vec::new(),
            hidden: Default::default(),
            apps: Default::default(),
            midi_input: None,
            controls: Vec::new(),
            recent_plugins: Vec::new(),
            learning: None,
            config_problem: None,
            lv2_off: None,
            playing: None,
            ..Graph::default()
        }
    }
}
