//! Headphone correction from AutoEQ (github.com/jaakkopasanen/AutoEq): its index of
//! measured models and, per model, a parametric curve (a preamp and peaking or shelf
//! filters) that becomes a Claritas EQ slot. The index is fetched once and cached for a
//! month; fetches run on a thread and the view polls.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use canalis_pw::claritas::{EqBand, EqShape};

use crate::i18n::{t, tf};

const RESULTS: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/";
/// The index is a few hundred KB; a profile is a few hundred bytes.
const MAX_FETCH: u64 = 4 * 1024 * 1024;
const INDEX_FRESH: Duration = Duration::from_secs(30 * 24 * 3600);

/// One measured headphone: a model and who measured it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// "oratory1990", "crinacle on GRAS 43AG-7", …
    pub source: String,
    /// The model's folder under `results/`, URL-encoded as the index has it.
    path: String,
}

/// A correction curve: the preamp that keeps it from clipping and the filters.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

/// `- [Name](./source/rig/Name) by Source on Rig` lines of `results/INDEX.md`.
pub fn parse_index(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("- [")?;
            let (name, rest) = rest.split_once("](")?;
            // The path is URL-encoded, so its first ") by " is where it ends, even when
            // the model's name has brackets of its own.
            let (path, source) = rest.split_once(") by ")?;
            let source = source.trim();
            Some(Entry {
                name: name.to_owned(),
                source: source.to_owned(),
                path: path.trim_start_matches("./").to_owned(),
            })
        })
        .collect()
}

/// A `ParametricEQ.txt`: `Preamp: -6.3 dB` and `Filter n: ON PK Fc 125 Hz Gain -2.7 dB
/// Q 0.55` lines (PK, LSC and HSC; filters that are OFF are skipped).
pub fn parse_profile(text: &str) -> Result<Profile, String> {
    let mut preamp_db = 0.0;
    let mut bands = Vec::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["Preamp:", db, "dB"] => {
                preamp_db = db
                    .parse()
                    .map_err(|_| tf("bad preamp: {line}", &[("line", &line)]))?;
            }
            [filter, _, "ON", kind, rest @ ..] if filter.starts_with("Filter") => {
                let shape = match *kind {
                    "PK" => EqShape::Bell,
                    "LSC" | "LS" => EqShape::LowShelf,
                    "HSC" | "HS" => EqShape::HighShelf,
                    _ => continue,
                };
                let value = |key: &str| {
                    rest.iter()
                        .position(|w| *w == key)
                        .and_then(|i| rest.get(i + 1))
                        .and_then(|v| v.parse::<f32>().ok())
                };
                let (Some(freq), Some(gain)) = (value("Fc"), value("Gain")) else {
                    return Err(tf("bad filter: {line}", &[("line", &line)]));
                };
                bands.push(EqBand {
                    shape,
                    freq_hz: freq,
                    gain_db: gain,
                    q: value("Q").unwrap_or(0.707),
                    slope_db: 12,
                    enabled: true,
                });
            }
            _ => {}
        }
    }
    if bands.is_empty() {
        return Err(t("no filters in the profile").into());
    }
    Ok(Profile { preamp_db, bands })
}

/// Where the index stands.
#[derive(Clone, Debug, Default)]
pub enum Index {
    #[default]
    Idle,
    Loading,
    Ready(Vec<Entry>),
    Failed(String),
}

/// The AutoEQ index and the profile asked for, fetched off the GUI thread.
#[derive(Default)]
pub struct Library {
    index: Index,
    index_rx: Option<mpsc::Receiver<Result<Vec<Entry>, String>>>,
    profile_rx: Option<mpsc::Receiver<(Entry, Result<Profile, String>)>>,
}

impl Library {
    /// The index, loading it (from the cache or the network) on first use.
    pub fn index(&mut self, wake: impl Fn() + Send + 'static) -> &Index {
        if let Some(answer) = self.index_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.index_rx = None;
            self.index = match answer {
                Ok(entries) => Index::Ready(entries),
                Err(e) => Index::Failed(e),
            };
        }
        if matches!(self.index, Index::Idle) {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(load_index());
                wake();
            });
            self.index_rx = Some(rx);
            self.index = Index::Loading;
        }
        &self.index
    }

    /// Fetches `entry`'s curve; `take_profile` hands it over.
    pub fn request(&mut self, entry: Entry, wake: impl Fn() + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let profile = fetch_profile(&entry);
            let _ = tx.send((entry, profile));
            wake();
        });
        self.profile_rx = Some(rx);
    }

    pub fn fetching(&self) -> bool {
        self.profile_rx.is_some()
    }

    /// The curve asked for, once it arrived.
    pub fn take_profile(&mut self) -> Option<(Entry, Result<Profile, String>)> {
        let answer = self.profile_rx.as_ref()?.try_recv().ok()?;
        self.profile_rx = None;
        Some(answer)
    }
}

