//! Wire framing: magic, version, per-frame codec.
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

use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};

use crate::message::Message;

/// Magic bytes at the very start of every connection — `b"VBOX"`.
/// Lets us reject random TCP probes before reading anything else.
pub const MAGIC: [u8; 4] = *b"VBOX";

/// Bumped on incompatible wire changes. Server and client must agree.
pub const PROTOCOL_VERSION: u16 = 15;

/// Hard cap on a single frame, to bound a malicious peer's allocations.
/// 64 MiB is comfortable for raw 4K screen tiles; tune later.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Tag duplicated outside the postcard payload so we can fast-reject on the
/// wire. `repr(u32)` keeps ordering stable across compilers.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hello = 1,
    Welcome = 2,
    Goodbye = 3,
    ViewRequest = 20,
    Ping = 10,
    Pong = 11,
    Error = 90,
    DataPlaneAuth = 91,
    DataPlaneChannel = 92,
    WindowEvent = 100,
    FrameTile = 101,
    InputEvent = 200,
    ImeEvent = 201,
    // 300-309 control plane RPC (daemon)
    RpcRequest = 300,
    RpcResponse = 301,
    // 400-409 user data plane (clipboard, primary selection, drag-and-drop)
    Clipboard = 400,
    // 410-419 host-side control signals applied to guest user services
    // (volume, future: brightness, default-sink switch). One frame per
    // change, lossy-OK, distinct from the input data plane.
    VolumeChange = 410,
}

impl Kind {
    pub fn from_u32(n: u32) -> Option<Self> {
        Some(match n {
            1 => Self::Hello,
            2 => Self::Welcome,
            3 => Self::Goodbye,
            10 => Self::Ping,
            11 => Self::Pong,
            20 => Self::ViewRequest,
            90 => Self::Error,
            91 => Self::DataPlaneAuth,
            92 => Self::DataPlaneChannel,
            100 => Self::WindowEvent,
            101 => Self::FrameTile,
            200 => Self::InputEvent,
            201 => Self::ImeEvent,
            300 => Self::RpcRequest,
            301 => Self::RpcResponse,
            400 => Self::Clipboard,
            410 => Self::VolumeChange,
            _ => return None,
        })
    }
}

// ── connection start: magic + version ──────────────────────────────────────

/// Send the connection prelude. Each side does this once before any frame.
pub fn write_prelude<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(&MAGIC)?;
    w.write_all(&PROTOCOL_VERSION.to_be_bytes())?;
    w.flush()?;
    Ok(())
}

/// Read the peer prelude. Errors out if magic or version mismatch.
pub fn read_prelude<R: Read>(r: &mut R) -> Result<u16> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).context("reading magic")?;
    if magic != MAGIC {
        bail!("bad magic: {magic:?} (expected {MAGIC:?})");
    }
    let mut ver_buf = [0u8; 2];
    r.read_exact(&mut ver_buf).context("reading version")?;
    let ver = u16::from_be_bytes(ver_buf);
    if ver != PROTOCOL_VERSION {
        bail!("protocol version mismatch: peer={ver}, ours={PROTOCOL_VERSION}");
    }
    Ok(ver)
}

// ── per-frame codec ────────────────────────────────────────────────────────

