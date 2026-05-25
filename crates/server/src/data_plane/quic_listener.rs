#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct QuicListenerConfig {
    pub(crate) addr: SocketAddr,
    pub(crate) token: String,
    pub(crate) cert_path: Option<PathBuf>,
    pub(crate) key_path: Option<PathBuf>,
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn spawn(_cfg: QuicListenerConfig) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn spawn(cfg: QuicListenerConfig) -> Result<()> {
    use anyhow::{Context, bail};

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("vbox-quic-listener".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_io()
                .enable_time()
                .worker_threads(2)
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("build QUIC listener runtime: {e:#}")));
                    eprintln!("quic listener runtime failed: {e:#}");
                    return;
                }
            };
            if let Err(e) = rt.block_on(run(cfg, ready_tx)) {
                eprintln!("quic listener stopped: {e:#}");
            }
        })?;
    match ready_rx
        .recv()
        .context("waiting for QUIC listener startup")?
    {
        Ok(addr) => {
            eprintln!("vbox-server QUIC data plane listening on {addr}");
            Ok(())
        }
        Err(message) => bail!("{message}"),
    }
}

#[cfg(target_os = "linux")]
async fn run(
    cfg: QuicListenerConfig,
    ready_tx: std::sync::mpsc::Sender<std::result::Result<SocketAddr, String>>,
) -> Result<()> {
    use std::sync::Arc;

    use anyhow::{Context, bail};
    use quinn::crypto::rustls::QuicServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    let (certs, key_der) = match (&cfg.cert_path, &cfg.key_path) {
        (Some(cert_path), Some(key_path)) => (
            load_certs(cert_path)
                .with_context(|| format!("load QUIC cert {}", cert_path.display()))?,
            load_private_key(key_path)
                .with_context(|| format!("load QUIC key {}", key_path.display()))?,
        ),
        (None, None) => {
            let key = rcgen::KeyPair::generate().context("generate QUIC cert key")?;
            let mut params = rcgen::CertificateParams::new(vec!["vbox-server".to_owned()])
                .context("QUIC cert params")?;
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "vbox-server");
            let cert = params.self_signed(&key).context("self-sign QUIC cert")?;
            let cert_der = CertificateDer::from(cert.der().to_vec());
            let key_der: PrivateKeyDer<'static> =
                PrivatePkcs8KeyDer::from(key.serialize_der()).into();
            (vec![cert_der], key_der)
        }
        _ => bail!("--quic-cert and --quic-key must be set together"),
    };

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key_der)
        .context("build QUIC rustls server config")?;
    server_crypto.alpn_protocols = vec![b"vbox-quic/1".to_vec()];
    let server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));
    let endpoint = match quinn::Endpoint::server(server_config, cfg.addr)
        .with_context(|| format!("bind QUIC {}", cfg.addr))
    {
        Ok(endpoint) => endpoint,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("{e:#}")));
            return Err(e);
        }
    };
    let local_addr = match endpoint.local_addr().context("read QUIC local addr") {
        Ok(addr) => addr,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("{e:#}")));
            return Err(e);
        }
    };
    let _ = ready_tx.send(Ok(local_addr));

    while let Some(incoming) = endpoint.accept().await {
        let token = cfg.token.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(incoming, token).await {
                eprintln!("quic connection ended: {e:#}");
            }
        });
    }
    bail!("QUIC endpoint closed")
}

#[cfg(target_os = "linux")]
fn load_certs(path: &std::path::Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use anyhow::Context;
    use std::io::BufReader;

    let mut rdr = BufReader::new(std::fs::File::open(path)?);
    rustls_pemfile::certs(&mut rdr)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("parse certificates {}", path.display()))
}

#[cfg(target_os = "linux")]
fn load_private_key(path: &std::path::Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    use anyhow::{Context, anyhow};
    use std::io::BufReader;

    let mut rdr = BufReader::new(std::fs::File::open(path)?);
    rustls_pemfile::private_key(&mut rdr)
        .with_context(|| format!("parse private key {}", path.display()))?
        .ok_or_else(|| anyhow!("no private key in {}", path.display()))
}

