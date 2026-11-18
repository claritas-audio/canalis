//! Whether this session may use the performance cores of a hybrid Intel CPU. Power savers
//! such as intel_lpmd confine user processes to the efficiency cores in the balanced and
//! power-saver profiles, where a heavy plugin can miss the audio deadline.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::i18n::tf;

/// The restriction follows the power profile, which can change at any time.
const REFRESH: Duration = Duration::from_secs(5);

/// The performance cores and the cores this process may use, as the kernel lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct Confinement {
    pub performance: String,
    pub allowed: String,
}

impl Confinement {
    pub fn explain(&self) -> String {
        tf(
            "Apps may use CPUs {allowed} only, not the performance cores {performance}; that \
             is usually intel_lpmd in the balanced or power-saver profile. Heavy plugins such \
             as denoisers can then crackle or get the chain stopped. Use the performance \
             profile, or turn intel_lpmd off: `sudo systemctl disable --now intel_lpmd`",
            &[
                ("allowed", &self.allowed),
                ("performance", &self.performance),
            ],
        )
    }
}

/// `Some` while performance cores exist and none of them is allowed to this process.
pub fn confinement() -> Option<Confinement> {
    static CACHE: Mutex<Option<(Instant, Option<Confinement>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, found)) = cache.as_ref()
        && at.elapsed() < REFRESH
    {
        return found.clone();
    }
    let found = check();
    *cache = Some((Instant::now(), found.clone()));
    found
}

fn check() -> Option<Confinement> {
    // Only hybrid Intel CPUs have this file: elsewhere there is nothing to be kept off.
    let performance = std::fs::read_to_string("/sys/devices/cpu_core/cpus").ok()?;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let allowed = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?;
    confined(performance.trim(), allowed.trim())
}

fn confined(performance: &str, allowed: &str) -> Option<Confinement> {
    let fast = cpus(performance)?;
    let usable = cpus(allowed)?;
    (!fast.is_empty() && !fast.iter().any(|c| usable.contains(c))).then(|| Confinement {
        performance: performance.to_owned(),
        allowed: allowed.to_owned(),
    })
}

/// A kernel CPU list such as `0-5,8,10-11`.
fn cpus(list: &str) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => out.extend(a.parse::<u32>().ok()?..=b.parse::<u32>().ok()?),
            None => out.push(part.parse().ok()?),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_cpu_lists_parse() {
        assert_eq!(cpus("0-5"), Some(vec![0, 1, 2, 3, 4, 5]));
        assert_eq!(cpus("0-1,4,10-11"), Some(vec![0, 1, 4, 10, 11]));
        assert_eq!(cpus(""), Some(vec![]));
        assert_eq!(cpus("x"), None);
    }

    #[test]
    fn efficiency_cores_only_is_a_confinement() {
        assert_eq!(
            confined("0-5", "6-15"),
            Some(Confinement {
                performance: "0-5".into(),
                allowed: "6-15".into()
            })
        );
        assert_eq!(confined("0-5", "0-15"), None, "the performance profile");
        assert_eq!(
            confined("0-5", "3,6-15"),
            None,
            "one performance core is enough"
        );
        assert_eq!(confined("", "6-15"), None, "no performance cores to miss");
    }
}
