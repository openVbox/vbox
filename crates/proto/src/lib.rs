//! vbox wire protocol.
//!
//! Layout: every frame on the wire is
//!
//! ```text
//! ┌──────────┬───────────┬────────────────────┐
//! │ u32 BE   │ u32 BE    │ N bytes            │
//! │ length N │ msg kind  │ postcard payload   │
//! └──────────┴───────────┴────────────────────┘
//! ```
//!
//! `length` covers `kind` + `payload`. The kind tag duplicates the discriminant
//! inside `Message` so decoders can fast-reject malformed frames before
//! deserialising.
//!
//! Both directions of the connection use the same frame; semantics differ by
//! variant. `Hello` / `Welcome` make up the handshake; `Ping` / `Pong` carry
//! liveness; everything else is reserved for future milestones.
//!
//! ## Module map
//!
//! - [`framing`] — magic, version, [`Kind`] tag, [`read_prelude`]/[`write_prelude`],
//!   [`read_frame`]/[`write_frame`].
//! - [`handshake`] — [`Hello`], [`Welcome`], [`Goodbye`], [`Ping`], [`Pong`], [`ProtoError`].
//! - [`view`] — [`ViewRequest`].
//! - [`window`] — [`WindowGeometry`], [`WindowEvent`], [`PixelEncoding`], [`FrameTile`].
//! - [`input`] — [`InputEvent`] and its key/button state enums.
//! - [`rpc`] — control-plane RPC (daemon): [`RpcRequest`], [`RpcResponse`], [`RpcMethod`],
//!   [`RpcResult`], [`RpcOk`], [`StatusReply`], [`InstanceSummary`].
//! - [`message`] — the top-level [`Message`] enum that ties every variant to a [`Kind`].
//!
//! All public items are re-exported at the crate root so existing call sites
//! such as `use vbox_proto::Hello` continue to compile unchanged.

pub mod clipboard;
pub mod data_plane;
pub mod framing;
pub mod handshake;
pub mod input;
pub mod message;
pub mod rpc;
pub mod volume;
pub mod window;