#[cfg(target_os = "linux")]
async fn handle_connection(incoming: quinn::Incoming, token: String) -> Result<()> {
    use anyhow::{Context, bail};

    let connection = incoming.await.context("accept QUIC connection")?;
    let (mut send, mut recv) = connection.accept_bi().await.context("accept QUIC stream")?;

    write_prelude(&mut send).await?;
    read_prelude(&mut recv).await?;

    let hello = match read_frame(&mut recv).await? {
        vbox_proto::Message::Hello(h) => h,
        other => bail!("expected Hello, got {:?}", other.kind()),
    };
    let session_id = super::session::next_quic_session_id();
    eprintln!(
        "quic session {session_id}: client='{}' (proto v{})",
        hello.client_name, hello.protocol_version
    );
    write_frame(
        &mut send,
        &vbox_proto::Message::Welcome(vbox_proto::Welcome {
            protocol_version: vbox_proto::PROTOCOL_VERSION,
            server_name: crate::server_name(),
            session_id,
        }),
    )
    .await?;

    match read_frame(&mut recv).await? {
        vbox_proto::Message::DataPlaneAuth(auth) if auth.token == token => {}
        vbox_proto::Message::DataPlaneAuth(_) => {
            write_frame(
                &mut send,
                &vbox_proto::Message::Error(vbox_proto::ProtoError {
                    code: 403,
                    message: "invalid QUIC data-plane token".into(),
                }),
            )
            .await?;
            bail!("invalid QUIC data-plane token");
        }
        other => bail!("expected DataPlaneAuth, got {:?}", other.kind()),
    }

    let req = match read_frame(&mut recv).await? {
        vbox_proto::Message::ViewRequest(req) => req,
        first => return handle_one_shot_stream(send, recv, first, session_id).await,
    };

    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel::<vbox_proto::Message>();
    let (outbound_tx, outbound_rx) = std::sync::mpsc::channel::<vbox_proto::Message>();
    let disconnected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let io = crate::wayland_session::WaylandIo::from_parts(
        inbound_rx,
        outbound_tx,
        std::sync::Arc::clone(&disconnected),
    );

    let (async_out_tx, mut async_out_rx) =
        tokio::sync::mpsc::unbounded_channel::<vbox_proto::Message>();
    std::thread::Builder::new()
        .name("vbox-quic-out-bridge".into())
        .spawn(move || {
            for msg in outbound_rx {
                if async_out_tx.send(msg).is_err() {
                    return;
                }
            }
        })
        .context("spawning QUIC outbound bridge")?;

    let read_disconnected = std::sync::Arc::clone(&disconnected);
    let reader = tokio::spawn(async move {
        loop {
            match read_frame(&mut recv).await {
                Ok(msg) => {
                    if inbound_tx.send(msg).is_err() {
                        read_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {
                    read_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            }
        }
    });

    let write_disconnected = std::sync::Arc::clone(&disconnected);
    let datagram_connection = connection.clone();
    let channel_connection = connection.clone();
    let writer = tokio::spawn(async move {
        let mut frame_seq = 0u64;
        let mut datagram_frames = 0u64;
        let mut datagram_chunks = 0u64;
        let mut datagram_bytes = 0u64;
        let mut channel_streams = HashMap::<u64, quinn::SendStream>::new();
        while let Some(msg) = async_out_rx.recv().await {
            match msg {
                vbox_proto::Message::FrameTile(tile) => {
                    frame_seq = frame_seq.wrapping_add(1);
                    match send_frame_tile_datagrams(&datagram_connection, &tile, frame_seq).await {
                        Ok(stats) => {
                            datagram_frames = datagram_frames.saturating_add(1);
                            datagram_chunks = datagram_chunks.saturating_add(stats.chunks as u64);
                            datagram_bytes = datagram_bytes.saturating_add(stats.bytes as u64);
                            if crate::debug_enabled()
                                && (datagram_frames <= 5 || datagram_frames.is_multiple_of(60))
                            {
                                eprintln!(
                                    "debug: quic datagram frame seq={frame_seq} id={} chunks={} bytes={} total_frames={} total_chunks={} total_bytes={}",
                                    tile.id,
                                    stats.chunks,
                                    stats.bytes,
                                    datagram_frames,
                                    datagram_chunks,
                                    datagram_bytes
                                );
                            }
                            continue;
                        }
                        Err(e) => {
                            eprintln!(
                                "quic session frame {frame_seq}: datagram send failed, using stream fallback: {e:#}"
                            );
                            let msg = vbox_proto::Message::FrameTile(tile.clone());
                            let write_result = match write_channel_frame(
                                &channel_connection,
                                &mut channel_streams,
                                tile.id,
                                &msg,
                            )
                            .await
                            {
                                Ok(()) => Ok(()),
                                Err(_) => write_frame(&mut send, &msg).await,
                            };
                            if write_result.is_err() {
                                write_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                }
                vbox_proto::Message::WindowEvent(event) => {
                    let channel_id = window_event_channel_id(&event);
                    let close_channel = matches!(event, vbox_proto::WindowEvent::Destroyed { .. });
                    let msg = vbox_proto::Message::WindowEvent(event);
                    let write_result = match write_channel_frame(
                        &channel_connection,
                        &mut channel_streams,
                        channel_id,
                        &msg,
                    )
                    .await
                    {
                        Ok(()) => Ok(()),
                        Err(_) => write_frame(&mut send, &msg).await,
                    };
                    if write_result.is_err() {
                        write_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                    if close_channel {
                        close_channel_stream(&mut channel_streams, channel_id);
                    }
                }
                other => {
                    if write_frame(&mut send, &other).await.is_err() {
                        write_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                }
            }
        }
        write_disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let wayland = tokio::task::spawn_blocking(move || crate::start_wayland_view(req, io));
    let outcome = wayland.await.context("join Wayland session")?;
    disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
    reader.abort();
    writer.abort();
    outcome
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct DatagramSendStats {
    chunks: usize,
    bytes: usize,
}

#[cfg(target_os = "linux")]
async fn handle_one_shot_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    first: vbox_proto::Message,
    session_id: u64,
) -> Result<()> {
    handle_one_shot_message(&mut send, first, session_id).await?;
    loop {
        match read_frame(&mut recv).await {
            Ok(msg) => handle_one_shot_message(&mut send, msg, session_id).await?,
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(target_os = "linux")]
async fn handle_one_shot_message(
    send: &mut quinn::SendStream,
    msg: vbox_proto::Message,
    session_id: u64,
) -> Result<()> {
    use anyhow::bail;

    match msg {
        vbox_proto::Message::Ping(ping) => {
            write_frame(
                send,
                &vbox_proto::Message::Pong(vbox_proto::Pong {
                    seq: ping.seq,
                    stamp_ns: crate::now_ns(),
                }),
            )
            .await
        }
        vbox_proto::Message::Goodbye(goodbye) => {
            if crate::debug_enabled() {
                eprintln!(
                    "quic session {session_id}: one-shot goodbye: {}",
                    goodbye.reason
                );
            }
            Ok(())
        }
        vbox_proto::Message::InputEvent(event) => {
            if !crate::forward_input_event(event) && crate::debug_enabled() {
                eprintln!("quic session {session_id}: dropping input event without active view");
            }
            Ok(())
        }
        vbox_proto::Message::Clipboard(payload) => {
            if !crate::forward_clipboard_event(payload) && crate::debug_enabled() {
                eprintln!(
                    "quic session {session_id}: dropping clipboard event without active view"
                );
            }
            Ok(())
        }
        vbox_proto::Message::VolumeChange(change) => {
            crate::forward_volume_event(change);
            Ok(())
        }
        other => bail!(
            "expected one-shot data-plane message, got {:?}",
            other.kind()
        ),
    }
}

#[cfg(target_os = "linux")]
async fn write_channel_frame(
    connection: &quinn::Connection,
    streams: &mut HashMap<u64, quinn::SendStream>,
    channel_id: u64,
    msg: &vbox_proto::Message,
) -> Result<()> {
    if let std::collections::hash_map::Entry::Vacant(e) = streams.entry(channel_id) {
        let mut stream = connection.open_uni().await?;
        write_prelude(&mut stream).await?;
        write_frame(
            &mut stream,
            &vbox_proto::Message::DataPlaneChannel(vbox_proto::DataPlaneChannel {
                channel_id,
                purpose: vbox_proto::DataPlaneChannelPurpose::WindowReliable,
            }),
        )
        .await?;
        if crate::debug_enabled() {
            eprintln!("debug: quic channel open id={channel_id}");
        }
        e.insert(stream);
    }

    let stream = streams
        .get_mut(&channel_id)
        .expect("channel stream inserted above");
    if let Err(e) = write_frame(stream, msg).await {
        streams.remove(&channel_id);
        return Err(e);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn close_channel_stream(streams: &mut HashMap<u64, quinn::SendStream>, channel_id: u64) {
    if let Some(mut stream) = streams.remove(&channel_id) {
        let _ = stream.finish();
        if crate::debug_enabled() {
            eprintln!("debug: quic channel closed id={channel_id}");
        }
    }
}

#[cfg(target_os = "linux")]
fn window_event_channel_id(event: &vbox_proto::WindowEvent) -> u64 {
    match event {
        vbox_proto::WindowEvent::Created { id, .. }
        | vbox_proto::WindowEvent::Configured { id, .. }
        | vbox_proto::WindowEvent::Destroyed { id }
        | vbox_proto::WindowEvent::TitleChanged { id, .. }
        | vbox_proto::WindowEvent::Minimized { id }
        | vbox_proto::WindowEvent::MoveRequested { id }
        | vbox_proto::WindowEvent::FullscreenChanged { id, .. } => *id,
    }
}

#[cfg(target_os = "linux")]
async fn send_frame_tile_datagrams(
    connection: &quinn::Connection,
    tile: &vbox_proto::FrameTile,
    frame_seq: u64,
) -> Result<DatagramSendStats> {
    use anyhow::Context;

    let max_size = connection
        .max_datagram_size()
        .context("QUIC datagrams unsupported by peer")?;
    let datagrams = vbox_proto::frame_tile_datagrams(tile, tile.id, frame_seq, max_size)?;
    let bytes = datagrams.iter().map(Vec::len).sum();
    let chunks = datagrams.len();
    for datagram in datagrams {
        connection
            .send_datagram_wait(bytes::Bytes::from(datagram))
            .await
            .context("send QUIC frame tile datagram")?;
    }
    Ok(DatagramSendStats { chunks, bytes })
}

#[cfg(target_os = "linux")]
async fn write_prelude(send: &mut quinn::SendStream) -> Result<()> {
    send.write_all(&vbox_proto::MAGIC).await?;
    send.write_all(&vbox_proto::PROTOCOL_VERSION.to_be_bytes())
        .await?;
    Ok(())
}

#[cfg(target_os = "linux")]
async fn read_prelude(recv: &mut quinn::RecvStream) -> Result<()> {
    use anyhow::{Context, bail};

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

#[cfg(target_os = "linux")]
async fn write_frame(send: &mut quinn::SendStream, msg: &vbox_proto::Message) -> Result<()> {
    let bytes = vbox_proto::encode_frame(msg)?;
    send.write_all(&bytes).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
async fn read_frame(recv: &mut quinn::RecvStream) -> Result<vbox_proto::Message> {
    use anyhow::{Context, anyhow, bail};

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
        .context("reading QUIC frame kind")?;
    let kind_raw = u32::from_be_bytes(kind_raw);
    let kind = vbox_proto::Kind::from_u32(kind_raw)
        .ok_or_else(|| anyhow!("unknown QUIC message kind {kind_raw}"))?;
    let mut payload = vec![0u8; (frame_len - 4) as usize];
    recv.read_exact(&mut payload)
        .await
        .context("reading QUIC frame payload")?;
    vbox_proto::decode_frame_payload(kind, &payload)
}
