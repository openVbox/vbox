//! Host → guest input translation.
//!
//! `winit` reports keyboard / mouse events in its own abstractions; the guest
//! Wayland compositor expects Linux input-event-codes integers (the same set
//! `/usr/include/linux/input-event-codes.h` defines). The mapping lives here
//! as pure functions so it can be exercised exhaustively from unit tests
//! without spawning a window.
//!
//! Each function takes plain winit primitives and returns either the target
//! keycode/code as an `Option<u32>` or a `bool` modifier predicate. None of
//! it touches `ViewerWindow` or any io.
use winit::event::MouseButton;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

pub(crate) const KEY_BACKSPACE: u32 = 14;
pub(crate) const KEY_LEFTCTRL: u32 = 29;
pub(crate) const KEY_LEFTSHIFT: u32 = 42;
pub(crate) const KEY_RIGHTSHIFT: u32 = 54;
pub(crate) const KEY_LEFTALT: u32 = 56;
pub(crate) const KEY_RIGHTCTRL: u32 = 97;
pub(crate) const KEY_RIGHTALT: u32 = 100;

// Option<u32> in case winit adds a new variant we don't yet know how to map;
// callers already use `if let Some(...)`.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn mouse_button_code(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
        MouseButton::Back => 0x116,
        MouseButton::Forward => 0x115,
        MouseButton::Other(n) => 0x110 + u32::from(n),
    })
}

pub(crate) fn guest_modifier_keycode(physical: PhysicalKey, logical: &Key) -> Option<u32> {
    match physical {
        PhysicalKey::Code(KeyCode::ControlLeft) => Some(KEY_LEFTCTRL),
        PhysicalKey::Code(KeyCode::ControlRight) => Some(KEY_RIGHTCTRL),
        PhysicalKey::Code(KeyCode::ShiftLeft) => Some(KEY_LEFTSHIFT),
        PhysicalKey::Code(KeyCode::ShiftRight) => Some(KEY_RIGHTSHIFT),
        PhysicalKey::Code(KeyCode::AltLeft) => Some(KEY_LEFTALT),
        PhysicalKey::Code(KeyCode::AltRight) => Some(KEY_RIGHTALT),
        // macOS Command is the app-shortcut modifier users expect. Linux apps
        // consume those shortcuts as Ctrl, so expose Command as guest Ctrl.
        PhysicalKey::Code(KeyCode::SuperLeft | KeyCode::SuperRight) => Some(KEY_LEFTCTRL),
        _ => match logical {
            Key::Named(NamedKey::Control) => Some(KEY_LEFTCTRL),
            Key::Named(NamedKey::Shift) => Some(KEY_LEFTSHIFT),
            Key::Named(NamedKey::Alt) => Some(KEY_LEFTALT),
            Key::Named(NamedKey::Super) => Some(KEY_LEFTCTRL),
            _ => None,
        },
    }
}

pub(crate) fn shortcut_keycode(physical: PhysicalKey) -> Option<u32> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::Digit1 => 2,
        KeyCode::Digit2 => 3,
        KeyCode::Digit3 => 4,
        KeyCode::Digit4 => 5,
        KeyCode::Digit5 => 6,
        KeyCode::Digit6 => 7,
        KeyCode::Digit7 => 8,
        KeyCode::Digit8 => 9,
        KeyCode::Digit9 => 10,
        KeyCode::Digit0 => 11,
        KeyCode::Minus => 12,
        KeyCode::Equal => 13,
        KeyCode::Tab => 15,
        KeyCode::KeyQ => 16,
        KeyCode::KeyW => 17,
        KeyCode::KeyE => 18,
        KeyCode::KeyR => 19,
        KeyCode::KeyT => 20,
        KeyCode::KeyY => 21,
        KeyCode::KeyU => 22,
        KeyCode::KeyI => 23,
        KeyCode::KeyO => 24,
        KeyCode::KeyP => 25,
        KeyCode::BracketLeft => 26,
        KeyCode::BracketRight => 27,
        KeyCode::Enter | KeyCode::NumpadEnter => 28,
        KeyCode::KeyA => 30,
        KeyCode::KeyS => 31,
        KeyCode::KeyD => 32,
        KeyCode::KeyF => 33,
        KeyCode::KeyG => 34,
        KeyCode::KeyH => 35,
        KeyCode::KeyJ => 36,
        KeyCode::KeyK => 37,
        KeyCode::KeyL => 38,
        KeyCode::Semicolon => 39,
        KeyCode::Quote => 40,
        KeyCode::Backquote => 41,
        KeyCode::Backslash | KeyCode::IntlBackslash | KeyCode::IntlYen => 43,
        KeyCode::KeyZ => 44,
        KeyCode::KeyX => 45,
        KeyCode::KeyC => 46,
        KeyCode::KeyV => 47,
        KeyCode::KeyB => 48,
        KeyCode::KeyN => 49,
        KeyCode::KeyM => 50,
        KeyCode::Comma => 51,
        KeyCode::Period | KeyCode::NumpadDecimal => 52,
        KeyCode::Slash | KeyCode::NumpadDivide => 53,
        KeyCode::Space => 57,
        KeyCode::Backspace => KEY_BACKSPACE,
        KeyCode::Escape => 1,
        KeyCode::ArrowLeft => 105,
        KeyCode::ArrowRight => 106,
        KeyCode::ArrowUp => 103,
        KeyCode::ArrowDown => 108,
        KeyCode::Delete => 111,
        KeyCode::Home => 102,
        KeyCode::End => 107,
        _ => return None,
    })
}

