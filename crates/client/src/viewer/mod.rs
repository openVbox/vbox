//! Viewer subsystem: everything between the network thread and the host
//! NSWindow / X11 toplevel.
//!
//! Split into peer modules so each concern is independently readable and
//! testable. The decomposition follows the data flow:
//!
//! - [`frame`] owns pixel storage and aspect-fit math.
//! - [`adopt`] decides when the host window should adopt the guest's geometry.
//! - [`move_resize`] holds drag/resize geometry and edge-snap math.
//! - [`input`] translates host keyboard / mouse events to Linux input codes.
//! - [`fullscreen`] mirrors guest borderless-fullscreen state onto the host
//!   window and recognises the F11 / Cmd+Shift+F shortcut.
//! - [`ime`] assembles decomposed IME text (Hangul jamo, Kana voicing
//!   marks, …) before it is forwarded to the guest.
//!
//! `pub(crate)` is the default; `main.rs` reaches into each submodule
//! directly via `crate::viewer::<sub>::*`.
pub(crate) mod adopt;
pub(crate) mod app;
// SIGUSR1 bridge is only meaningful on POSIX targets (where libc is in
// the dependency graph). Keeping the module behind the same gate as its
// only caller in `net.rs` lets rust-analyzer and rustc see one consistent
// shape across all configurations.
#[cfg(unix)]
pub(crate) mod dump_signal;
pub(crate) mod env;
pub(crate) mod frame;
pub(crate) mod fullscreen;
pub(crate) mod geometry;
pub(crate) mod ime;
pub(crate) mod input;
pub(crate) mod keyboard_shortcuts;
pub(crate) mod move_resize;
pub(crate) mod window_debug;
