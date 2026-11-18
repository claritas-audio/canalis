//! The service's socket: one control plane, many clients (the window, the tray, CLI
//! calls). One JSON object per line both ways. The `hello` and `shutdown` lines never
//! change shape, so a client and a service of different versions can always agree on a
//! restart.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::model::{ChainMeters, Graph, MeterValues};
use crate::session::{Command, Handle};

/// Bumped whenever a line changes shape.
pub const PROTOCOL: u32 = 1;
/// The build a client and a service must share to talk; anything else restarts the service.
pub const BUILD: &str = env!("CARGO_PKG_VERSION");
const METERS_EVERY: Duration = Duration::from_millis(33);
const TICK: Duration = Duration::from_millis(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// A client asks for the service to be started at most this often.
const START_EVERY: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_millis(200);
/// How long a second launch waits for the service to say whether it raised a window.
const RAISE_WAIT: Duration = Duration::from_secs(2);

/// Service to client. Externally tagged: an internally tagged enum buffers its content,
/// and the buffer cannot read the graph's integer map keys back from JSON strings.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Up {
    Hello {
        protocol: u32,
        build: String,
    },
    Graph {
        graph: Box<Graph>,
    },
    /// Every running chain's meters, about 30 times a second.
    Meters {
        chains: Vec<(String, MeterValues)>,
    },
    /// The service is stopping on purpose; clients must not start it again.
    Bye,
    /// To windows: come to the front.
    Raise,
    /// To windows: close; the service and its chains stay.
    Close,
    /// To the client that sent `Raise` or `Close`: whether a window was there for it.
    Windows {
        found: bool,
    },
}

/// Client to service.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Down {
    Command {
        command: Command,
    },
    /// Stop the service: its chains go down with it.
    Shutdown,
    /// This client is a window, the one `Raise` brings forward.
    Window,
    /// Bring an open window forward, so a second launch does not open another.
    Raise,
    /// Close the open windows: the tray icon's click toggles the window.
    Close,
}

/// `$XDG_RUNTIME_DIR/canalis/canalis.sock`, or the app's shared runtime directory in a
/// Flatpak, where every instance of the app sees the same one.
pub fn socket_path() -> Option<PathBuf> {
    let run = dirs::runtime_dir()?;
    Some(match std::env::var_os("FLATPAK_ID") {
        Some(id) => run.join("app").join(id).join("canalis.sock"),
        None => run.join("canalis").join("canalis.sock"),
    })
}

/// What the server serves: the session's handle, or a stand-in in tests.
pub trait Plane {
    fn latest(&self) -> Option<Arc<Graph>>;
    fn send(&self, command: Command);
}

impl Plane for Handle {
    fn latest(&self) -> Option<Arc<Graph>> {
        Handle::latest(self)
    }

    fn send(&self, command: Command) {
        Handle::send(self, command);
    }
}

/// Binds `path`; an error when a service already answers there, a stale file is replaced.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("a Canalis service already answers on {}", path.display()),
        ));
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    info!("serving on {}", path.display());
    Ok(listener)
}

