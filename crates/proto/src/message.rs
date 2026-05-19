//! The top-level wire `Message` enum that ties every variant to its [`Kind`].
//!
//! The framing layer ([`crate::framing`]) tags each frame with the discriminant
//! so peers can reject malformed kinds before deserialising the payload.

use serde::{Deserialize, Serialize};

use crate::clipboard::Clipboard;
use crate::data_plane::{DataPlaneAuth, DataPlaneChannel};
use crate::framing::Kind;
use crate::handshake::{Goodbye, Hello, Ping, Pong, ProtoError, ViewRequest, Welcome};
use crate::input::InputEvent;
use crate::rpc::{RpcRequest, RpcResponse};
use crate::volume::VolumeChange;
use crate::window::{FrameTile, WindowEvent};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Message {
    Hello(Hello),
    Welcome(Welcome),
    Goodbye(Goodbye),
    Ping(Ping),
    Pong(Pong),
    Error(ProtoError),
    DataPlaneAuth(DataPlaneAuth),
    DataPlaneChannel(DataPlaneChannel),
    ViewRequest(ViewRequest),
    WindowEvent(WindowEvent),
    FrameTile(FrameTile),
    InputEvent(InputEvent),
    RpcRequest(RpcRequest),
    RpcResponse(RpcResponse),
    Clipboard(Clipboard),
    VolumeChange(VolumeChange),
}

impl Message {
    /// Wire-format discriminant for this message; mirrors the variant.
    #[must_use]
    pub fn kind(&self) -> Kind {
        match self {
            Self::Hello(_) => Kind::Hello,
            Self::Welcome(_) => Kind::Welcome,
            Self::Goodbye(_) => Kind::Goodbye,
            Self::ViewRequest(_) => Kind::ViewRequest,
            Self::Ping(_) => Kind::Ping,
            Self::Pong(_) => Kind::Pong,
            Self::Error(_) => Kind::Error,
            Self::DataPlaneAuth(_) => Kind::DataPlaneAuth,
            Self::DataPlaneChannel(_) => Kind::DataPlaneChannel,
            Self::WindowEvent(_) => Kind::WindowEvent,
            Self::FrameTile(_) => Kind::FrameTile,
            Self::InputEvent(_) => Kind::InputEvent,
            Self::RpcRequest(_) => Kind::RpcRequest,
            Self::RpcResponse(_) => Kind::RpcResponse,
            Self::Clipboard(_) => Kind::Clipboard,
            Self::VolumeChange(_) => Kind::VolumeChange,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{Clipboard, ClipboardOrigin};
    use crate::data_plane::{DataPlaneChannelPurpose, TransportCapabilities};
    use crate::rpc::{RpcMethod, RpcOk, RpcResult};
    use crate::window::{PixelEncoding, WindowGeometry};

    #[test]
    fn every_message_variant_reports_its_wire_kind() {
        let cases = vec![
            Message::Hello(crate::handshake::Hello {
                protocol_version: 1,
                client_name: "client".into(),
            }),
            Message::Welcome(crate::handshake::Welcome {
                protocol_version: 1,
                server_name: "server".into(),
                session_id: 7,
            }),
            Message::Goodbye(crate::handshake::Goodbye {
                reason: "done".into(),
            }),
            Message::ViewRequest(crate::handshake::ViewRequest {
                socket_name: "sock".into(),
                width: 1,
                height: 1,
            }),
            Message::Ping(crate::handshake::Ping {
                seq: 1,
                stamp_ns: 2,
            }),
            Message::Pong(crate::handshake::Pong {
                seq: 1,
                stamp_ns: 2,
            }),
            Message::Error(crate::handshake::ProtoError {
                code: 400,
                message: "bad".into(),
            }),
            Message::DataPlaneAuth(DataPlaneAuth {
                token: "token".into(),
            }),
            Message::DataPlaneChannel(DataPlaneChannel {
                channel_id: 9,
                purpose: DataPlaneChannelPurpose::WindowReliable,
            }),
            Message::WindowEvent(crate::window::WindowEvent::Configured {
                id: 1,
                geom: WindowGeometry {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                },
            }),
            Message::FrameTile(crate::window::FrameTile {
                id: 1,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                stride: 4,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![0; 4],
            }),
            Message::InputEvent(crate::input::InputEvent::Close { id: 1 }),
            Message::RpcRequest(crate::rpc::RpcRequest {
                id: 1,
                method: RpcMethod::Status,
            }),
            Message::RpcResponse(crate::rpc::RpcResponse {
                id: 1,
                result: RpcResult::Ok(RpcOk::Authenticated),
            }),
            Message::Clipboard(Clipboard {
                origin: ClipboardOrigin::Host,
                serial: 1,
                text: "hello".into(),
            }),
            Message::VolumeChange(crate::volume::VolumeChange {
                level: 0.5,
                muted: false,
            }),
        ];

        for msg in cases {
            assert_eq!(Kind::from_u32(msg.kind() as u32), Some(msg.kind()));
        }
        assert!(TransportCapabilities::tcp().reliable_streams);
    }
}
