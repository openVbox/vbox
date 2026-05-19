//! Data-plane bootstrap metadata shared by the control plane and viewer.
//!
//! The existing TCP data plane remains the compatibility baseline. These
//! types describe optional QUIC endpoints and capabilities without forcing
//! older callers to use them.

use std::net::SocketAddr;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::window::{FrameTile, PixelEncoding};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DataPlaneMode {
    TcpOnly,
    QuicOnly,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapBundle {
    pub tcp_addr: SocketAddr,
    pub quic_addr: Option<SocketAddr>,
    pub session_token: String,
    pub quic_server_cert_sha256: Option<String>,
    pub capabilities: TransportCapabilities,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportCapabilities {
    pub reliable_streams: bool,
    pub datagrams: bool,
    pub multi_app_channels: bool,
}

impl TransportCapabilities {
    #[must_use]
    pub const fn tcp() -> Self {
        Self {
            reliable_streams: true,
            datagrams: false,
            multi_app_channels: false,
        }
    }

    #[must_use]
    pub const fn quic_phase1() -> Self {
        Self {
            reliable_streams: true,
            datagrams: true,
            multi_app_channels: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataPlaneAuth {
    pub token: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DataPlaneChannelPurpose {
    WindowReliable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DataPlaneChannel {
    pub channel_id: u64,
    pub purpose: DataPlaneChannelPurpose,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DataPlaneDatagram {
    FrameTileChunk(FrameTileChunk),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameTileHeader {
    pub id: u64,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub encoding: PixelEncoding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameTileChunk {
    pub channel_id: u64,
    pub frame_seq: u64,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub total_len: u32,
    pub header: FrameTileHeader,
    pub bytes: Vec<u8>,
}

impl From<&FrameTile> for FrameTileHeader {
    fn from(tile: &FrameTile) -> Self {
        Self {
            id: tile.id,
            x: tile.x,
            y: tile.y,
            w: tile.w,
            h: tile.h,
            stride: tile.stride,
            encoding: tile.encoding,
        }
    }
}

impl FrameTileHeader {
    #[must_use]
    pub fn into_tile(self, bytes: Vec<u8>) -> FrameTile {
        FrameTile {
            id: self.id,
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
            stride: self.stride,
            encoding: self.encoding,
            bytes,
        }
    }
}

pub fn encode_datagram(datagram: &DataPlaneDatagram) -> Result<Vec<u8>> {
    postcard::to_allocvec(datagram).context("serializing data-plane datagram")
}

pub fn decode_datagram(bytes: &[u8]) -> Result<DataPlaneDatagram> {
    postcard::from_bytes(bytes).context("deserializing data-plane datagram")
}

pub fn frame_tile_datagrams(
    tile: &FrameTile,
    channel_id: u64,
    frame_seq: u64,
    max_datagram_size: usize,
) -> Result<Vec<Vec<u8>>> {
    if max_datagram_size < 512 {
        bail!("QUIC datagram size too small: {max_datagram_size}");
    }
    let payload_limit = max_datagram_size.min(1200).saturating_sub(256).max(256);
    let chunk_count = tile.bytes.len().div_ceil(payload_limit);
    let chunk_count_u32 =
        u32::try_from(chunk_count).context("frame tile chunk count exceeds u32")?;
    let total_len_u32 = u32::try_from(tile.bytes.len()).context("frame tile too large")?;
    let header = FrameTileHeader::from(tile);
    let mut datagrams = Vec::with_capacity(chunk_count.max(1));
    if tile.bytes.is_empty() {
        let encoded = encode_datagram(&DataPlaneDatagram::FrameTileChunk(FrameTileChunk {
            channel_id,
            frame_seq,
            chunk_index: 0,
            chunk_count: 1,
            total_len: 0,
            header,
            bytes: Vec::new(),
        }))?;
        if encoded.len() > max_datagram_size {
            bail!(
                "encoded empty frame tile datagram too large: {} > {}",
                encoded.len(),
                max_datagram_size
            );
        }
        datagrams.push(encoded);
        return Ok(datagrams);
    }
    for (index, chunk) in tile.bytes.chunks(payload_limit).enumerate() {
        let chunk_index = u32::try_from(index).context("frame tile chunk index exceeds u32")?;
        let encoded = encode_datagram(&DataPlaneDatagram::FrameTileChunk(FrameTileChunk {
            channel_id,
            frame_seq,
            chunk_index,
            chunk_count: chunk_count_u32,
            total_len: total_len_u32,
            header: header.clone(),
            bytes: chunk.to_vec(),
        }))?;
        if encoded.len() > max_datagram_size {
            bail!(
                "encoded frame tile datagram too large: {} > {}",
                encoded.len(),
                max_datagram_size
            );
        }
        datagrams.push(encoded);
    }
    Ok(datagrams)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_bundle_round_trips_through_postcard() {
        let bundle = BootstrapBundle {
            tcp_addr: "127.0.0.1:5710".parse().unwrap(),
            quic_addr: Some("10.0.0.2:5710".parse().unwrap()),
            session_token: "abc123".into(),
            quic_server_cert_sha256: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            ),
            capabilities: TransportCapabilities::quic_phase1(),
        };
        let bytes = postcard::to_allocvec(&bundle).unwrap();
        let back: BootstrapBundle = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, bundle);
    }

    #[test]
    fn phase1_quic_advertises_reliable_stream_datagrams_and_channels() {
        let caps = TransportCapabilities::quic_phase1();
        assert!(caps.reliable_streams);
        assert!(caps.multi_app_channels);
        assert!(caps.datagrams);
    }

    #[test]
    fn tcp_capabilities_advertise_reliable_streams_only() {
        let caps = TransportCapabilities::tcp();
        assert!(caps.reliable_streams);
        assert!(!caps.multi_app_channels);
        assert!(!caps.datagrams);
    }

    #[test]
    fn frame_tile_header_round_trips_empty_tile() {
        let tile = FrameTile {
            id: 3,
            x: 4,
            y: 5,
            w: 6,
            h: 7,
            stride: 24,
            encoding: PixelEncoding::RawRgba,
            bytes: Vec::new(),
        };
        let datagrams = frame_tile_datagrams(&tile, 11, 12, 1200).unwrap();
        assert_eq!(datagrams.len(), 1);
        let DataPlaneDatagram::FrameTileChunk(chunk) = decode_datagram(&datagrams[0]).unwrap();
        assert_eq!(chunk.channel_id, 11);
        assert_eq!(chunk.frame_seq, 12);
        assert_eq!(chunk.chunk_index, 0);
        assert_eq!(chunk.chunk_count, 1);
        assert_eq!(chunk.total_len, 0);
        assert_eq!(chunk.header.into_tile(chunk.bytes), tile);
    }

    #[test]
    fn frame_tile_datagrams_reject_tiny_datagram_size() {
        let tile = FrameTile {
            id: 1,
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            stride: 4,
            encoding: PixelEncoding::RawRgba,
            bytes: vec![0; 4],
        };
        let err = frame_tile_datagrams(&tile, 1, 1, 511)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too small"), "err was: {err}");
    }

    #[test]
    fn frame_tile_datagrams_round_trip_chunks() {
        let tile = FrameTile {
            id: 7,
            x: 1,
            y: 2,
            w: 3,
            h: 4,
            stride: 12,
            encoding: PixelEncoding::RawRgba,
            bytes: (0..2500).map(|i| (i % 251) as u8).collect(),
        };
        let datagrams = frame_tile_datagrams(&tile, tile.id, 99, 1200).unwrap();
        assert!(datagrams.len() > 1);
        let mut chunks = Vec::new();
        for bytes in datagrams {
            match decode_datagram(&bytes).unwrap() {
                DataPlaneDatagram::FrameTileChunk(chunk) => chunks.push(chunk),
            }
        }
        chunks.sort_by_key(|chunk| chunk.chunk_index);
        let mut out = Vec::new();
        for chunk in &chunks {
            assert_eq!(chunk.channel_id, 7);
            assert_eq!(chunk.frame_seq, 99);
            out.extend_from_slice(&chunk.bytes);
        }
        assert_eq!(out, tile.bytes);
        assert_eq!(chunks[0].header.clone().into_tile(out), tile);
    }
}
