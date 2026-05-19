//! Bidirectional clipboard push.
//!
//! Text-only for now — the minimum viable cross-host paste that covers
//! 90% of real use (URLs, command snippets, log lines). Image / file URI
//! mime types stay out of scope until the basic text path is stable.
//!
//! Why push, not pull? A pull-on-paste handshake (the way Wayland's own
//! `wl_data_device` works locally) doubles the round-trips and forces both
//! sides to keep selection-source state alive across the network; for a
//! single workstation tunnel that's complexity without payoff. Each side
//! pushes once when its local clipboard changes, the peer mirrors the
//! payload onto its own local clipboard, and the next paste is purely
//! local. The trade-off: we always ship the bytes even when no one pastes,
//! which is fine for the kilobyte-sized text payloads we currently care
//! about.
//!
//! The `origin` field is the loop-breaker: when a side receives
//! `Clipboard { origin: X }`, applying it would normally re-trigger its
//! own change watcher and bounce a new `Clipboard { origin: Self }` back.
//! Receivers compare incoming text against the last value they themselves
//! installed under `origin == peer` and skip the bounce; see
//! `clipboard::echoes_remote_install` in client/server.

use serde::{Deserialize, Serialize};

/// Which side observed the clipboard change. The peer uses this purely to
/// route the apply path (server → guest selection, client → NSPasteboard);
/// it does NOT decide trust — both sides treat incoming text as untrusted
/// data and limit installs to `MAX_CLIPBOARD_TEXT_BYTES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardOrigin {
    /// The macOS host's NSPasteboard changed; guest should mirror.
    Host,
    /// A guest app set a Wayland selection; host should mirror to NSPasteboard.
    Guest,
}

/// Wire envelope for a single clipboard sync.
///
/// `serial` is a monotonic counter scoped per sender — receivers ignore
/// out-of-order or duplicate serials so a momentary disconnect followed by
/// reconnect can't replay a stale payload. The sender resets to 0 on each
/// connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Clipboard {
    pub origin: ClipboardOrigin,
    pub serial: u64,
    pub text: String,
}

/// Cap on a single clipboard payload. Large enough for a copied source
/// file or a few pages of log output (the realistic upper bound for a
/// "text" clipboard), small enough to keep an accidental megabyte-blob
/// paste off the wire. Receivers must also enforce this — never trust the
/// sender's framing.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 256 * 1024;

/// `text/plain;charset=utf-8` is the canonical Wayland mime type for UTF-8
/// text; `text/plain` and `UTF8_STRING` are commonly-accepted aliases and
/// `STRING` covers a few older X11-era apps that still run under Wayland's
/// xwayland bridge. Order matters: the server offers these in this order
/// so well-behaved clients pick the utf-8-aware one first.
pub const TEXT_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
];

/// Whether `mime` should be treated as plain-text. Case-insensitive on the
/// `text/plain` family because GTK and Qt both emit slightly different
/// casings, and exact-match on the X11-era aliases.
#[must_use]
pub fn is_text_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    if lower == "text/plain" || lower.starts_with("text/plain;") {
        return true;
    }
    matches!(mime, "UTF8_STRING" | "STRING")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mime_recognises_canonical_variants() {
        for mime in TEXT_MIME_TYPES {
            assert!(is_text_mime(mime), "{mime} should be recognised");
        }
        assert!(is_text_mime("TEXT/PLAIN"));
        assert!(is_text_mime("text/plain;charset=UTF-8"));
    }

    #[test]
    fn text_mime_rejects_non_text() {
        assert!(!is_text_mime("image/png"));
        assert!(!is_text_mime("application/octet-stream"));
        assert!(!is_text_mime(""));
    }
}
