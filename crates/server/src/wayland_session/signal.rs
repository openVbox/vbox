//! SIGUSR1 → "operator wants a window dump" bridge for `./vbox windows`.
//!
//! The compositor main loop polls [`take_window_dump_request`] every
//! tick; when it returns true, the loop calls
//! [`super::window_debug::dump_windows`] and clears the flag for the
//! next signal. Using a `signal()`-driven atomic flag avoids pulling in
//! signal-hook just for one toggle and keeps the dependency footprint of
//! the guest-only `vbox-server` crate identical to before
//! (smithay + libc + clap + std).
//!
//! Safety: the installed handler only does an atomic store, which is the
//! only operation POSIX guarantees is async-signal-safe for our use
//! case. The polling side reads with `Acquire` so the dump always sees
//! the request that the signal raised.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGUSR1 handler; cleared by the compositor main loop after
/// it has emitted one dump. `Acquire`/`Release` ordering keeps the
/// signal-side write visible to the polling thread without locking.
static DUMP_REQUEST: AtomicBool = AtomicBool::new(false);

/// `true` once [`install_handler`] has succeeded so we don't keep
/// re-installing on every compositor restart in the same process. The
/// install itself is idempotent but skipping the redundant `signal()`
/// call keeps the boot path quieter.
static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the SIGUSR1 handler if it hasn't been installed yet. Safe to
/// call multiple times — the second call is a cheap atomic load and a
/// noop.
pub(crate) fn install_handler() {
    if HANDLER_INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    // SAFETY: `libc::signal` is FFI to a POSIX primitive. Installing a
    // handler that only flips an AtomicBool is async-signal-safe under
    // POSIX (`atomic_store` on a lock-free atomic is signal-safe in
    // practice on the targets we ship — aarch64-linux). No allocation,
    // no syscall, no smithay state touched from inside the handler.
    // The `*const ()` indirection silences the
    // `function_casts_as_integer` lint (rustc 1.83+) which would
    // otherwise warn on a direct `fn → integer` cast.
    unsafe {
        libc::signal(
            libc::SIGUSR1,
            dump_handler as *const () as libc::sighandler_t,
        );
    }
}

extern "C" fn dump_handler(_signo: libc::c_int) {
    // Single atomic store — no locks, no allocation. The compositor
    // loop's next iteration will pick it up.
    DUMP_REQUEST.store(true, Ordering::Release);
}

/// Compositor-loop side: returns true exactly once per signal, then
/// resets to false so a subsequent SIGUSR1 can trigger another dump.
pub(crate) fn take_window_dump_request() -> bool {
    DUMP_REQUEST.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_returns_false_initially() {
        // No signal raised yet → no work to do. The flag is process-wide
        // static so other tests can flip it; tolerate either initial
        // state but assert that draining sticks at false afterwards.
        let _ = take_window_dump_request();
        assert!(!take_window_dump_request());
    }

    #[test]
    fn take_drains_a_raised_flag_exactly_once() {
        // Simulate what the signal handler does — flip the atomic — and
        // confirm the polling side picks it up on the first call and
        // returns false on the second.
        let _ = take_window_dump_request(); // drain anything left by other tests
        DUMP_REQUEST.store(true, Ordering::Release);
        assert!(take_window_dump_request());
        assert!(!take_window_dump_request());
    }

    #[test]
    fn install_handler_is_idempotent() {
        // Calling twice must not panic and must not re-register a
        // handler on every call (the in-process atomic gate handles
        // that). We can't observe the actual signal disposition from a
        // unit test, but we can confirm the call returns normally.
        install_handler();
        install_handler();
    }
}
