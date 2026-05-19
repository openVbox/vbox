use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Endpoint};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use vbox_proto::{
    DataPlaneAuth, DataPlaneDatagram, FrameTile, FrameTileChunk, FrameTileHeader, Hello, Message,
    PROTOCOL_VERSION, ViewRequest,
};
use winit::event_loop::EventLoopProxy;

use crate::data_plane::router::OutboundRx;
use crate::viewer::app::ViewerEvent;
use crate::{client_name, debug_enabled, now_ns};

pub(crate) struct QuicViewConfig {
    pub(crate) addr: SocketAddr,
    pub(crate) token: String,
    pub(crate) server_cert_sha256: Option<String>,
    pub(crate) socket_name: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) timeout: Duration,
}

pub(crate) struct QuicOneShotConfig {
    pub(crate) addr: SocketAddr,
    pub(crate) token: String,
    pub(crate) server_cert_sha256: Option<String>,
    pub(crate) timeout: Duration,
}

pub(crate) fn view_network(
    cfg: QuicViewConfig,
    outbound_rx: OutboundRx,
    proxy: EventLoopProxy<ViewerEvent>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("build QUIC client runtime")?;
    rt.block_on(view_network_async(cfg, outbound_rx, proxy))
}

pub(crate) fn send_messages(
    cfg: QuicOneShotConfig,
    messages: Vec<Message>,
    reason: &str,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("build QUIC one-shot runtime")?;
    rt.block_on(send_messages_async(cfg, messages, reason))
}

pub(crate) fn ping(cfg: QuicOneShotConfig) -> Result<Duration> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("build QUIC ping runtime")?;
    rt.block_on(ping_async(cfg))
}

async fn ping_async(cfg: QuicOneShotConfig) -> Result<Duration> {
    let timeout = cfg.timeout;
    let (endpoint, _connection, mut send, mut recv) = open_authenticated_stream(&cfg).await?;
    let started = Instant::now();
    write_frame(
        &mut send,
        &Message::Ping(vbox_proto::Ping {
            seq: 1,
            stamp_ns: now_ns(),
        }),
    )
    .await?;
    match read_frame(&mut recv).await? {
        Message::Pong(pong) if pong.seq == 1 => {
            let elapsed = started.elapsed();
            write_frame(
                &mut send,
                &Message::Goodbye(vbox_proto::Goodbye {
                    reason: "ping complete".into(),
                }),
            )
            .await?;
            send.finish()?;
            let _ = tokio::time::timeout(timeout, endpoint.wait_idle()).await;
            Ok(elapsed)
        }
        Message::Pong(pong) => bail!("pong seq mismatch: got {}, expected 1", pong.seq),
        other => bail!("unexpected QUIC ping reply: {:?}", other.kind()),
    }
}

async fn send_messages_async(
    cfg: QuicOneShotConfig,
    messages: Vec<Message>,
    reason: &str,
) -> Result<()> {
    let timeout = cfg.timeout;
    let (endpoint, _connection, mut send, _recv) = open_authenticated_stream(&cfg).await?;
    for msg in messages {
        write_frame(&mut send, &msg).await?;
    }
    write_frame(
        &mut send,
        &Message::Goodbye(vbox_proto::Goodbye {
            reason: reason.to_owned(),
        }),
    )
    .await?;
    send.finish()?;
    let _ = tokio::time::timeout(timeout, endpoint.wait_idle()).await;
    Ok(())
}

