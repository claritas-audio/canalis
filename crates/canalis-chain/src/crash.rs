//! Which slot the callback was in when the process died. A fatal signal handler writes
//! one JSON line to stdout with nothing but a stack buffer and `write(2)`, then lets the
//! signal kill the process; the control plane shows the slot next to the restart.

use std::sync::atomic::{AtomicUsize, Ordering};

/// `index + 1` of the slot being processed, 0 between slots and outside the callback.
pub static ACTIVE_SLOT: AtomicUsize = AtomicUsize::new(0);

const FATAL: [libc::c_int; 5] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
    libc::SIGABRT,
];

/// Installs the handler for the signals a crashing plugin raises.
pub fn install() {
    for signal in FATAL {
        // SAFETY: a zeroed sigaction is valid; the handler is async-signal-safe (atomics, a
        // stack buffer and write) and SA_RESETHAND makes the re-raised signal fatal.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fatal as *const () as usize;
            action.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER;
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

extern "C" fn on_fatal(signal: libc::c_int) {
    let active = ACTIVE_SLOT.load(Ordering::Relaxed);
    let mut line = [0u8; 40];
    let n = format_line(&mut line, active.checked_sub(1));
    // SAFETY: the buffer outlives the call and `n` is within it; write(2) is async-signal-safe.
    unsafe {
        libc::write(libc::STDOUT_FILENO, line.as_ptr().cast(), n);
    }
    // SAFETY: the handler was installed with SA_RESETHAND, so this ends the process.
    unsafe {
        libc::raise(signal);
    }
}

/// `{"type":"crash","slot":N}\n`, or `{"type":"crash"}\n` outside any slot, into `out`;
/// the length written. No allocation.
fn format_line(out: &mut [u8; 40], slot: Option<usize>) -> usize {
    const HEAD: &[u8] = br#"{"type":"crash""#;
    out[..HEAD.len()].copy_from_slice(HEAD);
    let mut n = HEAD.len();
    let Some(slot) = slot else {
        out[n] = b'}';
        out[n + 1] = b'\n';
        return n + 2;
    };
    const SLOT: &[u8] = br#","slot":"#;
    out[n..n + SLOT.len()].copy_from_slice(SLOT);
    n += SLOT.len();
    let mut digits = [0u8; 20];
    let mut count = 0;
    let mut rest = slot;
    loop {
        digits[count] = b'0' + (rest % 10) as u8;
        count += 1;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    for i in (0..count).rev() {
        out[n] = digits[i];
        n += 1;
    }
    out[n] = b'}';
    out[n + 1] = b'\n';
    n + 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crash_line_is_what_the_control_plane_parses() {
        let mut line = [0u8; 40];
        let n = format_line(&mut line, Some(2));
        assert_eq!(&line[..n], b"{\"type\":\"crash\",\"slot\":2}\n");
        let n = format_line(&mut line, Some(10));
        assert_eq!(&line[..n], b"{\"type\":\"crash\",\"slot\":10}\n");
        let n = format_line(&mut line, None);
        assert_eq!(&line[..n], b"{\"type\":\"crash\"}\n");
        for slot in [Some(2), None] {
            let expected = serde_json::to_string(&crate::ipc::FromChain::Crash { slot }).unwrap();
            let n = format_line(&mut line, slot);
            assert_eq!(expected.as_bytes(), &line[..n - 1]);
        }
    }
}
