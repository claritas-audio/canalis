//! One connection to the daemon: core, registry, the `default` metadata object and a
//! bound proxy per node (for `media.name` and the `Props` volume).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use log::{info, warn};
use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::link::Link;
use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::node::{Node, NodeListener};
use pipewire::port::{Port, PortListener};
use pipewire::registry::{GlobalObject, RegistryRc, RegistryWeak};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::Pod;
use pipewire::spa::utils::Direction;
use pipewire::spa::utils::dict::DictRef;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use pipewire::types::ObjectType;
use pipewire::{core, registry};

use super::{Session, client_properties};
use crate::midi;
use crate::pod;

/// Proxies and hooks of one connection; listeners first so they drop before their proxies.
pub(super) struct Connection {
    _core_listener: core::Listener,
    _registry_listener: registry::Listener,
    /// The MIDI input; events go to `Session::on_midi` on this thread.
    _midi: Option<(StreamRc, StreamListener<()>)>,
    /// WirePlumber's `default` metadata object, bound once it appears.
    pub(super) defaults: Option<DefaultsMetadata>,
    /// PipeWire's `settings` metadata, for the graph's rate and quantum.
    pub(super) settings: Option<DefaultsMetadata>,
    pub(super) nodes: HashMap<u32, BoundNode>,
    /// Every port, bound for its `Latency` param.
    pub(super) ports: HashMap<u32, BoundPort>,
    /// Objects this session created; kept so their proxies are not destroyed early.
    pub(super) created_links: Vec<Link>,
    pub(super) created_nodes: Vec<Node>,
    pub(super) registry: RegistryRc,
    pub(super) core: CoreRc,
}

pub(super) struct DefaultsMetadata {
    pub(super) id: u32,
    _listener: MetadataListener,
    pub(super) proxy: Metadata,
}

pub(super) struct BoundPort {
    _listener: PortListener,
    _proxy: Port,
}

pub(super) struct BoundNode {
    _listener: NodeListener,
    pub(super) proxy: Node,
}

impl Connection {
    pub(super) fn open(
        context: &ContextRc,
        session: Weak<RefCell<Session>>,
    ) -> Result<Self, pipewire::Error> {
        let core = context.connect_rc(Some(client_properties()))?;
        let registry = core.get_registry_rc()?;

        let on_done = session.clone();
        let on_error = session.clone();
        let core_listener = core
            .add_listener_local()
            .done(move |id, seq| {
                if let Some(s) = on_done.upgrade() {
                    s.borrow_mut().done(id, seq);
                }
            })
            .error(move |id, _seq, res, message| {
                if let Some(s) = on_error.upgrade() {
                    s.borrow_mut().error(id, res, message);
                }
            })
            .register();

        let on_global = session.clone();
        let on_remove = session.clone();
        let midi = match open_midi(&core, session) {
            Ok(midi) => Some(midi),
            Err(e) => {
                warn!("no MIDI input: {e}");
                None
            }
        };
        let registry_weak = registry.downgrade();
        let registry_listener = registry
            .add_listener_local()
            .global(move |object| {
                if let Some(s) = on_global.upgrade() {
                    Session::global(&s, &registry_weak, object);
                }
            })
            .global_remove(move |id| {
                if let Some(s) = on_remove.upgrade() {
                    s.borrow_mut().global_removed(id);
                }
            })
            .register();

        Ok(Connection {
            _core_listener: core_listener,
            _registry_listener: registry_listener,
            _midi: midi,
            defaults: None,
            settings: None,
            ports: HashMap::new(),
            nodes: HashMap::new(),
            created_links: Vec::new(),
            created_nodes: Vec::new(),
            registry,
            core,
        })
    }
}

