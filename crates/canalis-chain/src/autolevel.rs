//! Automatic level: a slow gain that steers the chain's short-term loudness toward its
//! target, so quiet and loud sources come out even. It reads the output's own loudness
//! (a feedback loop, kept stable by rising at most 1 dB/s), holds in silence so it never
//! lifts the noise floor, and backs off fast when the true peak nears full scale.
//! Plain arithmetic: it runs in the process callback.

/// Short-term loudness below this is silence: the gain holds.
const SILENT_LUFS: f32 = -60.0;
const MAX_BOOST_DB: f32 = 12.0;
const MAX_CUT_DB: f32 = -24.0;
/// How fast the gain may rise and fall toward the target, in dB per second.
const RISE_DB_PER_S: f32 = 1.0;
const FALL_DB_PER_S: f32 = 4.0;
/// Above this true peak the gain falls at `PEAK_FALL_DB_PER_S`, whatever the loudness.
const PEAK_CEILING_DBTP: f32 = -1.0;
const PEAK_FALL_DB_PER_S: f32 = 12.0;

#[derive(Debug, Default)]
pub struct AutoLevel {
    /// The loudness aimed at, in LUFS; `None` is off, and the gain eases back to 0 dB.
    target: Option<f32>,
    gain_db: f32,
}

impl AutoLevel {
    pub fn set_target(&mut self, target: Option<f32>) {
        self.target = target;
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// One step of `dt` seconds from the output's short-term loudness and true peak.
    pub fn update(&mut self, short_term: f32, true_peak: f32, dt: f32) {
        let wanted = match self.target {
            None => 0.0,
            Some(_) if true_peak > PEAK_CEILING_DBTP => {
                self.gain_db = (self.gain_db - PEAK_FALL_DB_PER_S * dt).max(MAX_CUT_DB);
                return;
            }
            Some(_) if short_term < SILENT_LUFS => return,
            Some(target) => self.gain_db + (target - short_term),
        };
        let step = (wanted - self.gain_db).clamp(-FALL_DB_PER_S * dt, RISE_DB_PER_S * dt);
        self.gain_db = (self.gain_db + step).clamp(MAX_CUT_DB, MAX_BOOST_DB);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `seconds` of a source at `source_lufs` through the loop: the output's loudness
    /// is the source's plus the gain (short-term loudness taken as instant here), its
    /// true peak 8 dB above that, as music's often is.
    fn settle(level: &mut AutoLevel, source_lufs: f32, seconds: f32) {
        let dt = 0.02;
        for _ in 0..(seconds / dt) as usize {
            let output = source_lufs + level.gain_db();
            level.update(output, output + 8.0, dt);
        }
    }

    #[test]
    fn a_quiet_source_is_brought_up_to_the_target_slowly() {
        let mut level = AutoLevel::default();
        level.set_target(Some(-16.0));
        settle(&mut level, -24.0, 2.0);
        assert!(
            (level.gain_db() - 2.0).abs() < 0.1,
            "1 dB/s up: {}",
            level.gain_db()
        );
        settle(&mut level, -24.0, 10.0);
        assert!(
            (level.gain_db() - 8.0).abs() < 0.05,
            "settled: {}",
            level.gain_db()
        );
    }

    #[test]
    fn it_cuts_faster_than_it_boosts_and_stays_in_range() {
        let mut level = AutoLevel::default();
        level.set_target(Some(-16.0));
        settle(&mut level, -10.0, 1.0);
        assert!(
            (level.gain_db() + 4.0).abs() < 0.1,
            "4 dB/s down: {}",
            level.gain_db()
        );
        settle(&mut level, -80.0 + 59.0, 60.0);
        assert!(level.gain_db() <= MAX_BOOST_DB);
    }

    #[test]
    fn silence_holds_and_a_hot_peak_backs_off() {
        let mut level = AutoLevel::default();
        level.set_target(Some(-16.0));
        settle(&mut level, -24.0, 3.0);
        let held = level.gain_db();
        level.update(-200.0, -200.0, 1.0);
        assert_eq!(level.gain_db(), held, "silence does not lift the floor");
        level.update(-16.0, -0.5, 0.5);
        assert!(
            (level.gain_db() - (held - 6.0)).abs() < 1e-4,
            "12 dB/s off a hot peak"
        );
    }

    #[test]
    fn off_eases_back_to_unity() {
        let mut level = AutoLevel::default();
        level.set_target(Some(-16.0));
        settle(&mut level, -24.0, 3.0);
        level.set_target(None);
        settle(&mut level, -24.0, 10.0);
        assert_eq!(level.gain_db(), 0.0);
    }
}
