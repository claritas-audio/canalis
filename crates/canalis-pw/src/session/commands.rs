//! Executing the GUI's commands against the live connection, and recording in the
//! config what the user meant to keep.

use std::collections::BTreeMap;

use log::{error, info, warn};
use pipewire::link::Link;
use pipewire::node::Node;
use pipewire::properties::PropertiesBox;
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::Pod;

use super::chains::{auto_level_line, key_line, mute_line, sends_line, slots_line};
use super::reconcile::{link_key, target_key};
use super::{Command, CompareStep, Session};
use crate::config::{
    AppStyle, ChainConfig, LinkByName, PortRef, Scene, VirtualDevice, chain_of_node, feeds_itself,
    would_cycle,
};
use crate::model::{
    ChainKind, Direction, LANE_BASE, NodeId, PortId, VirtualKind, virtual_node_name,
};
use crate::pod;

/// More objects than this from one connection means a request loop, not a user.
const MAX_CREATED_PER_CONNECTION: usize = 64;

impl Session {
    pub(super) fn execute(&mut self, command: Command) {
        if command == Command::ReplaceConfig {
            self.replace_config();
            return;
        }
        if self.connection.is_none() {
            warn!("not connected; dropping {command:?}");
            return;
        }
        match command {
            Command::Link { output, input } => {
                if let Some(by_name) = self.link_by_name(output, input)
                    && by_name.output.node == crate::SOUNDBOARD_NODE
                {
                    self.soundboard_link(&by_name.input.node);
                    return;
                }
                if let Some(by_name) = self.link_by_name(output, input) {
                    if feeds_itself(&self.config.chains, &by_name) {
                        warn!(
                            "refusing {} -> {}: a chain would feed itself",
                            by_name.output.node, by_name.input.node
                        );
                        return;
                    }
                    self.attempts.remove(&link_key(&by_name));
                    self.config.links.retain(|l| *l != by_name);
                    self.config.links.push(by_name);
                    self.save_config();
                }
                self.create_link(output, input);
            }
            Command::Unlink { link } => {
                if let Some(l) = self.mirror.link(link).copied()
                    && let Some(by_name) = self.link_by_name(l.output, l.input)
                    && by_name.output.node == crate::SOUNDBOARD_NODE
                {
                    self.soundboard_unlink(&by_name.input.node);
                    self.destroy_global(link);
                    return;
                }
                if let Some(l) = self.mirror.link(link).copied()
                    && let Some(by_name) = self.link_by_name(l.output, l.input)
                {
                    self.config.links.retain(|c| *c != by_name);
                    self.save_config();
                }
                self.destroy_global(link);
            }
            Command::CreateVirtual { kind, label } => {
                let device = VirtualDevice {
                    name: kind.node_name(&label),
                    label,
                    kind,
                };
                self.config
                    .virtual_devices
                    .retain(|d| d.name != device.name);
                self.config.virtual_devices.push(device.clone());
                self.save_config();
                self.create_virtual(&device);
            }
            Command::DeleteVirtual { name } => {
                self.config.virtual_devices.retain(|d| d.name != name);
                self.save_config();
                if let Some(node) = self.mirror.node_by_name(&name) {
                    self.destroy_global(node.id.0);
                }
            }
            Command::SetDefault { kind, name } => self.set_default(kind, Some(name.as_str())),
            Command::SetTarget { stream, target } => {
                if let Some(t) = &target {
                    self.attempts.remove(&target_key(stream, t));
                }
                if let Some(app) = self.mirror.node(stream.0).and_then(|n| n.app.clone()) {
                    match &target {
                        Some(t) => self.config.targets.insert(app, t.clone()),
                        None => self.config.targets.remove(&app),
                    };
                    self.save_config();
                }
                self.set_target(stream, target.as_deref());
            }
            Command::SetVolume { node, cubic } => self.set_volume(node, cubic),
            Command::SetMute { node, mute } => {
                if let Some(bytes) = pod::props_with_mute(mute) {
                    self.set_props(node, &bytes);
                }
            }
            Command::Hold { target } => self.hold(target),
            Command::PressPad { name } => self.press_pad(&name),
            Command::ReleasePad { name } => self.release_pad(&name),
            Command::StopPad { name } => self.stop_pad(&name),
            Command::StopPads => self.stop_pads(),
            Command::LearnMany { targets } => {
                self.learn_queue = targets.into();
                self.learning = self.learn_queue.pop_front();
                self.last_learned = None;
                self.changed();
            }
            Command::RenamePad { name, to } => self.rename_pad(&name, to),
            Command::SetSoundboard { board } => self.set_soundboard(board),
            Command::SetNotifications { notifications } => {
                self.config.notifications = notifications;
                self.save_config();
                self.changed();
            }
            Command::SetDeviceRule { device, rule } => {
                let rules = &mut self.config.device_rules;
                let at = rules.iter().position(|r| r.device == device);
                match (at, rule.filter(|r| !r.is_empty())) {
                    (Some(at), Some(rule)) => rules[at] = rule,
                    (None, Some(rule)) => rules.push(rule),
                    (Some(at), None) => {
                        rules.remove(at);
                        info!("device rule for {device} removed");
                    }
                    (None, None) => return,
                }
                self.save_config();
                self.changed();
            }
            Command::CreateChain { label, kind } => self.create_chain(label, kind),
            Command::DeleteChain { name } => {
                info!("{name} deleted on request");
                self.config.chains.retain(|c| c.name != name);
                self.save_config();
                if self.supervisor.stop(&name) {
                    self.changed();
                }
            }
            Command::SetChainSlots { name, slots } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    // A new name or folder is only for the strip: the chain keeps its plugins.
                    if same_audio(&chain.slots, &slots) {
                        chain.slots = slots;
                        self.save_config();
                        self.changed();
                        return;
                    }
                    chain.slots = slots;
                    let line = slots_line(&chain.slots, false, None);
                    self.save_config();
                    self.supervisor.forget_slot_infos(&name);
                    self.supervisor.send(&name, &line);
                    self.changed();
                }
            }
            Command::ChainMessage { name, message } => self.chain_message(&name, message),
            Command::SetChainEndpoints {
                name,
                inputs,
                outputs,
            } => self.set_chain_endpoints(&name, inputs, outputs),
            Command::SetChainKey { name, key } => self.set_chain_key(&name, key),
            Command::SetChainGain {
                name,
                target,
                gain_db,
            } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    if gain_db == 0.0 {
                        chain.gains.remove(&target);
                    } else {
                        chain.gains.insert(target, gain_db);
                    }
                    let line = sends_line(chain);
                    self.save_config();
                    if let Some(line) = line {
                        self.supervisor.send(&name, &line);
                    }
                    self.changed();
                }
            }
            Command::SetChainMute { name, mute } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    chain.mute = mute;
                    self.save_config();
                    self.supervisor.send(&name, &mute_line(mute));
                    self.changed();
                }
            }
            Command::SetLabel { node_name, label } => {
                match label.map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()) {
                    Some(label) => self.config.labels.insert(node_name, label),
                    None => self.config.labels.remove(&node_name),
                };
                self.save_config();
                self.changed();
            }
            Command::RenameChain { name, label } => {
                let label = label.trim().to_owned();
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name)
                    && !label.is_empty()
                    && chain.label != label
                {
                    chain.label = label;
                    self.save_config();
                    self.supervisor.stop(&name);
                    self.reconcile_chains();
                    self.changed();
                }
            }
            Command::SetChainColor { name, color } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    chain.color = color;
                    self.save_config();
                    self.changed();
                }
            }
            Command::SetChainIcon { name, icon } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    chain.icon = icon;
                    self.save_config();
                    self.changed();
                }
            }
            Command::SetChainCollapsed { name, collapsed } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    chain.collapsed = collapsed;
                    self.save_config();
                    self.changed();
                }
            }
            Command::SetLoudnessTarget { name, target } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    chain.loudness_target = target;
                    let line = chain.auto_level.then(|| auto_level_line(chain));
                    self.save_config();
                    if let Some(line) = line {
                        self.supervisor.send(&name, &line);
                    }
                    self.changed();
                }
            }
            Command::Compare { name, step } => self.compare(&name, step),
            Command::RestartChain { name } => {
                if let Some(config) = self.config.chains.iter().find(|c| c.name == name).cloned() {
                    self.supervisor.restart(&config);
                    self.changed();
                }
            }
            Command::SetAutoLevel { name, on } => {
                if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
                    chain.auto_level = on;
                    let line = auto_level_line(chain);
                    self.save_config();
                    self.supervisor.send(&name, &line);
                    self.changed();
                }
            }
            Command::ReorderChains { names } => {
                let rank = |c: &ChainConfig| names.iter().position(|n| *n == c.name);
                self.config
                    .chains
                    .sort_by_key(|c| rank(c).unwrap_or(usize::MAX));
                self.save_config();
                self.changed();
            }
            Command::SetHidden { key, hidden } => {
                if hidden {
                    self.config.hidden.insert(key);
                } else {
                    self.config.hidden.remove(&key);
                }
                self.save_config();
                self.changed();
            }
            Command::SetAppStyle { app, label, color } => {
                let label = label.map(|l| l.trim().to_owned()).filter(|l| !l.is_empty());
                if label.is_none() && color.is_none() {
                    self.config.apps.remove(&app);
                } else {
                    self.config.apps.insert(app, AppStyle { label, color });
                }
                self.save_config();
                self.changed();
            }
            Command::ApplyScene { scene } => self.apply_scene(&scene),
            Command::SaveScene { mut scene } => {
                // Saving a scene again takes the routing now; its trigger stays.
                if scene.when.is_none()
                    && let Some(old) = self.config.scenes.iter().find(|s| s.name == scene.name)
                {
                    scene.when = old.when.clone();
                }
                self.config.scenes.retain(|s| s.name != scene.name);
                self.config.scenes.push(scene);
                self.save_config();
                self.changed();
            }
            Command::PlayFile { path, delete_after } => self.play(&path, delete_after),
            Command::StopPlayback => self.end_playback(),
            Command::SetSceneTrigger { name, when } => {
                if let Some(scene) = self.config.scenes.iter_mut().find(|s| s.name == name) {
                    scene.when = when;
                    self.save_config();
                    self.changed();
                }
            }
            Command::DeleteScene { name } => {
                self.config.scenes.retain(|s| s.name != name);
                self.save_config();
                self.changed();
            }
            Command::Learn { target } => {
                self.learning = target;
                self.learn_queue.clear();
                self.last_learned = None;
                self.changed();
            }
            Command::ForgetControl { index } => {
                if index < self.config.controls.len() {
                    self.config.controls.remove(index);
                    self.save_config();
                    self.changed();
                }
            }
            Command::SetMidiInput { node } => {
                self.config.midi_input = node;
                self.save_config();
                self.attempts.retain(|k, _| !k.starts_with("midi "));
                self.reconcile_midi();
                self.changed();
            }
            Command::ScanPlugins => self.scan_plugins(),
            Command::RememberPlugin { plugin } => {
                self.config.remember_plugin(plugin);
                self.save_config();
                self.changed();
            }
            Command::ReplaceConfig | Command::Quit => {}
        }
    }

    fn create_chain(&mut self, label: String, kind: ChainKind) {
        let name = virtual_node_name(&label);
        if self.config.chains.iter().any(|c| c.name == name)
            || self.config.virtual_devices.iter().any(|d| d.name == name)
        {
            warn!("{name} exists already; not creating it twice");
            return;
        }
        self.config.chains.push(ChainConfig {
            name,
            label,
            kind,
            slots: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            mute: false,
            color: None,
            icon: None,
            collapsed: false,
            loudness_target: None,
            auto_level: false,
            compare: None,
            key: None,
            gains: BTreeMap::new(),
        });
        self.save_config();
        self.reconcile_chains();
        self.changed();
    }

    /// Recalls a scene: every default, target, end, mute and bypass that differs from the
    /// current state is set, nothing else is touched, so unchanged links keep playing.
    pub(super) fn apply_scene(&mut self, scene: &Scene) {
        let current = self.mirror.snapshot(true);
        for (kind, wanted, now) in [
            (VirtualKind::Sink, &scene.sink, &current.defaults.sink),
            (VirtualKind::Source, &scene.source, &current.defaults.source),
        ] {
            if let Some(wanted) = wanted
                && now.as_deref() != Some(wanted.as_str())
            {
                self.set_default(kind, Some(wanted.as_str()));
            }
        }
        let apps: Vec<(String, Option<String>)> = self
            .config
            .targets
            .keys()
            .filter(|app| !scene.targets.contains_key(*app))
            .map(|app| (app.clone(), None))
            .chain(
                scene
                    .targets
                    .iter()
                    .map(|(app, target)| (app.clone(), Some(target.clone()))),
            )
            .collect();
        for (app, target) in apps {
            match &target {
                Some(t) => self.config.targets.insert(app.clone(), t.clone()),
                None => self.config.targets.remove(&app),
            };
            let streams: Vec<NodeId> = current
                .nodes
                .iter()
                .filter(|n| n.is_stream() && n.app.as_deref() == Some(app.as_str()))
                .filter(|n| n.target != target)
                .map(|n| n.id)
                .collect();
            for stream in streams {
                if let Some(t) = &target {
                    self.attempts.remove(&target_key(stream, t));
                }
                self.set_target(stream, target.as_deref());
            }
        }
        for wanted in &scene.chains {
            let Some(chain) = self.config.chains.iter().find(|c| c.name == wanted.name) else {
                continue;
            };
            if chain.inputs != wanted.inputs || chain.outputs != wanted.outputs {
                self.set_chain_endpoints(
                    &wanted.name,
                    wanted.inputs.clone(),
                    wanted.outputs.clone(),
                );
            }
            let Some(chain) = self
                .config
                .chains
                .iter_mut()
                .find(|c| c.name == wanted.name)
            else {
                continue;
            };
            if chain.mute != wanted.mute {
                chain.mute = wanted.mute;
                self.supervisor.send(&wanted.name, &mute_line(wanted.mute));
            }
            if chain.gains != wanted.gains {
                chain.gains = wanted.gains.clone();
                if let Some(line) = sends_line(chain) {
                    self.supervisor.send(&wanted.name, &line);
                }
            }
            let key_differs = chain.key != wanted.key;
            let mut lines = Vec::new();
            for (index, bypass) in wanted.bypass.iter().enumerate() {
                let Some(slot) = chain.slots.get_mut(index).and_then(|s| s.as_table_mut()) else {
                    continue;
                };
                let now = slot.get("bypass").and_then(|b| b.as_bool()) == Some(true);
                if now != *bypass {
                    slot.insert("bypass".into(), toml::Value::Boolean(*bypass));
                    lines.push(
                        serde_json::json!({ "type": "bypass", "slot": index, "bypass": bypass })
                            .to_string(),
                    );
                }
            }
            for line in lines {
                self.supervisor.send(&wanted.name, &line);
            }
            if key_differs {
                self.set_chain_key(&wanted.name, wanted.key.clone());
            }
        }
        self.save_config();
        self.changed();
    }

    /// Forwards a live tweak and mirrors it into the saved slot so a restart keeps it.
    fn chain_message(&mut self, name: &str, message: toml::Value) {
        let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) else {
            return;
        };
        let index = message
            .get("slot")
            .and_then(|s| s.as_integer())
            .and_then(|i| usize::try_from(i).ok());
        if let Some(slot) = index.and_then(|i| slot_table_mut(&mut chain.slots, i)) {
            mirror_message(slot, &message);
        }
        self.save_config();
        match serde_json::to_string(&message) {
            Ok(line) => self.supervisor.send(name, &line),
            Err(e) => warn!("unserializable chain message: {e}"),
        }
        self.changed();
    }

    /// A/B on a chain (`ChainConfig::compare_step`); a switch loads the other version
    /// through `SetChainSlots`, so it reloads like any slot change.
    fn compare(&mut self, name: &str, step: CompareStep) {
        let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) else {
            return;
        };
        match chain.compare_step(step) {
            Some(slots) => self.execute(Command::SetChainSlots {
                name: name.to_owned(),
                slots,
            }),
            None => {
                self.save_config();
                self.changed();
            }
        }
    }

    fn set_chain_endpoints(&mut self, name: &str, inputs: Vec<String>, outputs: Vec<String>) {
        let Some(chain) = self.config.chains.iter().find(|c| c.name == name).cloned() else {
            return;
        };
        for target in &outputs {
            if would_cycle(&self.config.chains, &chain, target) {
                warn!("{name} -> {target} would close a cycle; refused");
                return;
            }
        }
        for source in &inputs {
            if let Some(from) = chain_of_node(&self.config.chains, source).cloned()
                && would_cycle(
                    &self.config.chains,
                    &from,
                    &chain.kind.capture_node_name(name),
                )
            {
                warn!("{source} -> {name} would close a cycle; refused");
                return;
            }
        }
        let mut line = None;
        if let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) {
            chain.inputs = inputs;
            chain.outputs = outputs;
            let destinations = chain.destinations();
            chain
                .gains
                .retain(|target, _| destinations.contains(target));
            line = sends_line(chain);
        }
        self.save_config();
        self.attempts.retain(|k, _| !k.starts_with("chain "));
        if let Some(line) = line {
            self.supervisor.send(name, &line);
        }
        self.reconcile_chains();
        self.changed();
    }

    /// The chain's key input follows: it exists while a key is named, and the reconciler
    /// links the key node into it.
    fn set_chain_key(&mut self, name: &str, key: Option<String>) {
        let Some(chain) = self.config.chains.iter_mut().find(|c| c.name == name) else {
            return;
        };
        let was = chain.key.is_some();
        chain.key = key;
        let now = chain.key.is_some();
        self.save_config();
        if was != now {
            self.supervisor.send(name, &key_line(now));
        }
        self.attempts.retain(|k, _| !k.starts_with("chain "));
        self.reconcile_chains();
        self.changed();
    }

    fn link_by_name(&self, output: PortId, input: PortId) -> Option<LinkByName> {
        let port_ref = |id: PortId| {
            let port = self.mirror.port(id.0)?;
            let node = self.mirror.node(port.node.0)?;
            Some(PortRef {
                node: node.name.clone(),
                port: port.name.clone(),
            })
        };
        Some(LinkByName {
            output: port_ref(output)?,
            input: port_ref(input)?,
        })
    }

    pub(super) fn create_link(&mut self, output: PortId, input: PortId) {
        let (Some(out_port), Some(in_port)) =
            (self.mirror.port(output.0), self.mirror.port(input.0))
        else {
            warn!("link {output:?} -> {input:?}: a port is gone");
            return;
        };
        if out_port.direction != Direction::Output || in_port.direction != Direction::Input {
            warn!("link {output:?} -> {input:?}: not output to input");
            return;
        }
        let mut props = PropertiesBox::new();
        props.insert("link.output.node", out_port.node.0.to_string());
        props.insert("link.output.port", output.0.to_string());
        props.insert("link.input.node", in_port.node.0.to_string());
        props.insert("link.input.port", input.0.to_string());
        props.insert("object.linger", "true");
        props.insert("canalis.link", "true");
        let Some(connection) = &mut self.connection else {
            return;
        };
        match connection
            .core
            .create_object::<Link>("link-factory", &props)
        {
            Ok(link) => {
                info!("link requested {} -> {}", output.0, input.0);
                connection.created_links.push(link);
            }
            Err(e) => warn!("link {} -> {} failed: {e}", output.0, input.0),
        }
    }

    pub(super) fn destroy_global(&mut self, id: u32) {
        let Some(connection) = &self.connection else {
            return;
        };
        if let Err(e) = connection.registry.destroy_global(id).into_result() {
            warn!("destroying {id} failed: {e}");
        } else {
            info!("destroy requested for {id}");
        }
    }

    pub(super) fn create_virtual(&mut self, device: &VirtualDevice) {
        let mut props = PropertiesBox::new();
        props.insert("factory.name", "support.null-audio-sink");
        props.insert("node.name", device.name.as_str());
        props.insert("node.description", device.label.as_str());
        props.insert("node.nick", device.label.as_str());
        props.insert("media.class", device.kind.media_class());
        props.insert("audio.position", "[ FL FR ]");
        props.insert("monitor.channel-volumes", "true");
        props.insert("object.linger", "true");
        let Some(connection) = &mut self.connection else {
            return;
        };
        if connection.created_nodes.len() >= MAX_CREATED_PER_CONNECTION {
            error!(
                "refusing to create {}: too many nodes created already",
                device.name
            );
            return;
        }
        match connection.core.create_object::<Node>("adapter", &props) {
            Ok(node) => {
                info!("virtual {:?} {} requested", device.kind, device.name);
                connection.created_nodes.push(node);
            }
            Err(e) => warn!("creating {} failed: {e}", device.name),
        }
    }

    /// `None` clears the configured default, so WirePlumber picks one by priority.
    pub(super) fn set_default(&mut self, kind: VirtualKind, name: Option<&str>) {
        let key = match kind {
            VirtualKind::Sink => "default.configured.audio.sink",
            VirtualKind::Source => "default.configured.audio.source",
        };
        let Some(metadata) = self.connection.as_ref().and_then(|c| c.defaults.as_ref()) else {
            warn!("no default metadata object; cannot set the default");
            return;
        };
        match name {
            Some(name) => {
                let value = format!("{{\"name\":\"{name}\"}}");
                metadata
                    .proxy
                    .set_property(0, key, Some("Spa:String:JSON"), Some(&value));
                info!("{key} = {name}");
            }
            None => {
                metadata.proxy.set_property(0, key, None, None);
                info!("{key} cleared");
            }
        }
    }

    pub(super) fn set_target(&mut self, stream: NodeId, target: Option<&str>) {
        let Some(metadata) = self.connection.as_ref().and_then(|c| c.defaults.as_ref()) else {
            warn!("no default metadata object; cannot set a target");
            return;
        };
        metadata
            .proxy
            .set_property(stream.0, "target.object", None, target);
        info!("target.object of {} = {target:?}", stream.0);
    }

    fn set_volume(&mut self, node: NodeId, cubic: f32) {
        let channels = self
            .mirror
            .node(node.0)
            .and_then(|n| n.volume.as_ref())
            .map_or(2, |v| v.channels.len());
        let linear = cubic.clamp(0.0, 1.0).powi(3);
        let Some(bytes) = pod::props_with_volume(linear, channels) else {
            warn!("could not build the volume pod");
            return;
        };
        self.set_props(node, &bytes);
    }

    /// Sends a serialized `Props` object to a bound node.
    fn set_props(&mut self, node: NodeId, bytes: &[u8]) {
        let Some(pod) = Pod::from_bytes(bytes) else {
            return;
        };
        let Some(bound) = self.connection.as_ref().and_then(|c| c.nodes.get(&node.0)) else {
            warn!("node {} is not bound; cannot set its props", node.0);
            return;
        };
        bound.proxy.set_param(ParamType::Props, 0, pod);
    }
}