impl Session {
    pub(super) fn global(
        session: &Rc<RefCell<Session>>,
        registry: &RegistryWeak,
        object: &GlobalObject<&DictRef>,
    ) {
        let mut s = session.borrow_mut();
        if s.mirror.add(object) {
            s.changed();
            if object.type_ == ObjectType::Node {
                drop(s);
                Session::bind_node(session, registry, object);
            } else if object.type_ == ObjectType::Port {
                drop(s);
                Session::bind_port(session, registry, object);
            }
            return;
        }
        let name = (object.type_ == ObjectType::Metadata)
            .then(|| object.props.and_then(|p| p.get("metadata.name")))
            .flatten();
        let is_defaults = match name {
            Some("default") => true,
            Some("settings") => false,
            _ => return,
        };
        let Some(registry) = registry.upgrade() else {
            return;
        };
        let bound = match registry.bind::<Metadata, _>(object) {
            Ok(proxy) => proxy,
            Err(e) => {
                warn!("binding the default metadata failed: {e}");
                return;
            }
        };
        let on_property = Rc::downgrade(session);
        let listener = bound
            .add_listener_local()
            .property(move |subject, key, _type, value| {
                if let Some(s) = on_property.upgrade() {
                    let mut s = s.borrow_mut();
                    if is_defaults {
                        s.metadata_property(subject, key, value);
                    } else if subject == pipewire::core::PW_ID_CORE {
                        s.settings_property(key, value);
                    }
                }
                0
            })
            .register();
        let metadata = DefaultsMetadata {
            id: object.id,
            _listener: listener,
            proxy: bound,
        };
        if let Some(connection) = &mut s.connection {
            if is_defaults {
                connection.defaults = Some(metadata);
            } else {
                connection.settings = Some(metadata);
            }
        }
    }

    /// A `settings` metadata update: the clock keys move the graph's rate and quantum.
    fn settings_property(&mut self, key: Option<&str>, value: Option<&str>) {
        let changed = match key {
            Some(key) => self.mirror.clock.apply(key, value),
            None => {
                self.mirror.clock = Default::default();
                true
            }
        };
        if changed {
            self.changed();
        }
    }

    fn bind_port(
        session: &Rc<RefCell<Session>>,
        registry: &RegistryWeak,
        object: &GlobalObject<&DictRef>,
    ) {
        let Some(registry) = registry.upgrade() else {
            return;
        };
        let proxy = match registry.bind::<Port, _>(object) {
            Ok(proxy) => proxy,
            Err(e) => {
                warn!("binding port {} failed: {e}", object.id);
                return;
            }
        };
        let id = object.id;
        let on_param = Rc::downgrade(session);
        let listener = proxy
            .add_listener_local()
            .param(move |_seq, param, _index, _next, pod| {
                if param != ParamType::Latency {
                    return;
                }
                let Some((direction, latency)) = pod.and_then(pod::latency_from_param) else {
                    return;
                };
                if let Some(s) = on_param.upgrade() {
                    let mut s = s.borrow_mut();
                    if s.mirror.set_port_latency(id, direction, latency) {
                        s.changed();
                    }
                }
            })
            .register();
        proxy.subscribe_params(&[ParamType::Latency]);
        let mut s = session.borrow_mut();
        if let Some(connection) = &mut s.connection {
            connection.ports.insert(
                id,
                BoundPort {
                    _listener: listener,
                    _proxy: proxy,
                },
            );
        }
    }

