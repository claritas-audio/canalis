//! Making the registry match the config: runs after every published snapshot and asks
//! for whatever named object is missing. Attempts per key are never reset by what the
//! registry shows (a flapping object would loop forever), only by a reconnect or by the
//! user touching that key again.

use log::{info, warn};

use super::Session;
use crate::config::{chain_of_node, feeds_itself};
use crate::model::{
    Direction, Graph, INSERT_CHANNELS, Node, PortId, VIRTUAL_PREFIX, send_channels,
};

/// After this many unanswered requests for the same object we stop asking.
const MAX_ATTEMPTS: u8 = 3;

/// The attempt key of a configured link.
pub(super) fn link_key(l: &crate::config::LinkByName) -> String {
    format!(
        "{}:{} -> {}:{}",
        l.output.node, l.output.port, l.input.node, l.input.port
    )
}

pub(super) fn target_key(stream: crate::model::NodeId, target: &str) -> String {
    format!("target {} -> {target}", stream.0)
}

impl Session {
    pub(super) fn reconcile(&mut self) {
        self.reconcile_virtual_devices();
        self.reconcile_links();
        self.reconcile_targets();
        self.reconcile_chains();
        self.reconcile_midi();
        self.reconcile_pads();
    }

    /// The configured MIDI source's first output port into `canalis.midi`, nothing else.
    pub(super) fn reconcile_midi(&mut self) {
        let graph = self.mirror.snapshot(true);
        let Some(ours) = graph.node_by_name(crate::MIDI_INPUT_NAME) else {
            return;
        };
        let Some(input) = graph
            .ports_of(ours.id)
            .find(|p| p.direction == Direction::Input)
        else {
            return;
        };
        let wanted = self
            .config
            .midi_input
            .as_deref()
            .and_then(|name| graph.node_by_name(name))
            .and_then(|source| {
                graph
                    .ports_of(source.id)
                    .find(|p| p.direction == Direction::Output)
            })
            .map(|p| p.id);
        let stale: Vec<u32> = graph
            .links
            .iter()
            .filter(|l| l.input == input.id && Some(l.output) != wanted)
            .map(|l| l.id)
            .collect();
        for id in stale {
            info!("link {id} into the MIDI input is not the configured source; removing it");
            self.destroy_global(id);
        }
        if let Some(output) = wanted
            && graph.link_between(output, input.id).is_none()
            && self.attempt(format!("midi {} -> {}", output.0, input.id.0))
        {
            self.create_link(output, input.id);
        }
    }

