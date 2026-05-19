//! Cross-thread clipboard bridge.
//!
//! Mirrors [`super::input_registration`] but for [`vbox_proto::Clipboard`]
//! frames. The TCP read thread in `main::handle` runs on a different OS
//! thread than the Wayland event loop, so it can't poke `App` directly;
//! instead it hands every incoming clipboard frame through this global
//! channel and the Wayland thread drains it on its next tick. The slot is
//! cleared in `Drop` so a server restart doesn't fire frames into the
//! previous run's dead receiver.
//!
//! Also exports [`read_guest_text_selection`] — the small worker-thread
//! helper that drains a `wl_data_source` pipe into a UTF-8 string. The
//! pipe read is blocking and must not happen on the Wayland thread; we
//! spawn one short-lived thread per `new_selection` callback rather than
//! invent an async runtime for two file descriptors.

use std::io::Read;
use std::os::unix::io::OwnedFd;
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;

use vbox_proto::{Clipboard, ClipboardOrigin, MAX_CLIPBOARD_TEXT_BYTES, Message};

static CLIPBOARD_SENDER: OnceLock<Mutex<Option<mpsc::Sender<Clipboard>>>> = OnceLock::new();

/// Try to hand a host-originated clipboard frame to the Wayland thread.
/// Returns `false` when there is no active Wayland session — the TCP
/// thread treats that as "drop on the floor" (same policy as input
/// events; a paste that lands during a re-handshake is rare and we'd
/// rather lose it than block).
pub fn send_clipboard(payload: Clipboard) -> bool {
    let Some(sender) = CLIPBOARD_SENDER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
    else {
        return false;
    };
    sender.send(payload).is_ok()
}

pub(crate) struct ClipboardRegistration;

impl ClipboardRegistration {
    pub(crate) fn new(sender: mpsc::Sender<Clipboard>) -> Self {
        if let Ok(mut slot) = CLIPBOARD_SENDER.get_or_init(|| Mutex::new(None)).lock() {
            *slot = Some(sender);
        }
        Self
    }
}

impl Drop for ClipboardRegistration {
    fn drop(&mut self) {
        if let Ok(mut slot) = CLIPBOARD_SENDER.get_or_init(|| Mutex::new(None)).lock() {
            *slot = None;
        }
    }
}

/// Drain `fd` (the read end of a pipe the guest's data-source writes into)
/// to a UTF-8 string, then enqueue a `Message::Clipboard` so the Wayland
/// thread relays it to the host. Runs on a dedicated short-lived OS thread
/// because the read is blocking; the thread exits as soon as the source
/// closes its end of the pipe (or `MAX_CLIPBOARD_TEXT_BYTES` is reached).
///
/// `tx` is the same outbound channel the Wayland thread uses for window
/// events and frame tiles — keeping the clipboard on it preserves a single
/// total order of frames on the wire, which matters when a paste request
/// races a window-close (Goodbye must always come last).
pub(crate) fn read_guest_text_selection(
    fd: OwnedFd,
    serial: u64,
    tx: mpsc::Sender<Message>,
    debug: bool,
) {
    thread::Builder::new()
        .name("vbox-clipboard-recv".into())
        .spawn(move || {
            let mut file = std::fs::File::from(fd);
            let mut buf = Vec::with_capacity(1024);
            // `take` enforces the cap without us having to count every read
            // — the source can still close early, which is the common case
            // for short text payloads.
            let limit = MAX_CLIPBOARD_TEXT_BYTES as u64 + 1;
            if let Err(e) = file.by_ref().take(limit).read_to_end(&mut buf) {
                // Read errors here are always operator-visible — they
                // mean the source closed the pipe abnormally, which is
                // worth logging unconditionally.
                eprintln!("trace clip: server.read_guest_selection read_err err={e:#}");
                return;
            }
            if buf.len() > MAX_CLIPBOARD_TEXT_BYTES {
                if debug {
                    eprintln!(
                        "trace clip: server.read_guest_selection drop=oversize bytes={} cap={}",
                        buf.len(),
                        MAX_CLIPBOARD_TEXT_BYTES
                    );
                }
                return;
            }
            let text = match String::from_utf8(buf) {
                Ok(t) => t,
                Err(e) => {
                    if debug {
                        eprintln!(
                            "trace clip: server.read_guest_selection drop=invalid_utf8 bytes={}",
                            e.as_bytes().len()
                        );
                    }
                    return;
                }
            };
            if debug {
                eprintln!(
                    "trace clip: server.read_guest_selection forward serial={} chars={}",
                    serial,
                    text.chars().count()
                );
            }
            let _ = tx.send(Message::Clipboard(Clipboard {
                origin: ClipboardOrigin::Guest,
                serial,
                text,
            }));
        })
        .expect("spawn clipboard reader thread");
}
