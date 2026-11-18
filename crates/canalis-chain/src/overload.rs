//! A chain that keeps overrunning its cycle never sleeps, and the kernel kills it once its
//! real-time thread has run 200 ms straight (RTKit's `RLIMIT_RTTIME`). Before that, the
//! callback bypasses the slowest slots and tells the main loop which, through atomics.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::slot::MAX_SLOTS;

/// Consecutive overrunning cycles before acting: about 64 ms at 1024 frames and 48 kHz.
const STREAK: u32 = 3;

/// The graph rate, set when the format is negotiated; 0 until then, which disables the check.
pub static RATE: AtomicU32 = AtomicU32::new(0);

/// Per slot, the µs it took when the callback bypassed it; 0 when nothing is waiting.
static PENDING: [AtomicU64; MAX_SLOTS] = [const { AtomicU64::new(0) }; MAX_SLOTS];
/// The cycle's budget in µs at the last bypass.
static CYCLE_US: AtomicU64 = AtomicU64::new(0);
/// The highest share of a cycle the chain used since `take_peak_load`, in ‰.
static PEAK_LOAD: AtomicU32 = AtomicU32::new(0);

/// The chain's peak load since the last call, as a percentage of the cycle; `None` when
/// no cycle ran (nothing plays through the chain).
pub fn take_peak_load() -> Option<f32> {
    let permille = PEAK_LOAD.swap(0, Ordering::Relaxed);
    (permille > 0).then(|| permille as f32 / 10.0)
}

/// Slots bypassed for overrunning and not yet reported: `(slot, slot_us, cycle_us)`.
pub fn take_reports() -> Vec<(usize, u64, u64)> {
    let cycle_us = CYCLE_US.load(Ordering::Acquire);
    PENDING
        .iter()
        .enumerate()
        .filter_map(|(slot, pending)| {
            let slot_us = pending.swap(0, Ordering::Acquire);
            (slot_us > 0).then_some((slot, slot_us, cycle_us))
        })
        .collect()
}

/// Counts overrunning cycles and picks the slots to bypass. Real-time: no allocation.
#[derive(Default)]
pub struct Watchdog {
    streak: u32,
}

impl Watchdog {
    /// One cycle: each slot's time (0 for an empty or bypassed one), the whole chain's,
    /// and the cycle's length, in ns. Once the chain has spent over 90% of its cycle
    /// `STREAK` times in a row, or over 150% once, the slots to bypass as a bit mask: the
    /// slowest first, until the rest fits in 75% of the cycle.
    pub fn check(&mut self, slot_ns: &[u64; MAX_SLOTS], total_ns: u64, budget_ns: u64) -> u32 {
        if let Some(permille) = (total_ns * 1000).checked_div(budget_ns) {
            let permille = permille.clamp(1, u64::from(u32::MAX)) as u32;
            PEAK_LOAD.fetch_max(permille, Ordering::Relaxed);
        }
        if budget_ns == 0 || total_ns * 10 <= budget_ns * 9 {
            self.streak = 0;
            return 0;
        }
        self.streak += 1;
        if self.streak < STREAK && total_ns * 2 <= budget_ns * 3 {
            return 0;
        }
        self.streak = 0;
        let mut mask = 0u32;
        let mut left = total_ns;
        while left * 4 > budget_ns * 3 {
            let Some((slot, ns)) = slot_ns
                .iter()
                .copied()
                .enumerate()
                .filter(|&(i, ns)| ns > 0 && mask & (1 << i) == 0)
                .max_by_key(|&(_, ns)| ns)
            else {
                break;
            };
            mask |= 1 << slot;
            left = left.saturating_sub(ns);
            PENDING[slot].store((ns / 1000).max(1), Ordering::Relaxed);
        }
        if mask != 0 {
            CYCLE_US.store(budget_ns / 1000, Ordering::Release);
        }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CYCLE: u64 = 21_333_000;

    fn slots(times: &[u64]) -> [u64; MAX_SLOTS] {
        let mut all = [0; MAX_SLOTS];
        all[..times.len()].copy_from_slice(times);
        all
    }

    #[test]
    fn the_peak_load_is_reported_once_then_starts_over() {
        let _serial = crate::plugin_test_guard();
        take_peak_load();
        let mut dog = Watchdog::default();
        dog.check(&slots(&[1_000_000]), CYCLE / 10, CYCLE);
        dog.check(&slots(&[1_000_000]), CYCLE / 4, CYCLE);
        dog.check(&slots(&[1_000_000]), CYCLE / 5, CYCLE);
        assert_eq!(take_peak_load(), Some(25.0), "the highest of the three");
        assert_eq!(take_peak_load(), None, "nothing ran since");
    }

    #[test]
    fn one_heavy_slot_goes_after_three_overrunning_cycles() {
        let _serial = crate::plugin_test_guard();
        take_reports();
        let mut dog = Watchdog::default();
        let cycle = slots(&[300_000, 20_000_000]);
        assert_eq!(dog.check(&cycle, 20_400_000, CYCLE), 0);
        assert_eq!(dog.check(&cycle, 20_400_000, CYCLE), 0);
        assert_eq!(dog.check(&cycle, 20_400_000, CYCLE), 0b10);
        assert_eq!(take_reports(), vec![(1, 20_000, 21_333)]);
        assert_eq!(take_reports(), vec![], "reported once");
    }

    #[test]
    fn many_medium_slots_go_until_the_rest_fits() {
        let _serial = crate::plugin_test_guard();
        take_reports();
        let mut dog = Watchdog::default();
        // Eight denoisers at 4–5 ms each on efficiency cores: none is heavy on its own.
        let cycle = slots(&[
            4_000_000, 5_000_000, 4_100_000, 4_200_000, 4_300_000, 4_400_000, 4_500_000, 4_600_000,
        ]);
        let total: u64 = cycle.iter().sum();
        let mask = dog.check(&cycle, total, CYCLE);
        assert_ne!(mask, 0, "35 ms in a 21 ms cycle acts at once");
        let kept: u64 = (0..MAX_SLOTS)
            .filter(|i| mask & (1 << i) == 0)
            .map(|i| cycle[i])
            .sum();
        assert!(kept * 4 <= CYCLE * 3, "{kept} ns left");
        assert_eq!(mask & 1, 0, "the quickest stays");
        assert_eq!(take_reports().len(), mask.count_ones() as usize);
    }

    #[test]
    fn one_quick_cycle_resets_the_streak() {
        let mut dog = Watchdog::default();
        let slow = slots(&[20_000_000]);
        assert_eq!(dog.check(&slow, 20_000_000, CYCLE), 0);
        assert_eq!(dog.check(&slow, 20_000_000, CYCLE), 0);
        assert_eq!(dog.check(&slots(&[2_000_000]), 2_000_000, CYCLE), 0);
        assert_eq!(dog.check(&slow, 20_000_000, CYCLE), 0);
        assert_eq!(dog.check(&slow, 20_000_000, CYCLE), 0);
    }

    #[test]
    fn an_unknown_rate_disables_the_check() {
        let mut dog = Watchdog::default();
        for _ in 0..10 {
            assert_eq!(dog.check(&slots(&[90_000_000]), 90_000_000, 0), 0);
        }
    }
}