/// Writes the field a live `canalis_chain::ipc::ToChain` message changes into the saved slot.
/// The slot table a slot id addresses, through a group's `lanes` when it is inside one.
pub(super) fn slot_table_mut(slots: &mut [toml::Value], id: usize) -> Option<&mut toml::Table> {
    if id < LANE_BASE {
        return slots.get_mut(id)?.as_table_mut();
    }
    let rest = id - LANE_BASE;
    let (top, lane, sub) = (rest / 100, (rest % 100) / 10, rest % 10);
    slots
        .get_mut(top)?
        .get_mut("spec")?
        .get_mut("lanes")?
        .get_mut(lane)?
        .get_mut("slots")?
        .get_mut(sub)?
        .as_table_mut()
}

fn mirror_message(slot: &mut toml::Table, message: &toml::Value) {
    let kind = message.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let spec = slot
        .entry("spec")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut();
    match kind {
        "lane_gain" | "lane_mute" | "lane_pan" => {
            let lane = message.get("lane").and_then(|v| v.as_integer());
            let (field, value) = match kind {
                "lane_gain" => ("gain_db", message.get("gain_db")),
                "lane_mute" => ("mute", message.get("mute")),
                _ => ("pan", message.get("pan")),
            };
            if let (Some(lane), Some(value), Some(spec)) = (lane, value, spec)
                && let Some(table) = spec
                    .get_mut("lanes")
                    .and_then(|l| l.get_mut(lane as usize))
                    .and_then(|l| l.as_table_mut())
            {
                table.insert(field.into(), value.clone());
            }
        }
        "bypass" | "mix" => {
            if let Some(v) = message.get(kind) {
                slot.insert(kind.into(), v.clone());
            }
        }
        "eq_bands" => {
            if let (Some(bands), Some(spec)) = (message.get("bands"), spec) {
                spec.insert("bands".into(), bands.clone());
            }
        }
        "param" => {
            let id = message.get("id").and_then(|v| v.as_integer());
            let value = message.get("value").and_then(|v| v.as_float());
            if let (Some(id), Some(value), Some(spec)) = (id, value, spec) {
                spec.entry("params")
                    .or_insert_with(|| toml::Value::Table(Default::default()))
                    .as_table_mut()
                    .map(|params| params.insert(id.to_string(), toml::Value::Float(value)));
            }
        }
        // A preset starts the plugin over: the chain reports the new state soon after.
        "preset" => {
            if let (Some(uri), Some(spec)) = (message.get("uri"), spec) {
                spec.insert("preset".into(), uri.clone());
                spec.remove("state");
                spec.remove("params");
            }
        }
        _ => {}
    }
}

