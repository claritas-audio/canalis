//! `canalis-chain`: one effect chain as a PipeWire stream pair, in its own process.
//! Usage: `canalis-chain --node-name canalis.desktop-eq --label "Desktop EQ" --kind output`;
//! commands arrive as JSON lines on stdin, reports leave as JSON lines on stdout.
//! `canalis-chain --scan <bundle.clap>` instead prints the bundle's plugins, one JSON
//! object per line, and exits: loading a bundle happens only in a process like this one.

mod control;
mod streams;

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[cfg(debug_assertions)]
use assert_no_alloc::AllocDisabler;
use log::{error, info, warn};
use pipewire::context::ContextRc;
use pipewire::core::PW_ID_CORE;
use pipewire::loop_::Signal;
use pipewire::main_loop::MainLoopRc;
use pipewire::spa::support::system::IoFlags;

use canalis_chain::ipc::{FromChain, ToChain};
use canalis_chain::{ChainKind, Meters};
use control::Control;

/// Debug builds abort on an allocation inside the process callback.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const MESSAGE_RING: usize = 256;
const GARBAGE_RING: usize = 64;
/// About one report per display frame; the GUI smooths between them.
const REPORT_INTERVAL: Duration = Duration::from_millis(16);

enum Mode {
    Chain(Args),
    Scan(String),
    ScanLv2,
    /// Debug builds only: die in slot N to exercise the crash report.
    CrashTest(usize),
}

struct Args {
    node_name: String,
    label: String,
    kind: ChainKind,
}

fn parse_args() -> Result<Mode, String> {
    let mut node_name = None;
    let mut label = None;
    let mut kind = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scan-lv2" {
            return Ok(Mode::ScanLv2);
        }
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--scan" => return Ok(Mode::Scan(value)),
            "--crash-test" if cfg!(debug_assertions) => {
                let slot = value
                    .parse()
                    .map_err(|_| "--crash-test needs a slot index")?;
                return Ok(Mode::CrashTest(slot));
            }
            "--node-name" => node_name = Some(value),
            "--label" => label = Some(value),
            "--kind" => {
                kind = Some(ChainKind::parse(&value).ok_or_else(|| {
                    format!("unknown kind {value}; one of output, mic, cable, insert")
                })?);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    let node_name = node_name.ok_or("--node-name is required")?;
    Ok(Mode::Chain(Args {
        label: label.unwrap_or_else(|| node_name.clone()),
        node_name,
        kind: kind.ok_or("--kind is required")?,
    }))
}