/// Serves `plane` until `stop` is set or a client asks for a shutdown, then removes the
/// socket file.
pub fn serve(plane: &impl Plane, listener: UnixListener, path: &Path, stop: &AtomicBool) {
    let (down_tx, down_rx) = mpsc::channel::<(u64, Down)>();
    let mut clients: Vec<Client> = Vec::new();
    let mut next_id = 0;
    let mut graph: Option<Arc<Graph>> = None;
    let mut graph_line = String::new();
    let mut meters_at = Instant::now();
    let hello = line(&Up::Hello {
        protocol: PROTOCOL,
        build: BUILD.to_owned(),
    });
    while !stop.load(Ordering::Relaxed) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    next_id += 1;
                    if let Some(stream) = attach(stream, next_id, &down_tx, &hello, &graph_line) {
                        clients.push(Client {
                            id: next_id,
                            stream,
                            window: false,
                        });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    warn!("accept: {e}");
                    break;
                }
            }
        }
        if let Some(latest) = plane.latest() {
            graph_line = graph_text(&latest);
            broadcast(&mut clients, &graph_line);
            graph = Some(latest);
        }
        if meters_at.elapsed() >= METERS_EVERY {
            meters_at = Instant::now();
            if let Some(graph) = graph.as_ref().filter(|g| !g.chains.is_empty())
                && !clients.is_empty()
            {
                let chains = graph
                    .chains
                    .iter()
                    .map(|c| (c.name.clone(), finite(c.meters.values())))
                    .collect();
                broadcast(&mut clients, &line(&Up::Meters { chains }));
            }
        }
        for (from, down) in down_rx.try_iter() {
            match down {
                Down::Command {
                    command: Command::Quit,
                } => warn!("a client sent Quit; ignored (Shutdown stops the service)"),
                Down::Command { command } => plane.send(command),
                Down::Shutdown => {
                    info!("a client asked the service to stop");
                    stop.store(true, Ordering::Relaxed);
                }
                Down::Window => {
                    if let Some(client) = clients.iter_mut().find(|c| c.id == from) {
                        client.window = true;
                    }
                }
                Down::Raise | Down::Close => {
                    let order = line(&match down {
                        Down::Raise => Up::Raise,
                        _ => Up::Close,
                    });
                    let found = clients
                        .iter()
                        .filter(|c| c.window)
                        .filter(|c| (&c.stream).write_all(order.as_bytes()).is_ok())
                        .count()
                        > 0;
                    if let Some(asker) = clients.iter_mut().find(|c| c.id == from) {
                        let _ = asker
                            .stream
                            .write_all(line(&Up::Windows { found }).as_bytes());
                    }
                }
            }
        }
        thread::sleep(TICK);
    }
    broadcast(&mut clients, &line(&Up::Bye));
    for client in &clients {
        let _ = client.stream.shutdown(std::net::Shutdown::Both);
    }
    let _ = std::fs::remove_file(path);
}

struct Client {
    id: u64,
    stream: UnixStream,
    window: bool,
}

/// Greets a new client, sends it the current graph and starts its reader thread.
fn attach(
    stream: UnixStream,
    id: u64,
    down: &mpsc::Sender<(u64, Down)>,
    hello: &str,
    graph_line: &str,
) -> Option<UnixStream> {
    stream.set_nonblocking(false).ok()?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok()?;
    let reader = stream.try_clone().ok()?;
    let down = down.clone();
    thread::Builder::new()
        .name("remote-client".into())
        .spawn(move || {
            for text in BufReader::new(reader).lines() {
                let Ok(text) = text else { break };
                match serde_json::from_str::<Down>(&text) {
                    Ok(message) => {
                        if down.send((id, message)).is_err() {
                            break;
                        }
                    }
                    Err(e) => warn!("bad line from a client ({e}): {text}"),
                }
            }
        })
        .ok()?;
    let mut stream = stream;
    stream.write_all(hello.as_bytes()).ok()?;
    if !graph_line.is_empty() {
        stream.write_all(graph_line.as_bytes()).ok()?;
    }
    info!("a client attached");
    Some(stream)
}

/// JSON writes NaN and infinity as `null`, which no `f32` reads back.
fn finite(mut values: MeterValues) -> MeterValues {
    let clean = |v: &mut f32, silent: f32| {
        if !v.is_finite() {
            *v = silent;
        }
    };
    values
        .input
        .iter_mut()
        .chain(&mut values.output)
        .for_each(|v| clean(v, 0.0));
    values
        .loudness
        .iter_mut()
        .for_each(|v| clean(v, crate::model::LOUDNESS_SILENCE));
    clean(&mut values.balance, 0.0);
    values
}

