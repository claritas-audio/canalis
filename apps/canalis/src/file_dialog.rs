//! The desktop's own file chooser, through the FileChooser portal (the one GNOME, KDE and
//! the Flatpak sandbox all answer), on a thread so the window keeps drawing; the view
//! polls for the answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::i18n::t;

/// A chooser that is open; `poll` hands back the answer once.
pub struct Dialog {
    answer: mpsc::Receiver<Result<Option<PathBuf>, String>>,
}

impl Dialog {
    /// `Some(Ok(Some(path)))` chosen, `Some(Ok(None))` cancelled, `None` still open.
    pub fn poll(&self) -> Option<Result<Option<PathBuf>, String>> {
        self.answer.try_recv().ok()
    }
}

/// What files a chooser offers: a name and its glob, e.g. `("Canalis chain", "*.toml")`.
pub struct Filter {
    pub name: &'static str,
    pub glob: &'static str,
}

/// "Save as" with `name` filled in, starting in `folder`.
pub fn save(
    title: &str,
    name: &str,
    folder: Option<&Path>,
    filter: Filter,
    wake: impl Fn() + Send + 'static,
) -> Dialog {
    let mut options = vec![("current_name", Value::from(name.to_owned()))];
    if let Some(folder) = folder {
        options.push(("current_folder", folder_bytes(folder)));
    }
    options.push(("filters", filters(&filter)));
    ask("SaveFile", title, options, wake)
}

/// "Open" for one file, starting in `folder` when given.
pub fn open(
    title: &str,
    folder: Option<&Path>,
    filter: Filter,
    wake: impl Fn() + Send + 'static,
) -> Dialog {
    let mut options = vec![
        ("multiple", Value::from(false)),
        ("filters", filters(&filter)),
    ];
    if let Some(folder) = folder {
        options.push(("current_folder", folder_bytes(folder)));
    }
    ask("OpenFile", title, options, wake)
}

/// A byte string ending in NUL, as the portal wants paths.
fn folder_bytes(folder: &Path) -> Value<'static> {
    let mut bytes = folder.as_os_str().as_encoded_bytes().to_vec();
    bytes.push(0);
    Value::from(bytes)
}

/// `a(sa(us))`: one named filter of glob patterns (kind 0), then "All files".
fn filters(filter: &Filter) -> Value<'static> {
    let globs = filter
        .glob
        .split_whitespace()
        .map(|g| (0, g.to_owned()))
        .collect();
    let list: Vec<(String, Vec<(u32, String)>)> = vec![
        (filter.name.to_owned(), globs),
        (t("All files").to_owned(), vec![(0, "*".to_owned())]),
    ];
    Value::from(list)
}

fn ask(
    method: &'static str,
    title: &str,
    options: Vec<(&'static str, Value<'static>)>,
    wake: impl Fn() + Send + 'static,
) -> Dialog {
    let (tx, answer) = mpsc::channel();
    let title = title.to_owned();
    let spawned = std::thread::Builder::new()
        .name("file-chooser".into())
        .spawn(move || {
            let _ = tx.send(run(method, &title, options));
            wake();
        });
    if let Err(e) = spawned {
        let (tx, answer) = mpsc::channel();
        let _ = tx.send(Err(e.to_string()));
        return Dialog { answer };
    }
    Dialog { answer }
}

/// One portal request: subscribe to the Request object the portal will create (its path
/// is known from our handle token), call, wait for its `Response`.
fn run(
    method: &str,
    title: &str,
    mut options: Vec<(&'static str, Value<'static>)>,
) -> Result<Option<PathBuf>, String> {
    let text = |e: zbus::Error| e.to_string();
    let bus = Connection::session().map_err(text)?;
    let sender = bus
        .unique_name()
        .ok_or(t("no bus name"))?
        .trim_start_matches(':')
        .replace('.', "_");
    let token = format!("canalis{}", std::process::id() ^ rand_bits());
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(
        &bus,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(text)?;
    let mut responses = request.receive_signal("Response").map_err(text)?;
    options.push(("handle_token", Value::from(token)));
    let options: HashMap<&str, Value<'_>> = options.into_iter().collect();
    let chooser = Proxy::new(
        &bus,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
    )
    .map_err(text)?;
    let _: OwnedObjectPath = chooser.call(method, &("", title, options)).map_err(text)?;
    let message = responses.next().ok_or(t("the file chooser went away"))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        message.body().deserialize().map_err(text)?;
    if code != 0 {
        return Ok(None);
    }
    let uris: Vec<String> = results
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
        .unwrap_or_default();
    Ok(uris.first().and_then(|uri| path_of(uri)))
}

/// Some bits that differ between two choosers opened in one second.
fn rand_bits() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos())
}

/// `file:///home/me/My%20chain.toml` → `/home/me/My chain.toml`.
fn path_of(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = encoded
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a desktop session: the portal must accept our SaveFile request as built (a
    /// wrong type is refused at once); the dialog it opens is closed straight away.
    #[test]
    #[ignore = "opens and closes a file chooser on the desktop"]
    fn the_portal_takes_a_save_request() {
        let bus = Connection::session().expect("session bus");
        let options: HashMap<&str, Value<'_>> = [
            ("current_name", Value::from("Voice.canalis-chain.toml")),
            ("current_folder", Value::from(b"/tmp\0".to_vec())),
            (
                "filters",
                filters(&Filter {
                    name: "Canalis chain",
                    glob: "*.canalis-chain.toml",
                }),
            ),
            ("handle_token", Value::from("canalistest")),
        ]
        .into_iter()
        .collect();
        let chooser = Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.FileChooser",
        )
        .expect("proxy");
        let request: OwnedObjectPath = chooser
            .call("SaveFile", &("", "Canalis test", options))
            .expect("the portal accepts the request");
        assert!(request.as_str().ends_with("/canalistest"), "{request:?}");
        std::thread::sleep(std::time::Duration::from_millis(800));
        let close = Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            request.as_str(),
            "org.freedesktop.portal.Request",
        )
        .expect("request proxy");
        let _: () = close.call("Close", &()).expect("closes");
    }

    #[test]
    fn file_uris_decode_to_paths() {
        assert_eq!(
            path_of("file:///home/me/My%20chain.canalis-chain.toml"),
            Some(PathBuf::from("/home/me/My chain.canalis-chain.toml"))
        );
        assert_eq!(path_of("https://example.org/x"), None);
    }
}
