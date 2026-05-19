//! Connection-level handshake, liveness, and view-request messages.
//!
//! `Hello` / `Welcome` make up the handshake; `Ping` / `Pong` carry liveness;
//! `Goodbye` and `ProtoError` close or signal a per-frame failure;
//! `ViewRequest` is the viewer's request to open or resize a surface
//! (a follow-up to Welcome, so it lives here rather than in its own file).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub protocol_version: u16,
    pub client_name: String, // free-form, e.g. "vbox-client/0.1.1 (macOS 26)"
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Welcome {
    pub protocol_version: u16,
    pub server_name: String, // e.g. "vbox-server/0.1.1 (Fedora 42, Wayland-first)"
    pub session_id: u64,     // server-issued, useful for logs
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Goodbye {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ping {
    pub seq: u64,
    pub stamp_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pong {
    pub seq: u64,
    pub stamp_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtoError {
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ViewRequest {
    pub socket_name: String,
    pub width: u32,
    pub height: u32,
}
