//! The user's own plugin folders: a `.clap` file or an LV2 bundle folder dropped on the
//! window is copied into `~/.clap` or `~/.lv2`, and Settings can remove it again. System
//! packages go through `crate::packages`; plugins no distribution carries are downloaded.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::i18n::{t, tf};

/// A plugin Canalis downloads itself: a release zip, pinned by hash, whose LV2 bundles
/// go into `~/.lv2`.
pub struct Download {
    pub name: &'static str,
    pub about: &'static str,
    url: &'static str,
    sha256: &'static str,
    /// Bundle folders inside the zip; each lands in `~/.lv2` under its last component.
    bundles: &'static [&'static str],
}

pub const DOWNLOADS: &[Download] = &[Download {
    name: "Noise suppression",
    about: "Alt Denoiser: DeepFilterNet3 voice denoiser for a mic chain, 40 ms latency; LV2, from GitHub",
    url: "https://github.com/Altinus/Alt-Denoiser/releases/download/v1.0.3/Alt-Denoiser-linux.zip",
    sha256: "db91ab94d448fdb0a1b61c9a214a2b7de12a8065b385a946c33e460d254283cc",
    bundles: &["Alt-Denoiser-linux/LV2/Alt Denoiser.lv2"],
}];

impl Download {
    pub fn label(&self) -> &'static str {
        match self.name {
            "Noise suppression" => t("Noise suppression"),
            other => other,
        }
    }

    pub fn blurb(&self) -> &'static str {
        match self.name {
            "Noise suppression" => t(
                "Alt Denoiser: DeepFilterNet3 voice denoiser for a mic chain, 40 ms latency; LV2, from GitHub",
            ),
            _ => self.about,
        }
    }
}

/// Release zips run to 130 MB (Alt Denoiser bundles its model); room to grow, still bounded.
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;

/// What the download thread is doing; `changes` counts finished downloads, for a rescan.
#[derive(Clone)]
pub struct Fetching {
    pub busy: Option<&'static str>,
    pub error: Option<String>,
    pub changes: u64,
}

static FETCHING: Mutex<Fetching> = Mutex::new(Fetching {
    busy: None,
    error: None,
    changes: 0,
});

fn fetching_mut() -> std::sync::MutexGuard<'static, Fetching> {
    FETCHING.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn fetching() -> Fetching {
    fetching_mut().clone()
}

/// Whether every bundle of `download` is in `~/.lv2`.
pub fn downloaded(download: &Download) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let lv2 = Path::new(&home).join(".lv2");
    download
        .bundles
        .iter()
        .all(|b| lv2.join(bundle_name(b)).join("manifest.ttl").is_file())
}

/// Downloads, checks and unpacks `download` on its own thread; `wake` repaints when done.
pub fn fetch(download: &'static Download, wake: impl Fn() + Send + 'static) {
    {
        let mut state = fetching_mut();
        if state.busy.is_some() {
            return;
        }
        state.busy = Some(download.name);
        state.error = None;
    }
    std::thread::spawn(move || {
        let result = std::env::var_os("HOME")
            .ok_or_else(|| t("HOME is not set").to_owned())
            .and_then(|home| {
                let bytes = get(download.url)?;
                verify(&bytes, download.sha256)?;
                unpack(&bytes, download.bundles, &Path::new(&home).join(".lv2"))
            });
        let mut state = fetching_mut();
        state.busy = None;
        match result {
            Ok(bundles) => {
                log::info!("downloaded {}: {}", download.name, bundles.join(", "));
                state.changes += 1;
            }
            Err(e) => {
                log::warn!("downloading {}: {e}", download.name);
                state.error = Some(tf(
                    "downloading {name} failed: {error}",
                    &[("name", &download.label()), ("error", &e)],
                ));
            }
        }
        drop(state);
        wake();
    });
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    ureq::get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

fn verify(bytes: &[u8], sha256: &str) -> Result<(), String> {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest == sha256 {
        Ok(())
    } else {
        Err(tf(
            "the file's SHA-256 is {digest}, expected {expected}",
            &[("digest", &digest), ("expected", &sha256)],
        ))
    }
}

fn bundle_name(bundle: &str) -> &str {
    bundle.rsplit('/').next().unwrap_or(bundle)
}

/// Unpacks only `bundles` from the zip into `lv2`, through a staging folder so a failure
/// leaves no half bundle behind; the bundle names it put there.
fn unpack(bytes: &[u8], bundles: &[&str], lv2: &Path) -> Result<Vec<String>, String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let staging = lv2.join(".canalis-download");
    let _ = std::fs::remove_dir_all(&staging);
    let io = |e: std::io::Error| e.to_string();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(path) = file.enclosed_name() else {
            continue;
        };
        let Some((bundle, inner)) = bundles
            .iter()
            .find_map(|b| Some((*b, path.strip_prefix(b).ok()?)))
        else {
            continue;
        };
        let target = staging.join(bundle_name(bundle)).join(inner);
        if file.is_dir() {
            std::fs::create_dir_all(&target).map_err(io)?;
            continue;
        }
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).map_err(io)?;
        std::fs::write(&target, contents).map_err(io)?;
    }
    let mut placed = Vec::new();
    for bundle in bundles {
        let name = bundle_name(bundle);
        let from = staging.join(name);
        if !from.join("manifest.ttl").is_file() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(tf(
                "the zip has no {bundle}/manifest.ttl",
                &[("bundle", bundle)],
            ));
        }
        let to = lv2.join(name);
        let _ = std::fs::remove_dir_all(&to);
        std::fs::rename(&from, &to).map_err(io)?;
        placed.push(name.to_owned());
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(placed)
}

