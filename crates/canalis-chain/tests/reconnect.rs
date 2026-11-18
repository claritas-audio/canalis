//! The playback end reconnects under running audio without the callbacks touching it.
//! Ignored: needs the PipeWire session, pactl, pw-cat and pw-link.
//! `cargo test -p canalis-chain --test reconnect -- --ignored`

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Outside `canalis.*`, so a running Canalis service does not remove it as an orphan.
const NAME: &str = "reconnect-test";
const SINK: &str = "reconnect_test_sink";

struct Cleanup {
    module: String,
    children: Vec<Child>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("pactl")
            .args(["unload-module", &self.module])
            .status();
    }
}

fn run(program: &str, args: &[&str]) -> Child {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{program} runs: {e}"))
}

#[test]
#[ignore]
fn sends_toggled_under_audio_keep_the_chain_alive() {
    let out = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK}"),
            "channel_map=front-left,front-right",
        ])
        .output()
        .expect("pactl runs");
    let mut cleanup = Cleanup {
        module: String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        children: Vec::new(),
    };
    assert!(!cleanup.module.is_empty(), "pactl load-module failed");

    let mut chain = Command::new(env!("CARGO_BIN_EXE_canalis-chain"))
        .args(["--node-name", NAME, "--label", NAME, "--kind", "output"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("the chain binary runs");
    let mut stdin = chain.stdin.take().expect("piped stdin");
    cleanup.children.push(chain);
    sleep(Duration::from_secs(1));
    cleanup.children.push(run(
        "pw-cat",
        &[
            "-p",
            "--target",
            NAME,
            "--raw",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
            "/dev/zero",
        ],
    ));
    sleep(Duration::from_secs(1));
    for channel in ["FL", "FR"] {
        let status = Command::new("pw-link")
            .args([
                &format!("{NAME}.out:output_{channel}"),
                &format!("{SINK}:playback_{channel}"),
            ])
            .status()
            .expect("pw-link runs");
        assert!(status.success(), "linking {channel}");
    }

    let start = Instant::now();
    let mut reconnects = 0;
    while start.elapsed() < Duration::from_secs(15) {
        for line in [
            r#"{"type":"sends","count":2,"gains_db":[0,0]}"#,
            r#"{"type":"sends","count":1,"gains_db":[0]}"#,
        ] {
            writeln!(stdin, "{line}").expect("the chain reads its stdin");
            sleep(Duration::from_millis(25));
            reconnects += 1;
        }
        if let Some(status) = cleanup.children[0].try_wait().expect("wait") {
            panic!("the chain died after {reconnects} reconnects: {status:?}");
        }
    }
}