pub use clipboard::{
    Clipboard, ClipboardOrigin, MAX_CLIPBOARD_TEXT_BYTES, TEXT_MIME_TYPES, is_text_mime,
};
pub use data_plane::{
    BootstrapBundle, DataPlaneAuth, DataPlaneChannel, DataPlaneChannelPurpose, DataPlaneDatagram,
    DataPlaneMode, FrameTileChunk, FrameTileHeader, TransportCapabilities, decode_datagram,
    encode_datagram, frame_tile_datagrams,
};
pub use framing::{
    Kind, MAGIC, MAX_FRAME_BYTES, PROTOCOL_VERSION, decode_frame_payload, encode_frame, read_frame,
    read_prelude, write_frame, write_prelude,
};
pub use handshake::{Goodbye, Hello, Ping, Pong, ProtoError, ViewRequest, Welcome};
pub use input::{InputButtonState, InputEvent, InputKeyState};
pub use message::Message;
pub use rpc::{InstanceSummary, RpcMethod, RpcOk, RpcRequest, RpcResponse, RpcResult, StatusReply};
pub use volume::{LEVEL_EPSILON, MAX_UPDATES_PER_SEC, VolumeChange};
pub use window::{FrameTile, PixelEncoding, WindowEvent, WindowGeometry};

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn prelude_round_trip() {
        let mut buf = vec![];
        write_prelude(&mut buf).unwrap();
        let mut r = Cursor::new(buf);
        let v = read_prelude(&mut r).unwrap();
        assert_eq!(v, PROTOCOL_VERSION);
    }

    #[test]
    fn prelude_rejects_garbage_magic() {
        let mut r = Cursor::new(b"XXX1\x00\x01".to_vec());
        assert!(read_prelude(&mut r).is_err());
    }

    #[test]
    fn prelude_rejects_version_skew() {
        let mut buf = MAGIC.to_vec();
        buf.extend_from_slice(&u16::to_be_bytes(PROTOCOL_VERSION + 7));
        let mut r = Cursor::new(buf);
        assert!(read_prelude(&mut r).is_err());
    }

    #[test]
    fn frame_round_trip_hello() {
        let msg = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "tester".into(),
        });
        let mut buf = vec![];
        write_frame(&mut buf, &msg).unwrap();
        let mut r = Cursor::new(buf);
        let back = read_frame(&mut r).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn frame_round_trip_ping_pong() {
        for msg in [
            Message::Ping(Ping {
                seq: 1,
                stamp_ns: 1234,
            }),
            Message::Pong(Pong {
                seq: 1,
                stamp_ns: 5678,
            }),
        ] {
            let mut buf = vec![];
            write_frame(&mut buf, &msg).unwrap();
            let mut r = Cursor::new(buf);
            assert_eq!(read_frame(&mut r).unwrap(), msg);
        }
    }

    #[test]
    fn frame_round_trip_view_request() {
        let msg = Message::ViewRequest(ViewRequest {
            socket_name: "vbox-0".into(),
            width: 1024,
            height: 768,
        });
        let mut buf = vec![];
        write_frame(&mut buf, &msg).unwrap();
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap(), msg);
    }

    #[test]
    fn frame_round_trip_window_event() {
        for msg in [
            Message::WindowEvent(WindowEvent::Created {
                id: 1,
                geom: WindowGeometry {
                    x: 0,
                    y: 0,
                    w: 800,
                    h: 600,
                },
                title: "Calculator".into(),
                app_id: "org.gnome.Calculator".into(),
            }),
            Message::WindowEvent(WindowEvent::Configured {
                id: 1,
                geom: WindowGeometry {
                    x: 0,
                    y: 0,
                    w: 1024,
                    h: 768,
                },
            }),
            Message::WindowEvent(WindowEvent::TitleChanged {
                id: 1,
                title: "Editor".into(),
            }),
            Message::WindowEvent(WindowEvent::Minimized { id: 1 }),
            Message::WindowEvent(WindowEvent::MoveRequested { id: 1 }),
            Message::WindowEvent(WindowEvent::FullscreenChanged {
                id: 1,
                fullscreen: true,
            }),
            Message::WindowEvent(WindowEvent::FullscreenChanged {
                id: 1,
                fullscreen: false,
            }),
            Message::WindowEvent(WindowEvent::Destroyed { id: 1 }),
        ] {
            let mut buf = vec![];
            write_frame(&mut buf, &msg).unwrap();
            let mut r = Cursor::new(buf);
            assert_eq!(read_frame(&mut r).unwrap(), msg);
        }
    }

    #[test]
    fn frame_round_trip_frame_tile() {
        let msg = Message::FrameTile(FrameTile {
            id: 1,
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            stride: 8,
            encoding: PixelEncoding::RawRgba,
            bytes: vec![255, 0, 0, 255, 0, 255, 0, 255],
        });
        let mut buf = vec![];
        write_frame(&mut buf, &msg).unwrap();
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap(), msg);
    }

    #[test]
    fn frame_round_trip_input_event() {
        for msg in [
            Message::InputEvent(InputEvent::PointerMotion {
                id: 7,
                x: 12,
                y: 34,
            }),
            Message::InputEvent(InputEvent::PointerButton {
                id: 7,
                button: 0x110,
                state: InputButtonState::Pressed,
            }),
            Message::InputEvent(InputEvent::Key {
                id: 7,
                keycode: 28,
                state: InputKeyState::Released,
            }),
            Message::InputEvent(InputEvent::Text {
                id: 7,
                text: "안녕 hello".into(),
            }),
            Message::InputEvent(InputEvent::Focus {
                id: 7,
                focused: true,
            }),
            Message::InputEvent(InputEvent::Resize {
                id: 7,
                width: 800,
                height: 600,
            }),
            Message::InputEvent(InputEvent::ToggleMaximize { id: 7 }),
            Message::InputEvent(InputEvent::SetFullscreen {
                id: 7,
                fullscreen: true,
            }),
            Message::InputEvent(InputEvent::SetFullscreen {
                id: 7,
                fullscreen: false,
            }),
            Message::InputEvent(InputEvent::Close { id: 7 }),
            Message::InputEvent(InputEvent::Preedit {
                id: 7,
                text: "안".into(),
                cursor_begin: 3,
                cursor_end: 3,
            }),
        ] {
            let mut buf = vec![];
            write_frame(&mut buf, &msg).unwrap();
            let mut r = Cursor::new(buf);
            assert_eq!(read_frame(&mut r).unwrap(), msg);
        }
    }

    #[test]
    fn frame_round_trip_clipboard() {
        for msg in [
            Message::Clipboard(Clipboard {
                origin: ClipboardOrigin::Host,
                serial: 1,
                text: "hello from macOS".into(),
            }),
            Message::Clipboard(Clipboard {
                origin: ClipboardOrigin::Guest,
                serial: 2,
                text: "한글 텍스트 with emoji 🎉".into(),
            }),
            Message::Clipboard(Clipboard {
                origin: ClipboardOrigin::Host,
                serial: 3,
                text: String::new(),
            }),
        ] {
            let mut buf = vec![];
            write_frame(&mut buf, &msg).unwrap();
            let mut r = Cursor::new(buf);
            assert_eq!(read_frame(&mut r).unwrap(), msg);
        }
    }

    #[test]
    fn frame_round_trip_volume_change() {
        for msg in [
            Message::VolumeChange(VolumeChange {
                level: 0.0,
                muted: false,
            }),
            Message::VolumeChange(VolumeChange {
                level: 1.0,
                muted: false,
            }),
            Message::VolumeChange(VolumeChange {
                level: 0.42,
                muted: true,
            }),
        ] {
            let mut buf = vec![];
            write_frame(&mut buf, &msg).unwrap();
            let mut r = Cursor::new(buf);
            // f32 round-trips bit-exact through postcard, so PartialEq is fine.
            assert_eq!(read_frame(&mut r).unwrap(), msg);
        }
    }

    #[test]
    fn frame_rejects_unknown_kind() {
        // header: length=5 (kind+1B payload), kind=999, payload=[0]
        let mut buf = vec![];
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(&999u32.to_be_bytes());
        buf.push(0);
        let mut r = Cursor::new(buf);
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn frame_rejects_oversize() {
        let mut buf = vec![];
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes());
        let mut r = Cursor::new(buf);
        assert!(read_frame(&mut r).is_err());
    }
}