/// Keeps the plugin's own state in the saved slot; it supersedes the params map.
/// Whether two slot lists differ in names and folders at most, inside lanes too.
fn same_audio(a: &[toml::Value], b: &[toml::Value]) -> bool {
    fn audible(slot: &toml::Value) -> toml::Value {
        let mut slot = slot.clone();
        if let Some(table) = slot.as_table_mut() {
            table.remove("folder");
            table.remove("label");
            if let Some(lanes) = table
                .get_mut("spec")
                .and_then(|s| s.get_mut("lanes"))
                .and_then(|l| l.as_array_mut())
            {
                for lane in lanes {
                    if let Some(slots) = lane.get_mut("slots").and_then(|s| s.as_array_mut()) {
                        for sub in slots.iter_mut() {
                            *sub = audible(sub);
                        }
                    }
                }
            }
        }
        slot
    }
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| audible(a) == audible(b))
}

pub(super) fn store_state(slot: &mut toml::Table, state: &str) {
    if let Some(spec) = slot
        .entry("spec")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
    {
        spec.insert("state".into(), toml::Value::String(state.to_owned()));
        spec.remove("params");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_folders_leave_the_audio_alone() {
        let slot = |extra: &str| -> toml::Value {
            toml::from_str(&format!(
                "{extra}\n[spec]\nkind = \"group\"\n[[spec.lanes]]\nlabel = \"a\"\n[[spec.lanes.slots]]\n[spec.lanes.slots.spec]\nkind = \"comp\""
            ))
            .unwrap()
        };
        let plain = vec![slot("")];
        assert!(same_audio(
            &plain,
            &[slot("label = \"Wide\"\nfolder = \"F\"")]
        ));
        let mut renamed_inside = slot("");
        renamed_inside["spec"]["lanes"][0]["slots"][0]
            .as_table_mut()
            .unwrap()
            .insert("label".into(), "Glue".into());
        assert!(same_audio(&plain, &[renamed_inside]));
        assert!(!same_audio(&plain, &[slot("bypass = true")]));
        assert!(!same_audio(&plain, &[]));
    }

    #[test]
    fn a_lane_slot_id_reaches_into_the_group() {
        let mut slots: Vec<toml::Value> = vec![
            toml::from_str("[spec]\nkind = \"comp\"").unwrap(),
            toml::from_str(
                "[spec]\nkind = \"group\"\n[[spec.lanes]]\nlabel = \"a\"\n[[spec.lanes.slots]]\n[spec.lanes.slots.spec]\nkind = \"eq\"",
            )
            .unwrap(),
        ];
        assert!(slot_table_mut(&mut slots, 0).is_some());
        assert!(slot_table_mut(&mut slots, 2).is_none());
        let lane_slot = slot_table_mut(&mut slots, LANE_BASE + 100).expect("lane 0 slot 0");
        assert_eq!(lane_slot["spec"]["kind"].as_str(), Some("eq"));
        assert!(slot_table_mut(&mut slots, LANE_BASE + 101).is_none());
        let message: toml::Value =
            toml::from_str("type = \"lane_gain\"\nslot = 1\nlane = 0\ngain_db = -6.0").unwrap();
        let group = slot_table_mut(&mut slots, 1).expect("the group");
        mirror_message(group, &message);
        assert_eq!(
            slots[1]["spec"]["lanes"][0]["gain_db"].as_float(),
            Some(-6.0)
        );
    }

    #[test]
    fn plugin_state_replaces_the_params_map() {
        let mut table = slot();
        let param: toml::Value =
            toml::from_str("type = \"param\"\nslot = 0\nid = 42\nvalue = 0.25").expect("toml");
        mirror_message(&mut table, &param);
        store_state(&mut table, "AAEC");
        assert_eq!(table["spec"]["state"].as_str(), Some("AAEC"));
        assert!(table["spec"].get("params").is_none());
    }

    fn slot() -> toml::Table {
        toml::from_str(r#"spec = { kind = "clap", path = "/x.clap" }"#).expect("a slot table")
    }

    #[test]
    fn live_messages_land_in_the_saved_slot() {
        let mut table = slot();
        let bypass: toml::Value = toml::from_str(
            r#"type = "bypass"
slot = 0
bypass = true"#,
        )
        .expect("toml");
        mirror_message(&mut table, &bypass);
        assert_eq!(table.get("bypass"), Some(&toml::Value::Boolean(true)));

        let param: toml::Value = toml::from_str(
            r#"type = "param"
slot = 0
id = 42
value = 0.25"#,
        )
        .expect("toml");
        mirror_message(&mut table, &param);
        let saved = table["spec"]["params"]["42"].as_float();
        assert_eq!(saved, Some(0.25));
    }
}