fn cache_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("canalis").join("autoeq-index.md"))
}

/// The cached index when fresh, else the network's (cached again), else a stale cache.
fn load_index() -> Result<Vec<Entry>, String> {
    let cache = cache_path();
    let cached = cache
        .as_ref()
        .and_then(|p| Some((std::fs::read_to_string(p).ok()?, std::fs::metadata(p).ok()?)));
    let fresh = cached.as_ref().is_some_and(|(_, meta)| {
        meta.modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < INDEX_FRESH)
    });
    if fresh && let Some((text, _)) = &cached {
        return Ok(parse_index(text));
    }
    match get(&format!("{RESULTS}INDEX.md")) {
        Ok(text) => {
            if let Some(path) = &cache {
                let _ = path.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(path, &text);
            }
            Ok(parse_index(&text))
        }
        Err(e) => match cached {
            Some((text, _)) => Ok(parse_index(&text)),
            None => Err(tf("cannot reach AutoEQ ({error})", &[("error", &e)])),
        },
    }
}

fn fetch_profile(entry: &Entry) -> Result<Profile, String> {
    let folder = entry.path.trim_end_matches('/');
    let file = folder.rsplit('/').next().unwrap_or(folder);
    parse_profile(&get(&format!(
        "{RESULTS}{folder}/{file}%20ParametricEQ.txt"
    ))?)
}

fn get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_FETCH)
        .read_to_string()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lines_become_entries() {
        let text = "# Index\n\n\
            - [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990\n\
            - [1MORE Aero (ANC Off)](./HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)) by HypetheSonics on GRAS RA0045\n";
        let entries = parse_index(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Sennheiser HD 600");
        assert_eq!(entries[0].source, "oratory1990");
        assert_eq!(
            entries[0].path,
            "oratory1990/over-ear/Sennheiser%20HD%20600"
        );
        assert_eq!(entries[1].name, "1MORE Aero (ANC Off)");
        assert!(entries[1].path.ends_with("(ANC%20Off)"), "a ) in the name");
        assert_eq!(entries[1].source, "HypetheSonics on GRAS RA0045");
    }

    #[test]
    #[ignore = "fetches from GitHub"]
    fn the_real_index_and_a_real_profile_load() {
        let entries = load_index().expect("the index");
        assert!(entries.len() > 1000, "{} entries", entries.len());
        let hd600 = entries
            .iter()
            .find(|e| e.name == "Sennheiser HD 600" && e.source == "oratory1990")
            .expect("HD 600 by oratory1990");
        let profile = fetch_profile(hd600).expect("its curve");
        assert!(profile.preamp_db < 0.0);
        assert!(profile.bands.len() >= 5, "{profile:?}");
        let odd = entries
            .iter()
            .find(|e| e.name.contains('(') && e.name.contains(' '))
            .expect("a model with brackets");
        fetch_profile(odd).unwrap_or_else(|e| panic!("{}: {e}", odd.name));
    }

    #[test]
    fn a_parametric_profile_becomes_bands() {
        let text = "Preamp: -6.3 dB\n\
            Filter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70\n\
            Filter 2: ON PK Fc 125 Hz Gain -2.7 dB Q 0.55\n\
            Filter 3: OFF PK Fc 400 Hz Gain 1.0 dB Q 1.00\n\
            Filter 4: ON HSC Fc 10000 Hz Gain -3.1 dB Q 0.70\n";
        let profile = parse_profile(text).expect("parses");
        assert_eq!(profile.preamp_db, -6.3);
        assert_eq!(profile.bands.len(), 3, "the OFF filter is skipped");
        assert_eq!(profile.bands[0].shape, EqShape::LowShelf);
        assert_eq!(profile.bands[0].gain_db, 6.5);
        assert_eq!(profile.bands[1].shape, EqShape::Bell);
        assert_eq!(profile.bands[1].q, 0.55);
        assert_eq!(profile.bands[2].shape, EqShape::HighShelf);
        assert_eq!(profile.bands[2].freq_hz, 10000.0);
        assert!(parse_profile("Preamp: -1 dB\n").is_err());
    }
}
