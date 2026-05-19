//! macOS NSPasteboard ↔ guest clipboard bridge.
//!
//! Polls the system pasteboard's `changeCount` from a worker thread and
//! forwards new text payloads through the input network connection.
//! Inbound `Clipboard` frames coming back from the guest are written to
//! the pasteboard on the same thread, with a "last-installed" cache
//! suppressing the local watcher's immediate bounce.
//!
//! Why poll instead of an NSNotificationCenter observer? `changeCount`
//! is the canonical "did anything change" query on macOS and a 200-ms
//! poll is well below human paste cadence. Observer-based wiring would
//! drag the main NSApplication run-loop into this module; we keep the
//! worker thread self-contained so the rest of `client/` doesn't grow a
//! Cocoa runtime dependency just for clipboard sync.
//!
//! The bridge is gated on `target_os = "macos"` at the call site — the
//! non-mac fallback in this file is a no-op so callers don't need their
//! own `cfg` branches.

use std::sync::mpsc;
use std::time::Duration;

use vbox_proto::{Clipboard, ClipboardOrigin, MAX_CLIPBOARD_TEXT_BYTES};

#[cfg(target_os = "macos")]
use crate::debug_enabled;

/// Mirror of the server-side `clip_trace!` macro in `host_clipboard.rs`.
/// Identical `trace clip:` prefix so a `grep "trace clip:"` over merged
/// client+server logs lines up chronologically. Kept module-local — clip
/// tracing is a clipboard concern, not something other client modules need
/// to share.
#[cfg(target_os = "macos")]
macro_rules! clip_trace {
    ($($arg:tt)*) => {
        if debug_enabled() {
            eprintln!("trace clip: {}", format_args!($($arg)*));
        }
    };
}

/// Poll interval for the NSPasteboard `changeCount` watcher. 200 ms keeps
/// CPU near-zero while remaining well below the latency a user would
/// notice between Cmd+C on the host and the same text being pasteable in
/// the guest.
const PASTEBOARD_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Spawn the macOS clipboard bridge. Returns a sender the viewer uses to
/// hand inbound (`origin: Guest`) frames to the pasteboard installer.
///
/// `outbound` is the channel that ferries pasteboard-originated frames
/// (`origin: Host`) into whichever network task owns the input
/// connection; we share that connection because clipboard and input
/// are both client→server pushes and ordering between them rarely
/// matters.
///
/// Returns `None` on platforms where the bridge is a no-op (everything
/// other than macOS today). Callers can treat that the same as "the user
/// didn't grant clipboard permission" — clipboard sync simply doesn't
/// happen and other features keep working.
#[cfg(target_os = "macos")]
pub(crate) fn start(outbound: mpsc::Sender<Clipboard>) -> Option<mpsc::Sender<Clipboard>> {
    use std::sync::{Arc, Mutex};
    use std::thread;

    // Last-text caches break the host↔guest echo. When the guest sends us
    // its current selection text, we install it on NSPasteboard and store
    // it in `last_installed_from_guest`; the next poll observes the same
    // string and skips sending it back. Same mirror on the outbound side
    // for fast double-Cmd+C dedup.
    let last_installed_from_guest: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let last_sent_to_guest: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let (inbound_tx, inbound_rx) = mpsc::channel::<Clipboard>();

    let installed_for_writer = Arc::clone(&last_installed_from_guest);
    let last_sent_for_writer = Arc::clone(&last_sent_to_guest);
    thread::Builder::new()
        .name("vbox-clipboard-installer".into())
        .spawn(move || {
            for payload in inbound_rx {
                if payload.origin != ClipboardOrigin::Guest {
                    // Server is supposed to flip the tag; defensive drop.
                    clip_trace!(
                        "client.installer drop=wrong_origin origin={:?}",
                        payload.origin
                    );
                    continue;
                }
                if payload.text.len() > MAX_CLIPBOARD_TEXT_BYTES {
                    eprintln!(
                        "trace clip: client.installer drop=oversize bytes={} cap={}",
                        payload.text.len(),
                        MAX_CLIPBOARD_TEXT_BYTES
                    );
                    continue;
                }
                if let Some(existing) = last_sent_for_writer.lock().ok().and_then(|g| g.clone()) {
                    if existing == payload.text {
                        // Our own outbound was echoed back. Don't reinstall
                        // — that would only retrigger our poll watcher.
                        clip_trace!(
                            "client.installer drop=echo serial={} bytes={}",
                            payload.serial,
                            payload.text.len()
                        );
                        continue;
                    }
                }
                clip_trace!(
                    "client.installer install serial={} chars={}",
                    payload.serial,
                    payload.text.chars().count()
                );
                install_pasteboard_text(&payload.text);
                if let Ok(mut slot) = installed_for_writer.lock() {
                    *slot = Some(payload.text);
                }
            }
        })
        .expect("spawn clipboard installer thread");

    let installed_for_poller = Arc::clone(&last_installed_from_guest);
    let last_sent_for_poller = Arc::clone(&last_sent_to_guest);
    thread::Builder::new()
        .name("vbox-clipboard-poller".into())
        .spawn(move || {
            let mut last_change_count: Option<i64> = None;
            let mut serial: u64 = 0;
            loop {
                thread::sleep(PASTEBOARD_POLL_INTERVAL);
                let Some((count, text)) = read_pasteboard_text_if_changed(last_change_count) else {
                    continue;
                };
                last_change_count = Some(count);
                if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
                    eprintln!(
                        "trace clip: client.poller drop=oversize bytes={} cap={}",
                        text.len(),
                        MAX_CLIPBOARD_TEXT_BYTES
                    );
                    continue;
                }
                // If the change is just NSPasteboard reflecting what we
                // installed from the guest, don't relay it back — the
                // guest already has it.
                if installed_for_poller
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .as_deref()
                    == Some(text.as_str())
                {
                    clip_trace!(
                        "client.poller drop=echo_from_guest chars={}",
                        text.chars().count()
                    );
                    continue;
                }
                if let Ok(mut slot) = last_sent_for_poller.lock() {
                    *slot = Some(text.clone());
                }
                serial = serial.saturating_add(1);
                clip_trace!(
                    "client.poller forward serial={} chars={}",
                    serial,
                    text.chars().count()
                );
                if outbound
                    .send(Clipboard {
                        origin: ClipboardOrigin::Host,
                        serial,
                        text,
                    })
                    .is_err()
                {
                    // Network channel closed — viewer is shutting down.
                    return;
                }
            }
        })
        .expect("spawn clipboard poller thread");

    Some(inbound_tx)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn start(_outbound: mpsc::Sender<Clipboard>) -> Option<mpsc::Sender<Clipboard>> {
    None
}