fn broadcast(clients: &mut Vec<Client>, text: &str) {
    clients.retain_mut(|c| {
        let ok = c.stream.write_all(text.as_bytes()).is_ok();
        if !ok {
            info!("a client left");
        }
        ok
    });
}

fn line<T: Serialize>(value: &T) -> String {
    let mut text = serde_json::to_string(value).expect("protocol types serialize");
    text.push('\n');
    text
}

fn graph_text(graph: &Graph) -> String {
    line(&serde_json::json!({ "graph": { "graph": graph } }))
}

/// One blocking connection to the service, past its `hello`.
pub struct Connection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    pub protocol: u32,
    pub build: String,
}

impl Connection {
    pub fn open(path: &Path) -> io::Result<Connection> {
        let writer = UnixStream::connect(path)?;
        let mut reader = BufReader::new(writer.try_clone()?);
        let mut text = String::new();
        reader.read_line(&mut text)?;
        match serde_json::from_str::<Up>(&text) {
            Ok(Up::Hello { protocol, build }) => Ok(Connection {
                reader,
                writer,
                protocol,
                build,
            }),
            _ => Err(io::Error::other(format!(
                "no hello from the service: {text}"
            ))),
        }
    }

    /// Whether the service is this very build; otherwise only `Shutdown` is safe to send.
    pub fn compatible(&self) -> bool {
        self.protocol == PROTOCOL && self.build == BUILD
    }

    /// The next line from the service; an error when it went away.
    pub fn receive(&mut self) -> io::Result<Up> {
        let mut text = String::new();
        if self.reader.read_line(&mut text)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        serde_json::from_str(&text).map_err(io::Error::other)
    }

    pub fn send(&mut self, down: &Down) -> io::Result<()> {
        self.writer.write_all(line(down).as_bytes())
    }

    /// The first graph the service sends, which it does right after `hello`.
    pub fn graph(&mut self) -> io::Result<Graph> {
        loop {
            if let Up::Graph { graph } = self.receive()? {
                return Ok(*graph);
            }
        }
    }
}

/// Asks the service on `path` to bring its open window forward; `false` when there is no
/// service of this build or no window, so the caller should open one.
pub fn raise_window(path: &Path) -> bool {
    to_windows(path, &Down::Raise)
}

/// Asks the service on `path` to close its open windows; `false` when there were none.
pub fn close_windows(path: &Path) -> bool {
    to_windows(path, &Down::Close)
}

fn to_windows(path: &Path, order: &Down) -> bool {
    let Ok(mut connection) = Connection::open(path) else {
        return false;
    };
    if !connection.compatible()
        || connection
            .writer
            .set_read_timeout(Some(RAISE_WAIT))
            .is_err()
        || connection.send(order).is_err()
    {
        return false;
    }
    loop {
        match connection.receive() {
            Ok(Up::Windows { found }) => return found,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Other => {}
            Err(_) => return false,
        }
    }
}

/// Asks the service on `path` to stop and waits until its socket is gone; `false` when
/// it is still there after `timeout`.
pub fn shut_down(path: &Path, timeout: Duration) -> bool {
    if let Ok(mut connection) = Connection::open(path) {
        let _ = connection.send(&Down::Shutdown);
    }
    gone(path, timeout)
}

/// Waits until nothing answers on `path`; `false` after `timeout`.
fn gone(path: &Path, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if UnixStream::connect(path).is_err() {
            return true;
        }
        thread::sleep(RETRY);
    }
    false
}