pub fn write_frame<W: Write>(w: &mut W, msg: &Message) -> Result<()> {
    let bytes = encode_frame(msg)?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

pub fn encode_frame(msg: &Message) -> Result<Vec<u8>> {
    let payload = postcard::to_allocvec(msg).context("serializing message")?;
    let kind = msg.kind() as u32;
    let payload_len = u64::try_from(payload.len()).context("payload length overflow")?;
    let frame_len = payload_len + 4; // 4 bytes for kind
    if frame_len > u64::from(MAX_FRAME_BYTES) {
        bail!("frame too big: {frame_len} > {MAX_FRAME_BYTES}");
    }
    // frame_len <= MAX_FRAME_BYTES (u32), so u32::try_from is infallible here.
    let frame_len_u32 =
        u32::try_from(frame_len).map_err(|_| anyhow!("frame_len {frame_len} exceeds u32::MAX"))?;
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&frame_len_u32.to_be_bytes());
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

pub fn read_frame<R: Read>(r: &mut R) -> Result<Message> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).context("reading frame length")?;
    let frame_len = u32::from_be_bytes(len_buf);
    if frame_len < 4 {
        bail!("frame too small: {frame_len} bytes");
    }
    if frame_len > MAX_FRAME_BYTES {
        bail!("frame too big: {frame_len} > {MAX_FRAME_BYTES}");
    }
    let mut kind_buf = [0u8; 4];
    r.read_exact(&mut kind_buf).context("reading kind")?;
    let kind_raw = u32::from_be_bytes(kind_buf);
    let kind =
        Kind::from_u32(kind_raw).ok_or_else(|| anyhow!("unknown message kind {kind_raw}"))?;

    let payload_len = (frame_len - 4) as usize;
    let mut payload = vec![0u8; payload_len];
    r.read_exact(&mut payload).context("reading payload")?;
    decode_frame_payload(kind, &payload)
}

pub fn decode_frame_payload(kind: Kind, payload: &[u8]) -> Result<Message> {
    let msg: Message = postcard::from_bytes(payload).context("deserializing message")?;

    if msg.kind() != kind {
        bail!(
            "kind tag mismatch: header={:?}, payload={:?}",
            kind,
            msg.kind()
        );
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handshake::{Hello, Ping};
    use std::io::Cursor;

    #[test]
    fn kind_from_u32_covers_all_wire_discriminants() {
        for (raw, kind) in [
            (1, Kind::Hello),
            (2, Kind::Welcome),
            (3, Kind::Goodbye),
            (10, Kind::Ping),
            (11, Kind::Pong),
            (20, Kind::ViewRequest),
            (90, Kind::Error),
            (91, Kind::DataPlaneAuth),
            (92, Kind::DataPlaneChannel),
            (100, Kind::WindowEvent),
            (101, Kind::FrameTile),
            (200, Kind::InputEvent),
            (201, Kind::ImeEvent),
            (300, Kind::RpcRequest),
            (301, Kind::RpcResponse),
            (400, Kind::Clipboard),
            (410, Kind::VolumeChange),
        ] {
            assert_eq!(Kind::from_u32(raw), Some(kind));
        }
        assert!(Kind::from_u32(999).is_none());
    }

    #[test]
    fn write_frame_flushes_encoded_bytes() {
        let msg = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "client".into(),
        });
        let mut out = Vec::new();
        write_frame(&mut out, &msg).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(out)).unwrap(), msg);
    }

    #[test]
    fn read_frame_rejects_too_small_too_big_and_unknown_kind() {
        let mut too_small = Cursor::new(3u32.to_be_bytes().to_vec());
        assert!(
            read_frame(&mut too_small)
                .unwrap_err()
                .to_string()
                .contains("too small")
        );

        let mut too_big = Cursor::new((MAX_FRAME_BYTES + 1).to_be_bytes().to_vec());
        assert!(
            read_frame(&mut too_big)
                .unwrap_err()
                .to_string()
                .contains("too big")
        );

        let mut unknown = Vec::new();
        unknown.extend_from_slice(&4u32.to_be_bytes());
        unknown.extend_from_slice(&999u32.to_be_bytes());
        assert!(
            read_frame(&mut Cursor::new(unknown))
                .unwrap_err()
                .to_string()
                .contains("unknown message kind")
        );
    }

    #[test]
    fn decode_frame_payload_rejects_kind_mismatch() {
        let msg = Message::Ping(Ping {
            seq: 1,
            stamp_ns: 2,
        });
        let payload = postcard::to_allocvec(&msg).unwrap();
        let err = decode_frame_payload(Kind::Pong, &payload)
            .unwrap_err()
            .to_string();
        assert!(err.contains("kind tag mismatch"), "err was: {err}");
    }
}
