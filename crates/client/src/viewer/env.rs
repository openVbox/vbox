//! Viewer-domain constants and host-environment toggles.
//!
//! Geometry/scale/timing knobs and `VBOX_*` env probes used only inside
//! `viewer/`. Kept here (rather than in `main.rs`) so adding a new viewer
//! constant doesn't require touching the crate root.
use std::time::Duration;

pub(crate) const INITIAL_HIDDEN_WINDOW_SIZE: u32 = 1;
pub(crate) const MIN_VIEW_SCALE: f32 = 0.35;
pub(crate) const MAX_VIEW_SCALE: f32 = 3.0;
pub(crate) const VIEW_SCALE_STEP: f32 = 1.2;
pub(crate) const INITIAL_WINDOW_X: i32 = 80;
pub(crate) const INITIAL_WINDOW_Y: i32 = 80;
pub(crate) const INITIAL_WINDOW_CASCADE_PX: i32 = 28;
pub(crate) const MACOS_MENUBAR_LOGICAL_PX: f64 = 38.0;
pub(crate) const SCROLL_LINE_TO_AXIS_UNITS: f64 = 15.0;
pub(crate) const TITLEBAR_DOUBLE_CLICK_DISTANCE_PX: i32 = 24;
pub(crate) const TITLEBAR_DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
// Non-macOS only: window-move release-grace for begin_requested_move. macOS
// lets AppKit drive the NSWindow drag, so the whole path is gated out.
#[cfg(not(target_os = "macos"))]
pub(crate) const REMOTE_MOVE_RELEASE_GRACE: Duration = Duration::from_millis(350);
pub(crate) const REMOTE_RESIZE_DEBOUNCE: Duration = Duration::from_millis(80);
pub(crate) const FULLSCREEN_EXIT_RESYNC_DELAY: Duration = Duration::from_millis(180);
pub(crate) const HOST_RESIZE_RESYNC_DELAY: Duration = Duration::from_millis(180);
pub(crate) const WINDOW_REPLACEMENT_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn edge_snap_enabled() -> bool {
    edge_snap_enabled_from(crate::brand::env_var("VBOX_EDGE_SNAP").as_deref())
}

fn edge_snap_enabled_from(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "TRUE" | "yes" | "on"))
}

pub(crate) fn macos_menubar_inset_logical_pt() -> f64 {
    parse_menubar_inset(crate::brand::env_var("VBOX_MENUBAR_INSET_PT").as_deref())
}

/// Parse `VBOX_MENUBAR_INSET_PT`, falling back to the default when the
/// value is missing, unparseable, or outside the sane [0, 200) range
/// (anything 200+ pt is clearly an operator typo and would push the
/// viewport off-screen). Pure helper for testability.
fn parse_menubar_inset(value: Option<&str>) -> f64 {
    value
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v >= 0.0 && *v < 200.0)
        .unwrap_or(MACOS_MENUBAR_LOGICAL_PX)
}

pub(crate) fn should_log_count(count: u64) -> bool {
    count <= 5 || count.is_multiple_of(60)
}

/// Environment variable that toggles the macOS host chrome (titlebar +
/// traffic light) on the viewer window. Kept as a single const so the
/// stderr hint in `maybe_warn_about_guest_chrome` and the probe below
/// can't drift on a rename.
pub(crate) const HOST_CHROME_ENV: &str = "VBOX_HOST_CHROME";

/// Whether the macOS viewer window should be drawn with the standard
/// titlebar (traffic light + title) — "Parallels-style chrome".
///
/// Default: enabled. Set `VBOX_HOST_CHROME=0` (or `off`/`false`/`no`) to
/// turn it off and fall back to the old borderless layout, useful for
/// apps whose own header bar clashes with a host titlebar.
pub(crate) fn host_chrome_enabled() -> bool {
    !matches!(
        crate::brand::env_var(HOST_CHROME_ENV).as_deref(),
        Some("0" | "false" | "FALSE" | "no" | "off")
    )
}

