//! A chain that dies in a slot says which one on its way out.

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

#[test]
fn a_fatal_signal_reports_the_active_slot_then_kills_the_process() {
    let output = Command::new(env!("CARGO_BIN_EXE_canalis-chain"))
        .args(["--crash-test", "2"])
        .output()
        .expect("the chain binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(r#"{"type":"crash","slot":2}"#),
        "stdout was {stdout:?}"
    );
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGSEGV),
        "{:?}",
        output.status
    );
}
