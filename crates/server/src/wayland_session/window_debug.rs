//! Read-only, one-line-per-window dump of guest-side compositor state for
//! the `./vbox windows` diagnostic. Triggered by SIGUSR1 (see
//! [`super::signal`]) so the operator can capture the *current* state of
//! every tracked toplevel without redeploying or attaching a debugger.
//!
//! Design:
//!
//! - Pure formatting: this module never touches I/O or wayland_server
//!   handles directly. The caller passes a borrowed `&App` and a `Write`
//!   sink (stderr in production, `Vec<u8>` in tests). That keeps the
//!   coupling thin — the dump logic only depends on what is on `App`,
//!   never on smithay protocol objects, the network channel, or
//!   `eprintln!`.
//! - Stable line prefix `window-dump:` so an operator can `grep` server.log
//!   for the slice that the most recent SIGUSR1 produced. Each line is one
//!   window so a wide terminal can scan the table without wrapping.
//! - One snapshot per call is captured into `WindowSnapshot` first; the
//!   formatter then walks the snapshot. That intermediate type lets the
//!   unit tests construct synthetic windows without spinning up a wayland
//!   display.
//!
//! Output shape (one window per line, repeated for every entry in
//! `App.windows`, sorted by id for deterministic output):
//!
//! ```text
//! window-dump: ts=… id=1 size=1280x720 mode=Normal surface=wl_surface@7 last_move=none children=0
//! window-dump: ts=… id=2 size=1920x1080 mode=Fullscreen{restore=1024x768} surface=wl_surface@13 last_move=540ms children=3
//! ```

use std::io::Write;
use std::time::Instant;

use smithay::reexports::wayland_server::Resource;

use super::{App, RemoteWindow, WindowMode};

/// Stable prefix shared by every dump line. Matched verbatim by
/// `./vbox windows` and any operator-side `grep`. Keep it short — it
/// gets repeated on every window row.
pub(crate) const DUMP_PREFIX: &str = "window-dump:";

