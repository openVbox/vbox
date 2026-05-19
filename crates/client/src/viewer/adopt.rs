//! Host-window adoption policy.
//!
//! The viewer creates a winit window at a placeholder 1x1 and lets the guest
//! tell it the "right" size via two paths:
//!
//! 1. `RemoteWindowEvent::Configured` — explicit geometry from the compositor.
//! 2. The first `FrameTile` whose content rect changes the display size.
//!
//! [`should_adopt_host_window`] decides whether either path's candidate
//! should drive a real resize; [`initial_adopt_cap`] caps the first
//! adoption so an oversized guest (monitor-sized Firefox, image viewers)
//! doesn't seize the whole screen.

/// Should the next Configured drive a host-window resize?
///
/// Adoption is one-shot: the first Configured with a real size change wins,
/// and every subsequent one leaves the user's window alone. Mirroring every
/// Configured was tried in this session and regressed badly — Firefox in
/// particular relayouts its toplevel several times during startup, and each
/// Configured triggered a host-window resize, producing a visible flicker.
pub(crate) fn should_adopt_host_window(already_adopted: bool, size_changed: bool) -> bool {
    size_changed && !already_adopted
}

/// Minimum side length for an "adoptable" content rect. Anything below this
/// is almost certainly a partial render (Firefox's tab bar before the page
/// loads, a splash screen drawing only its logo, etc.) — adopting it locks
/// the host window into the partial size and the user sees a wide-and-short
/// or tall-and-narrow letterbox until they manually resize. The 240/160
/// minima match `MIN_VIEWER_WIDTH`/`MIN_VIEWER_HEIGHT` so a real toplevel
/// at the floor still adopts.
pub(crate) const ADOPTION_MIN_CONTENT_W: u32 = 240;
pub(crate) const ADOPTION_MIN_CONTENT_H: u32 = 160;

/// Outer bound on `width / height` ratio (and its inverse) before we
/// flag a content rect as "still drawing." Picked at 5.0 so a 16:9 video
/// player (1.78) and a banking-app sidebar (~0.4) both adopt verbatim,
/// while Firefox's initial 2000×80 tab-bar-only frame (≈25.0) is
/// correctly deferred. Tuned empirically — bump only with a regression
/// case attached.
pub(crate) const ADOPTION_MAX_ASPECT: f64 = 5.0;

/// Whether `(content_w, content_h)` is a "real" toplevel size suitable for
/// host-window adoption. False means "skip this content-rect change and
/// wait for a fuller one." The dual-gate one-shot stays exactly as
/// documented in [`should_adopt_host_window`] — this is an *additional*
/// gate on the FrameTile path that prevents partial-paint adoption.
#[must_use]
pub(crate) fn is_content_rect_adoptable(content_w: u32, content_h: u32) -> bool {
    if content_w < ADOPTION_MIN_CONTENT_W || content_h < ADOPTION_MIN_CONTENT_H {
        return false;
    }
    // Compare the long side against the short side so the bound is
    // symmetric to within fp rounding — `aspect = max / min ≥ 1.0`, and
    // exact 5:1 (`5.0`) lands at the boundary cleanly regardless of
    // whether the input is portrait or landscape.
    let w = f64::from(content_w);
    let h = f64::from(content_h);
    let aspect = if w >= h { w / h } else { h / w };
    aspect <= ADOPTION_MAX_ASPECT
}

