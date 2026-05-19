//! Borderless-fullscreen toggle for the host viewer window.
//!
//! The guest already knows about `xdg_toplevel::State::Fullscreen` and uses it
//! to let GTK/Wayland apps repaint without any decoration. The host viewer
//! mirrors that state onto its `winit::window::Window` so video players,
//! presentation tools, and similar can claim the whole screen on the host
//! side as well.
//!
//! On macOS this uses [`WindowExtMacOS::set_simple_fullscreen`] so the
//! viewer stays on the current Space (Parallels-style), not the default
//! winit `Fullscreen::Borderless` which would create a new Space. On other
//! platforms `Fullscreen::Borderless(None)` is the natural fit.
//!
//! Triggers (recognised by [`is_fullscreen_shortcut`]):
//! - `F11` on its own (Shift is tolerated for keyboard remap quirks; any
//!   Control/Super/Alt disqualifies). Works on every platform.
//! - `Cmd+Shift+F` on macOS. On other platforms this combination doesn't
//!   apply because the Super modifier is rare on non-Mac keyboards.
//!
//! State is read back via [`is_window_fullscreen`] (which checks
//! `simple_fullscreen` on macOS and the standard winit flag elsewhere)
//! and applied through [`apply_window_fullscreen`], which is idempotent so
//! a SetFullscreen echo for the state we're already in costs only a
//! comparison.

use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::Window;

#[cfg(not(target_os = "macos"))]
use winit::window::Fullscreen;

#[cfg(target_os = "macos")]
use winit::platform::macos::WindowExtMacOS;

pub(crate) fn is_fullscreen_shortcut(modifiers: ModifiersState, logical: &Key) -> bool {
    // F11 acts as a fullscreen toggle on its own. Shift is tolerated because
    // some host keyboards (e.g. Mac fn-row remappings) report Shift as held
    // during the F11 press; Control/Super/Alt always disqualify so app
    // shortcuts like Ctrl+F11 are not stolen.
    if matches!(logical, Key::Named(NamedKey::F11))
        && !modifiers.control_key()
        && !modifiers.super_key()
        && !modifiers.alt_key()
    {
        return true;
    }

    // Cmd+Shift+F: matches the cross-platform "fullscreen" convention used by
    // many video apps on macOS. Plain Cmd+F stays available for "find".
    if !modifiers.super_key() || !modifiers.shift_key() {
        return false;
    }
    if modifiers.control_key() || modifiers.alt_key() {
        return false;
    }
    matches!(logical, Key::Character(text) if text.eq_ignore_ascii_case("f"))
}

/// Whether the host window is currently in our borderless fullscreen mode.
/// On macOS this is `simple_fullscreen` (no Space switch); on other
/// platforms it falls back to the standard winit fullscreen flag.
///
/// macOS also covers the case where the user clicks the green window
/// button (`toggleFullScreen:`) — winit's `windowWillEnterFullScreen`
/// delegate updates the internal fullscreen ivar to
/// `Some(Fullscreen::Borderless(_))`, so checking `window.fullscreen()`
/// catches that path even though `simple_fullscreen()` stays false.
/// Without this, the Resized handler classifies the OS-driven
/// fullscreen resize as a user gesture and bounces it back to the
/// guest, producing the empty-grey/blocky window combo the user
/// reported when toggling video fullscreen + macOS fullscreen.
#[cfg(target_os = "macos")]
pub(crate) fn is_window_fullscreen(window: &Window) -> bool {
    window.simple_fullscreen() || window.fullscreen().is_some()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn is_window_fullscreen(window: &Window) -> bool {
    window.fullscreen().is_some()
}

/// Drive the host window to match `fullscreen` exactly. Idempotent so the
/// guest-side `xdg_toplevel.set_fullscreen` ↔ host `set_fullscreen` handshake
/// can fire from either direction without flicker, and so a SetFullscreen
/// echo for a state we're already in costs only a comparison.
///
/// macOS uses `simple_fullscreen` so the viewer stays on the current Space
/// (borderless windowed fullscreen — the standard "video player fullscreen"
/// behaviour). The default winit `Fullscreen::Borderless` would push the
/// window into its own macOS Space, which is not what users want for a
/// nested-compositor viewer.
#[cfg(target_os = "macos")]
pub(crate) fn apply_window_fullscreen(window: &Window, fullscreen: bool) {
    if is_window_fullscreen(window) == fullscreen {
        return;
    }
    window.set_simple_fullscreen(fullscreen);
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn apply_window_fullscreen(window: &Window, fullscreen: bool) {
    if is_window_fullscreen(window) == fullscreen {
        return;
    }
    window.set_fullscreen(fullscreen.then(|| Fullscreen::Borderless(None)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    fn char_key(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    #[test]
    fn f11_alone_triggers_fullscreen() {
        assert!(is_fullscreen_shortcut(
            ModifiersState::empty(),
            &Key::Named(NamedKey::F11),
        ));
        assert!(is_fullscreen_shortcut(
            ModifiersState::SHIFT,
            &Key::Named(NamedKey::F11),
        ));
    }

    #[test]
    fn f11_with_command_or_control_is_ignored() {
        assert!(!is_fullscreen_shortcut(
            ModifiersState::SUPER,
            &Key::Named(NamedKey::F11),
        ));
        assert!(!is_fullscreen_shortcut(
            ModifiersState::CONTROL,
            &Key::Named(NamedKey::F11),
        ));
        assert!(!is_fullscreen_shortcut(
            ModifiersState::ALT,
            &Key::Named(NamedKey::F11),
        ));
    }

    #[test]
    fn f11_with_super_and_shift_is_ignored() {
        // Mac fn-row remapping can tag Shift onto F11; that one bit is
        // tolerated. But if Super (Cmd) also rides along we treat it as
        // an app-level shortcut and refuse to steal it.
        assert!(!is_fullscreen_shortcut(
            ModifiersState::SUPER | ModifiersState::SHIFT,
            &Key::Named(NamedKey::F11),
        ));
    }

    #[test]
    fn cmd_shift_f_with_alt_or_control_is_ignored() {
        let base = ModifiersState::SUPER | ModifiersState::SHIFT;
        assert!(!is_fullscreen_shortcut(
            base | ModifiersState::CONTROL,
            &char_key("F"),
        ));
        assert!(!is_fullscreen_shortcut(
            base | ModifiersState::ALT,
            &char_key("F"),
        ));
    }

    #[test]
    fn cmd_shift_f_triggers_fullscreen() {
        let mods = ModifiersState::SUPER | ModifiersState::SHIFT;
        assert!(is_fullscreen_shortcut(mods, &char_key("F")));
        assert!(is_fullscreen_shortcut(mods, &char_key("f")));
    }

    #[test]
    fn cmd_f_without_shift_is_ignored() {
        // Plain Cmd+F is "find" in most apps; do not steal it.
        assert!(!is_fullscreen_shortcut(
            ModifiersState::SUPER,
            &char_key("f")
        ));
    }

    #[test]
    fn cmd_shift_other_key_is_ignored() {
        let mods = ModifiersState::SUPER | ModifiersState::SHIFT;
        assert!(!is_fullscreen_shortcut(mods, &char_key("g")));
        assert!(!is_fullscreen_shortcut(mods, &Key::Named(NamedKey::Enter)));
    }
}
