//! Host-side counterpart of `wayland_session::window_debug`: one-line-per
//! window dump of the viewer state when SIGUSR1 lands.
//!
//! The host viewer is a winit application — we can't service the signal
//! from inside the handler thread, so [`super::dump_signal`] forwards
//! the request through `EventLoopProxy` as a [`super::app::ViewerEvent`]
//! variant. By the time we land in [`dump_viewer_windows`] we are back
//! on the winit thread with a `&ViewerApp` borrow.
//!
//! The dump format mirrors the server side: stable `window-dump:`
//! prefix, one row per viewer window, sorted by id. Operators can
//! `grep "window-dump:" .vbox/logs/{server,client}.log` to line up
//! both sides of the bridge for the same SIGUSR1 fan-out.

use std::io::Write;

use winit::dpi::PhysicalSize;
use winit::window::Window;

use super::app::ViewerApp;

pub(crate) const DUMP_PREFIX: &str = "window-dump:";

/// Loosely-typed projection of the per-window state used by the
/// formatter. Keeping it as `pub(crate)` (rather than crossing the
/// module boundary as `&ViewerWindow`) lets unit tests construct rows
/// without instantiating a real `winit::Window` / `softbuffer`.
#[derive(Debug, Clone)]
pub(crate) struct ViewerWindowSnapshot {
    pub id: u64,
    pub inner_size: (u32, u32),
    pub frame_size: (u32, u32),
    pub last_host_fullscreen: bool,
    pub winit_fullscreen: bool,
    pub simple_fullscreen: bool,
    pub pending_programmatic_resize: Option<(u32, u32)>,
    pub pending_remote_resize: Option<(u32, u32)>,
    pub view_scale: f32,
}

/// Dump the entire viewer surface state to `writer`. Returns the number
/// of window rows written (excluding the header line).
pub(crate) fn dump_viewer_windows(
    app: &ViewerApp,
    writer: &mut dyn Write,
) -> std::io::Result<usize> {
    let snapshots = app.collect_viewer_snapshots();
    writeln!(
        writer,
        "{DUMP_PREFIX} header windows={} host_chrome={} retired={}",
        snapshots.len(),
        app.host_chrome_debug_flag(),
        app.retired_window_count(),
    )?;
    for snap in &snapshots {
        writeln!(writer, "{}", format_viewer_window_line(snap))?;
    }
    Ok(snapshots.len())
}

pub(crate) fn format_viewer_window_line(snap: &ViewerWindowSnapshot) -> String {
    let pending_prog = match snap.pending_programmatic_resize {
        Some((w, h)) => format!("{w}x{h}"),
        None => "none".to_string(),
    };
    let pending_remote = match snap.pending_remote_resize {
        Some((w, h)) => format!("{w}x{h}"),
        None => "none".to_string(),
    };
    format!(
        "{DUMP_PREFIX} id={id} inner={iw}x{ih} frame={fw}x{fh} \
         last_host_fs={lhfs} winit_fs={wfs} simple_fs={sfs} \
         pending_prog={pending_prog} pending_remote={pending_remote} scale={scale:.2}",
        id = snap.id,
        iw = snap.inner_size.0,
        ih = snap.inner_size.1,
        fw = snap.frame_size.0,
        fh = snap.frame_size.1,
        lhfs = snap.last_host_fullscreen,
        wfs = snap.winit_fullscreen,
        sfs = snap.simple_fullscreen,
        scale = snap.view_scale,
    )
}

/// Snapshot a single (`Window`, frame/state) pair into a serializable
/// struct. Pulled out of `dump_viewer_windows` so the test below can
/// construct synthetic windows without running winit.
pub(crate) fn snapshot_from_parts(
    id: u64,
    window: &Window,
    frame_size: (u32, u32),
    last_host_fullscreen: bool,
    pending_programmatic_resize: Option<PhysicalSize<u32>>,
    pending_remote_resize: Option<(u32, u32)>,
    view_scale: f32,
) -> ViewerWindowSnapshot {
    let inner = window.inner_size();
    let inner_size = (inner.width.max(1), inner.height.max(1));
    let winit_fullscreen = window.fullscreen().is_some();
    let simple_fullscreen = simple_fullscreen_of(window);
    ViewerWindowSnapshot {
        id,
        inner_size,
        frame_size,
        last_host_fullscreen,
        winit_fullscreen,
        // is_window_fullscreen also folds simple_fullscreen on macOS — but
        // we want to show both signals on the dump line so an operator
        // can tell which path is engaged (green-button vs F11 simple).
        simple_fullscreen,
        pending_programmatic_resize: pending_programmatic_resize
            .map(|s| (s.width.max(1), s.height.max(1))),
        pending_remote_resize,
        view_scale,
    }
}

