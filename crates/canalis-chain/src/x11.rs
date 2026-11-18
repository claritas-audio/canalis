//! Top-level X11 windows that plugin GUIs are parented into (XWayland on Wayland desktops).
//! One connection per chain process, opened the first time a GUI is asked for; its socket
//! is pumped from the loop thread, never from the callback.

use std::cell::RefCell;
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask,
    PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// A closed window lives unmapped this long before it is destroyed, so the plugin's own
/// window thread gets to tear down its child first instead of finding it gone.
const GRACE: Duration = Duration::from_millis(250);

pub struct X11 {
    conn: RustConnection,
    screen: usize,
    wm_protocols: u32,
    wm_delete_window: u32,
    graveyard: RefCell<Vec<(Instant, u32)>>,
}

/// What a window of ours did, for the plugin that lives in it.
pub enum WindowEvent {
    /// The user closed the window through the window manager.
    Closed(u32),
    /// The window manager or the user gave the window a new size.
    Resized {
        window: u32,
        width: u32,
        height: u32,
    },
}

/// Something that holds the connection's fd for the loop's io source.
pub struct Fd(RawFd);

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl X11 {
    pub fn connect() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X11 display: {e}"))?;
        let atom = |name: &[u8]| -> Result<u32, String> {
            let cookie = conn
                .intern_atom(false, name)
                .map_err(|e| format!("X11 atom: {e}"))?;
            cookie
                .reply()
                .map(|r| r.atom)
                .map_err(|e| format!("X11 atom: {e}"))
        };
        let wm_protocols = atom(b"WM_PROTOCOLS")?;
        let wm_delete_window = atom(b"WM_DELETE_WINDOW")?;
        Ok(X11 {
            conn,
            screen,
            wm_protocols,
            wm_delete_window,
            graveyard: RefCell::new(Vec::new()),
        })
    }

    pub fn fd(&self) -> Fd {
        Fd(self.conn.stream().as_raw_fd())
    }

    /// A mapped top-level window of `width` x `height` named `title`; the plugin embeds in it.
    pub fn create_window(&self, title: &str, width: u32, height: u32) -> Result<u32, String> {
        let screen = &self.conn.setup().roots[self.screen];
        let window = self.conn.generate_id().map_err(|e| e.to_string())?;
        let (width, height) = (
            width.clamp(1, u16::MAX as u32),
            height.clamp(1, u16::MAX as u32),
        );
        self.conn
            .create_window(
                screen.root_depth,
                window,
                screen.root,
                0,
                0,
                width as u16,
                height as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new()
                    .background_pixel(screen.black_pixel)
                    .event_mask(EventMask::STRUCTURE_NOTIFY),
            )
            .map_err(|e| e.to_string())?;
        self.conn
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        self.conn
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"canalis-chain\0Canalis\0",
            )
            .map_err(|e| e.to_string())?;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                self.wm_protocols,
                AtomEnum::ATOM,
                &[self.wm_delete_window],
            )
            .map_err(|e| e.to_string())?;
        self.conn.map_window(window).map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())?;
        Ok(window)
    }

    pub fn resize(&self, window: u32, width: u32, height: u32) {
        let _ = self.conn.configure_window(
            window,
            &ConfigureWindowAux::new()
                .width(width.max(1))
                .height(height.max(1)),
        );
        let _ = self.conn.flush();
    }

    /// Hides the window now and destroys it after `GRACE`, from `poll_events`.
    pub fn close_window(&self, window: u32) {
        let _ = self.conn.unmap_window(window);
        let _ = self.conn.flush();
        self.graveyard.borrow_mut().push((Instant::now(), window));
    }

    fn bury(&self) {
        let mut graveyard = self.graveyard.borrow_mut();
        let due: Vec<u32> = graveyard
            .iter()
            .filter(|(at, _)| at.elapsed() >= GRACE)
            .map(|(_, w)| *w)
            .collect();
        if due.is_empty() {
            return;
        }
        graveyard.retain(|(at, _)| at.elapsed() < GRACE);
        for window in due {
            let _ = self.conn.destroy_window(window);
        }
        let _ = self.conn.flush();
    }

    /// Everything the server sent since the last call.
    pub fn poll_events(&self) -> Vec<WindowEvent> {
        self.bury();
        let mut events = Vec::new();
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::ClientMessage(ClientMessageEvent {
                    window,
                    type_,
                    data,
                    format: 32,
                    ..
                }) if type_ == self.wm_protocols
                    && data.as_data32()[0] == self.wm_delete_window =>
                {
                    events.push(WindowEvent::Closed(window));
                }
                Event::ConfigureNotify(e) => events.push(WindowEvent::Resized {
                    window: e.window,
                    width: u32::from(e.width),
                    height: u32::from(e.height),
                }),
                _ => {}
            }
        }
        events
    }
}