/// Apps known to ship a non-standard / heavy GTK or CSD chrome (header bar,
/// own traffic-light replacement, integrated tabs, custom decorations) or
/// broad uniform content areas that must not be trimmed as padding. Terminal
/// emulators are in this bucket: an empty terminal viewport is still real
/// content, and treating it as padding makes fullscreen render only the
/// prompt/header strip centered inside a dark matte.
///
/// We don't disable anything for them, just emit a one-time stderr hint so
/// the user knows what to expect and how to opt out.
///
/// Matching is on substrings of either the `xdg_toplevel.app_id` or the
/// initial window title, both lowercased. Order is significant only
/// because the first match wins for the hint message.
pub(crate) fn guest_app_uses_own_chrome(app_id: &str, title: &str) -> Option<&'static str> {
    let needle_app = app_id.to_ascii_lowercase().replace(['.', '_'], "-");
    let needle_title = title.to_ascii_lowercase();
    const KNOWN: &[(&str, &str)] = &[
        ("firefox", "Firefox"),
        ("chrome", "Chrome"),
        ("chromium", "Chromium"),
        ("brave", "Brave"),
        ("electron", "Electron"),
        ("code", "VS Code"),
        ("vscodium", "VSCodium"),
        ("ptyxis", "Ptyxis"),
        ("gnome.terminal", "GNOME Terminal"),
        ("gnome-terminal", "GNOME Terminal"),
        ("gnome.console", "GNOME Console"),
        ("gnome-console", "GNOME Console"),
        ("kgx", "GNOME Console"),
        ("terminal", "Terminal"),
        ("터미널", "Terminal"),
        ("@", "Terminal"),
    ];
    KNOWN
        .iter()
        .find(|(needle, _)| needle_app.contains(needle) || needle_title.contains(needle))
        .map(|(_, label)| *label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env;

    #[test]
    fn guest_app_chrome_matches_firefox_app_id() {
        assert_eq!(
            guest_app_uses_own_chrome("org.mozilla.firefox", ""),
            Some("Firefox")
        );
    }

    #[test]
    fn guest_app_chrome_matches_chromium_title_case_insensitive() {
        assert_eq!(
            guest_app_uses_own_chrome("", "CHROMIUM Settings"),
            Some("Chromium")
        );
    }

    #[test]
    fn guest_app_chrome_matches_terminal_emulators() {
        assert_eq!(
            guest_app_uses_own_chrome("org.gnome.Ptyxis", ""),
            Some("Ptyxis")
        );
        assert_eq!(
            guest_app_uses_own_chrome("", "user@fedora-linux-42:~"),
            Some("Terminal")
        );
        assert_eq!(guest_app_uses_own_chrome("", "Terminal"), Some("Terminal"));
        assert_eq!(guest_app_uses_own_chrome("", "터미널"), Some("Terminal"));
        assert_eq!(
            guest_app_uses_own_chrome("org.gnome.Terminal", ""),
            Some("GNOME Terminal")
        );
        assert_eq!(
            guest_app_uses_own_chrome("org.gnome.Console", ""),
            Some("GNOME Console")
        );
        assert_eq!(
            guest_app_uses_own_chrome("org.gnome.kgx", ""),
            Some("GNOME Console")
        );
    }

    #[test]
    fn guest_app_chrome_ignores_non_chrome_apps() {
        assert_eq!(guest_app_uses_own_chrome("org.gnome.Calculator", ""), None);
        assert_eq!(guest_app_uses_own_chrome("", "Untitled"), None);
        assert_eq!(guest_app_uses_own_chrome("", ""), None);
    }

    fn with_env<F: FnOnce()>(key: &str, val: Option<&str>, f: F) {
        let _guard = test_env::lock();
        let prev = std::env::var(key).ok();
        match val {
            Some(v) => test_env::set_var(key, v),
            None => test_env::remove_var(key),
        }
        f();
        match prev {
            Some(p) => test_env::set_var(key, p),
            None => test_env::remove_var(key),
        }
    }

    #[test]
    fn host_chrome_default_is_enabled() {
        with_env(HOST_CHROME_ENV, None, || {
            assert!(host_chrome_enabled());
        });
    }

    #[test]
    fn host_chrome_recognises_off_aliases() {
        for v in ["0", "false", "FALSE", "no", "off"] {
            with_env(HOST_CHROME_ENV, Some(v), || {
                assert!(!host_chrome_enabled(), "unexpectedly enabled for {v:?}");
            });
        }
    }

    #[test]
    fn host_chrome_unknown_values_remain_enabled() {
        // Anything we don't explicitly recognise as "off" stays on — avoids
        // accidentally disabling chrome from a typo like `VBOX_HOST_CHROME=`.
        for v in ["", "1", "true", "yes", "on", "anything"] {
            with_env(HOST_CHROME_ENV, Some(v), || {
                assert!(host_chrome_enabled(), "unexpectedly disabled for {v:?}");
            });
        }
    }

    // ---- edge_snap_enabled_from ------------------------------------------

    #[test]
    fn edge_snap_off_by_default() {
        assert!(!edge_snap_enabled_from(None));
    }

    #[test]
    fn edge_snap_recognises_documented_truthy_values() {
        for v in ["1", "true", "TRUE", "yes", "on"] {
            assert!(edge_snap_enabled_from(Some(v)), "expected on for {v:?}");
        }
    }

    #[test]
    fn edge_snap_rejects_unknown_values() {
        for v in ["", "0", "false", "no", "off", "True"] {
            assert!(!edge_snap_enabled_from(Some(v)), "expected off for {v:?}");
        }
    }

    // ---- parse_menubar_inset ----------------------------------------------
    //
    // Story: a few ops users tune the macOS menubar inset (the strip the
    // viewer should treat as "system-owned, don't draw under"). The
    // parser falls back to the default for unset/unparseable/out-of-range
    // values so a typo can't push the viewport off-screen.

    #[test]
    fn menubar_inset_uses_default_when_unset() {
        assert_eq!(parse_menubar_inset(None), MACOS_MENUBAR_LOGICAL_PX);
    }

    #[test]
    fn menubar_inset_uses_default_when_unparseable() {
        assert_eq!(
            parse_menubar_inset(Some("not-a-number")),
            MACOS_MENUBAR_LOGICAL_PX
        );
    }

    #[test]
    fn menubar_inset_accepts_valid_value() {
        assert!((parse_menubar_inset(Some("42.5")) - 42.5).abs() < f64::EPSILON);
        assert!((parse_menubar_inset(Some("0")) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn menubar_inset_rejects_negative_value() {
        // Negative inset would mean "draw above the screen edge" — never
        // useful. The fallback keeps the viewer sane.
        assert_eq!(parse_menubar_inset(Some("-1")), MACOS_MENUBAR_LOGICAL_PX);
    }

    #[test]
    fn menubar_inset_rejects_huge_value() {
        // Anything 200+ pt is clearly a typo (a real macOS menubar is
        // ~25-38 pt). Reject and fall back so the viewer doesn't shrink
        // the viewport to nothing.
        assert_eq!(parse_menubar_inset(Some("200")), MACOS_MENUBAR_LOGICAL_PX);
        assert_eq!(parse_menubar_inset(Some("9999")), MACOS_MENUBAR_LOGICAL_PX);
    }

    // ---- should_log_count -------------------------------------------------
    //
    // Story: per-frame log lines are noisy; we want one for the first
    // five frames (to confirm the pipe is up) and then one every 60
    // (one per second at 60 Hz) for ongoing visibility.

    #[test]
    fn should_log_count_emits_first_five_frames() {
        for n in 0..=5u64 {
            assert!(should_log_count(n), "frame {n} should log");
        }
    }

    #[test]
    fn should_log_count_emits_every_sixtieth_frame() {
        // 60, 120, 180 etc. are the "second" markers.
        assert!(should_log_count(60));
        assert!(should_log_count(120));
        assert!(should_log_count(1_800)); // 30 sec
    }

    #[test]
    fn should_log_count_drops_intermediate_frames() {
        for n in [6u64, 7, 30, 59, 61, 119] {
            assert!(!should_log_count(n), "frame {n} should NOT log");
        }
    }
}