async fn open_authenticated_stream(
    cfg: &QuicOneShotConfig,
) -> Result<(
    Endpoint,
    quinn::Connection,
    quinn::SendStream,
    quinn::RecvStream,
)> {
    let mut endpoint = Endpoint::client(bind_addr_for(cfg.addr))?;
    endpoint.set_default_client_config(client_config(cfg.server_cert_sha256.as_deref())?);

    let connecting = endpoint
        .connect(cfg.addr, "vbox-server")
        .with_context(|| format!("connect QUIC {}", cfg.addr))?;
    let connection = tokio::time::timeout(cfg.timeout, connecting)
        .await
        .context("QUIC connect timeout")?
        .context("QUIC connect")?;
    let (mut send, mut recv) = tokio::time::timeout(cfg.timeout, connection.open_bi())
        .await
        .context("QUIC open stream timeout")?
        .context("open QUIC stream")?;

    write_prelude(&mut send).await?;
    read_prelude(&mut recv).await?;
    write_frame(
        &mut send,
        &Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: client_name(),
        }),
    )
    .await?;
    let welcome = match read_frame(&mut recv).await? {
        Message::Welcome(w) => w,
        Message::Goodbye(g) => bail!("QUIC server refused: {}", g.reason),
        Message::Error(e) => bail!("QUIC server error {}: {}", e.code, e.message),
        other => bail!("expected QUIC Welcome, got {:?}", other.kind()),
    };
    if welcome.protocol_version != PROTOCOL_VERSION {
        bail!(
            "QUIC protocol version mismatch: server={}, client={}",
            welcome.protocol_version,
            PROTOCOL_VERSION
        );
    }
    write_frame(
        &mut send,
        &Message::DataPlaneAuth(DataPlaneAuth {
            token: cfg.token.clone(),
        }),
    )
    .await?;
    Ok((endpoint, connection, send, recv))
}