fn scan(path: &str) -> ExitCode {
    match canalis_chain::clap::scan(path) {
        Ok(plugins) => {
            let mut stdout = std::io::stdout().lock();
            for plugin in plugins {
                if let Ok(text) = serde_json::to_string(&plugin) {
                    let _ = writeln!(stdout, "{text}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("{path}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), pipewire::Error> {
    canalis_chain::crash::install();
    pipewire::init();
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(Some(pipewire::properties::properties! {
        *pipewire::keys::APP_NAME => "Canalis chain",
    }))?;

    let on_error = main_loop.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, res, message| {
            error!("PipeWire error on {id} ({res}): {message}");
            if id == PW_ID_CORE {
                on_error.quit();
            }
        })
        .register();

    let (messages_in, messages_out) = rtrb::RingBuffer::new(MESSAGE_RING);
    let (garbage_in, garbage_out) = rtrb::RingBuffer::new(GARBAGE_RING);
    let meters = Arc::new(Meters::default());
    let control = Rc::new(RefCell::new(Control::new(
        &args.label,
        messages_in,
        garbage_out,
    )));

    let on_format = Rc::clone(&control);
    let pair = Rc::new(streams::connect(
        &core,
        &args.node_name,
        &args.label,
        args.kind,
        streams::Wiring {
            meters: Arc::clone(&meters),
            messages: messages_out,
            garbage: garbage_in,
            on_format: Box::new(move |rate, channels| {
                canalis_chain::overload::RATE.store(rate, std::sync::atomic::Ordering::Relaxed);
                let mut control = on_format.borrow_mut();
                control.set_format(rate, channels);
                report(&FromChain::Format { rate, channels });
                report_all(&mut control);
            }),
        },
    )?);
    info!(
        "{} ({}) is up as {:?}",
        args.node_name, args.label, args.kind
    );
    report(&FromChain::Up {
        node_name: args.node_name.clone(),
    });

    let (command_tx, command_rx) = pipewire::channel::channel::<ToChain>();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || read_commands(command_tx))
        .map_err(|_| pipewire::Error::CreationFailed)?;
    let on_command = Rc::clone(&control);
    let pair_for_commands = Rc::clone(&pair);
    let quit_on_eof = main_loop.clone();
    let _commands = command_rx.attach(main_loop.loop_(), move |command| {
        if command == ToChain::Quit {
            quit_on_eof.quit();
            return;
        }
        let mut control = on_command.borrow_mut();
        if control.apply(command) {
            publish_latency(&mut control, &pair_for_commands);
        }
        if let Some(sends) = control.take_sends_change()
            && let Err(e) = pair_for_commands.set_sends(sends)
        {
            warn!("cannot reconnect the playback end with {sends} pairs: {e}");
        }
        if let Some(enabled) = control.take_key_change()
            && let Err(e) = pair_for_commands.set_key(enabled)
        {
            warn!("cannot change the key input: {e}");
        }
        if let Some(enabled) = control.take_insert_change()
            && let Err(e) = pair_for_commands.set_insert(enabled)
        {
            warn!("cannot change the hardware insert: {e}");
        }
        report_all(&mut control);
    });

    let x11_fd = control.borrow().x11_fd();
    let on_window = Rc::clone(&control);
    let _windows = x11_fd.map(|fd| {
        main_loop.loop_().add_io(fd, IoFlags::IN, move |_| {
            let mut control = on_window.borrow_mut();
            control.pump_windows();
            report_all(&mut control);
        })
    });

    let on_tick = Rc::clone(&control);
    let pair_for_tick = Rc::clone(&pair);
    let timer = main_loop.loop_().add_timer(move |_| {
        let mut control = on_tick.borrow_mut();
        if control.tick() {
            publish_latency(&mut control, &pair_for_tick);
        }
        report_all(&mut control);
        for (slot, slot_us, cycle_us) in canalis_chain::overload::take_reports() {
            log::warn!("slot {slot} bypassed: it took {slot_us} µs of a {cycle_us} µs cycle");
            report(&FromChain::Overload {
                slot,
                slot_us,
                cycle_us,
            });
        }
        let failed = streams::TRIGGER_FAILURES.swap(0, std::sync::atomic::Ordering::Relaxed);
        if failed > 0 {
            log::warn!("{failed} capture cycle(s) could not trigger the playback end");
        }
        let (input, output) = meters.take();
        report(&FromChain::Meters {
            input,
            output,
            loudness: meters.loudness(),
            balance: meters.balance(),
            auto_gain_db: meters.auto_gain_db(),
            spectrum: control.spectrum_bands(),
        });
    });
    timer
        .update_timer(Some(REPORT_INTERVAL), Some(REPORT_INTERVAL))
        .into_result()?;

    let on_int = main_loop.clone();
    let _sigint = main_loop
        .loop_()
        .add_signal_local(Signal::INT, move || on_int.quit());
    let on_term = main_loop.clone();
    let _sigterm = main_loop
        .loop_()
        .add_signal_local(Signal::TERM, move || on_term.quit());

    main_loop.run();
    control.borrow_mut().finish_recording();
    info!("{} stopped", args.node_name);
    Ok(())
}

fn publish_latency(control: &mut Control, pair: &streams::Pair) {
    let slots = control.slot_latencies();
    let samples = slots.iter().sum();
    match pair.set_process_latency(samples) {
        Ok(()) => report(&FromChain::Latency { samples, slots }),
        Err(e) => warn!("latency update failed: {e}"),
    }
}

fn report_all(control: &mut Control) {
    for message in control.take_reports() {
        report(&message);
    }
}

/// Forwards stdin lines to the loop thread; EOF means the control plane is gone.
fn read_commands(tx: pipewire::channel::Sender<ToChain>) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ToChain>(&line) {
            Ok(command) => {
                if tx.send(command).is_err() {
                    return;
                }
            }
            Err(e) => warn!("ignoring unreadable command: {e}"),
        }
    }
    let _ = tx.send(ToChain::Quit);
}

fn report(message: &FromChain) {
    let mut stdout = std::io::stdout().lock();
    if let Ok(text) = serde_json::to_string(message) {
        let _ = writeln!(stdout, "{text}");
        let _ = stdout.flush();
    }
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();
    let args = match parse_args() {
        Ok(Mode::Chain(args)) => args,
        Ok(Mode::Scan(path)) => return scan(&path),
        Ok(Mode::ScanLv2) => {
            let plugins = match canalis_chain::lv2::scan() {
                Ok(plugins) => plugins,
                Err(e) => {
                    eprintln!("{e}");
                    // canalis-pw reads exit code 3 as "lilv is not installed".
                    return ExitCode::from(3);
                }
            };
            let mut stdout = std::io::stdout().lock();
            for plugin in plugins {
                if let Ok(text) = serde_json::to_string(&plugin) {
                    let _ = writeln!(stdout, "{text}");
                }
            }
            return ExitCode::SUCCESS;
        }
        Ok(Mode::CrashTest(slot)) => {
            canalis_chain::crash::install();
            canalis_chain::crash::ACTIVE_SLOT.store(slot + 1, std::sync::atomic::Ordering::Relaxed);
            // SAFETY: raising a signal at ourselves; the handler reports and the process ends.
            unsafe {
                libc::raise(libc::SIGSEGV);
            }
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("canalis-chain: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}