    fn bind_node(
        session: &Rc<RefCell<Session>>,
        registry: &RegistryWeak,
        object: &GlobalObject<&DictRef>,
    ) {
        let Some(registry) = registry.upgrade() else {
            return;
        };
        let proxy = match registry.bind::<Node, _>(object) {
            Ok(proxy) => proxy,
            Err(e) => {
                warn!("binding node {} failed: {e}", object.id);
                return;
            }
        };
        let id = object.id;
        let on_info = Rc::downgrade(session);
        let on_param = Rc::downgrade(session);
        let listener = proxy
            .add_listener_local()
            .info(move |info| {
                if let (Some(s), Some(props)) = (on_info.upgrade(), info.props()) {
                    let mut s = s.borrow_mut();
                    if s.mirror.update_node(id, |key| props.get(key)) {
                        s.changed();
                    }
                }
            })
            .param(move |_seq, param, _index, _next, pod| {
                if param != ParamType::Props {
                    return;
                }
                let Some(volume) = pod.and_then(pod::volume_from_props) else {
                    return;
                };
                if let Some(s) = on_param.upgrade() {
                    let mut s = s.borrow_mut();
                    if s.mirror.set_volume(id, volume) {
                        s.changed();
                    }
                }
            })
            .register();
        proxy.subscribe_params(&[ParamType::Props]);
        let mut s = session.borrow_mut();
        if let Some(connection) = &mut s.connection {
            connection.nodes.insert(
                id,
                BoundNode {
                    _listener: listener,
                    proxy,
                },
            );
        }
    }

    pub(super) fn global_removed(&mut self, id: u32) {
        let was_defaults = self
            .connection
            .as_ref()
            .and_then(|c| c.defaults.as_ref())
            .is_some_and(|d| d.id == id);
        if was_defaults {
            if let Some(connection) = &mut self.connection {
                connection.defaults = None;
            }
            self.mirror.defaults = Default::default();
            self.changed();
            return;
        }
        if let Some(connection) = &mut self.connection {
            connection.nodes.remove(&id);
            connection.ports.remove(&id);
            if connection.settings.as_ref().is_some_and(|m| m.id == id) {
                connection.settings = None;
                self.mirror.clock = Default::default();
            }
        }
        // PipeWire hands the id to the next stream, which must not inherit a spent count.
        let prefix = format!("target {id} -> ");
        self.attempts.retain(|k, _| !k.starts_with(&prefix));
        if self.mirror.remove(id) {
            self.changed();
        }
    }

    /// A `default` metadata update; `key == None` clears every property of the subject.
    fn metadata_property(&mut self, subject: u32, key: Option<&str>, value: Option<&str>) {
        let changed = if subject == pipewire::core::PW_ID_CORE {
            match key {
                Some(key) => self.mirror.defaults.apply(key, value),
                None => {
                    self.mirror.defaults = Default::default();
                    true
                }
            }
        } else {
            match key {
                Some("target.object") | None => self.mirror.set_target(subject, value),
                Some(_) => false,
            }
        };
        if changed {
            if subject == pipewire::core::PW_ID_CORE {
                info!(
                    "defaults: sink {:?}, source {:?}",
                    self.mirror.defaults.sink, self.mirror.defaults.source
                );
            }
            self.changed();
        }
    }
}

/// A control stream, `canalis.midi`, whose buffers are parsed on the loop thread.
fn open_midi(
    core: &CoreRc,
    session: Weak<RefCell<Session>>,
) -> Result<(StreamRc, StreamListener<()>), pipewire::Error> {
    let props = pipewire::properties::properties! {
        "node.name" => crate::MIDI_INPUT_NAME,
        "node.description" => "Canalis control",
        "media.type" => "Midi",
        "media.category" => "Capture",
        "media.role" => "DSP",
        "node.autoconnect" => "false",
    };
    let stream = StreamRc::new(core.clone(), "midi", props)?;
    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, ()| {
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let mut events = Vec::new();
                for data in buffer.datas_mut() {
                    let size = data.chunk().size() as usize;
                    if let Some(bytes) = data.data() {
                        events.extend(midi::parse_sequence(&bytes[..size.min(bytes.len())]));
                    }
                }
                if let Some(s) = session.upgrade() {
                    for event in events {
                        s.borrow_mut().on_midi(event);
                    }
                }
            }
        })
        .register()?;
    let format = midi::control_format_pod();
    let mut params = [Pod::from_bytes(&format).expect("a valid format pod")];
    stream.connect(
        Direction::Input,
        None,
        StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    Ok((stream, listener))
}