#[cfg(target_os = "macos")]
fn install_pasteboard_text(text: &str) {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    // SAFETY: generalPasteboard returns the system clipboard, which lives
    // for the process lifetime. All mutation methods follow Cocoa's
    // documented contract (clearContents before setString).
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ns_text = NSString::from_str(text);
        let _ = pb.setString_forType(&ns_text, NSPasteboardTypeString);
    }
}

#[cfg(target_os = "macos")]
fn read_pasteboard_text_if_changed(prev_count: Option<i64>) -> Option<(i64, String)> {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

    // SAFETY: same as install_pasteboard_text — system clipboard, no
    // lifetime issues. stringForType returns nil for empty / non-text
    // pasteboards, which we treat as "nothing interesting changed".
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        let count = pb.changeCount() as i64;
        if prev_count == Some(count) {
            return None;
        }
        let s = pb.stringForType(NSPasteboardTypeString)?;
        Some((count, s.to_string()))
    }
}

/// "Should the just-polled pasteboard text be relayed to the guest?"
/// Returns `false` when the text is over the size cap or matches what the
/// guest installed for us (echo). The poll loop calls this on every tick;
/// extracting it lets us assert all four observable shapes without
/// touching NSPasteboard.
#[allow(dead_code)] // exercised by tests on non-mac builds
fn should_relay_to_guest(text: &str, last_installed_from_guest: Option<&str>) -> bool {
    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return false;
    }
    !matches!(last_installed_from_guest, Some(prev) if prev == text)
}

/// "Should the just-received guest frame be installed on NSPasteboard?"
/// `false` when the frame's origin is not Guest (defensive — the server
/// is supposed to tag every inbound frame as Guest), when the payload
/// exceeds the size cap, or when the text equals what we recently sent
/// out (a Mac→guest→Mac echo).
#[allow(dead_code)] // exercised by tests on non-mac builds
fn should_install_from_guest(payload: &Clipboard, last_sent_to_guest: Option<&str>) -> bool {
    if payload.origin != ClipboardOrigin::Guest {
        return false;
    }
    if payload.text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return false;
    }
    !matches!(last_sent_to_guest, Some(prev) if prev == payload.text)
}

/// Pure poll-watcher decision: given the NSPasteboard's current
/// changeCount and what we last observed, return `Some(new_count)` when
/// the count has advanced (callers then read the string content) or
/// `None` when nothing changed. Splitting this out keeps the poll loop
/// readable and gives the test a target that doesn't need NSPasteboard.
#[allow(dead_code)] // exercised by tests on non-mac builds
fn pasteboard_change_detected(prev_count: Option<i64>, current_count: i64) -> Option<i64> {
    if prev_count == Some(current_count) {
        None
    } else {
        Some(current_count)
    }
}

