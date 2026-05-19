//! Client → server input events: pointer, keyboard, IME, window control.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputKeyState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputEvent {
    PointerMotion {
        id: u64,
        x: i32,
        y: i32,
    },
    PointerButton {
        id: u64,
        button: u32,
        state: InputButtonState,
    },
    PointerScroll {
        id: u64,
        delta_x_millis: i32,
        delta_y_millis: i32,
    },
    Key {
        id: u64,
        keycode: u32,
        state: InputKeyState,
    },
    Text {
        id: u64,
        text: String,
    },
    Focus {
        id: u64,
        focused: bool,
    },
    Resize {
        id: u64,
        width: u32,
        height: u32,
    },
    ToggleMaximize {
        id: u64,
    },
    /// Set the host viewer's idea of the target fullscreen state for this
    /// window. The server makes its mode match and echoes the outcome via
    /// `WindowEvent::FullscreenChanged` — that echo is the single source of
    /// truth for the actual host-side `winit::Window::set_fullscreen` call,
    /// so client and guest can't drift if the server treats the request as a
    /// no-op (already in the requested mode, stale window id, etc.).
    SetFullscreen {
        id: u64,
        fullscreen: bool,
    },
    Close {
        id: u64,
    },
    Preedit {
        id: u64,
        text: String,
        cursor_begin: i32,
        cursor_end: i32,
    },
}