pub(crate) fn named_keycode(key: &Key) -> Option<u32> {
    Some(match key {
        Key::Named(NamedKey::Enter) => 28,
        Key::Named(NamedKey::Tab) => 15,
        Key::Named(NamedKey::Backspace) => KEY_BACKSPACE,
        Key::Named(NamedKey::Escape) => 1,
        Key::Named(NamedKey::ArrowLeft) => 105,
        Key::Named(NamedKey::ArrowRight) => 106,
        Key::Named(NamedKey::ArrowUp) => 103,
        Key::Named(NamedKey::ArrowDown) => 108,
        Key::Named(NamedKey::Delete) => 111,
        Key::Named(NamedKey::Home) => 102,
        Key::Named(NamedKey::End) => 107,
        _ => return None,
    })
}

pub(crate) fn printable_keyboard_text(text: Option<&str>) -> Option<String> {
    let text = text?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    Some(text.to_string())
}

pub(crate) fn shortcut_modifiers_active(modifiers: ModifiersState) -> bool {
    modifiers.control_key() || modifiers.super_key()
}

pub(crate) fn keyboard_command_modifiers_active(modifiers: ModifiersState) -> bool {
    shortcut_modifiers_active(modifiers) || modifiers.alt_key()
}

/// True when the press is `Ctrl+Cmd+Space` — the macOS chord that opens the
/// Character Viewer (emoji picker). The viewer must swallow it so macOS can
/// raise the picker, instead of forwarding the chord as `Ctrl+Space` (which
/// guest GTK apps consume as an IME toggle / shortcut).
pub(crate) fn is_macos_emoji_picker_shortcut(
    physical: PhysicalKey,
    modifiers: ModifiersState,
) -> bool {
    matches!(physical, PhysicalKey::Code(KeyCode::Space))
        && modifiers.super_key()
        && modifiers.control_key()
        && !modifiers.alt_key()
        && !modifiers.shift_key()
}

pub(crate) fn host_move_modifier_active(modifiers: ModifiersState) -> bool {
    modifiers.alt_key() && !shortcut_modifiers_active(modifiers)
}

pub(crate) fn ime_cursor_range(cursor: Option<(usize, usize)>) -> (i32, i32) {
    cursor.map_or((-1, -1), |(begin, end)| {
        (usize_to_i32(begin), usize_to_i32(end))
    })
}