/// Logical-pixel cap for the *first* host-window adoption. Returns Some
/// when the guest's reported frame size exceeds 85% of the monitor (apps
/// that come up monitor-sized — Firefox, Files, image viewers), pointing at
/// a saner 70% target. Returns None when the frame is already small enough
/// to adopt verbatim.
///
/// The 85% threshold and 70% target are tuned so a launch never auto-seizes
/// the screen but also never undersizes a content-rich app (Firefox at 70%
/// is roomy enough to read web pages without an immediate user-resize).
pub(crate) fn initial_adopt_cap(
    frame_logical_w: f64,
    frame_logical_h: f64,
    monitor_logical_w: f64,
    monitor_logical_h: f64,
    min_w: f64,
    min_h: f64,
) -> Option<(f64, f64)> {
    let oversized =
        frame_logical_w > monitor_logical_w * 0.85 || frame_logical_h > monitor_logical_h * 0.85;
    if !oversized {
        return None;
    }
    Some((
        (monitor_logical_w * 0.7).max(min_w),
        (monitor_logical_h * 0.7).max(min_h),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_adopt_host_window_is_one_shot() {
        // First Configured with a real size change adopts.
        assert!(should_adopt_host_window(false, true));
        // No size change → never adopts even on first call.
        assert!(!should_adopt_host_window(false, false));
        // Already adopted → every subsequent call is a no-op, regardless of
        // whether size changed. This is the gate that stops the Firefox
        // launch flicker (guest relayout fires repeat Configureds) and the
        // Split View shake the user reported in this session.
        assert!(!should_adopt_host_window(true, true));
        assert!(!should_adopt_host_window(true, false));
    }

    /// Documents the dual-gate adoption used by `viewer::app`: the
    /// Configured/Created path and the FrameTile path each get their own
    /// one-shot gate. The FrameTile path's separate gate lets the first real
    /// content-rect override an oversized Configured/Created geometry — fixes
    /// "앱 실행시 여백이 보이는" 버그 where a GTK toplevel reports a larger
    /// geom than its actually-painted content, locking the host window into
    /// the bigger size and leaving fat letterbox margins under the app.
    #[test]
    fn dual_gate_lets_frame_tile_override_oversized_configured() {
        // Independent gates per source.
        let mut configured_gate = false;
        let mut frame_tile_gate = false;

        // Created arrives with geom (W, H) → first Configured-path adoption.
        assert!(should_adopt_host_window(configured_gate, true));
        configured_gate = true;

        // Subsequent Configureds during the same launch are ignored
        // (Firefox-style guest relayout flicker is suppressed).
        assert!(!should_adopt_host_window(configured_gate, true));

        // FrameTile arrives with a *smaller* content rect: the separate gate
        // is still open, so we re-adopt the host to the actual content size.
        assert!(should_adopt_host_window(frame_tile_gate, true));
        frame_tile_gate = true;

        // Subsequent FrameTile relayouts during the same launch (Firefox
        // relayout, IME panel resize, …) are ignored.
        assert!(!should_adopt_host_window(frame_tile_gate, true));
    }

    #[test]
    fn initial_adopt_cap_returns_none_for_small_guests() {
        // Calculator-sized toplevel on a 1440×900 logical monitor: nowhere
        // near the 85% threshold, accept verbatim.
        assert_eq!(
            initial_adopt_cap(360.0, 600.0, 1440.0, 900.0, 240.0, 160.0),
            None
        );
    }

    #[test]
    fn initial_adopt_cap_caps_oversized_width() {
        // Firefox launching at monitor width on a 1440×900 monitor: the
        // width trips the 85% threshold and we cap to 70% × 70%.
        let (w, h) = initial_adopt_cap(1400.0, 700.0, 1440.0, 900.0, 240.0, 160.0)
            .expect("oversized launches must be capped");
        assert!((w - 1008.0).abs() < 0.01); // 1440 * 0.7
        assert!((h - 630.0).abs() < 0.01); // 900 * 0.7
    }

    #[test]
    fn initial_adopt_cap_caps_oversized_height() {
        // Tall guest (file manager with long file list) on the same monitor.
        let cap = initial_adopt_cap(600.0, 850.0, 1440.0, 900.0, 240.0, 160.0);
        assert!(cap.is_some());
    }

    #[test]
    fn content_rect_adoptable_accepts_typical_apps() {
        // 16:9 video, 4:3 calculator, square thumbnail, file-manager portrait.
        assert!(is_content_rect_adoptable(1920, 1080));
        assert!(is_content_rect_adoptable(800, 600));
        assert!(is_content_rect_adoptable(600, 600));
        assert!(is_content_rect_adoptable(400, 900));
        // Floor-sized toplevel (the actual MIN_VIEWER_* values) still adopts.
        assert!(is_content_rect_adoptable(240, 160));
    }

    #[test]
    fn content_rect_adoptable_rejects_partial_renders() {
        // Firefox launch: full-width buffer but only the tab bar painted.
        // This is the bug that motivated the heuristic — the user sees a
        // wide horizontal strip until they manually resize. Defer adoption.
        assert!(!is_content_rect_adoptable(2030, 80));
        // Tiny splash render before the real UI lands.
        assert!(!is_content_rect_adoptable(120, 120));
        // Pathologically tall, e.g. a single-column sidebar before the
        // content panel paints.
        assert!(!is_content_rect_adoptable(80, 2000));
    }

    #[test]
    fn content_rect_adoptable_borderline_aspect() {
        // Exactly 5:1 and 1:5 should still adopt (boundary inclusive) — but
        // both dimensions must also clear the minimum-side floor so the
        // examples are scaled up.
        assert!(is_content_rect_adoptable(1200, 240));
        assert!(is_content_rect_adoptable(240, 1200));
        // A hair over the aspect bound is rejected.
        assert!(!is_content_rect_adoptable(1201, 240));
        // Sub-floor dimensions are rejected regardless of aspect.
        assert!(!is_content_rect_adoptable(239, 159));
    }

    #[test]
    fn initial_adopt_cap_respects_minimum_size() {
        // Pathological tiny monitor: cap math would drop below the minimum
        // viewer size, so the floor takes over.
        let (w, h) = initial_adopt_cap(400.0, 400.0, 200.0, 200.0, 240.0, 160.0)
            .expect("oversized vs tiny monitor still caps");
        assert!(w >= 240.0);
        assert!(h >= 160.0);
    }
}
