//! The plugin catalogue: `.clap` bundles under the CLAP search path, each scanned by a
//! throwaway `canalis-chain --scan` process so a crashing bundle never touches this one.
//! Results are cached by bundle path and modification time in `plugins.toml` under
//! `$XDG_CACHE_HOME/canalis`: a cache, so a missing or stale file only costs a rescan.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::config::{PluginEntry, write_atomic};

/// Bumped when `PluginEntry` changes shape; a cache of another format is dropped.
const CACHE_FORMAT: i64 = 1;

#[derive(Serialize, Deserialize)]
struct Cache {
    format: i64,
    #[serde(default, rename = "plugin")]
    plugins: Vec<PluginEntry>,
}

/// `$XDG_CACHE_HOME/canalis/plugins.toml`, or `None` without a cache directory.
pub(super) fn default_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("canalis").join("plugins.toml"))
}

/// The catalogue as last saved; empty when the file is missing, unreadable or of
/// another format.
pub(super) fn load_cache(path: &Path) -> Vec<PluginEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match toml::from_str::<Cache>(&text) {
        Ok(cache) if cache.format == CACHE_FORMAT => cache.plugins,
        Ok(_) => Vec::new(),
        Err(e) => {
            warn!("ignoring plugin cache {}: {e}", path.display());
            Vec::new()
        }
    }
}

pub(super) fn save_cache(path: &Path, plugins: &[PluginEntry]) {
    let cache = Cache {
        format: CACHE_FORMAT,
        plugins: plugins.to_vec(),
    };
    let written = toml::to_string_pretty(&cache)
        .map_err(std::io::Error::other)
        .and_then(|text| write_atomic(path, &text));
    if let Err(e) = written {
        warn!("cannot save plugin cache {}: {e}", path.display());
    }
}

/// How long one bundle may take to list itself before it is given up on.
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
/// Bundles nested deeper than this under a search directory are not looked at.
const MAX_DEPTH: usize = 3;

/// `$CLAP_PATH`, then `~/.clap`, `/usr/lib/clap` and `/usr/lib64/clap`, then the CLAPs
/// Canalis ships with (`<bin>/../lib/clap`, ADR 0007), last so a user's own build of
/// the same plugin wins.
pub(super) fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("CLAP_PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".clap"));
    }
    dirs.push("/usr/lib/clap".into());
    dirs.push("/usr/lib64/clap".into());
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("lib").join("clap")))
    {
        dirs.push(bundled);
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    dirs
}

/// Every `.clap` under `dirs` with its modification time, in the order of `dirs` (sorted
/// by path within each), each bundle once.
pub(super) fn bundles(dirs: &[PathBuf]) -> Vec<(PathBuf, u64)> {
    let mut found: Vec<(PathBuf, u64)> = Vec::new();
    for dir in dirs {
        let mut here = Vec::new();
        walk(dir, 0, &mut here);
        here.sort();
        for bundle in here {
            if !found.iter().any(|(p, _)| *p == bundle.0) {
                found.push(bundle);
            }
        }
    }
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "clap") {
            found.push((path, modified(&entry)));
        } else if depth < MAX_DEPTH && path.is_dir() {
            walk(&path, depth + 1, found);
        }
    }
}

fn modified(entry: &std::fs::DirEntry) -> u64 {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The catalogue for `bundles`: cached CLAP entries whose bundle is unchanged are kept,
/// the rest are scanned with `binary`; LV2 plugins are listed afresh each time (lilv
/// parses their TTL in a second). Sorted by name, then vendor; with why LV2 is off, if it is.
pub(super) fn catalogue(
    binary: &Path,
    cached: &[PluginEntry],
    bundles: &[(PathBuf, u64)],
) -> (Vec<PluginEntry>, Option<String>) {
    let (mut plugins, lv2_off) = match scan_lv2(binary) {
        Ok(plugins) => (plugins, None),
        Err(why) => (Vec::new(), Some(why)),
    };
    for (path, modified) in bundles {
        let path_text = path.to_string_lossy();
        let fresh: Vec<&PluginEntry> = cached
            .iter()
            .filter(|p| !p.is_lv2() && p.path == path_text && p.modified == *modified)
            .collect();
        if fresh.is_empty() {
            plugins.extend(scan(binary, path, *modified));
        } else {
            plugins.extend(fresh.into_iter().cloned());
        }
    }
    // A plugin id found twice keeps its first bundle: the earlier search directory.
    let mut ids = std::collections::HashSet::new();
    plugins.retain(|p| p.is_lv2() || ids.insert(p.id.clone()));
    plugins.sort_by(|a, b| a.name.cmp(&b.name).then(a.vendor.cmp(&b.vendor)));
    (plugins, lv2_off)
}

#[derive(Deserialize)]
struct Scanned {
    id: String,
    name: String,
    vendor: String,
}

#[derive(Deserialize)]
struct ScannedLv2 {
    uri: String,
    name: String,
    vendor: String,
    bundle: String,
}

/// Every LV2 plugin lilv knows, through one `canalis-chain --scan-lv2`.
/// What `canalis-chain --scan-lv2` exits with when lilv is not installed.
const EXIT_NO_LILV: i32 = 3;

/// The LV2 plugins lilv lists, or why there are none to list (lilv missing).
fn scan_lv2(binary: &Path) -> Result<Vec<PluginEntry>, String> {
    let output = Process::new(binary)
        .arg("--scan-lv2")
        .stdin(Stdio::null())
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(output) if output.status.code() == Some(EXIT_NO_LILV) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = stderr
                .lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("LV2 is off");
            warn!("{why}");
            return Err(why.to_owned());
        }
        Ok(_) => {
            warn!("LV2 scan failed; no LV2 plugins listed");
            return Ok(Vec::new());
        }
        Err(e) => {
            warn!("cannot start {} for the LV2 scan: {e}", binary.display());
            return Ok(Vec::new());
        }
    };
    let plugins: Vec<PluginEntry> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<ScannedLv2>(line).ok())
        .map(|s| PluginEntry {
            path: s.bundle,
            id: s.uri,
            name: s.name,
            vendor: s.vendor,
            modified: 0,
            format: "lv2".to_owned(),
        })
        .collect();
    info!("LV2: {} plugin(s)", plugins.len());
    Ok(plugins)
}

