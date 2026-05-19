//! Operator shortcuts that the viewer recognises and consumes locally
//! instead of forwarding to the guest.
//!
//! The viewer already has [`super::fullscreen::is_fullscreen_shortcut`]
//! for the F11 / Cmd+Shift+F toggle; this module mirrors that pattern
//! for the `./vbox windows` instrumentation. Keeping these recognisers
//! as small, single-purpose pure functions (no `Window` / `ViewerApp`
//! coupling) makes them trivial to unit-test against every modifier
//! combination an OEM keyboard might emit — see the tests below.

use winit::keyboard::{Key, ModifiersState};

/// Whether the given (modifier, logical key) pair is the operator's
/// "dump windows" shortcut.
///
/// Trigger: **Cmd+Option+W** on every platform. The same shortcut works
/// in the viewer regardless of host, but a non-macOS host would just
/// emit Super+Alt+W — which is fine because neither GTK nor Qt steal
/// that combo for anything common. Plain Cmd+W (close) and Cmd+Alt+I
/// (devtools) intentionally stay untouched.
pub(crate) fn is_window_dump_shortcut(modifiers: ModifiersState, logical: &Key) -> bool {
    if !modifiers.super_key() || !modifiers.alt_key() {
        return false;
    }
    // Shift / Control would push this into different territory (e.g.
    // Cmd+Ctrl+Alt is the macOS "this is a system action" stack). Stay
    // narrow on purpose so an app shortcut that uses Cmd+Ctrl+Alt+W
    // can still travel to the guest unchanged.
    if modifiers.control_key() || modifiers.shift_key() {
        return false;
    }
    matches!(logical, Key::Character(text) if text.eq_ignore_ascii_case("w"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, SmolStr};

    fn key(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    #[test]
    fn cmd_alt_w_triggers_dump() {
        let mods = ModifiersState::SUPER | ModifiersState::ALT;
        assert!(is_window_dump_shortcut(mods, &key("w")));
        assert!(is_window_dump_shortcut(mods, &key("W")));
    }

    #[test]
    fn cmd_alone_does_not_trigger() {
        // Cmd+W is "close window" everywhere — we forward it to the
        // guest unchanged so an app's own close handler can fire.
        assert!(!is_window_dump_shortcut(ModifiersState::SUPER, &key("w")));
    }

    #[test]
    fn cmd_shift_or_ctrl_blocks_the_match() {
        // Cmd+Ctrl+Alt+W stays available for the guest to consume; we
        // narrow to the bare two-modifier chord so we never steal a
        // more specific shortcut.
        let base = ModifiersState::SUPER | ModifiersState::ALT;
        assert!(!is_window_dump_shortcut(
            base | ModifiersState::SHIFT,
            &key("w")
        ));
        assert!(!is_window_dump_shortcut(
            base | ModifiersState::CONTROL,
            &key("w")
        ));
    }

    #[test]
    fn other_keys_with_the_same_modifiers_pass_through() {
        let mods = ModifiersState::SUPER | ModifiersState::ALT;
        assert!(!is_window_dump_shortcut(mods, &key("a")));
        assert!(!is_window_dump_shortcut(
            mods,
            &Key::Named(winit::keyboard::NamedKey::Enter)
        ));
    }
}