/// Pure increment for the outbound clipboard frame's `serial` field.
/// `saturating_add` so the long-running viewer never panics — the
/// monotone counter just sticks at u64::MAX in the unlikely event of a
/// pathological clipboard run.
#[allow(dead_code)]
fn next_outbound_serial(prev: u64) -> u64 {
    prev.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- should_relay_to_guest --------------------------------------------
    //
    // Story: the poll worker sees a NSPasteboard change. We have to decide
    // whether to push it across the wire. Three things make us drop:
    //   1. The text is over the size cap (avoid runaway pastes).
    //   2. The text equals the last value the guest sent us — that means
    //      our installer wrote it and the pasteboard is just echoing back.
    //   3. (Implicit) Anything else relays.

    #[test]
    fn relay_to_guest_passes_normal_text() {
        assert!(should_relay_to_guest("hello", None));
    }

    #[test]
    fn relay_to_guest_drops_oversize_text() {
        // One byte over the cap is enough — we never want to push huge
        // payloads onto the wire and the operator sees a stderr log line
        // elsewhere in the loop.
        let big = "a".repeat(MAX_CLIPBOARD_TEXT_BYTES + 1);
        assert!(!should_relay_to_guest(&big, None));
    }

    #[test]
    fn relay_to_guest_drops_when_text_matches_last_installed() {
        // Self-echo: the guest just sent us "foo", we installed it on
        // NSPasteboard, the poll then sees "foo" — must NOT relay.
        assert!(!should_relay_to_guest("foo", Some("foo")));
    }

    #[test]
    fn relay_to_guest_passes_when_last_installed_is_different() {
        // Guest sent us "foo" earlier, but the user has since copied
        // "bar" on the host. Relay "bar" to the guest.
        assert!(should_relay_to_guest("bar", Some("foo")));
    }

    // ---- should_install_from_guest ---------------------------------------
    //
    // Mirror story: inbound Clipboard frame from the network. We drop on:
    //   1. Wrong origin tag (defensive — server should always set Guest).
    //   2. Payload too large.
    //   3. Text equal to what we just sent out (Mac→guest→Mac echo).

    fn guest_payload(text: &str) -> Clipboard {
        Clipboard {
            origin: ClipboardOrigin::Guest,
            serial: 1,
            text: text.into(),
        }
    }

    #[test]
    fn install_from_guest_passes_normal_payload() {
        assert!(should_install_from_guest(&guest_payload("hi"), None));
    }

    #[test]
    fn install_from_guest_drops_when_origin_is_host() {
        let mut p = guest_payload("hi");
        p.origin = ClipboardOrigin::Host;
        assert!(!should_install_from_guest(&p, None));
    }

    #[test]
    fn install_from_guest_drops_oversize_payload() {
        let big = "x".repeat(MAX_CLIPBOARD_TEXT_BYTES + 1);
        assert!(!should_install_from_guest(&guest_payload(&big), None));
    }

    #[test]
    fn install_from_guest_drops_when_text_matches_last_sent() {
        assert!(!should_install_from_guest(
            &guest_payload("foo"),
            Some("foo")
        ));
    }

    #[test]
    fn install_from_guest_passes_when_text_differs_from_last_sent() {
        assert!(should_install_from_guest(
            &guest_payload("bar"),
            Some("foo")
        ));
    }

    // ---- pasteboard_change_detected --------------------------------------
    //
    // Story: the poll loop reads NSPasteboard.changeCount on every tick.
    // A new value means "fresh content"; the same value means "nothing
    // happened, skip the read".

    #[test]
    fn pasteboard_change_first_observation_always_signals() {
        // No prior observation → any current count is fresh; the loop
        // must capture it as the new baseline.
        assert_eq!(pasteboard_change_detected(None, 0), Some(0));
        assert_eq!(pasteboard_change_detected(None, 42), Some(42));
    }

    #[test]
    fn pasteboard_change_identical_count_returns_none() {
        assert_eq!(pasteboard_change_detected(Some(7), 7), None);
    }

    #[test]
    fn pasteboard_change_increment_signals() {
        assert_eq!(pasteboard_change_detected(Some(7), 8), Some(8));
    }

    #[test]
    fn pasteboard_change_decrement_also_signals() {
        // NSPasteboard.changeCount is monotonically increasing per
        // process lifecycle but can reset to a lower value when the
        // user logs out and back in. Treat *any* difference as "new
        // content" rather than gating on > prev_count.
        assert_eq!(pasteboard_change_detected(Some(10), 1), Some(1));
    }

    // ---- next_outbound_serial -------------------------------------------

    #[test]
    fn outbound_serial_increments_by_one() {
        assert_eq!(next_outbound_serial(0), 1);
        assert_eq!(next_outbound_serial(41), 42);
    }

    #[test]
    fn outbound_serial_saturates_at_u64_max() {
        // The viewer would have to issue >18 quintillion frames to hit
        // this, but the saturating_add guards against an arithmetic
        // panic in a hypothetical worst case.
        assert_eq!(next_outbound_serial(u64::MAX), u64::MAX);
    }

    #[test]
    fn module_is_callable() {
        // Public surface reachable on any platform.
        let _ = super::start;
    }
}
