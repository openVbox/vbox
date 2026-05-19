//! TCP session + per-channel network threads for `vbox-client`.
//!
//! Every subcommand starts with [`handshake`], which performs the
//! prelude + Hello/Welcome exchange and returns a [`Session`]. The viewer
//! has two long-lived sockets (view and input) driven by [`view_network`]
//! and [`input_network`]; CLI utilities like [`one_shot_ping`],
//! [`interactive`], and [`send_input_command`] use a single short-lived
//! handshake and exit.
//!
//! `ViewerEvent` is owned by `viewer::app`; this module only constructs
//! `ViewerEvent::Message` from the wire stream and forwards it through the
//! winit `EventLoopProxy`.
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vbox_proto::{
    DataPlaneMode, InputButtonState, InputEvent, InputKeyState, Message, Ping, ViewRequest,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::{EventLoop, EventLoopProxy};

use crate::cli::InputCmd;
use crate::data_plane::tcp::handshake;
use crate::viewer::app::{ViewerApp, ViewerEvent};
use crate::viewer::input::usize_to_i32;
use crate::{debug_enabled, now_ns};

fn one_shot_tcp_ping(addr: SocketAddr, timeout_secs: u64) -> Result<Duration> {
    let mut s = handshake(addr, Duration::from_secs(timeout_secs))?;
    println!("connected: {} (session {})", s.server_name, s.session_id);

    let started = Instant::now();
    let seq = s.next_seq;
    s.next_seq += 1;
    vbox_proto::write_frame(
        &mut s.writer,
        &Message::Ping(Ping {
            seq,
            stamp_ns: now_ns(),
        }),
    )?;
    let resp = vbox_proto::read_frame(&mut s.reader)?;
    let elapsed = started.elapsed();
    match classify_ping_reply(&resp, seq) {
        PingReply::Pong { seq: pong_seq } => {
            println!("{}", format_pong_line(pong_seq, elapsed));
            let _ = vbox_proto::write_frame(
                &mut s.writer,
                &Message::Goodbye(vbox_proto::Goodbye {
                    reason: "ping complete".into(),
                }),
            );
            Ok(elapsed)
        }
        PingReply::SeqMismatch { got, expected } => {
            bail!("pong seq mismatch: got {got}, expected {expected}")
        }
        PingReply::UnexpectedKind { kind } => bail!("unexpected reply: {kind:?}"),
    }
}

#[allow(dead_code)] // compatibility wrapper for tests and direct TCP-only callers.
pub(crate) fn one_shot_ping(addr: SocketAddr, timeout_secs: u64) -> Result<()> {
    let _ = one_shot_tcp_ping(addr, timeout_secs)?;
    Ok(())
}

pub(crate) fn one_shot_ping_with_transport(
    addr: SocketAddr,
    transport: CommandTransport,
    timeout_secs: u64,
) -> Result<()> {
    match transport.mode {
        DataPlaneMode::TcpOnly => {
            let _ = one_shot_tcp_ping(addr, timeout_secs)?;
            Ok(())
        }
        DataPlaneMode::QuicOnly | DataPlaneMode::Auto => {
            let Some(quic_addr) = transport.quic_addr else {
                if transport.mode == DataPlaneMode::Auto {
                    let _ = one_shot_tcp_ping(addr, timeout_secs)?;
                    return Ok(());
                }
                bail!("--data-plane quic-only requires --quic-addr");
            };
            let Some(quic_token) = transport.quic_token else {
                if transport.mode == DataPlaneMode::Auto {
                    let _ = one_shot_tcp_ping(addr, timeout_secs)?;
                    return Ok(());
                }
                bail!("--data-plane quic-only requires --quic-token");
            };
            let timeout = Duration::from_secs(timeout_secs);
            let result =
                crate::data_plane::quic::ping(crate::data_plane::quic::QuicOneShotConfig {
                    addr: quic_addr,
                    token: quic_token,
                    server_cert_sha256: transport.quic_cert_sha256,
                    timeout,
                });
            match (transport.mode, result) {
                (_, Ok(elapsed)) => {
                    println!("pong seq=1 rtt={:.2}ms", elapsed.as_secs_f64() * 1000.0);
                    Ok(())
                }
                (DataPlaneMode::Auto, Err(e)) => {
                    eprintln!("QUIC ping failed, falling back to TCP: {e:#}");
                    let _ = one_shot_tcp_ping(addr, timeout_secs)?;
                    Ok(())
                }
                (_, Err(e)) => Err(e),
            }
        }
    }
}

/// Outcome of validating a wire response against the seq we sent in a
/// one-shot Ping. Splitting the decision out of `one_shot_ping` lets us
/// test the three observable shapes without standing up a server.
#[derive(Debug, PartialEq, Eq)]
enum PingReply {
    /// Matching Pong arrived; carries the seq for the output line.
    Pong { seq: u64 },
    /// Wrong seq — the server is replying to a previous Ping or
    /// behaving badly.
    SeqMismatch { got: u64, expected: u64 },
    /// Some other frame arrived where Pong was expected.
    UnexpectedKind { kind: vbox_proto::Kind },
}

fn classify_ping_reply(msg: &Message, expected_seq: u64) -> PingReply {
    match msg {
        Message::Pong(p) if p.seq == expected_seq => PingReply::Pong { seq: p.seq },
        Message::Pong(p) => PingReply::SeqMismatch {
            got: p.seq,
            expected: expected_seq,
        },
        other => PingReply::UnexpectedKind { kind: other.kind() },
    }
}

/// Format the `pong seq=… rtt=…ms` line printed for the operator.
/// Splitting from `println!` keeps the format pinned so a future tweak
/// to the precision or label can't slip through silently.
fn format_pong_line(seq: u64, elapsed: Duration) -> String {
    format!("pong seq={seq} rtt={:.2}ms", elapsed.as_secs_f64() * 1000.0)
}

/// What the `interactive` REPL should do with a line of stdin. Pure
/// parser so the test can pin every operator-visible command and
/// typo. Whitespace stripping is built into the parser — the loop
/// hands us the raw line.
#[derive(Debug, PartialEq, Eq)]
enum InteractiveCommand<'a> {
    /// Send one Ping and print the round-trip.
    Ping,
    /// Goodbye + exit.
    Quit,
    /// Blank line — print the prompt again, do nothing.
    Nothing,
    /// Anything else — print `unknown: <text>` for the operator.
    Unknown(&'a str),
}

fn parse_interactive_line(line: &str) -> InteractiveCommand<'_> {
    match line.trim() {
        "ping" => InteractiveCommand::Ping,
        "quit" | "exit" => InteractiveCommand::Quit,
        "" => InteractiveCommand::Nothing,
        other => InteractiveCommand::Unknown(other),
    }
}

