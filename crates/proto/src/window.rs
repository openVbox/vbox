//! Window lifecycle events and frame tile payloads from server → client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WindowEvent {
    Created {
        id: u64,
        geom: WindowGeometry,
        title: String,
        /// `xdg_toplevel.app_id` if the guest set it (e.g. `org.gnome.Calculator`).
        /// Empty string when the toplevel never advertised one.
        app_id: String,
    },
    Configured {
        id: u64,
        geom: WindowGeometry,
    },
    Destroyed {
        id: u64,
    },
    TitleChanged {
        id: u64,
        title: String,
    },
    Minimized {
        id: u64,
    },
    MoveRequested {
        id: u64,
    },
    FullscreenChanged {
        id: u64,
        fullscreen: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PixelEncoding {
    RawRgba,
    ZstdRgba,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameTile {
    pub id: u64,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Bytes per row in `bytes`, after applying `encoding`.
    pub stride: u32,
    pub encoding: PixelEncoding,
    pub bytes: Vec<u8>,
}