    /// Processes for every configured chain, none for the rest, and the links their
    /// stream ends asked for: destination `i` gets the playback end's channel pair `i`,
    /// every input to the capture end's port of the same channel.
    pub(super) fn reconcile_chains(&mut self) {
        let configs = self.config.chains.clone();
        let mut changed = false;
        for config in &configs {
            changed |= self.supervisor.ensure(config);
        }
        let stale: Vec<String> = self
            .supervisor
            .running()
            .filter(|n| !configs.iter().any(|c| c.name == *n))
            .map(str::to_owned)
            .collect();
        for name in stale {
            changed |= self.supervisor.stop(&name);
        }
        if changed {
            self.changed();
        }

        let graph = self.mirror.snapshot(true);
        let mut wanted: Vec<(PortId, PortId)> = Vec::new();
        let mut owned_ends: Vec<(crate::model::NodeId, Direction)> = Vec::new();
        for config in &configs {
            let insert = config.hardware_insert();
            if let Some(from) = graph.node_by_name(&config.kind.playback_node_name(&config.name)) {
                owned_ends.push((from.id, Direction::Output));
                for (send, target) in config.destinations().iter().enumerate() {
                    if let Some(to) = graph.node_by_name(target) {
                        wanted.extend(send_pairs(&graph, from, to, send));
                    }
                }
                if let Some(to) = insert
                    .as_ref()
                    .and_then(|(send, _)| graph.node_by_name(send))
                {
                    wanted.extend(pairs_by_channel(
                        &graph,
                        from,
                        INSERT_CHANNELS,
                        to,
                        ["FL", "FR"],
                    ));
                }
            }
            if let Some(to) = graph.node_by_name(&config.kind.insert_return_node_name(&config.name))
            {
                owned_ends.push((to.id, Direction::Input));
                if let Some(from) = insert.as_ref().and_then(|(_, ret)| graph.node_by_name(ret)) {
                    wanted.extend(channel_pairs(&graph, from, to));
                }
            }
            if config.kind.picks_inputs()
                && let Some(to) = graph.node_by_name(&config.kind.capture_node_name(&config.name))
            {
                owned_ends.push((to.id, Direction::Input));
                for source in &config.inputs {
                    if let Some(from) = graph.node_by_name(source) {
                        wanted.extend(channel_pairs(&graph, from, to));
                    }
                }
            }
            if let Some(to) = graph.node_by_name(&config.kind.key_node_name(&config.name)) {
                owned_ends.push((to.id, Direction::Input));
                if let Some(from) = config.key.as_deref().and_then(|k| graph.node_by_name(k)) {
                    wanted.extend(channel_pairs(&graph, from, to));
                }
            }
        }
        for (output, input) in &wanted {
            if graph.link_between(*output, *input).is_none()
                && self.attempt(format!("chain link {} -> {}", output.0, input.0))
            {
                self.create_link(*output, *input);
            }
        }
        let stale_links: Vec<u32> = graph
            .links
            .iter()
            .filter(|l| {
                let touches_owned_end = owned_ends.iter().any(|(node, direction)| {
                    let port = match direction {
                        Direction::Output => graph.port(l.output),
                        Direction::Input => graph.port(l.input),
                    };
                    port.is_some_and(|p| p.node == *node)
                });
                touches_owned_end && !wanted.contains(&(l.output, l.input))
            })
            .map(|l| l.id)
            .collect();
        for id in stale_links {
            info!("link {id} is not one a chain asked for; removing it");
            self.destroy_global(id);
        }
    }

    /// One attempt for `key`; `false` once the object has failed to appear too often.
    pub(super) fn attempt(&mut self, key: String) -> bool {
        let count = self.attempts.entry(key.clone()).or_insert(0);
        if *count >= MAX_ATTEMPTS {
            if *count == MAX_ATTEMPTS {
                warn!("giving up on {key} after {MAX_ATTEMPTS} attempts");
                *count += 1;
            }
            return false;
        }
        *count += 1;
        true
    }

    fn reconcile_virtual_devices(&mut self) {
        let wanted = self.config.virtual_devices.clone();
        for device in &wanted {
            if self.mirror.node_by_name(&device.name).is_some() {
                continue;
            }
            if self.attempt(device.name.clone()) {
                self.create_virtual(device);
            }
        }
        let chains = self.config.chains.clone();
        let orphans: Vec<(u32, String)> = self
            .mirror
            .snapshot(true)
            .nodes
            .iter()
            .filter(|n| n.name.starts_with(VIRTUAL_PREFIX) && !n.is_stream())
            .filter(|n| n.name != crate::MIDI_INPUT_NAME && n.name != crate::SOUNDBOARD_NODE)
            .filter(|n| !wanted.iter().any(|d| d.name == n.name))
            .filter(|n| chain_of_node(&chains, &n.name).is_none())
            .map(|n| (n.id.0, n.name.clone()))
            .collect();
        for (id, name) in orphans {
            info!("{name} is not in the config any more; removing it");
            self.destroy_global(id);
        }
    }