pub(crate) fn interactive(addr: SocketAddr) -> Result<()> {
    let mut s = handshake(addr, Duration::from_secs(5))?;
    println!("connected: {} (session {})", s.server_name, s.session_id);
    println!("commands:  ping  |  quit");
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        print!("> ");
        std::io::stdout().flush().ok();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(());
        }
        match parse_interactive_line(&line) {
            InteractiveCommand::Ping => {
                let started = Instant::now();
                let seq = s.next_seq;
                s.next_seq += 1;
                vbox_proto::write_frame(
                    &mut s.writer,
                    &Message::Ping(Ping {
                        seq,
                        stamp_ns: now_ns(),
                    }),
                )?;
                match vbox_proto::read_frame(&mut s.reader)? {
                    Message::Pong(p) => println!(
                        "pong seq={} rtt={:.2}ms",
                        p.seq,
                        started.elapsed().as_secs_f64() * 1000.0
                    ),
                    other => println!("got: {:?}", other.kind()),
                }
            }
            InteractiveCommand::Quit => {
                let _ = vbox_proto::write_frame(
                    &mut s.writer,
                    &Message::Goodbye(vbox_proto::Goodbye {
                        reason: "user".into(),
                    }),
                );
                return Ok(());
            }
            InteractiveCommand::Nothing => {}
            InteractiveCommand::Unknown(other) => println!("unknown: {other}"),
        }
    }
}

