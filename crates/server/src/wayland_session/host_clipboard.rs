//! Host → guest clipboard installation and focus sync.
//!
//! The macOS host's `NSPasteboard` mirror lives in [`HostClipboard`] inside
//! [`super::App`]. When the host posts a fresh `vbox_proto::Clipboard` frame
//! we install it as the compositor-side `wl_data_device` selection so any
//! guest paste resolves through smithay's standard `wl_data_source.send`
//! path — which lands on `SelectionHandler::send_selection` over in `mod.rs`.
//!
//! ### Why focus sync lives here
//!
//! smithay's `set_data_device_selection` stores the selection but only
//! *offers* it to the client currently holding clipboard focus (see
//! `SeatData::send_selection` in smithay 0.7 — it filters devices by
//! `clipboard_selection_focus`). Smithay does *not* piggyback clipboard
//! focus on keyboard focus: `KeyboardHandle::set_focus` updates the keyboard
//! seat alone. So a session that calls `keyboard.set_focus(..)` without a
//! matching `set_data_device_focus(..)` will have selections silently
//! dropped — every guest app sees "no offer" no matter how many times the
//! host copies.
//!
//! [`App::sync_clipboard_focus_to_surface`] closes that gap: every time we
//! move keyboard focus we mirror it to the data device, which is enough for
//! `SeatData::set_clipboard_focus` to re-broadcast the already-installed
//! offer to the newly-focused client.

use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::selection::data_device::{set_data_device_focus, set_data_device_selection};
use vbox_proto::{ClipboardOrigin, MAX_CLIPBOARD_TEXT_BYTES, TEXT_MIME_TYPES};

use super::App;

/// Emit a clipboard trace line gated on `$debug`. All clipboard diagnostics
/// across the server (this file, `mod.rs::send_selection`, `clipboard.rs`,
/// `main.rs`'s `Message::Clipboard` arm) flow through this macro so the
/// `trace clip:` prefix and key=value layout stay in one place — change
/// the format here and every call site updates with it.
///
/// Kept inside this module rather than a standalone `clip_trace.rs` because
/// (a) the diagnostic policy is a clipboard concern, (b) all call sites are
/// already in `super::`, and (c) a separate file would only add coupling
/// without earning any single-responsibility win.
macro_rules! clip_trace {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            eprintln!("trace clip: {}", format_args!($($arg)*));
        }
    };
}
pub(super) use clip_trace;

/// Compositor-side mirror of whatever text the macOS host last copied. We
/// install this as a `data_device` selection whenever it changes so any
/// guest app pasting reads our copy through the standard
/// `wl_data_source.send` flow. Kept inside `App` (not the Wayland seat)
/// because Smithay's seat-attached user_data has a `Clone + Send` bound
/// that would force us to clone the whole text on every read.
#[derive(Debug, Default)]
pub(super) struct HostClipboard {
    /// `None` means "no host text installed yet". Empty string means
    /// "host deliberately cleared the clipboard" — distinct from None so
    /// guest paste sees the empty result rather than a stale prior copy.
    pub(super) text: Option<String>,
    /// Highest serial we've accepted from the host. Out-of-order frames
    /// (older serial) are dropped so a reconnect hiccup can't replay a
    /// stale paste payload.
    pub(super) last_serial: u64,
}

impl App {
    /// Install the latest host clipboard payload as the active
    /// `wl_data_device` selection on our seat. Guest apps then read it
    /// through the normal `wl_data_source.send` flow, which lands on
    /// `SelectionHandler::send_selection` in `mod.rs`.
    ///
    /// Drops stale frames (older `serial` than what we've already
    /// installed) and ignores empty payloads when nothing is installed —
    /// no point sending a fresh `data_offer` event to the guest for the
    /// transition from "nothing" to "nothing".
    pub(super) fn apply_host_clipboard(&mut self, payload: vbox_proto::Clipboard) {
        if payload.origin != ClipboardOrigin::Host {
            // Guest origin coming back from the host would be an echo
            // loop. Drop silently rather than disconnect — the client has
            // its own bounce filter; this is just defence in depth.
            return;
        }
        if payload.text.len() > MAX_CLIPBOARD_TEXT_BYTES {
            clip_trace!(
                self.debug,
                "server.apply_host_clipboard drop=oversize bytes={} cap={}",
                payload.text.len(),
                MAX_CLIPBOARD_TEXT_BYTES
            );
            return;
        }
        if payload.serial <= self.host_clipboard.last_serial && self.host_clipboard.text.is_some() {
            // Strictly-monotonic check, with one exception: the very
            // first install (`text == None`) is allowed at serial 0 so a
            // session that opens with a clipboard already populated on
            // the host still primes the guest.
            clip_trace!(
                self.debug,
                "server.apply_host_clipboard drop=stale_serial got={} have={}",
                payload.serial,
                self.host_clipboard.last_serial
            );
            return;
        }
        self.host_clipboard.last_serial = payload.serial;
        self.host_clipboard.text = Some(payload.text);
        let mime_types: Vec<String> = TEXT_MIME_TYPES.iter().map(|m| (*m).to_string()).collect();
        set_data_device_selection::<App>(&self.display_handle, &self.seat, mime_types, ());
        let chars = self
            .host_clipboard
            .text
            .as_deref()
            .map_or(0, |t| t.chars().count());
        clip_trace!(
            self.debug,
            "server.apply_host_clipboard installed serial={} chars={} active_window={}",
            self.host_clipboard.last_serial,
            chars,
            self.active_window_id.is_some()
        );
    }

    /// Synchronize the data-device clipboard focus to the client that owns
    /// `surface`. `None` clears the focus.
    ///
    /// This is the missing handshake between `KeyboardHandle::set_focus`
    /// and the selection subsystem — without it, smithay routes selection
    /// offers to a client that doesn't exist, and guest apps never see the
    /// host's clipboard. See module docs for the gory details.
    pub(super) fn sync_clipboard_focus_to_surface(&mut self, surface: Option<&WlSurface>) {
        let client = surface.and_then(Resource::client);
        clip_trace!(
            self.debug,
            "server.sync_clipboard_focus client={:?} has_selection={}",
            client.as_ref().map(|c| c.id()),
            self.host_clipboard.text.is_some()
        );
        set_data_device_focus::<App>(&self.display_handle, &self.seat, client);
    }
}