pub(crate) fn usize_to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_keyboard_text_allows_plain_text() {
        assert_eq!(
            printable_keyboard_text(Some("hello")).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn printable_keyboard_text_filters_empty_and_control_chars() {
        assert!(printable_keyboard_text(Some("")).is_none());
        assert!(printable_keyboard_text(Some("\r")).is_none());
        assert!(printable_keyboard_text(Some("\t")).is_none());
        assert!(printable_keyboard_text(None).is_none());
    }

    #[test]
    fn macos_emoji_picker_shortcut_matches_only_ctrl_cmd_space() {
        let chord = ModifiersState::SUPER | ModifiersState::CONTROL;
        assert!(is_macos_emoji_picker_shortcut(
            PhysicalKey::Code(KeyCode::Space),
            chord,
        ));
        // Wrong key.
        assert!(!is_macos_emoji_picker_shortcut(
            PhysicalKey::Code(KeyCode::KeyA),
            chord,
        ));
        // Missing one of the modifiers.
        assert!(!is_macos_emoji_picker_shortcut(
            PhysicalKey::Code(KeyCode::Space),
            ModifiersState::SUPER,
        ));
        assert!(!is_macos_emoji_picker_shortcut(
            PhysicalKey::Code(KeyCode::Space),
            ModifiersState::CONTROL,
        ));
        // Extra modifier present — defer to the regular shortcut path.
        assert!(!is_macos_emoji_picker_shortcut(
            PhysicalKey::Code(KeyCode::Space),
            chord | ModifiersState::SHIFT,
        ));
        assert!(!is_macos_emoji_picker_shortcut(
            PhysicalKey::Code(KeyCode::Space),
            chord | ModifiersState::ALT,
        ));
    }

    #[test]
    fn shortcut_modifiers_are_ctrl_or_super_only() {
        assert!(shortcut_modifiers_active(ModifiersState::CONTROL));
        assert!(shortcut_modifiers_active(ModifiersState::SUPER));
        assert!(!shortcut_modifiers_active(ModifiersState::SHIFT));
        assert!(!shortcut_modifiers_active(ModifiersState::ALT));
    }

    #[test]
    fn keyboard_command_modifiers_include_option_alt() {
        assert!(keyboard_command_modifiers_active(ModifiersState::CONTROL));
        assert!(keyboard_command_modifiers_active(ModifiersState::SUPER));
        assert!(keyboard_command_modifiers_active(ModifiersState::ALT));
        assert!(!keyboard_command_modifiers_active(ModifiersState::SHIFT));
    }

    #[test]
    fn host_move_modifier_is_plain_option_only() {
        assert!(host_move_modifier_active(ModifiersState::ALT));
        assert!(!host_move_modifier_active(ModifiersState::SUPER));
        assert!(!host_move_modifier_active(
            ModifiersState::ALT | ModifiersState::SUPER
        ));
        assert!(!host_move_modifier_active(
            ModifiersState::ALT | ModifiersState::CONTROL
        ));
    }

    #[test]
    fn named_keycode_covers_editing_keys() {
        assert_eq!(named_keycode(&Key::Named(NamedKey::Backspace)), Some(14));
        assert_eq!(named_keycode(&Key::Named(NamedKey::Enter)), Some(28));
        assert_eq!(named_keycode(&Key::Named(NamedKey::Tab)), Some(15));
        assert_eq!(named_keycode(&Key::Named(NamedKey::Delete)), Some(111));
        assert_eq!(named_keycode(&Key::Named(NamedKey::ArrowLeft)), Some(105));
        assert_eq!(named_keycode(&Key::Named(NamedKey::ArrowRight)), Some(106));
        assert_eq!(named_keycode(&Key::Named(NamedKey::ArrowUp)), Some(103));
        assert_eq!(named_keycode(&Key::Named(NamedKey::ArrowDown)), Some(108));
    }

    #[test]
    fn guest_modifier_keycode_maps_command_to_guest_control() {
        assert_eq!(
            guest_modifier_keycode(
                PhysicalKey::Code(KeyCode::ControlLeft),
                &Key::Named(NamedKey::Control)
            ),
            Some(KEY_LEFTCTRL)
        );
        assert_eq!(
            guest_modifier_keycode(
                PhysicalKey::Code(KeyCode::SuperLeft),
                &Key::Named(NamedKey::Super)
            ),
            Some(KEY_LEFTCTRL)
        );
        assert_eq!(
            guest_modifier_keycode(
                PhysicalKey::Code(KeyCode::ShiftRight),
                &Key::Named(NamedKey::Shift)
            ),
            Some(KEY_RIGHTSHIFT)
        );
    }

    #[test]
    fn shortcut_keycode_maps_physical_letters_for_linux_shortcuts() {
        assert_eq!(shortcut_keycode(PhysicalKey::Code(KeyCode::KeyT)), Some(20));
        assert_eq!(shortcut_keycode(PhysicalKey::Code(KeyCode::KeyW)), Some(17));
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::Digit1)),
            Some(2)
        );
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::Equal)),
            Some(13)
        );
    }

    // ---- mouse_button_code -----------------------------------------------

    #[test]
    fn mouse_button_codes_match_linux_input_event_codes() {
        // 0x110..=0x116 are BTN_LEFT/RIGHT/MIDDLE/SIDE/EXTRA in the
        // Linux kernel's <linux/input-event-codes.h>; the viewer must
        // emit them exactly so guest GTK apps see the right buttons.
        assert_eq!(mouse_button_code(MouseButton::Left), Some(0x110));
        assert_eq!(mouse_button_code(MouseButton::Right), Some(0x111));
        assert_eq!(mouse_button_code(MouseButton::Middle), Some(0x112));
        assert_eq!(mouse_button_code(MouseButton::Back), Some(0x116));
        assert_eq!(mouse_button_code(MouseButton::Forward), Some(0x115));
    }

    #[test]
    fn mouse_button_other_offsets_from_btn_left_base() {
        // MouseButton::Other(n) → 0x110 + n, so extra buttons on a
        // gaming mouse don't collide with the canonical 5 codes.
        assert_eq!(mouse_button_code(MouseButton::Other(0)), Some(0x110));
        assert_eq!(mouse_button_code(MouseButton::Other(8)), Some(0x118));
        assert_eq!(mouse_button_code(MouseButton::Other(15)), Some(0x11f));
    }

    // ---- shortcut_keycode coverage ---------------------------------------

    #[test]
    fn shortcut_keycode_returns_none_for_unmapped_keys() {
        // Random non-shortcut keys must not collide with any mapped
        // code; the caller treats None as "don't translate".
        assert_eq!(shortcut_keycode(PhysicalKey::Code(KeyCode::F11)), None);
        assert_eq!(shortcut_keycode(PhysicalKey::Code(KeyCode::PageUp)), None);
    }

    #[test]
    fn shortcut_keycode_covers_arrow_keys() {
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::ArrowLeft)),
            Some(105)
        );
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::ArrowRight)),
            Some(106)
        );
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::ArrowUp)),
            Some(103)
        );
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::ArrowDown)),
            Some(108)
        );
    }

    #[test]
    fn shortcut_keycode_treats_numpad_enter_as_enter() {
        // NumpadEnter must produce the same scan code (28) as the main
        // Enter key — Linux apps watch for KEY_ENTER and don't usually
        // care which physical key produced it.
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::NumpadEnter)),
            Some(28)
        );
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::Enter)),
            Some(28)
        );
    }

    #[test]
    fn shortcut_keycode_treats_numpad_division_as_slash() {
        assert_eq!(
            shortcut_keycode(PhysicalKey::Code(KeyCode::NumpadDivide)),
            Some(53)
        );
    }

    // ---- named_keycode coverage -----------------------------------------

    #[test]
    fn named_keycode_returns_none_for_unsupported_named_keys() {
        // F-keys are intentionally not mapped in named_keycode (they go
        // through shortcut_keycode's physical path). Confirm we don't
        // silently map them via the wrong route.
        assert_eq!(named_keycode(&Key::Named(NamedKey::F11)), None);
        assert_eq!(named_keycode(&Key::Character("a".into())), None);
    }

    // ---- ime_cursor_range / usize_to_i32 --------------------------------

    #[test]
    fn ime_cursor_range_returns_sentinels_for_no_selection() {
        assert_eq!(ime_cursor_range(None), (-1, -1));
    }

    #[test]
    fn ime_cursor_range_passes_through_explicit_offsets() {
        assert_eq!(ime_cursor_range(Some((0, 0))), (0, 0));
        assert_eq!(ime_cursor_range(Some((3, 7))), (3, 7));
    }

    #[test]
    fn usize_to_i32_passes_through_small_values() {
        assert_eq!(usize_to_i32(0), 0);
        assert_eq!(usize_to_i32(42), 42);
    }

    #[test]
    fn usize_to_i32_saturates_at_i32_max() {
        // A pathological preedit longer than 2^31 bytes (effectively
        // impossible) must not panic — saturate so the IME cursor lands
        // at the end of the i32 range.
        assert_eq!(usize_to_i32(usize::MAX), i32::MAX);
    }

    // ---- guest_modifier_keycode falls back via Key::Named ---------------

    #[test]
    fn guest_modifier_keycode_falls_back_to_named_for_unknown_physical() {
        // If winit hands us a Key::Named(Shift) but a physical we don't
        // recognise, we still want to translate it.
        assert_eq!(
            guest_modifier_keycode(
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
                &Key::Named(NamedKey::Alt)
            ),
            Some(KEY_LEFTALT)
        );
        assert_eq!(
            guest_modifier_keycode(
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
                &Key::Character("a".into())
            ),
            None,
        );
    }
}