#[cfg(target_os = "macos")]
fn simple_fullscreen_of(window: &Window) -> bool {
    use winit::platform::macos::WindowExtMacOS;
    window.simple_fullscreen()
}

#[cfg(not(target_os = "macos"))]
fn simple_fullscreen_of(_window: &Window) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_snap(id: u64) -> ViewerWindowSnapshot {
        ViewerWindowSnapshot {
            id,
            inner_size: (1280, 720),
            frame_size: (1280, 720),
            last_host_fullscreen: false,
            winit_fullscreen: false,
            simple_fullscreen: false,
            pending_programmatic_resize: None,
            pending_remote_resize: None,
            view_scale: 1.0,
        }
    }

    #[test]
    fn baseline_row_has_every_key_an_operator_greps_for() {
        let line = format_viewer_window_line(&base_snap(1));
        assert!(line.starts_with(DUMP_PREFIX), "line was: {line}");
        for needle in [
            " id=1 ",
            " inner=1280x720 ",
            " frame=1280x720 ",
            " last_host_fs=false ",
            " winit_fs=false ",
            " simple_fs=false ",
            " pending_prog=none ",
            " pending_remote=none ",
            " scale=1.00",
        ] {
            assert!(line.contains(needle), "missing {needle:?} in {line}");
        }
    }

    #[test]
    fn fullscreen_flags_render_independently() {
        // The point of separate winit_fs + simple_fs + last_host_fs cols
        // is to spot drift: e.g. a stuck simple_fullscreen=true with
        // last_host_fs=false would tell us the host believes it left
        // fullscreen while the NSWindow ivar still says otherwise.
        let mut s = base_snap(2);
        s.last_host_fullscreen = true;
        s.simple_fullscreen = true;
        s.winit_fullscreen = false;
        let line = format_viewer_window_line(&s);
        assert!(line.contains(" last_host_fs=true "), "line was: {line}");
        assert!(line.contains(" winit_fs=false "), "line was: {line}");
        assert!(line.contains(" simple_fs=true "), "line was: {line}");
    }

    #[test]
    fn pending_resizes_render_when_present() {
        // Both pending_programmatic_resize and pending_remote_resize feed
        // into the fullscreen-exit bug: a stale pending_programmatic
        // could swallow a real user-driven Resized, and a queued remote
        // resize that never sent could explain a viewer-server geometry
        // mismatch.
        let mut s = base_snap(3);
        s.pending_programmatic_resize = Some((1600, 900));
        s.pending_remote_resize = Some((1280, 720));
        let line = format_viewer_window_line(&s);
        assert!(line.contains(" pending_prog=1600x900 "), "line was: {line}");
        assert!(
            line.contains(" pending_remote=1280x720 "),
            "line was: {line}"
        );
    }

    #[test]
    fn distinct_inner_and_frame_sizes_render_separately() {
        // A guest viewport (frame) larger than the host display (inner)
        // is the common "viewer downscale" case — operators rely on
        // seeing both columns when debugging a blurry frame.
        let mut s = base_snap(4);
        s.inner_size = (800, 600);
        s.frame_size = (1920, 1080);
        let line = format_viewer_window_line(&s);
        assert!(line.contains(" inner=800x600 "), "line was: {line}");
        assert!(line.contains(" frame=1920x1080 "), "line was: {line}");
    }

    #[test]
    fn fractional_view_scale_rounds_to_two_decimals() {
        // operators frequently set view_scale to 1.25 or 1.5 — the
        // formatter must show those values precisely.
        let mut s = base_snap(5);
        s.view_scale = 1.25;
        let line = format_viewer_window_line(&s);
        assert!(line.contains(" scale=1.25"), "line was: {line}");
    }

    #[test]
    fn high_window_id_renders_unmodified() {
        // The viewer assigns monotonically increasing ids; eventually we
        // hit values > u32::MAX after long-running sessions with many
        // adopted/retired windows. The dump must show the full u64.
        let snap = base_snap(u64::MAX);
        let line = format_viewer_window_line(&snap);
        assert!(line.contains(&format!(" id={} ", u64::MAX)));
    }
}