fn scan(binary: &Path, bundle: &Path, modified: u64) -> Vec<PluginEntry> {
    let started = Instant::now();
    let spawned = Process::new(binary)
        .arg("--scan")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            warn!(
                "cannot start {} to scan {}: {e}",
                binary.display(),
                bundle.display()
            );
            return Vec::new();
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let finished = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed() > SCAN_TIMEOUT => {
                warn!("{}: scan timed out; skipped", bundle.display());
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break false,
        }
    };
    let text = reader.join().unwrap_or_default();
    if !finished {
        warn!("{}: scan failed; skipped", bundle.display());
        return Vec::new();
    }
    let plugins: Vec<PluginEntry> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Scanned>(line).ok())
        .map(|s| PluginEntry {
            path: bundle.to_string_lossy().into_owned(),
            id: s.id,
            name: s.name,
            vendor: s.vendor,
            modified,
            format: "clap".to_owned(),
        })
        .collect();
    info!(
        "{}: {} plugin(s) in {:.0?}",
        bundle.display(),
        plugins.len(),
        started.elapsed()
    );
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, id: &str, modified: u64) -> PluginEntry {
        PluginEntry {
            path: path.into(),
            id: id.into(),
            name: id.to_uppercase(),
            vendor: "v".into(),
            modified,
            format: "clap".into(),
        }
    }

    #[test]
    fn unchanged_bundles_come_from_the_cache_and_vanished_ones_drop() {
        let cached = [
            entry("/a.clap", "a", 10),
            entry("/a.clap", "a2", 10),
            entry("/gone.clap", "g", 5),
        ];
        let bundles = [(PathBuf::from("/a.clap"), 10)];
        // A binary that cannot run: anything not cached would scan to nothing.
        let (plugins, _) = catalogue(Path::new("/nonexistent/canalis-chain"), &cached, &bundles);
        let ids: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "a2"]);
    }

    #[test]
    fn a_changed_bundle_is_rescanned() {
        let cached = [entry("/a.clap", "a", 10)];
        let bundles = [(PathBuf::from("/a.clap"), 11)];
        let (plugins, _) = catalogue(Path::new("/nonexistent/canalis-chain"), &cached, &bundles);
        assert!(plugins.is_empty(), "stale cache entry kept: {plugins:?}");
    }

    #[test]
    fn a_plugin_in_two_folders_keeps_the_first() {
        let cached = [
            entry("/home/u/.clap/Claritas EQ.clap", "audio.claritas.eq", 1),
            entry("/app/lib/clap/Claritas EQ.clap", "audio.claritas.eq", 2),
        ];
        let bundles = [
            (PathBuf::from("/home/u/.clap/Claritas EQ.clap"), 1),
            (PathBuf::from("/app/lib/clap/Claritas EQ.clap"), 2),
        ];
        let (plugins, _) = catalogue(Path::new("/nonexistent/canalis-chain"), &cached, &bundles);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].path, "/home/u/.clap/Claritas EQ.clap");
    }

    #[test]
    fn bundles_follow_the_search_order() {
        let root = std::env::temp_dir().join(format!("canalis-bundles-{}", std::process::id()));
        let (user, shipped) = (root.join("user"), root.join("shipped"));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::write(user.join("z.clap"), "").unwrap();
        std::fs::write(shipped.join("a.clap"), "").unwrap();
        let found: Vec<PathBuf> = bundles(&[user.clone(), shipped.clone()])
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(found, [user.join("z.clap"), shipped.join("a.clap")]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