/// The window's end of the service: the same `latest`/`send` as a local `Handle`, over
/// the socket, reconnecting and asking `start` for a service whenever there is none.
pub struct Remote {
    graphs: mpsc::Receiver<Arc<Graph>>,
    writer: Arc<Mutex<Option<UnixStream>>>,
    attached: Arc<AtomicBool>,
    bye: Arc<AtomicBool>,
    raise: Arc<AtomicBool>,
    close: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Remote {
    /// `wake` runs after every graph and meters line; `start` is asked for a service when
    /// none answers, and again when the one that answers is another build. A `window`
    /// registers as the one a second launch raises.
    pub fn spawn(
        path: PathBuf,
        window: bool,
        wake: impl Fn() + Send + 'static,
        start: impl Fn() + Send + 'static,
    ) -> io::Result<Remote> {
        let (graph_tx, graphs) = mpsc::channel();
        let writer = Arc::new(Mutex::new(None));
        let attached = Arc::new(AtomicBool::new(false));
        let bye = Arc::new(AtomicBool::new(false));
        let raise = Arc::new(AtomicBool::new(false));
        let close = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let link = Link {
            path,
            window,
            raise: Arc::clone(&raise),
            close: Arc::clone(&close),
            graphs: graph_tx,
            writer: Arc::clone(&writer),
            attached: Arc::clone(&attached),
            bye: Arc::clone(&bye),
            stop: Arc::clone(&stop),
            meters: HashMap::new(),
        };
        let thread = thread::Builder::new()
            .name("remote".into())
            .spawn(move || link.run(&wake, &start))?;
        Ok(Remote {
            graphs,
            writer,
            attached,
            bye,
            raise,
            close,
            stop,
            thread: Some(thread),
        })
    }

    /// The newest graph received since the last call, if any.
    pub fn latest(&self) -> Option<Arc<Graph>> {
        self.graphs.try_iter().last()
    }

    pub fn send(&self, command: Command) {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let sent = writer.as_mut().is_some_and(|w| {
            w.write_all(line(&Down::Command { command }).as_bytes())
                .is_ok()
        });
        if !sent {
            warn!("no service attached; command dropped");
        }
    }

    /// Whether a service of this build is attached right now.
    pub fn attached(&self) -> bool {
        self.attached.load(Ordering::Relaxed)
    }

    /// The service said `bye`: it was stopped on purpose and is not started again.
    pub fn stopped(&self) -> bool {
        self.bye.load(Ordering::Relaxed)
    }

    /// A second launch asked for this window since the last call.
    pub fn take_raise(&self) -> bool {
        self.raise.swap(false, Ordering::Relaxed)
    }

    /// The tray icon asked this window to close.
    pub fn closing(&self) -> bool {
        self.close.load(Ordering::Relaxed)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The reader thread's state.
struct Link {
    path: PathBuf,
    window: bool,
    raise: Arc<AtomicBool>,
    close: Arc<AtomicBool>,
    graphs: mpsc::Sender<Arc<Graph>>,
    writer: Arc<Mutex<Option<UnixStream>>>,
    attached: Arc<AtomicBool>,
    bye: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Meters by chain name, shared with every graph handed out.
    meters: HashMap<String, Arc<ChainMeters>>,
}

impl Link {
    fn run(mut self, wake: &dyn Fn(), start: &dyn Fn()) {
        let mut started: Option<Instant> = None;
        let mut restarted_for = None;
        while !self.stop.load(Ordering::Relaxed) && !self.bye.load(Ordering::Relaxed) {
            let connection = match Connection::open(&self.path) {
                Ok(c) => c,
                Err(_) => {
                    if started.is_none_or(|t| t.elapsed() > START_EVERY) {
                        start();
                        started = Some(Instant::now());
                    }
                    thread::sleep(RETRY);
                    continue;
                }
            };
            if !connection.compatible() {
                let other = (connection.protocol, connection.build.clone());
                if restarted_for.as_ref() == Some(&other) {
                    warn!("the service is build {} and stays so", other.1);
                    thread::sleep(START_EVERY);
                    continue;
                }
                info!("the service is build {}; restarting it as {BUILD}", other.1);
                restarted_for = Some(other);
                let mut connection = connection;
                let _ = connection.send(&Down::Shutdown);
                drop(connection);
                gone(&self.path, START_EVERY);
                started = None;
                continue;
            }
            self.attach(connection, wake);
            self.attached.store(false, Ordering::Relaxed);
            *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = self.graphs.send(Arc::new(Graph::default()));
            wake();
        }
    }

    /// Reads from an attached service until it goes away or the client stops.
    fn attach(&mut self, mut connection: Connection, wake: &dyn Fn()) {
        if self.window && connection.send(&Down::Window).is_err() {
            return;
        }
        let Ok(writer) = connection.writer.try_clone() else {
            return;
        };
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(writer);
        self.attached.store(true, Ordering::Relaxed);
        info!("attached to the Canalis service");
        while !self.stop.load(Ordering::Relaxed) {
            match connection.receive() {
                Ok(Up::Graph { mut graph }) => {
                    for chain in &mut graph.chains {
                        chain.meters =
                            Arc::clone(self.meters.entry(chain.name.clone()).or_default());
                    }
                    self.meters
                        .retain(|name, _| graph.chains.iter().any(|c| c.name == *name));
                    if self.graphs.send(Arc::from(graph)).is_err() {
                        return;
                    }
                    wake();
                }
                Ok(Up::Meters { chains }) => {
                    for (name, values) in &chains {
                        if let Some(meters) = self.meters.get(name) {
                            meters.set_values(values);
                        }
                    }
                    wake();
                }
                Ok(Up::Raise) => {
                    self.raise.store(true, Ordering::Relaxed);
                    wake();
                }
                Ok(Up::Close) => {
                    self.close.store(true, Ordering::Relaxed);
                    wake();
                }
                Ok(Up::Hello { .. } | Up::Windows { .. }) => {}
                // JSON has no NaN or infinity; one odd value must not cost the connection.
                Err(e) if e.kind() == io::ErrorKind::Other => warn!("skipped a line: {e}"),
                Ok(Up::Bye) => {
                    info!("the Canalis service was stopped");
                    self.bye.store(true, Ordering::Relaxed);
                    wake();
                    return;
                }
                Err(e) => {
                    if !self.stop.load(Ordering::Relaxed) {
                        warn!("lost the Canalis service: {e}");
                    }
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChainKind, ChainState, ChainStatus};

    struct Fake {
        graphs: Mutex<mpsc::Receiver<Arc<Graph>>>,
        commands: Mutex<mpsc::Sender<Command>>,
    }

    impl Plane for Fake {
        fn latest(&self) -> Option<Arc<Graph>> {
            self.graphs.lock().unwrap().try_iter().last()
        }

        fn send(&self, command: Command) {
            self.commands.lock().unwrap().send(command).unwrap();
        }
    }

    fn chain(name: &str) -> ChainState {
        ChainState {
            name: name.into(),
            label: name.into(),
            kind: ChainKind::Output,
            slots: vec![toml::from_str("bypass = true\n[spec]\nkind = \"comp\"").unwrap()],
            inputs: vec![],
            outputs: vec!["alsa_output.x".into()],
            status: ChainStatus::Running,
            pid: Some(7),
            latency_samples: 0,
            slot_latencies: vec![],
            restarts: 0,
            error: None,
            meters: Arc::default(),
            slot_infos: [(
                0,
                crate::model::SlotInfo {
                    name: "Claritas Comp".into(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            gui_open: [(1003, true)].into_iter().collect(),
            crashed_slot: None,
            restarting: false,
            overloads: Default::default(),
            recording: None,
            load: None,
            auto_level: false,
            compare: None,
            mute: false,
            color: None,
            icon: None,
            collapsed: false,
            loudness_target: None,
            key: None,
            gains: Default::default(),
        }
    }

    fn socket(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("canalis-remote-{}-{name}", std::process::id()));
        dir.join("canalis.sock")
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let started = Instant::now();
        while !ok() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "timed out: {what}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_client_gets_graphs_and_meters_and_its_commands_arrive() {
        let path = socket("round-trip");
        let (graph_tx, graph_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let mut graph = Graph {
            connected: true,
            ..Graph::default()
        };
        graph.chains.push(chain("canalis.eq"));
        let live = Arc::clone(&graph.chains[0].meters);
        let graph_sent = Arc::new(graph);
        graph_tx.send(Arc::clone(&graph_sent)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let listener = bind(&path).unwrap();
        assert!(bind(&path).is_err(), "a second service must not bind");
        let server = {
            let (path, stop) = (path.clone(), Arc::clone(&stop));
            thread::spawn(move || {
                let fake = Fake {
                    graphs: Mutex::new(graph_rx),
                    commands: Mutex::new(command_tx),
                };
                serve(&fake, listener, &path, &stop);
            })
        };
        let remote = Remote::spawn(path.clone(), false, || {}, || {}).unwrap();
        let mut seen = None;
        wait_for("first graph", || {
            seen = remote.latest().or(seen.take());
            seen.as_ref().is_some_and(|g| g.connected)
        });
        let seen = seen.unwrap();
        let mut expected = (*graph_sent).clone();
        expected.chains[0].meters = Arc::clone(&seen.chains[0].meters);
        assert_eq!(*seen, expected, "the graph arrives whole");
        live.set([0.5, 0.25], [0.125, 0.0625]);
        wait_for("meters", || seen.chains[0].meters.input() == [0.5, 0.25]);
        let scene = crate::config::Scene {
            name: "Calls".into(),
            ..Default::default()
        };
        remote.send(Command::ApplyScene {
            scene: scene.clone(),
        });
        remote.send(Command::Quit);
        remote.send(Command::ChainMessage {
            name: "canalis.eq".into(),
            message: toml::from_str("type = \"bypass\"\nslot = 0\nbypass = true").unwrap(),
        });
        let first = command_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first, Command::ApplyScene { scene });
        let second = command_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(second, Command::ChainMessage { .. }),
            "Quit must never reach the session: {second:?}"
        );
        assert!(!raise_window(&path), "no window to raise yet");
        let window = Remote::spawn(path.clone(), true, || {}, || {}).unwrap();
        wait_for("a second launch raises the window", || raise_window(&path));
        wait_for("the window hears it", || window.take_raise());
        assert!(!remote.take_raise(), "only windows are raised");
        assert!(close_windows(&path));
        wait_for("the window closes", || window.closing());
        assert!(!remote.closing(), "only windows close");
        assert!(shut_down(&path, Duration::from_secs(5)));
        server.join().unwrap();
        assert!(!path.exists(), "socket removed");
        wait_for("detached", || !remote.attached());
        assert!(remote.stopped(), "a deliberate stop says bye");
    }

    #[test]
    fn a_client_of_another_build_restarts_the_service() {
        let path = socket("other-build");
        let listener = bind(&path).unwrap();
        let old = thread::spawn(move || {
            let (stream, _) = loop {
                if let Ok(s) = listener.accept() {
                    break s;
                }
                thread::sleep(Duration::from_millis(5));
            };
            stream.set_nonblocking(false).unwrap();
            let mut writer = stream.try_clone().unwrap();
            writer
                .write_all(b"{\"hello\":{\"protocol\":0,\"build\":\"0.0.1\"}}\n")
                .unwrap();
            let mut text = String::new();
            BufReader::new(stream).read_line(&mut text).unwrap();
            text
        });
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&starts);
        let _remote = Remote::spawn(
            path.clone(),
            true,
            || {},
            move || {
                counted.fetch_add(1, Ordering::Relaxed);
            },
        )
        .unwrap();
        let asked = old.join().unwrap();
        assert_eq!(asked.trim(), "\"shutdown\"");
        let _ = std::fs::remove_file(&path);
        wait_for("a new service asked for", || {
            starts.load(Ordering::Relaxed) > 0
        });
    }
}