/// One window's slice of state, pre-formatted for the dump writer. The
/// indirection exists so we can unit-test [`format_window_line`] without
/// constructing a real smithay `WlSurface`.
#[derive(Debug, Clone)]
pub(crate) struct WindowSnapshot {
    pub id: u64,
    pub size: (u32, u32),
    pub mode: WindowModeSnapshot,
    pub surface_label: String,
    pub last_move_ms: Option<u128>,
    pub child_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WindowModeSnapshot {
    Normal,
    Maximized { restore: (u32, u32) },
    Fullscreen { restore: (u32, u32) },
}

impl From<WindowMode> for WindowModeSnapshot {
    fn from(value: WindowMode) -> Self {
        match value {
            WindowMode::Normal => Self::Normal,
            WindowMode::Maximized { restore_size } => Self::Maximized {
                restore: restore_size,
            },
            WindowMode::Fullscreen { restore_size } => Self::Fullscreen {
                restore: restore_size,
            },
        }
    }
}

/// Walk every entry in `App.windows` (sorted by id) and emit one
/// `window-dump:` line per window into `writer`. Returns the number of
/// rows written so the caller can log a one-line summary header if it
/// wants.
pub(crate) fn dump_windows(app: &App, writer: &mut dyn Write) -> std::io::Result<usize> {
    let snapshots = collect_snapshots(app);
    let ts_ms = app.start.elapsed().as_millis();
    writeln!(
        writer,
        "{DUMP_PREFIX} ts={ts_ms}ms header windows={} active={:?} output={}x{}",
        snapshots.len(),
        app.active_window_id,
        app.output_size.0,
        app.output_size.1,
    )?;
    for snap in &snapshots {
        writeln!(writer, "{}", format_window_line(snap, ts_ms))?;
    }
    Ok(snapshots.len())
}

fn collect_snapshots(app: &App) -> Vec<WindowSnapshot> {
    let now = Instant::now();
    let mut ids: Vec<u64> = app.windows.keys().copied().collect();
    ids.sort_unstable();
    ids.into_iter()
        .filter_map(|id| app.windows.get(&id).map(|w| snapshot_from(id, w, app, now)))
        .collect()
}

fn snapshot_from(id: u64, window: &RemoteWindow, app: &App, now: Instant) -> WindowSnapshot {
    let surface_label = format!("{:?}", window.surface.id());
    let last_move_ms = window
        .last_move_request_at
        .map(|at| now.saturating_duration_since(at).as_millis());
    let child_count = app
        .child_surface_window
        .values()
        .filter(|owner| **owner == id)
        .count();
    WindowSnapshot {
        id,
        size: window.size,
        mode: window.mode.into(),
        surface_label,
        last_move_ms,
        child_count,
    }
}

pub(crate) fn format_window_line(snap: &WindowSnapshot, ts_ms: u128) -> String {
    let mode = format_mode(snap.mode);
    let last_move = match snap.last_move_ms {
        Some(ms) => format!("{ms}ms"),
        None => "none".to_string(),
    };
    format!(
        "{DUMP_PREFIX} ts={ts_ms}ms id={id} size={w}x{h} mode={mode} surface={surface} last_move={last_move} children={children}",
        id = snap.id,
        w = snap.size.0,
        h = snap.size.1,
        surface = snap.surface_label,
        children = snap.child_count,
    )
}

fn format_mode(mode: WindowModeSnapshot) -> String {
    match mode {
        WindowModeSnapshot::Normal => "Normal".to_string(),
        WindowModeSnapshot::Maximized { restore } => {
            format!("Maximized{{restore={}x{}}}", restore.0, restore.1)
        }
        WindowModeSnapshot::Fullscreen { restore } => {
            format!("Fullscreen{{restore={}x{}}}", restore.0, restore.1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: u64, mode: WindowModeSnapshot) -> WindowSnapshot {
        WindowSnapshot {
            id,
            size: (1280, 720),
            mode,
            surface_label: "wl_surface@42".to_string(),
            last_move_ms: None,
            child_count: 0,
        }
    }

    #[test]
    fn normal_mode_renders_compactly() {
        let s = format_window_line(&snap(1, WindowModeSnapshot::Normal), 17);
        assert!(s.starts_with(DUMP_PREFIX), "line was: {s}");
        assert!(s.contains(" id=1 "));
        assert!(s.contains(" size=1280x720 "));
        assert!(s.contains(" mode=Normal "));
        assert!(s.contains(" surface=wl_surface@42 "));
        assert!(s.contains(" last_move=none "));
        assert!(s.contains(" children=0"));
    }

    #[test]
    fn fullscreen_includes_restore_size_for_post_exit_debugging() {
        // The whole point of `./vbox windows` is to spot when a viewer is
        // stuck in fullscreen geometry — that means the restore size has
        // to be visible on the same line as the mode.
        let s = format_window_line(
            &snap(
                3,
                WindowModeSnapshot::Fullscreen {
                    restore: (1024, 768),
                },
            ),
            42,
        );
        assert!(s.contains("Fullscreen{restore=1024x768}"), "line was: {s}");
    }

    #[test]
    fn maximized_includes_restore_size() {
        let s = format_window_line(
            &snap(
                2,
                WindowModeSnapshot::Maximized {
                    restore: (800, 600),
                },
            ),
            5,
        );
        assert!(s.contains("Maximized{restore=800x600}"), "line was: {s}");
    }

    #[test]
    fn last_move_millis_render_when_present() {
        // A non-None last_move means a recent xdg_toplevel.move request
        // is still inside the grace window the server uses to suppress
        // racy unfullscreen/unmaximize echoes. Surfacing it on the
        // dump line tells the operator why the next interactive toggle
        // might be ignored.
        let mut s = snap(1, WindowModeSnapshot::Normal);
        s.last_move_ms = Some(540);
        let line = format_window_line(&s, 100);
        assert!(line.contains(" last_move=540ms"), "line was: {line}");
    }

    #[test]
    fn mode_from_protocol_preserves_restore_sizes() {
        let normal: WindowModeSnapshot = WindowMode::Normal.into();
        assert!(matches!(normal, WindowModeSnapshot::Normal));

        let fs: WindowModeSnapshot = WindowMode::Fullscreen {
            restore_size: (1024, 768),
        }
        .into();
        match fs {
            WindowModeSnapshot::Fullscreen {
                restore: (1024, 768),
            } => {}
            other => panic!("expected Fullscreen{{restore=1024x768}}, got {other:?}"),
        }

        let max: WindowModeSnapshot = WindowMode::Maximized {
            restore_size: (640, 480),
        }
        .into();
        match max {
            WindowModeSnapshot::Maximized {
                restore: (640, 480),
            } => {}
            other => panic!("expected Maximized{{restore=640x480}}, got {other:?}"),
        }
    }
}