async fn view_network_async(
    cfg: QuicViewConfig,
    outbound_rx: OutboundRx,
    proxy: EventLoopProxy<ViewerEvent>,
) -> Result<()> {
    let mut endpoint = Endpoint::client(bind_addr_for(cfg.addr))?;
    endpoint.set_default_client_config(client_config(cfg.server_cert_sha256.as_deref())?);

    let connecting = endpoint
        .connect(cfg.addr, "vbox-server")
        .with_context(|| format!("connect QUIC {}", cfg.addr))?;
    let connection = tokio::time::timeout(cfg.timeout, connecting)
        .await
        .context("QUIC connect timeout")?
        .context("QUIC connect")?;
    let (mut send, mut recv) = tokio::time::timeout(cfg.timeout, connection.open_bi())
        .await
        .context("QUIC open stream timeout")?
        .context("open QUIC stream")?;

    write_prelude(&mut send).await?;
    read_prelude(&mut recv).await?;
    write_frame(
        &mut send,
        &Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: client_name(),
        }),
    )
    .await?;
    let welcome = match read_frame(&mut recv).await? {
        Message::Welcome(w) => w,
        Message::Goodbye(g) => bail!("QUIC server refused: {}", g.reason),
        Message::Error(e) => bail!("QUIC server error {}: {}", e.code, e.message),
        other => bail!("expected QUIC Welcome, got {:?}", other.kind()),
    };
    if welcome.protocol_version != PROTOCOL_VERSION {
        bail!(
            "QUIC protocol version mismatch: server={}, client={}",
            welcome.protocol_version,
            PROTOCOL_VERSION
        );
    }
    if debug_enabled() {
        eprintln!(
            "debug: quic connected: {} (session {})",
            welcome.server_name, welcome.session_id
        );
    }
    write_frame(
        &mut send,
        &Message::DataPlaneAuth(DataPlaneAuth { token: cfg.token }),
    )
    .await?;
    write_frame(
        &mut send,
        &Message::ViewRequest(ViewRequest {
            socket_name: cfg.socket_name,
            width: cfg.width.max(1),
            height: cfg.height.max(1),
        }),
    )
    .await?;

    let datagram_connection = connection.clone();
    let datagram_proxy = proxy.clone();
    let datagram_reader = tokio::spawn(async move {
        let mut assembler = FrameTileDatagramAssembler::default();
        let mut frames = 0u64;
        let mut chunks = 0u64;
        let mut payload_bytes = 0u64;
        loop {
            match datagram_connection.read_datagram().await {
                Ok(datagram_bytes) => match vbox_proto::decode_datagram(&datagram_bytes) {
                    Ok(DataPlaneDatagram::FrameTileChunk(chunk)) => {
                        chunks = chunks.saturating_add(1);
                        payload_bytes = payload_bytes.saturating_add(chunk.bytes.len() as u64);
                        if let Some(tile) = assembler.push(chunk) {
                            frames = frames.saturating_add(1);
                            if debug_enabled() && (frames <= 5 || frames % 60 == 0) {
                                eprintln!(
                                    "debug: quic datagram reassembled frame id={} bytes={} total_frames={} total_chunks={} total_payload_bytes={}",
                                    tile.id,
                                    tile.bytes.len(),
                                    frames,
                                    chunks,
                                    payload_bytes
                                );
                            }
                            if datagram_proxy
                                .send_event(ViewerEvent::Message(Message::FrameTile(tile)))
                                .is_err()
                            {
                                datagram_connection.close(0u32.into(), b"viewer closed");
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        if debug_enabled() {
                            eprintln!("debug: dropping invalid QUIC datagram: {e:#}");
                        }
                    }
                },
                Err(e) => {
                    if debug_enabled() {
                        eprintln!("debug: QUIC datagram reader stopped: {e:#}");
                    }
                    return;
                }
            }
        }
    });

    let channel_connection = connection.clone();
    let channel_proxy = proxy.clone();
    let channel_reader = tokio::spawn(async move {
        loop {
            match channel_connection.accept_uni().await {
                Ok(mut recv) => {
                    let proxy = channel_proxy.clone();
                    tokio::spawn(async move {
                        if let Err(e) = read_channel_stream(&mut recv, proxy).await {
                            if debug_enabled() {
                                eprintln!("debug: QUIC channel stream ended: {e:#}");
                            }
                        }
                    });
                }
                Err(e) => {
                    if debug_enabled() {
                        eprintln!("debug: QUIC channel accept stopped: {e:#}");
                    }
                    return;
                }
            }
        }
    });

    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    std::thread::Builder::new()
        .name("vbox-quic-outbound-bridge".into())
        .spawn(move || {
            for msg in outbound_rx {
                if async_tx.send(msg).is_err() {
                    return;
                }
            }
        })
        .context("spawning QUIC outbound bridge")?;

    let writer = tokio::spawn(async move {
        while let Some(msg) = async_rx.recv().await {
            write_frame(&mut send, &msg).await?;
        }
        write_frame(
            &mut send,
            &Message::Goodbye(vbox_proto::Goodbye {
                reason: "viewer closed".into(),
            }),
        )
        .await
    });

    loop {
        let msg = read_frame(&mut recv).await?;
        if proxy.send_event(ViewerEvent::Message(msg)).is_err() {
            connection.close(0u32.into(), b"viewer closed");
            endpoint.wait_idle().await;
            writer.abort();
            datagram_reader.abort();
            channel_reader.abort();
            return Ok(());
        }
    }
}

async fn read_channel_stream(
    recv: &mut quinn::RecvStream,
    proxy: EventLoopProxy<ViewerEvent>,
) -> Result<()> {
    read_prelude(recv).await?;
    let channel = match read_frame(recv).await? {
        Message::DataPlaneChannel(channel) => channel,
        other => bail!("expected QUIC DataPlaneChannel, got {:?}", other.kind()),
    };
    if debug_enabled() {
        eprintln!(
            "debug: quic channel opened id={} purpose={:?}",
            channel.channel_id, channel.purpose
        );
    }
    loop {
        let msg = read_frame(recv).await?;
        if proxy.send_event(ViewerEvent::Message(msg)).is_err() {
            return Ok(());
        }
    }
}

fn bind_addr_for(remote: SocketAddr) -> SocketAddr {
    match remote.ip() {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    }
}

fn client_config(server_cert_sha256: Option<&str>) -> Result<ClientConfig> {
    if let Some(pin) = server_cert_sha256 {
        return pinned_client_config(pin);
    }
    if crate::brand::env_var("VBOX_QUIC_INSECURE_SKIP_VERIFY").as_deref() == Some("1") {
        return insecure_client_config();
    }
    bail!("QUIC requires --quic-cert-sha256 (or VBOX_QUIC_INSECURE_SKIP_VERIFY=1 for development)");
}

fn pinned_client_config(server_cert_sha256: &str) -> Result<ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(PinnedServerVerification::new(server_cert_sha256)?)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"vbox-quic/1".to_vec()];
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        crypto,
    )?)))
}