#[allow(dead_code)] // kept for the legacy TCP-only caller shape; launch uses view_with_transport.
pub(crate) fn view(addr: SocketAddr, socket_name: String, width: u32, height: u32) -> Result<()> {
    view_with_transport(addr, socket_name, width, height, ViewTransport::tcp())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewTransport {
    pub(crate) mode: DataPlaneMode,
    pub(crate) quic_addr: Option<SocketAddr>,
    pub(crate) quic_token: Option<String>,
    pub(crate) quic_cert_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandTransport {
    pub(crate) mode: DataPlaneMode,
    pub(crate) quic_addr: Option<SocketAddr>,
    pub(crate) quic_token: Option<String>,
    pub(crate) quic_cert_sha256: Option<String>,
}

impl CommandTransport {
    #[allow(dead_code)] // used by tests and future call sites that need an explicit TCP transport.
    pub(crate) const fn tcp() -> Self {
        Self {
            mode: DataPlaneMode::TcpOnly,
            quic_addr: None,
            quic_token: None,
            quic_cert_sha256: None,
        }
    }
}

impl ViewTransport {
    pub(crate) const fn tcp() -> Self {
        Self {
            mode: DataPlaneMode::TcpOnly,
            quic_addr: None,
            quic_token: None,
            quic_cert_sha256: None,
        }
    }
}

pub(crate) fn view_with_transport(
    addr: SocketAddr,
    socket_name: String,
    width: u32,
    height: u32,
    transport: ViewTransport,
) -> Result<()> {
    let debug = debug_enabled();
    if debug {
        eprintln!("debug: viewer addr={addr} socket={socket_name} size={width}x{height}");
    }
    let event_loop = EventLoop::<ViewerEvent>::with_user_event()
        .build()
        .context("creating winit event loop")?;
    let proxy = event_loop.create_proxy();
    // Wire SIGUSR1 → ViewerEvent::DumpWindows so `./vbox windows` can
    // ask the running viewer for a one-shot state dump. Installed
    // before any network thread starts so the signal handler is in
    // place from the very first frame.
    #[cfg(unix)]
    crate::viewer::dump_signal::install(proxy.clone());
    let (outbound_tx, outbound_rx) = mpsc::channel::<Message>();
    spawn_view_network(
        addr,
        socket_name,
        width,
        height,
        transport,
        outbound_rx,
        proxy,
    )?;

    // The clipboard bridge — when present — funnels NSPasteboard changes
    // back through the same outbound channel as input events, and stashes
    // an `inbound_tx` we can hand inbound `Message::Clipboard` frames to.
    // On non-macOS targets `clipboard::start` is a no-op and returns None;
    // the viewer then has no clipboard subsystem and skips the relay.
    let (clip_outbound_tx, clip_outbound_rx) = mpsc::channel::<vbox_proto::Clipboard>();
    let clipboard_inbound = crate::clipboard::start(clip_outbound_tx);
    if clipboard_inbound.is_some() {
        let forward = outbound_tx.clone();
        std::thread::Builder::new()
            .name("vbox-clipboard-out".into())
            .spawn(move || {
                for payload in clip_outbound_rx {
                    if forward.send(Message::Clipboard(payload)).is_err() {
                        return;
                    }
                }
            })
            .context("spawning clipboard outbound forwarder")?;
    }

    // Volume bridge — Mac master volume changes flow into the same
    // outbound channel as input/clipboard. On non-macOS it's a no-op.
    // Unlike clipboard, volume is push-only (no inbound channel needed).
    crate::volume::start(outbound_tx.clone());

    let tile_screen_size = PhysicalSize::new(width.max(1), height.max(1));
    let mut app = ViewerApp::new(outbound_tx, clipboard_inbound, debug, tile_screen_size);
    event_loop.run_app(&mut app).context("running viewer")?;
    Ok(())
}

fn spawn_view_network(
    addr: SocketAddr,
    socket_name: String,
    width: u32,
    height: u32,
    transport: ViewTransport,
    outbound_rx: mpsc::Receiver<Message>,
    proxy: EventLoopProxy<ViewerEvent>,
) -> Result<()> {
    match transport.mode {
        DataPlaneMode::TcpOnly => {
            spawn_tcp_view_network(addr, socket_name, width, height, outbound_rx, proxy)
        }
        DataPlaneMode::QuicOnly | DataPlaneMode::Auto => {
            let Some(quic_addr) = transport.quic_addr else {
                if transport.mode == DataPlaneMode::Auto {
                    return spawn_tcp_view_network(
                        addr,
                        socket_name,
                        width,
                        height,
                        outbound_rx,
                        proxy,
                    );
                }
                bail!("--data-plane quic-only requires --quic-addr");
            };
            let Some(quic_token) = transport.quic_token else {
                if transport.mode == DataPlaneMode::Auto {
                    return spawn_tcp_view_network(
                        addr,
                        socket_name,
                        width,
                        height,
                        outbound_rx,
                        proxy,
                    );
                }
                bail!("--data-plane quic-only requires --quic-token");
            };
            let mode = transport.mode;
            let mut fallback_active = None;
            let (network_rx, fallback_rx) = if mode == DataPlaneMode::Auto {
                let (quic_tx, quic_rx) = mpsc::channel();
                let (tcp_tx, tcp_rx) = mpsc::channel();
                let fallback_gate = Arc::new(AtomicBool::new(false));
                let tee_fallback_gate = Arc::clone(&fallback_gate);
                fallback_active = Some(fallback_gate);
                std::thread::Builder::new()
                    .name("vbox-outbound-tee".into())
                    .spawn(move || {
                        for msg in outbound_rx {
                            let _ = quic_tx.send(msg.clone());
                            if tee_fallback_gate.load(Ordering::Acquire) {
                                let _ = tcp_tx.send(msg);
                            }
                        }
                    })
                    .context("spawning outbound tee")?;
                (quic_rx, Some(tcp_rx))
            } else {
                (outbound_rx, None)
            };
            std::thread::Builder::new()
                .name("vbox-quic-net".into())
                .spawn(move || {
                    let result = crate::data_plane::quic::view_network(
                        crate::data_plane::quic::QuicViewConfig {
                            addr: quic_addr,
                            token: quic_token,
                            server_cert_sha256: transport.quic_cert_sha256.clone(),
                            socket_name: socket_name.clone(),
                            width,
                            height,
                            timeout: Duration::from_millis(2_500),
                        },
                        network_rx,
                        proxy.clone(),
                    );
                    if let Err(e) = result {
                        if mode == DataPlaneMode::Auto {
                            eprintln!("QUIC data plane failed, falling back to TCP: {e:#}");
                            if let Some(gate) = fallback_active {
                                gate.store(true, Ordering::Release);
                            }
                            if let Some(rx) = fallback_rx {
                                let _ = spawn_tcp_view_network(
                                    addr,
                                    socket_name,
                                    width,
                                    height,
                                    rx,
                                    proxy.clone(),
                                );
                            }
                        } else {
                            let _ = proxy.send_event(ViewerEvent::Disconnected(format!("{e:#}")));
                        }
                    }
                })
                .context("spawning QUIC network thread")?;
            Ok(())
        }
    }
}

fn spawn_tcp_view_network(
    addr: SocketAddr,
    socket_name: String,
    width: u32,
    height: u32,
    outbound_rx: mpsc::Receiver<Message>,
    proxy: EventLoopProxy<ViewerEvent>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("vbox-input-net".into())
        .spawn(move || {
            if let Err(e) = input_network(addr, outbound_rx) {
                eprintln!("input channel disconnected: {e:#}");
            }
        })
        .context("spawning input network thread")?;
    std::thread::Builder::new()
        .name("vbox-view-net".into())
        .spawn(move || {
            if let Err(e) = view_network(addr, socket_name, width, height, proxy.clone()) {
                let _ = proxy.send_event(ViewerEvent::Disconnected(format!("{e:#}")));
            }
        })
        .context("spawning viewer network thread")?;
    Ok(())
}

fn view_network(
    addr: SocketAddr,
    socket_name: String,
    width: u32,
    height: u32,
    proxy: EventLoopProxy<ViewerEvent>,
) -> Result<()> {
    let mut s = handshake(addr, Duration::from_secs(5))?;
    println!("connected: {} (session {})", s.server_name, s.session_id);
    if debug_enabled() {
        eprintln!("debug: sending ViewRequest socket={socket_name} size={width}x{height}");
    }
    vbox_proto::write_frame(
        &mut s.writer,
        &Message::ViewRequest(build_view_request(socket_name, width, height)),
    )?;
    s.reader.get_mut().set_read_timeout(None).ok();

    loop {
        let msg = vbox_proto::read_frame(&mut s.reader)?;
        if proxy.send_event(ViewerEvent::Message(msg)).is_err() {
            return Ok(());
        }
    }
}

/// Drain the unified client→server outbound channel and write each
/// payload as a wire frame. The channel is typed on `Message` rather
/// than `InputEvent` so peripheral subsystems (currently only
/// `clipboard`) can ride the same TCP connection without a second
/// `handshake` round-trip.
fn input_network(addr: SocketAddr, rx: mpsc::Receiver<Message>) -> Result<()> {
    let mut s = handshake(addr, Duration::from_secs(5))?;
    if debug_enabled() {
        eprintln!(
            "debug: input connected: {} (session {})",
            s.server_name, s.session_id
        );
    }
    for msg in rx {
        vbox_proto::write_frame(&mut s.writer, &msg)?;
    }
    let _ = vbox_proto::write_frame(
        &mut s.writer,
        &Message::Goodbye(vbox_proto::Goodbye {
            reason: "viewer closed".into(),
        }),
    );
    Ok(())
}

pub(crate) fn send_input_command(
    addr: SocketAddr,
    transport: CommandTransport,
    id: u64,
    event: InputCmd,
) -> Result<()> {
    let messages = build_input_command_sequence(id, event)
        .into_iter()
        .map(Message::InputEvent)
        .collect();
    send_command_messages(addr, transport, messages, "input command complete")
}

fn send_tcp_command_messages(addr: SocketAddr, messages: Vec<Message>, reason: &str) -> Result<()> {
    let mut s = handshake(addr, Duration::from_secs(5))?;
    for msg in messages {
        vbox_proto::write_frame(&mut s.writer, &msg)?;
    }
    let _ = vbox_proto::write_frame(
        &mut s.writer,
        &Message::Goodbye(vbox_proto::Goodbye {
            reason: reason.into(),
        }),
    );
    Ok(())
}

fn send_command_messages(
    addr: SocketAddr,
    transport: CommandTransport,
    messages: Vec<Message>,
    reason: &str,
) -> Result<()> {
    match transport.mode {
        DataPlaneMode::TcpOnly => send_tcp_command_messages(addr, messages, reason),
        DataPlaneMode::QuicOnly | DataPlaneMode::Auto => {
            let Some(quic_addr) = transport.quic_addr else {
                if transport.mode == DataPlaneMode::Auto {
                    return send_tcp_command_messages(addr, messages, reason);
                }
                bail!("--data-plane quic-only requires --quic-addr");
            };
            let Some(quic_token) = transport.quic_token else {
                if transport.mode == DataPlaneMode::Auto {
                    return send_tcp_command_messages(addr, messages, reason);
                }
                bail!("--data-plane quic-only requires --quic-token");
            };
            let result = crate::data_plane::quic::send_messages(
                crate::data_plane::quic::QuicOneShotConfig {
                    addr: quic_addr,
                    token: quic_token,
                    server_cert_sha256: transport.quic_cert_sha256,
                    timeout: Duration::from_millis(2_500),
                },
                messages.clone(),
                reason,
            );
            match (transport.mode, result) {
                (_, Ok(())) => Ok(()),
                (DataPlaneMode::Auto, Err(e)) => {
                    eprintln!("QUIC command path failed, falling back to TCP: {e:#}");
                    send_tcp_command_messages(addr, messages, reason)
                }
                (_, Err(e)) => Err(e),
            }
        }
    }
}

/// Translate a CLI input subcommand into the InputEvent sequence the
/// guest should see. Split out of `send_input_command` so tests pin the
/// shapes without standing up a server. Each variant maps to one or
/// more events in fixed order — operators rely on the ordering (e.g.
/// motion before button-press on Click) when scripting flows.
fn build_input_command_sequence(id: u64, event: InputCmd) -> Vec<InputEvent> {
    match event {
        InputCmd::Motion { x, y } => vec![InputEvent::PointerMotion { id, x, y }],
        InputCmd::Click { x, y } => vec![
            InputEvent::PointerMotion { id, x, y },
            InputEvent::PointerButton {
                id,
                button: 0x110,
                state: InputButtonState::Pressed,
            },
            InputEvent::PointerButton {
                id,
                button: 0x110,
                state: InputButtonState::Released,
            },
        ],
        InputCmd::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
        } => build_drag_sequence(id, from_x, from_y, to_x, to_y),
        InputCmd::Text { text } => vec![InputEvent::Text {
            id,
            text: text.join(" "),
        }],
        InputCmd::Preedit { text } => {
            let text = text.join(" ");
            let cursor = usize_to_i32(text.len());
            vec![InputEvent::Preedit {
                id,
                text,
                cursor_begin: cursor,
                cursor_end: cursor,
            }]
        }
        InputCmd::Key { keycode } => vec![
            InputEvent::Key {
                id,
                keycode,
                state: InputKeyState::Pressed,
            },
            InputEvent::Key {
                id,
                keycode,
                state: InputKeyState::Released,
            },
        ],
    }
}

/// Build a `ViewRequest` carrying the socket name and a clamped
/// width/height pair. The clamp guards against momentary 0x0 sizes
/// (winit can deliver them briefly during a macOS resize gesture).
/// Pulled out so tests pin the contract without standing up a viewer.
fn build_view_request(socket_name: String, width: u32, height: u32) -> ViewRequest {
    ViewRequest {
        socket_name,
        width: width.max(1),
        height: height.max(1),
    }
}

fn lerp_i32(from: i32, to: i32, step: i32, steps: i32) -> i32 {
    from + (to - from) * step / steps.max(1)
}

/// Build the sequence of input events `vbox-client input drag` emits
/// for a single drag command. Separated from the wire-write loop so we can
/// assert the exact frame ordering without standing up a server.
fn build_drag_sequence(id: u64, from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Vec<InputEvent> {
    let mut events = Vec::with_capacity(11);
    events.push(InputEvent::PointerMotion {
        id,
        x: from_x,
        y: from_y,
    });
    events.push(InputEvent::PointerButton {
        id,
        button: 0x110,
        state: InputButtonState::Pressed,
    });
    for step in 1..=8 {
        events.push(InputEvent::PointerMotion {
            id,
            x: lerp_i32(from_x, to_x, step, 8),
            y: lerp_i32(from_y, to_y, step, 8),
        });
    }
    events.push(InputEvent::PointerButton {
        id,
        button: 0x110,
        state: InputButtonState::Released,
    });
    events
}

/// Map the operator's 0..=100 percent input onto the wire's 0.0..=1.0
/// scalar. Clamps to the upper bound; the percent type is `u8` already so
/// values can't go negative.
fn percent_to_wire_level(percent: u8) -> f32 {
    (percent.min(100) as f32) / 100.0
}

/// Single-shot Mac→Linux volume push. Used by `./vbox volume <N>` and
/// any future scripting that wants to exercise the volume path without
/// running the full viewer. `level_percent` is 0..=100 to mirror the
/// shell's expectation; we divide by 100 to land on the wire's scalar.
pub(crate) fn send_volume_command(
    addr: SocketAddr,
    transport: CommandTransport,
    level_percent: u8,
    muted: Option<bool>,
) -> Result<()> {
    use vbox_proto::VolumeChange;
    let level = percent_to_wire_level(level_percent);
    // Caller may omit the mute flag, in which case we don't touch it
    // (default false = unmuted) — matches the wire contract that every
    // frame is an absolute state.
    let muted = muted.unwrap_or(false);
    send_command_messages(
        addr,
        transport,
        vec![Message::VolumeChange(VolumeChange { level, muted })],
        "volume command complete",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- lerp_i32 ---------------------------------------------------------

    #[test]
    fn lerp_returns_endpoints_at_step_bounds() {
        assert_eq!(lerp_i32(10, 50, 0, 8), 10, "step 0 → from");
        assert_eq!(lerp_i32(10, 50, 8, 8), 50, "step 8 → to");
    }

    #[test]
    fn lerp_returns_midpoint_at_half_step() {
        assert_eq!(lerp_i32(0, 80, 4, 8), 40);
    }

    #[test]
    fn lerp_steps_zero_falls_back_to_one() {
        // The `.max(1)` guards against a divide-by-zero panic if a caller
        // ever passes `steps=0`. We don't promise a useful value, only that
        // it doesn't crash and stays between the endpoints for step=0.
        let v = lerp_i32(0, 100, 0, 0);
        assert!(
            (0..=100).contains(&v),
            "lerp with steps=0 must not panic, got {v}"
        );
    }

    // ---- build_drag_sequence ----------------------------------------------
    //
    // Story: `vbox-client input drag` synthesizes one Pressed + eight
    // motion frames between from→to + one Released. That's 11 frames in a
    // fixed order: motion(from), press, motion×8 (lerp), release. Operators
    // depend on this for tests; flipping the order would silently break
    // any UI macro that records drags.

    #[test]
    fn drag_sequence_emits_press_eight_motions_release_in_order() {
        let events = build_drag_sequence(7, 10, 20, 90, 100);
        assert_eq!(events.len(), 11);

        // Frame 0: motion to the drag start (the cursor must already be
        // there before the button goes down).
        match events[0] {
            InputEvent::PointerMotion { id, x, y } => {
                assert_eq!((id, x, y), (7, 10, 20));
            }
            ref other => panic!("frame 0 must be motion, got {other:?}"),
        }

        // Frame 1: button press.
        match events[1] {
            InputEvent::PointerButton { id, button, state } => {
                assert_eq!(id, 7);
                assert_eq!(button, 0x110, "BTN_LEFT");
                assert!(matches!(state, InputButtonState::Pressed));
            }
            ref other => panic!("frame 1 must be press, got {other:?}"),
        }

        // Frames 2..=9: motion at steps 1..=8.
        for step in 1..=8 {
            let frame = &events[1 + step as usize];
            match frame {
                InputEvent::PointerMotion { id, x, y } => {
                    assert_eq!(*id, 7);
                    assert_eq!(*x, lerp_i32(10, 90, step, 8));
                    assert_eq!(*y, lerp_i32(20, 100, step, 8));
                }
                other => panic!("frame {step} must be motion, got {other:?}"),
            }
        }

        // Last frame: button release.
        match events[10] {
            InputEvent::PointerButton { state, .. } => {
                assert!(matches!(state, InputButtonState::Released));
            }
            ref other => panic!("last frame must be release, got {other:?}"),
        }
    }

    #[test]
    fn drag_sequence_endpoints_match_inputs_when_axis_aligned() {
        // A horizontal drag from (0,0)→(80,0): the 8th motion frame must
        // land exactly on the target so the guest sees the cursor arrive
        // at the operator-supplied end point (no off-by-one rounding).
        let events = build_drag_sequence(1, 0, 0, 80, 0);
        match events.last().unwrap() {
            InputEvent::PointerButton { .. } => {}
            other => panic!("last frame must be a release, got {other:?}"),
        }
        // The motion immediately before the release is the final lerp step.
        match &events[events.len() - 2] {
            InputEvent::PointerMotion { x, y, .. } => {
                assert_eq!((*x, *y), (80, 0));
            }
            other => panic!("expected motion before release, got {other:?}"),
        }
    }

    // ---- percent_to_wire_level --------------------------------------------

    #[test]
    fn percent_to_wire_level_anchors_zero_fifty_hundred() {
        assert!((percent_to_wire_level(0) - 0.0).abs() < f32::EPSILON);
        assert!((percent_to_wire_level(50) - 0.5).abs() < f32::EPSILON);
        assert!((percent_to_wire_level(100) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn percent_to_wire_level_clamps_above_one_hundred() {
        // u8 max is 255; the wire is 0..=1. The function must clamp to 1.0
        // so the guest doesn't see a bogus level=2.55.
        assert!((percent_to_wire_level(200) - 1.0).abs() < f32::EPSILON);
        assert!((percent_to_wire_level(u8::MAX) - 1.0).abs() < f32::EPSILON);
    }

    // ---- build_input_command_sequence ------------------------------------
    //
    // Story: `vbox-client input <verb>` reads one CLI subcommand and
    // synthesizes the corresponding InputEvent stream. The shape of each
    // sequence is part of the documented behaviour (one wire frame per
    // physical action) — operators rely on it when scripting workflows.

    #[test]
    fn motion_command_emits_single_pointer_motion() {
        let seq = build_input_command_sequence(7, InputCmd::Motion { x: 100, y: 50 });
        assert_eq!(seq.len(), 1);
        match &seq[0] {
            InputEvent::PointerMotion { id, x, y } => {
                assert_eq!((*id, *x, *y), (7, 100, 50));
            }
            other => panic!("expected PointerMotion, got {other:?}"),
        }
    }

    #[test]
    fn click_command_emits_motion_press_release_in_order() {
        // Three frames in fixed order: position the cursor, then press,
        // then release. The button code 0x110 is BTN_LEFT in Linux's
        // <linux/input-event-codes.h>.
        let seq = build_input_command_sequence(1, InputCmd::Click { x: 10, y: 20 });
        assert_eq!(seq.len(), 3);
        assert!(matches!(
            seq[0],
            InputEvent::PointerMotion { x: 10, y: 20, .. }
        ));
        match &seq[1] {
            InputEvent::PointerButton { button, state, .. } => {
                assert_eq!(*button, 0x110);
                assert!(matches!(state, InputButtonState::Pressed));
            }
            other => panic!("frame 1 must be press, got {other:?}"),
        }
        match &seq[2] {
            InputEvent::PointerButton { state, .. } => {
                assert!(matches!(state, InputButtonState::Released));
            }
            other => panic!("frame 2 must be release, got {other:?}"),
        }
    }

    #[test]
    fn drag_command_delegates_to_build_drag_sequence() {
        // build_input_command_sequence(Drag) must produce the same 11
        // frames as build_drag_sequence directly — proves the dispatch
        // doesn't reorder anything.
        let direct = build_drag_sequence(1, 0, 0, 80, 80);
        let via_cmd = build_input_command_sequence(
            1,
            InputCmd::Drag {
                from_x: 0,
                from_y: 0,
                to_x: 80,
                to_y: 80,
            },
        );
        assert_eq!(direct, via_cmd);
    }

    #[test]
    fn text_command_joins_space_separated_words() {
        // `vbox-client input text foo bar baz` sends "foo bar baz"
        // — clap parses the trailing args as a Vec<String>; we re-join
        // with a single space so the guest sees the operator's exact
        // word boundaries.
        let seq = build_input_command_sequence(
            5,
            InputCmd::Text {
                text: vec!["hello".into(), "world".into()],
            },
        );
        assert_eq!(seq.len(), 1);
        match &seq[0] {
            InputEvent::Text { id, text } => {
                assert_eq!(*id, 5);
                assert_eq!(text, "hello world");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn preedit_command_carries_end_cursor() {
        // Preedit semantics: cursor_begin == cursor_end == length →
        // the IME shows the cursor parked at the end of the composed
        // string, which is the expected UI for in-progress typing.
        let seq = build_input_command_sequence(
            5,
            InputCmd::Preedit {
                text: vec!["abc".into()],
            },
        );
        assert_eq!(seq.len(), 1);
        match &seq[0] {
            InputEvent::Preedit {
                text,
                cursor_begin,
                cursor_end,
                ..
            } => {
                assert_eq!(text, "abc");
                assert_eq!(*cursor_begin, 3);
                assert_eq!(*cursor_end, 3);
            }
            other => panic!("expected Preedit, got {other:?}"),
        }
    }

    // ---- build_view_request -----------------------------------------------
    //
    // Story: the viewer wraps the ViewRequest carrying the operator's
    // requested viewport size. The wire contract demands non-zero
    // dimensions; without the clamp a momentary 0x0 inner_size would
    // tear the guest compositor's surface allocation.

    #[test]
    fn view_request_passes_normal_dimensions_through() {
        let req = build_view_request("wayland-0".into(), 1024, 768);
        assert_eq!(req.socket_name, "wayland-0");
        assert_eq!(req.width, 1024);
        assert_eq!(req.height, 768);
    }

    #[test]
    fn view_request_clamps_zero_to_one() {
        // The guest's wl_compositor refuses zero dimensions. The helper
        // must clamp both axes independently so a brief 0x768 during
        // resize doesn't kill the surface.
        let req = build_view_request("wayland-0".into(), 0, 0);
        assert_eq!(req.width, 1);
        assert_eq!(req.height, 1);
        let half = build_view_request("wayland-0".into(), 0, 600);
        assert_eq!((half.width, half.height), (1, 600));
        let other = build_view_request("wayland-0".into(), 800, 0);
        assert_eq!((other.width, other.height), (800, 1));
    }

    #[test]
    fn view_request_preserves_socket_name_verbatim() {
        // Wayland socket names can be `wayland-0` through `wayland-N`;
        // operators sometimes hand-name them (`vbox-dev`). The helper
        // must pass them through with no normalization.
        let req = build_view_request("vbox-dev".into(), 100, 100);
        assert_eq!(req.socket_name, "vbox-dev");
    }

    // ---- classify_ping_reply / format_pong_line -------------------------
    //
    // Story: `vbox-client ping` writes one Ping with a known seq
    // and expects exactly one matching Pong back. Three outcomes the
    // operator can see in their terminal: matching pong, seq mismatch,
    // or some other frame entirely. We pin each one without standing
    // up a server.

    #[test]
    fn ping_reply_matches_when_seq_aligns() {
        let msg = Message::Pong(vbox_proto::Pong {
            seq: 5,
            stamp_ns: 100,
        });
        assert_eq!(classify_ping_reply(&msg, 5), PingReply::Pong { seq: 5 });
    }

    #[test]
    fn ping_reply_flags_seq_mismatch() {
        // Server replies to a stale Ping seq — the wrapper should
        // surface "got X, expected Y" so the operator sees which side
        // is out of sync.
        let msg = Message::Pong(vbox_proto::Pong {
            seq: 1,
            stamp_ns: 0,
        });
        assert_eq!(
            classify_ping_reply(&msg, 99),
            PingReply::SeqMismatch {
                got: 1,
                expected: 99,
            }
        );
    }

    #[test]
    fn ping_reply_flags_unexpected_kind() {
        // A Goodbye where Pong was expected — the server shut down
        // before answering. The unwrapper preserves the kind name so
        // logs are scannable.
        let msg = Message::Goodbye(vbox_proto::Goodbye {
            reason: "shutdown".into(),
        });
        assert_eq!(
            classify_ping_reply(&msg, 1),
            PingReply::UnexpectedKind {
                kind: vbox_proto::Kind::Goodbye
            }
        );
    }

    #[test]
    fn pong_line_renders_seq_and_two_decimal_rtt() {
        // Two decimal places gives operators sub-millisecond visibility
        // without overwhelming with noise. 1.5ms expressed as 1500us:
        let line = format_pong_line(7, Duration::from_micros(1_500));
        assert_eq!(line, "pong seq=7 rtt=1.50ms");
    }

    #[test]
    fn pong_line_handles_zero_elapsed_gracefully() {
        // Pathological — the response came back before we even
        // recorded the start (impossible on real clocks, but the
        // helper must not panic on zero).
        let line = format_pong_line(0, Duration::ZERO);
        assert_eq!(line, "pong seq=0 rtt=0.00ms");
    }

    // ---- parse_interactive_line ----------------------------------------
    //
    // Story: `vbox-client connect <addr>` drops into a tiny stdin
    // REPL. Operators rely on the documented two commands (`ping`,
    // `quit`/`exit`) plus blank-line passthrough; everything else is
    // surfaced as `unknown:` so a typo doesn't silently no-op.

    #[test]
    fn interactive_ping_command() {
        assert_eq!(parse_interactive_line("ping"), InteractiveCommand::Ping);
        // Whitespace around the verb is tolerated — common Enter-key
        // habit of typing "ping " before pressing return.
        assert_eq!(parse_interactive_line("  ping  "), InteractiveCommand::Ping);
        assert_eq!(parse_interactive_line("ping\n"), InteractiveCommand::Ping);
    }

    #[test]
    fn interactive_quit_and_exit_both_close() {
        assert_eq!(parse_interactive_line("quit"), InteractiveCommand::Quit);
        assert_eq!(parse_interactive_line("exit"), InteractiveCommand::Quit);
        assert_eq!(parse_interactive_line("  quit\n"), InteractiveCommand::Quit);
    }

    #[test]
    fn interactive_blank_line_is_a_no_op() {
        // Just pressing Enter at the prompt — common gesture. The
        // REPL must not interpret this as a command.
        assert_eq!(parse_interactive_line(""), InteractiveCommand::Nothing);
        assert_eq!(parse_interactive_line("   "), InteractiveCommand::Nothing);
        assert_eq!(parse_interactive_line("\n"), InteractiveCommand::Nothing);
    }

    #[test]
    fn interactive_unknown_command_is_surfaced_for_visibility() {
        // Operator typo — return the unknown text so the loop can
        // `println!("unknown: {text}")` and the operator sees the
        // misspelling rather than wondering why nothing happened.
        assert_eq!(
            parse_interactive_line("png"),
            InteractiveCommand::Unknown("png")
        );
        assert_eq!(
            parse_interactive_line("  help  "),
            InteractiveCommand::Unknown("help")
        );
    }

    #[test]
    fn interactive_command_words_are_case_sensitive() {
        // `Quit` vs `quit` — uppercase doesn't match. Make sure that
        // assumption is pinned so a future refactor doesn't widen
        // the parser silently.
        assert_eq!(
            parse_interactive_line("QUIT"),
            InteractiveCommand::Unknown("QUIT")
        );
        assert_eq!(
            parse_interactive_line("Ping"),
            InteractiveCommand::Unknown("Ping")
        );
    }

    #[test]
    fn pong_line_includes_large_seq_unmodified() {
        // After a long interactive session, seq passes u32::MAX —
        // format-wise it's still a single decimal number.
        let line = format_pong_line(u64::MAX, Duration::from_millis(1));
        assert!(line.contains(&format!("seq={}", u64::MAX)));
        assert!(line.ends_with("rtt=1.00ms"));
    }

    #[test]
    fn key_command_emits_pressed_then_released_pair() {
        // KEY_LEFTSHIFT (42) — operator triggers it with `input key 42`;
        // we synthesise both halves of the press/release pair so the
        // guest sees a complete keystroke.
        let seq = build_input_command_sequence(1, InputCmd::Key { keycode: 42 });
        assert_eq!(seq.len(), 2);
        match &seq[0] {
            InputEvent::Key { keycode, state, .. } => {
                assert_eq!(*keycode, 42);
                assert!(matches!(state, InputKeyState::Pressed));
            }
            other => panic!("frame 0 must be Pressed, got {other:?}"),
        }
        match &seq[1] {
            InputEvent::Key { state, .. } => {
                assert!(matches!(state, InputKeyState::Released));
            }
            other => panic!("frame 1 must be Released, got {other:?}"),
        }
    }
}
