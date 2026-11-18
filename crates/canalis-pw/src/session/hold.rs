//! Push-to-talk on compositors that repeat a held key but have no release bind (niri):
//! each repeat of `Command::Hold` keeps the target open, and it mutes again once the
//! repeats have stopped for `HOLD_TAIL`.

use std::time::{Duration, Instant};

use super::{Command, Session};
use crate::model::NodeId;

/// Longer than niri's default 600 ms repeat delay, so the gap before the first repeat
/// does not close the mic.
pub const HOLD_TAIL: Duration = Duration::from_millis(700);

/// What a held key opens: a device's own mute, or a chain's.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HoldTarget {
    Node { node: NodeId },
    Chain { name: String },
}

/// Targets held open and when each closes.
#[derive(Debug, Default)]
pub(super) struct Holds(Vec<(HoldTarget, Instant)>);

impl Holds {
    /// Pushes the target's close back to `now + HOLD_TAIL`; `true` when it was not held.
    fn press(&mut self, target: HoldTarget, now: Instant) -> bool {
        let close = now + HOLD_TAIL;
        if let Some(held) = self.0.iter_mut().find(|(t, _)| *t == target) {
            held.1 = close;
            return false;
        }
        self.0.push((target, close));
        true
    }

    /// The targets due to close by `now`, and how long until the next one still held.
    fn expire(&mut self, now: Instant) -> (Vec<HoldTarget>, Option<Duration>) {
        let (due, kept) = std::mem::take(&mut self.0)
            .into_iter()
            .partition::<Vec<_>, _>(|(_, close)| *close <= now);
        self.0 = kept;
        let next = self.0.iter().map(|(_, close)| *close - now).min();
        (due.into_iter().map(|(t, _)| t).collect(), next)
    }
}

impl Session {
    pub(super) fn hold(&mut self, target: HoldTarget) {
        if self.holds.press(target.clone(), Instant::now()) {
            self.execute(mute_command(target, false));
            self.wake_in(HOLD_TAIL);
        }
    }

    /// Runs when a `wake_in` sleeper fires: mutes what was let go, re-arms for the rest.
    pub(super) fn close_holds(&mut self) {
        let (due, next) = self.holds.expire(Instant::now());
        for target in due {
            self.execute(mute_command(target, true));
        }
        if let Some(next) = next {
            self.wake_in(next);
        }
    }

    // A sleeper thread instead of a loop timer: a `TimerSource` borrows the loop and
    // cannot live in the session.
    fn wake_in(&self, delay: Duration) {
        let wake = self.hold_wake.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = wake.send(());
        });
    }
}

fn mute_command(target: HoldTarget, mute: bool) -> Command {
    match target {
        HoldTarget::Node { node } => Command::SetMute { node, mute },
        HoldTarget::Chain { name } => Command::SetChainMute { name, mute },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(name: &str) -> HoldTarget {
        HoldTarget::Chain { name: name.into() }
    }

    #[test]
    fn repeats_keep_a_hold_open_until_they_stop() {
        let mut holds = Holds::default();
        let start = Instant::now();
        assert!(holds.press(chain("voice"), start));
        let repeat = start + Duration::from_millis(600);
        assert!(!holds.press(chain("voice"), repeat));
        let (due, next) = holds.expire(start + HOLD_TAIL);
        assert!(due.is_empty());
        assert_eq!(next, Some(Duration::from_millis(600)));
        let (due, next) = holds.expire(repeat + HOLD_TAIL);
        assert_eq!(due, vec![chain("voice")]);
        assert_eq!(next, None);
        assert!(holds.press(chain("voice"), repeat + HOLD_TAIL));
    }

    #[test]
    fn each_target_closes_on_its_own() {
        let mut holds = Holds::default();
        let start = Instant::now();
        holds.press(chain("a"), start);
        holds.press(
            HoldTarget::Node { node: NodeId(42) },
            start + Duration::from_millis(300),
        );
        let (due, next) = holds.expire(start + HOLD_TAIL);
        assert_eq!(due, vec![chain("a")]);
        assert_eq!(next, Some(Duration::from_millis(300)));
    }
}