fn insecure_client_config() -> Result<ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"vbox-quic/1".to_vec()];
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        crypto,
    )?)))
}

async fn write_prelude(send: &mut quinn::SendStream) -> Result<()> {
    send.write_all(&vbox_proto::MAGIC).await?;
    send.write_all(&vbox_proto::PROTOCOL_VERSION.to_be_bytes())
        .await?;
    Ok(())
}

async fn read_prelude(recv: &mut quinn::RecvStream) -> Result<()> {
    let mut magic = [0u8; 4];
    recv.read_exact(&mut magic)
        .await
        .context("reading QUIC magic")?;
    if magic != vbox_proto::MAGIC {
        bail!("bad QUIC magic: {magic:?}");
    }
    let mut ver = [0u8; 2];
    recv.read_exact(&mut ver)
        .await
        .context("reading QUIC version")?;
    let ver = u16::from_be_bytes(ver);
    if ver != vbox_proto::PROTOCOL_VERSION {
        bail!(
            "QUIC protocol version mismatch: peer={ver}, ours={}",
            vbox_proto::PROTOCOL_VERSION
        );
    }
    Ok(())
}

async fn write_frame(send: &mut quinn::SendStream, msg: &Message) -> Result<()> {
    send.write_all(&vbox_proto::encode_frame(msg)?).await?;
    Ok(())
}

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<Message> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .context("reading QUIC frame length")?;
    let frame_len = u32::from_be_bytes(len);
    if frame_len < 4 {
        bail!("QUIC frame too small: {frame_len}");
    }
    if frame_len > vbox_proto::MAX_FRAME_BYTES {
        bail!("QUIC frame too big: {frame_len}");
    }
    let mut kind_raw = [0u8; 4];
    recv.read_exact(&mut kind_raw)
        .await
        .context("reading QUIC kind")?;
    let kind_raw = u32::from_be_bytes(kind_raw);
    let kind = vbox_proto::Kind::from_u32(kind_raw)
        .ok_or_else(|| anyhow!("unknown QUIC message kind {kind_raw}"))?;
    let mut payload = vec![0u8; (frame_len - 4) as usize];
    recv.read_exact(&mut payload)
        .await
        .context("reading QUIC payload")?;
    vbox_proto::decode_frame_payload(kind, &payload)
}

#[derive(Default)]
struct FrameTileDatagramAssembler {
    pending: HashMap<(u64, u64), PendingFrameTile>,
}

struct PendingFrameTile {
    header: FrameTileHeader,
    total_len: usize,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
}