/// Copies a dropped plugin into `~/.clap` or `~/.lv2`; where it went, or why not.
pub fn install(source: &Path) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or(t("HOME is not set"))?;
    install_into(source, Path::new(&home))
}

fn install_into(source: &Path, home: &Path) -> Result<PathBuf, String> {
    let name = source
        .file_name()
        .ok_or_else(|| tf("{path} has no file name", &[("path", &source.display())]))?;
    let extension = source.extension().and_then(|e| e.to_str()).unwrap_or("");
    let folder = match extension {
        "clap" => ".clap",
        "lv2" if source.join("manifest.ttl").is_file() => ".lv2",
        "lv2" => {
            return Err(tf(
                "{path} has no manifest.ttl",
                &[("path", &source.display())],
            ));
        }
        _ => {
            return Err(tf(
                "{path} is not a .clap plugin or an LV2 bundle folder",
                &[("path", &source.display())],
            ));
        }
    };
    let target = home.join(folder).join(name);
    if target == source {
        return Ok(target);
    }
    copy(source, &target).map_err(|e| {
        tf(
            "copying to {path}: {error}",
            &[("path", &target.display()), ("error", &e)],
        )
    })?;
    log::info!("installed {} as {}", source.display(), target.display());
    Ok(target)
}

fn copy(source: &Path, target: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(source, target).map(|_| ())
    }
}

/// Deletes a plugin file or bundle from `~/.clap` or `~/.lv2`; nothing outside them.
pub fn remove(path: &str) -> Result<(), String> {
    let home = std::env::var_os("HOME").ok_or(t("HOME is not set"))?;
    let path = Path::new(path);
    let allowed = [".clap", ".lv2"].map(|dir| Path::new(&home).join(dir));
    if !path
        .parent()
        .is_some_and(|p| allowed.iter().any(|a| a == p))
    {
        return Err(tf(
            "{path} is not in ~/.clap or ~/.lv2",
            &[("path", &path.display())],
        ));
    }
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|e| {
        tf(
            "removing {path}: {error}",
            &[("path", &path.display()), ("error", &e)],
        )
    })?;
    log::info!("removed {}", path.display());
    Ok(())
}

/// Opens `~/<folder>` (`.clap` or `.lv2`) in the file manager, creating it first.
pub fn open_folder(folder: &str) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let folder = Path::new(&home).join(folder);
    let _ = std::fs::create_dir_all(&folder);
    match std::process::Command::new("xdg-open").arg(&folder).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => log::warn!("xdg-open {}: {e}", folder.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_clap_files_and_lv2_bundles_are_taken() {
        let dir = std::env::temp_dir().join(format!("canalis-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("empty.lv2")).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let home = dir.join("home");
        assert!(install_into(&dir.join("notes.txt"), &home).is_err());
        assert!(
            install_into(&dir.join("empty.lv2"), &home).is_err(),
            "no manifest.ttl"
        );
        std::fs::create_dir_all(dir.join("verb.lv2/modgui")).unwrap();
        std::fs::write(dir.join("verb.lv2/manifest.ttl"), "@prefix lv2: <x> .").unwrap();
        std::fs::write(dir.join("verb.lv2/modgui/icon.png"), "png").unwrap();
        std::fs::write(dir.join("Delay.clap"), "elf").unwrap();
        assert_eq!(
            install_into(&dir.join("verb.lv2"), &home),
            Ok(home.join(".lv2/verb.lv2"))
        );
        assert!(
            home.join(".lv2/verb.lv2/modgui/icon.png").is_file(),
            "copied whole"
        );
        assert_eq!(
            install_into(&dir.join("Delay.clap"), &home),
            Ok(home.join(".clap/Delay.clap"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn a_download_must_match_its_pinned_hash() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify(b"", empty).is_ok());
        assert!(verify(b"tampered", empty).is_err());
    }

    #[test]
    fn only_the_named_bundles_are_unpacked_into_lv2() {
        let dir = std::env::temp_dir().join(format!("canalis-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lv2 = dir.join(".lv2");
        let bytes = zip_of(&[
            ("pkg/denoise.lv2/manifest.ttl", "@prefix lv2: <x> ."),
            ("pkg/denoise.lv2/denoise.so", "elf"),
            ("pkg/vst3/denoise.so", "elf"),
            ("../escape.lv2/manifest.ttl", "x"),
        ]);
        assert_eq!(
            unpack(&bytes, &["pkg/denoise.lv2"], &lv2),
            Ok(vec!["denoise.lv2".to_owned()])
        );
        assert!(lv2.join("denoise.lv2/denoise.so").is_file());
        let left: Vec<_> = std::fs::read_dir(&lv2).unwrap().collect();
        assert_eq!(left.len(), 1, "no other folders, no staging left");
        assert!(!dir.join("escape.lv2").exists());
        assert!(
            unpack(&bytes, &["pkg/missing.lv2"], &lv2).is_err(),
            "a bundle the zip lacks"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
