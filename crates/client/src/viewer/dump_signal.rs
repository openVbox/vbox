//! SIGUSR1 → "operator wants a window dump" bridge on the **host**
//! viewer side.
//!
//! The compositor side (in `wayland_session::signal`) can poll an
//! atomic flag from its plain loop, but the host viewer is a winit
//! application — we can't service the signal from the handler thread
//! directly because winit APIs are bound to the main thread.
//!
//! Solution:
//!
//! 1. Install a process-wide SIGUSR1 handler that just flips an
//!    `AtomicBool`. (Same pattern as the server side; signal-safe.)
//! 2. Spawn one polling thread that watches the flag, and when it
//!    flips, sends a `ViewerEvent::DumpWindows` through the winit
//!    `EventLoopProxy`. winit's `user_event` then lands the dump on
//!    the right thread.
//!
//! The polling cost is one atomic load every 100 ms when idle, which
//! is negligible compared to the rest of the viewer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use super::app::ViewerEvent;

static DUMP_REQUEST: AtomicBool = AtomicBool::new(false);
static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Wire SIGUSR1 to a `ViewerEvent::DumpWindows` send through the given
/// `EventLoopProxy`. Idempotent across the process — calling twice
/// only installs once.
pub(crate) fn install(proxy: EventLoopProxy<ViewerEvent>) {
    if HANDLER_INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    // SAFETY: see wayland_session::signal — installing a handler that
    // only flips an AtomicBool is async-signal-safe on the targets we
    // ship. The polling thread is what wakes winit, not the handler.
    // The `*const ()` indirection silences the `function_casts_as_integer`
    // lint (rustc 1.83+) which would otherwise warn on a direct
    // `fn → integer` cast.
    unsafe {
        libc::signal(
            libc::SIGUSR1,
            dump_handler as *const () as libc::sighandler_t,
        );
    }
    std::thread::Builder::new()
        .name("vbox-dump-signal".into())
        .spawn(move || poll_loop(proxy))
        .expect("spawning SIGUSR1 dump bridge thread");
}

extern "C" fn dump_handler(_signo: libc::c_int) {
    DUMP_REQUEST.store(true, Ordering::Release);
}

fn poll_loop(proxy: EventLoopProxy<ViewerEvent>) {
    loop {
        // Sleep first so we never burn CPU when no signal has arrived.
        // A 100 ms latency for a manual diagnostic command is well
        // within "feels instant" for the operator and a tiny fraction
        // of the viewer's normal frame budget.
        std::thread::sleep(Duration::from_millis(100));
        if DUMP_REQUEST.swap(false, Ordering::AcqRel)
            && proxy.send_event(ViewerEvent::DumpWindows).is_err()
        {
            // Event loop has shut down; nothing meaningful to do
            // here. Drop the request and exit the bridge.
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_handler_flips_the_atomic() {
        let _ = DUMP_REQUEST.swap(false, Ordering::AcqRel);
        dump_handler(0);
        assert!(DUMP_REQUEST.swap(false, Ordering::AcqRel));
    }
}