impl FrameTileDatagramAssembler {
    fn push(&mut self, chunk: FrameTileChunk) -> Option<FrameTile> {
        const MAX_PENDING_FRAMES: usize = 32;
        const MAX_CHUNKS_PER_FRAME: u32 = 16_384;

        if chunk.chunk_count == 0
            || chunk.chunk_count > MAX_CHUNKS_PER_FRAME
            || chunk.chunk_index >= chunk.chunk_count
        {
            return None;
        }
        let total_len = usize::try_from(chunk.total_len).ok()?;
        if total_len > vbox_proto::MAX_FRAME_BYTES as usize {
            return None;
        }

        let key = (chunk.channel_id, chunk.frame_seq);
        if !self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING_FRAMES {
            self.pending.clear();
        }
        let slot = self.pending.entry(key).or_insert_with(|| PendingFrameTile {
            header: chunk.header.clone(),
            total_len,
            chunks: vec![None; chunk.chunk_count as usize],
            received: 0,
        });

        if slot.total_len != total_len
            || slot.header != chunk.header
            || slot.chunks.len() != chunk.chunk_count as usize
        {
            self.pending.remove(&key);
            return None;
        }

        let index = chunk.chunk_index as usize;
        if slot.chunks[index].is_some() {
            return None;
        }
        slot.received = slot.received.checked_add(chunk.bytes.len())?;
        if slot.received > slot.total_len {
            self.pending.remove(&key);
            return None;
        }
        slot.chunks[index] = Some(chunk.bytes);

        if slot.chunks.iter().any(Option::is_none) {
            return None;
        }

        let slot = self.pending.remove(&key)?;
        let mut bytes = Vec::with_capacity(slot.total_len);
        for chunk in slot.chunks {
            bytes.extend_from_slice(&chunk?);
        }
        if bytes.len() != slot.total_len {
            return None;
        }
        Some(slot.header.into_tile(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembler_rebuilds_out_of_order_frame_tile_datagrams() {
        let tile = FrameTile {
            id: 42,
            x: 10,
            y: 20,
            w: 64,
            h: 32,
            stride: 256,
            encoding: vbox_proto::PixelEncoding::RawRgba,
            bytes: (0..4096).map(|i| (i % 251) as u8).collect(),
        };
        let mut datagrams = vbox_proto::frame_tile_datagrams(&tile, tile.id, 7, 1200).unwrap();
        datagrams.reverse();

        let mut assembler = FrameTileDatagramAssembler::default();
        let mut rebuilt = None;
        for datagram in datagrams {
            let DataPlaneDatagram::FrameTileChunk(chunk) =
                vbox_proto::decode_datagram(&datagram).unwrap();
            rebuilt = assembler.push(chunk).or(rebuilt);
        }

        assert_eq!(rebuilt, Some(tile));
    }

    #[test]
    fn parses_colon_separated_quic_cert_pin() {
        let pin = "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff:10:21:32:43:54:65:76:87:98:a9:ba:cb:dc:ed:fe:0f";
        let parsed = parse_sha256_hex(pin).unwrap();

        assert_eq!(parsed[0], 0x00);
        assert_eq!(parsed[1], 0x11);
        assert_eq!(parsed[31], 0x0f);
    }

    #[test]
    fn rejects_malformed_quic_cert_pin() {
        let err = parse_sha256_hex("not-a-pin").unwrap_err();

        assert!(format!("{err:#}").contains("64"));
    }
}

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(
            Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        ))
    }
}

#[derive(Debug)]
struct PinnedServerVerification {
    expected_sha256: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl PinnedServerVerification {
    fn new(expected_sha256: &str) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            expected_sha256: parse_sha256_hex(expected_sha256)?,
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        }))
    }
}

impl rustls::client::danger::ServerCertVerifier for PinnedServerVerification {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use sha2::{Digest, Sha256};

        let got = Sha256::digest(end_entity.as_ref());
        if got.as_slice() == self.expected_sha256 {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "QUIC server certificate pin mismatch".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn parse_sha256_hex(input: &str) -> Result<[u8; 32]> {
    let compact: String = input.chars().filter(|ch| *ch != ':').collect();
    if compact.len() != 64 || !compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("QUIC certificate pin must be 64 lowercase/uppercase hex characters");
    }
    let mut out = [0u8; 32];
    for (idx, pair) in compact.as_bytes().chunks_exact(2).enumerate() {
        let hex = std::str::from_utf8(pair).context("parse QUIC certificate pin hex")?;
        out[idx] = u8::from_str_radix(hex, 16).context("parse QUIC certificate pin byte")?;
    }
    Ok(out)
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