    fn reconcile_links(&mut self) {
        let before = self.config.links.len();
        let chains = self.config.chains.clone();
        self.config.links.retain(|l| {
            let bad = feeds_itself(&chains, l);
            if bad {
                warn!(
                    "dropping saved link {}: a chain would feed itself",
                    link_key(l)
                );
            }
            !bad
        });
        if self.config.links.len() != before {
            self.save_config();
        }
        let graph = self.mirror.snapshot(true);
        let port_id = |node: &str, port: &str| -> Option<PortId> {
            let node = graph.node_by_name(node)?;
            graph
                .ports_of(node.id)
                .find(|p| p.name == port)
                .map(|p| p.id)
        };
        let wanted: Vec<(String, Option<(PortId, PortId)>)> = self
            .config
            .links
            .iter()
            .map(|l| {
                let key = link_key(l);
                let ports = Some((
                    port_id(&l.output.node, &l.output.port)?,
                    port_id(&l.input.node, &l.input.port)?,
                ));
                Some((key, ports))
            })
            .map(|r| r.unwrap_or_else(|| (String::new(), None)))
            .collect();
        let wanted_pairs: Vec<(PortId, PortId)> = wanted.iter().filter_map(|(_, p)| *p).collect();
        for (key, ports) in wanted {
            let Some((output, input)) = ports else {
                continue;
            };
            if graph.link_between(output, input).is_some() {
                continue;
            }
            if self.attempt(key) {
                self.create_link(output, input);
            }
        }
        // Our links linger past the app on purpose; the ones between plain nodes that the
        // config dropped since would otherwise stay forever. Chain ends and the MIDI input
        // sweep their own.
        let chains = self.config.chains.clone();
        let owned = |port: PortId| -> bool {
            graph
                .port(port)
                .and_then(|p| graph.node(p.node))
                .is_some_and(|n| {
                    chain_of_node(&chains, &n.name).is_some() || n.name == crate::MIDI_INPUT_NAME
                })
        };
        let stale: Vec<u32> = graph
            .links
            .iter()
            .filter(|l| l.ours && !wanted_pairs.contains(&(l.output, l.input)))
            .filter(|l| !owned(l.output) && !owned(l.input))
            .map(|l| l.id)
            .collect();
        for id in stale {
            info!("link {id} is ours but not in the config any more; removing it");
            self.destroy_global(id);
        }
    }

    fn reconcile_targets(&mut self) {
        let graph = self.mirror.snapshot(true);
        let moves: Vec<(crate::model::NodeId, String)> = graph
            .nodes
            .iter()
            .filter(|n| n.is_stream())
            .filter_map(|n| {
                let app = n.app.as_ref()?;
                let wanted = self.config.targets.get(app)?;
                (n.target.as_deref() != Some(wanted)).then(|| (n.id, wanted.clone()))
            })
            .collect();
        for (stream, target) in moves {
            if self.attempt(target_key(stream, &target)) {
                self.set_target(stream, Some(&target));
            }
        }
    }
}

/// Send pair `send` of a chain's playback end into `to`'s FL/FR; pair 0 goes by name so
/// a destination with more channels still gets each of its own.
fn send_pairs(graph: &Graph, from: &Node, to: &Node, send: usize) -> Vec<(PortId, PortId)> {
    if send == 0 {
        return channel_pairs(graph, from, to);
    }
    let Some(channels) = send_channels(send) else {
        return Vec::new();
    };
    pairs_by_channel(graph, from, channels, to, ["FL", "FR"])
}

/// `from`'s output ports named `outs` into `to`'s input ports named `ins`, in order.
fn pairs_by_channel(
    graph: &Graph,
    from: &Node,
    outs: [&str; 2],
    to: &Node,
    ins: [&str; 2],
) -> Vec<(PortId, PortId)> {
    outs.iter()
        .zip(ins)
        .filter_map(|(out_channel, in_channel)| {
            let out = graph.ports_of(from.id).find(|p| {
                p.direction == Direction::Output && p.channel.as_deref() == Some(out_channel)
            })?;
            let input = graph.ports_of(to.id).find(|p| {
                p.direction == Direction::Input && p.channel.as_deref() == Some(in_channel)
            })?;
            Some((out.id, input.id))
        })
        .collect()
}

/// `from`'s output ports paired with `to`'s input ports of the same `audio.channel`.
fn channel_pairs(graph: &Graph, from: &Node, to: &Node) -> Vec<(PortId, PortId)> {
    // A null source's capture ports are monitor ports to PipeWire: for a source they
    // are the real outputs, for a sink they are the copy we leave alone.
    let monitors_count = from.class == crate::model::MediaClass::Source;
    graph
        .ports_of(from.id)
        .filter(|p| p.direction == Direction::Output && (monitors_count || !p.monitor))
        .filter_map(|out| {
            let channel = out.channel.as_deref()?;
            let input = graph.ports_of(to.id).find(|p| {
                p.direction == Direction::Input && p.channel.as_deref() == Some(channel)
            })?;
            Some((out.id, input.id))
        })
        .collect()
}
