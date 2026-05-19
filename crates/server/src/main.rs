// vbox-server — runs inside the Linux guest.
//
// Listen on TCP, do prelude + Hello/Welcome handshake, reply to Ping with
// Pong, and stream the Wayland-first nested compositor view after
// ViewRequest.

mod brand;
mod data_plane;
#[cfg(target_os = "linux")]
mod volume;
#[cfg(target_os = "linux")]
mod wayland_session;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::{BufReader, BufWriter};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use vbox_proto::{Goodbye, Message, PROTOCOL_VERSION, Ping, Pong, ViewRequest, Welcome};

#[derive(Parser, Debug)]
#[command(name = "vbox-server", version)]
struct Args {
    /// Address to bind. Default 127.0.0.1 because the client reaches us
    /// through `ssh -L`; binding on 0.0.0.0 is opt-in only.
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,

    /// TCP port. 5710 is unassigned in IANA's range as of this writing.
    #[arg(long, default_value_t = 5710)]
    port: u16,

    /// Maximum concurrent client connections. Excess connections are accepted
    /// then immediately closed with a Goodbye explaining the reason.
    #[arg(long, default_value_t = 4)]
    max_clients: usize,

    /// Optional QUIC data-plane bind address. Omit to keep the current
    /// TCP-only server behavior.
    #[arg(long)]
    quic_bind: Option<IpAddr>,

    /// UDP port for the optional QUIC data plane. Defaults to the TCP port
    /// when --quic-bind is set.
    #[arg(long)]
    quic_port: Option<u16>,

    /// Required application-level token for the optional QUIC data plane.
    #[arg(long)]
    quic_token: Option<String>,

    /// PEM certificate for the optional QUIC data plane. When omitted, the
    /// listener generates an ephemeral self-signed certificate for development.
    #[arg(long)]
    quic_cert: Option<PathBuf>,

    /// PEM private key paired with --quic-cert.
    #[arg(long)]
    quic_key: Option<PathBuf>,
}

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn main() -> Result<()> {
    let args = Args::parse();
    let addr = SocketAddr::new(args.bind, args.port);
    let listener = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    eprintln!("{}", format_listening_banner(addr));
    if debug_enabled() {
        eprintln!(
            "{}",
            format_startup_debug(args.bind, args.port, args.max_clients)
        );
    }
    if let Some(bind) = args.quic_bind {
        let token = args
            .quic_token
            .clone()
            .context("--quic-token is required when --quic-bind is set")?;
        let quic_addr = SocketAddr::new(bind, args.quic_port.unwrap_or(args.port));
        data_plane::quic_listener::spawn(data_plane::quic_listener::QuicListenerConfig {
            addr: quic_addr,
            token,
            cert_path: args.quic_cert.clone(),
            key_path: args.quic_key.clone(),
        })?;
    }

    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };
        let peer = stream.peer_addr().ok();
        let current = active.load(Ordering::SeqCst);
        match decide_accept(current, args.max_clients) {
            AcceptDecision::Reject { .. } => {
                eprintln!(
                    "rejecting {peer:?}: max_clients={} reached",
                    args.max_clients
                );
                // Best-effort polite refusal so the client sees a real error.
                let _ = refuse(stream, "server at max_clients");
                continue;
            }
            AcceptDecision::Accept { .. } => {
                active.fetch_add(1, Ordering::SeqCst);
            }
        }
        let active = active.clone();
        std::thread::Builder::new()
            .name("vbox-conn".into())
            .spawn(move || {
                if let Err(e) = handle(stream) {
                    eprintln!("client {peer:?} ended: {e:#}");
                }
                active.fetch_sub(1, Ordering::SeqCst);
            })
            .context("spawning connection thread")?;
    }
    Ok(())
}

fn refuse(stream: TcpStream, reason: &str) -> Result<()> {
    use std::io::Write;
    let bytes = encode_refuse(reason)?;
    let mut w = BufWriter::new(stream);
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

fn handle(stream: TcpStream) -> Result<()> {
    stream.set_nodelay(true).ok();
    let read = stream.try_clone().context("cloning stream")?;
    let mut r = BufReader::new(read);
    let mut w = BufWriter::new(stream);

    vbox_proto::write_prelude(&mut w)?;
    let _ver = vbox_proto::read_prelude(&mut r)?;

    // Expect Hello first.
    let hello = match vbox_proto::read_frame(&mut r)? {
        Message::Hello(h) => h,
        other => bail!("expected Hello, got {:?}", other.kind()),
    };
    let session_id = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
    eprintln!(
        "{}",
        format_session_accept_line(session_id, &hello.client_name, hello.protocol_version)
    );

    let welcome = build_welcome_for_session(session_id);
    vbox_proto::write_frame(&mut w, &Message::Welcome(welcome))?;

    // Main loop. Ping remains cheap; ViewRequest hands this connection to the
    // Wayland compositor loop and streams WindowEvent/FrameTile messages.
    loop {
        let msg = match vbox_proto::read_frame(&mut r) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("session {session_id}: read error: {e:#}");
                return Ok(());
            }
        };
        let decision = decide_handle_action(&msg);
        match (decision, msg) {
            (HandleDecision::Pong { seq }, _) => {
                let pong = Pong {
                    seq,
                    stamp_ns: now_ns(),
                };
                vbox_proto::write_frame(&mut w, &Message::Pong(pong))?;
            }
            (HandleDecision::Close { reason }, _) => {
                eprintln!("session {session_id}: client said goodbye: {reason}");
                return Ok(());
            }
            (HandleDecision::StartView, Message::ViewRequest(req)) => {
                eprintln!(
                    "session {session_id}: starting Wayland view on WAYLAND_DISPLAY={}",
                    req.socket_name
                );
                let io = wayland_io_from_tcp(r, w)?;
                return start_wayland_view(req, io);
            }
            (HandleDecision::ForwardInput, Message::InputEvent(event)) => {
                if !forward_input_event(event) && debug_enabled() {
                    eprintln!("session {session_id}: dropping input event without active view");
                }
            }
            (HandleDecision::ForwardClipboard, Message::Clipboard(payload)) => {
                if debug_enabled() {
                    eprintln!(
                        "trace clip: server.main.recv session={session_id} origin={:?} serial={} bytes={}",
                        payload.origin,
                        payload.serial,
                        payload.text.len()
                    );
                }
                if !forward_clipboard_event(payload) && debug_enabled() {
                    eprintln!(
                        "trace clip: server.main.forward drop=no_session session={session_id}"
                    );
                }
            }
            (HandleDecision::ForwardVolume, Message::VolumeChange(change)) => {
                // Volume is a host-side control signal, not bound to a
                // compositor session — apply unconditionally, fire-and-forget.
                forward_volume_event(change);
            }
            (HandleDecision::Ignore { kind }, _) => {
                // Anything else is a future-feature client talking to a current
                // server. We log and ignore rather than disconnect — clients
                // should only emit features they negotiated, but be defensive.
                eprintln!("session {session_id}: ignoring unsupported {kind:?}");
            }
            // The decision/message arms above pair 1:1 by construction. If a
            // future variant lands without updating both, refuse to silently
            // proceed.
            (decision, msg) => {
                eprintln!(
                    "session {session_id}: dispatcher/message mismatch: decision={decision:?} kind={:?}",
                    msg.kind()
                );
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn forward_input_event(event: vbox_proto::InputEvent) -> bool {
    wayland_session::send_input(event)
}

#[cfg(not(target_os = "linux"))]
fn forward_input_event(_event: vbox_proto::InputEvent) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn forward_clipboard_event(payload: vbox_proto::Clipboard) -> bool {
    wayland_session::send_clipboard(payload)
}

#[cfg(not(target_os = "linux"))]
fn forward_clipboard_event(_payload: vbox_proto::Clipboard) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn forward_volume_event(change: vbox_proto::VolumeChange) {
    volume::apply_volume(change, debug_enabled());
}

#[cfg(not(target_os = "linux"))]
fn forward_volume_event(_change: vbox_proto::VolumeChange) {
    // No wpctl on non-Linux hosts; volume control is a guest-side concern.
}

#[cfg(target_os = "linux")]
fn wayland_io_from_tcp(
    r: BufReader<TcpStream>,
    w: BufWriter<TcpStream>,
) -> Result<wayland_session::WaylandIo> {
    wayland_session::WaylandIo::from_tcp(r, w)
}

#[cfg(target_os = "linux")]
fn start_wayland_view(req: ViewRequest, io: wayland_session::WaylandIo) -> Result<()> {
    wayland_session::run(req, io)
}

#[cfg(not(target_os = "linux"))]
fn wayland_io_from_tcp(
    _r: BufReader<TcpStream>,
    w: BufWriter<TcpStream>,
) -> Result<data_plane::router::WaylandIo> {
    data_plane::router::WaylandIo::from_tcp_writer_only(w)
}

#[cfg(not(target_os = "linux"))]
fn start_wayland_view(_req: ViewRequest, io: data_plane::router::WaylandIo) -> Result<()> {
    io.send(Message::Error(vbox_proto::ProtoError {
        code: 501,
        message: "Wayland view is only implemented for Linux guests".into(),
    }))
}

fn server_name() -> String {
    let os = uname_release().unwrap_or_else(|| "linux".into());
    format_server_name(env!("CARGO_PKG_VERSION"), &os)
}

/// Pure formatter for the Welcome.server_name string. Split out so tests
/// don't have to pin the in-tree CARGO_PKG_VERSION.
fn format_server_name(version: &str, os: &str) -> String {
    format!("vbox-server/{version} ({os}, Wayland-first)")
}

fn uname_release() -> Option<String> {
    // Cheap, no extra deps: read /etc/os-release if present.
    let s = std::fs::read_to_string("/etc/os-release").ok()?;
    parse_os_release_pretty_name(&s)
}

/// Extract the `PRETTY_NAME=` value from an `/etc/os-release` file body.
/// Returns the first match with surrounding double-quotes stripped, or
/// `None` if no such line exists. Pure string handling — no IO.
fn parse_os_release_pretty_name(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

pub(crate) fn debug_enabled() -> bool {
    debug_flag_from(crate::brand::env_var("VBOX_DEBUG").as_deref())
}

/// Pure VBOX_DEBUG parser. Splitting from `debug_enabled()` lets tests
/// pin the accepted truthy values without poking the process-wide env.
fn debug_flag_from(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "TRUE" | "yes" | "on"))
}

/// Encode the wire bytes a "refuse" sends — prelude + Goodbye(reason).
/// Tests use this to assert what a rejected client actually sees on the
/// socket; production wraps it in a TcpStream write.
fn encode_refuse(reason: &str) -> Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    vbox_proto::write_prelude(&mut buf)?;
    vbox_proto::write_frame(
        &mut buf,
        &Message::Goodbye(Goodbye {
            reason: reason.into(),
        }),
    )?;
    Ok(buf)
}

/// What the main loop in `handle()` should do with the next inbound
/// message. Splitting the decision out of the giant match lets us
/// assert each branch without standing up a TCP server.
#[derive(Debug, PartialEq, Eq)]
enum HandleDecision {
    /// Reply with `Message::Pong { seq, stamp_ns: <now> }`.
    Pong { seq: u64 },
    /// Client sent Goodbye — close the session cleanly.
    Close { reason: String },
    /// Client requested the Wayland view — switch to view mode.
    StartView,
    /// Inbound input event — forward to the active view (if any).
    ForwardInput,
    /// Inbound clipboard payload — forward to the active view (if any).
    ForwardClipboard,
    /// Volume change — apply unconditionally, fire-and-forget.
    ForwardVolume,
    /// Anything else — log and ignore.
    Ignore { kind: vbox_proto::Kind },
}

/// Format the `session N: client='…' (proto vN)` line the server
/// emits on each accepted client. Pure formatter so the test pins
/// the shape ops scripts grep when correlating client+server logs.
fn format_session_accept_line(session_id: u64, client_name: &str, proto: u16) -> String {
    format!("session {session_id}: client='{client_name}' (proto v{proto})")
}

/// Build the debug-startup log line emitted when `VBOX_DEBUG=1`. Pure
/// formatter so a test pins the operator-visible startup banner shape
/// without invoking main(). The line is grep-friendly: a single line
/// with `key=value` pairs for bind/port/max_clients.
fn format_startup_debug(bind: std::net::IpAddr, port: u16, max_clients: usize) -> String {
    format!("debug: bind={bind} port={port} max_clients={max_clients}")
}

/// Build the operator-visible "listening" banner. Two variants — plain
/// and with extra debug info — pinned so a future format change to the
/// listening line doesn't silently break ops scripts that grep for it.
fn format_listening_banner(addr: SocketAddr) -> String {
    format!("vbox-server listening on {addr}")
}

/// Construct the Welcome frame the server hands to each accepted
/// client. Pure helper so a test pins the protocol version, the
/// "Wayland-first" server name string, and the session-id passthrough
/// without standing up a TCP listener.
fn build_welcome_for_session(session_id: u64) -> Welcome {
    Welcome {
        protocol_version: PROTOCOL_VERSION,
        server_name: server_name(),
        session_id,
    }
}

/// Capacity decision for an incoming connection: accept it (and tell
/// the caller the new active count) or reject it because the server is
/// already at `max_clients`. Pure helper so the test pins both branches
/// without standing up a real TCP listener.
#[derive(Debug, PartialEq, Eq)]
enum AcceptDecision {
    Accept { active_after: usize },
    Reject { active_unchanged: usize },
}

fn decide_accept(current_active: usize, max_clients: usize) -> AcceptDecision {
    let attempted = current_active + 1;
    if attempted > max_clients {
        AcceptDecision::Reject {
            active_unchanged: current_active,
        }
    } else {
        AcceptDecision::Accept {
            active_after: attempted,
        }
    }
}

/// Pure dispatcher: pick the next action for the given inbound message.
/// `handle()` runs side effects (writing pongs, forwarding events); this
/// helper just picks the verb.
fn decide_handle_action(msg: &Message) -> HandleDecision {
    match msg {
        Message::Ping(Ping { seq, .. }) => HandleDecision::Pong { seq: *seq },
        Message::Goodbye(gb) => HandleDecision::Close {
            reason: gb.reason.clone(),
        },
        Message::ViewRequest(_) => HandleDecision::StartView,
        Message::InputEvent(_) => HandleDecision::ForwardInput,
        Message::Clipboard(_) => HandleDecision::ForwardClipboard,
        Message::VolumeChange(_) => HandleDecision::ForwardVolume,
        other => HandleDecision::Ignore { kind: other.kind() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    // ---- format_server_name -----------------------------------------------
    //
    // Story: the Welcome frame carries a human-readable server identity that
    // shows up in client logs. The string format is part of the operator
    // contract — change it and ops scripts that grep for "vbox-server/"
    // start breaking. Pin the shape, with examples for both Linux and
    // fallback paths.

    #[test]
    fn format_server_name_shape_matches_operator_contract() {
        let s = format_server_name("9.9.9", "Ubuntu 22.04 LTS");
        assert_eq!(s, "vbox-server/9.9.9 (Ubuntu 22.04 LTS, Wayland-first)");
    }

    #[test]
    fn format_server_name_embeds_fallback_os() {
        // When /etc/os-release is missing the caller passes "linux" — the
        // string still has to parse as `vbox-server/<ver> (<os>...)`.
        let s = format_server_name("0.1.0", "linux");
        assert!(s.contains("vbox-server/0.1.0"));
        assert!(s.contains("(linux, Wayland-first)"));
    }

    // ---- parse_os_release_pretty_name -------------------------------------
    //
    // Story: stable identification on Debian/Ubuntu/Arch differs only in
    // os-release's PRETTY_NAME line. We strip the quotes and return the
    // value; missing key → None so the caller falls back to "linux".

    #[test]
    fn parse_pretty_name_extracts_quoted_value() {
        let body = "NAME=\"Ubuntu\"\nID=ubuntu\nPRETTY_NAME=\"Ubuntu 22.04.4 LTS\"\nVERSION_ID=\"22.04\"\n";
        assert_eq!(
            parse_os_release_pretty_name(body),
            Some("Ubuntu 22.04.4 LTS".to_string())
        );
    }

    #[test]
    fn parse_pretty_name_returns_none_when_key_missing() {
        let body = "NAME=Arch\nID=arch\nBUILD_ID=rolling\n";
        assert!(parse_os_release_pretty_name(body).is_none());
    }

    #[test]
    fn parse_pretty_name_keeps_unquoted_value() {
        // Some distros omit the quotes; the trim_matches('"') no-ops then.
        let body = "PRETTY_NAME=Fedora\n";
        assert_eq!(
            parse_os_release_pretty_name(body),
            Some("Fedora".to_string())
        );
    }

    #[test]
    fn parse_pretty_name_returns_first_match() {
        // If a misconfigured file has two PRETTY_NAME= lines (real-world
        // accident: editor appended a duplicate), we take the first. That
        // matches `head -n 1 | grep` ops shorthand.
        let body = "PRETTY_NAME=\"first\"\nPRETTY_NAME=\"second\"\n";
        assert_eq!(
            parse_os_release_pretty_name(body),
            Some("first".to_string())
        );
    }

    // ---- debug_flag_from --------------------------------------------------
    //
    // Story: VBOX_DEBUG is the one toggle ops uses to crank up the verbose
    // log. We accept the common truthy spellings and treat anything else
    // (including unset) as off.

    #[test]
    fn debug_flag_unset_is_off() {
        assert!(!debug_flag_from(None));
    }

    #[test]
    fn debug_flag_truthy_values_are_on() {
        for v in ["1", "true", "TRUE", "yes", "on"] {
            assert!(
                debug_flag_from(Some(v)),
                "VBOX_DEBUG={v:?} should turn debug on"
            );
        }
    }

    #[test]
    fn debug_flag_other_values_are_off() {
        // Be strict — only the documented spellings flip the bit. A typo
        // like "True" or "Yes" stays off so a future patch can broaden
        // without churn.
        for v in ["", "0", "false", "no", "off", "True", "Yes"] {
            assert!(
                !debug_flag_from(Some(v)),
                "VBOX_DEBUG={v:?} should stay off"
            );
        }
    }

    // ---- encode_refuse ----------------------------------------------------
    //
    // Story: when the server is at max_clients, the connection still has to
    // see a real protocol error rather than a TCP reset — that's how the
    // client surfaces "server refused" instead of "connection lost". The
    // bytes we write must round-trip through read_prelude + read_frame and
    // produce exactly a Goodbye with the supplied reason.

    #[test]
    fn refuse_bytes_round_trip_as_prelude_plus_goodbye() {
        let bytes =
            encode_refuse("server at max_clients").expect("encoding the refuse must succeed");
        assert!(
            !bytes.is_empty(),
            "refuse should produce a non-empty wire payload"
        );

        // Read it back through the same protocol decoder the client uses.
        let cursor = std::io::Cursor::new(&bytes);
        let mut r = BufReader::new(cursor);
        vbox_proto::read_prelude(&mut r).expect("prelude must be valid");
        match vbox_proto::read_frame(&mut r).expect("frame after prelude must decode") {
            Message::Goodbye(gb) => assert_eq!(gb.reason, "server at max_clients"),
            other => panic!("expected Goodbye, got {:?}", other.kind()),
        }
    }

    #[test]
    fn refuse_bytes_carry_arbitrary_reason() {
        // Whatever reason we pass in must survive verbatim — operators rely
        // on the string when correlating client failures with server logs.
        let bytes =
            encode_refuse("nope: invalid handshake (-EPROTO)").expect("encoding must succeed");
        let mut r = BufReader::new(std::io::Cursor::new(&bytes));
        vbox_proto::read_prelude(&mut r).unwrap();
        let frame = vbox_proto::read_frame(&mut r).unwrap();
        match frame {
            Message::Goodbye(gb) => {
                assert_eq!(gb.reason, "nope: invalid handshake (-EPROTO)");
            }
            other => panic!("expected Goodbye, got {:?}", other.kind()),
        }
    }

    // ---- decide_accept ---------------------------------------------------
    //
    // Story: every accepted TCP connection bumps a shared `active` counter.
    // When the bump pushes us over `--max-clients`, we refuse politely and
    // leave the counter alone. The decision is pure — splitting it out
    // lets us pin both branches without a real TcpListener.

    #[test]
    fn accept_below_capacity_increments_active() {
        assert_eq!(
            decide_accept(0, 4),
            AcceptDecision::Accept { active_after: 1 }
        );
        assert_eq!(
            decide_accept(3, 4),
            AcceptDecision::Accept { active_after: 4 }
        );
    }

    #[test]
    fn accept_at_capacity_rejects_without_changing_count() {
        // Already at 4 with --max-clients=4. The 5th connection must
        // bounce and leave the count untouched so the rejection doesn't
        // perma-disable the slot.
        assert_eq!(
            decide_accept(4, 4),
            AcceptDecision::Reject {
                active_unchanged: 4
            }
        );
    }

    // ---- build_welcome_for_session ---------------------------------------
    //
    // Story: every accepted client gets back a Welcome with the
    // protocol version, the server identity string, and the unique
    // session id the daemon assigned. Pin the fields so a future
    // PROTOCOL_VERSION bump or server_name format tweak doesn't
    // silently break the client's parser.

    // ---- format_startup_debug / format_listening_banner -----------------
    //
    // Story: ops scripts grep the server log for "listening on" and
    // "debug: bind=" lines to confirm startup. Pin the exact shapes
    // so a future cosmetic edit doesn't silently break those tools.

    // ---- format_session_accept_line ------------------------------------

    #[test]
    fn session_accept_line_has_session_id_client_name_and_proto() {
        let line = format_session_accept_line(7, "vbox-client/0.1.0", 12);
        assert_eq!(line, "session 7: client='vbox-client/0.1.0' (proto v12)");
    }

    #[test]
    fn session_accept_line_preserves_quoted_client_name() {
        // The quoting around the client name is the marker that the
        // tail of the line is the client_name field, not free-form
        // text. Pin it so a future tweak doesn't strip the quotes.
        let line = format_session_accept_line(1, "name with spaces", 1);
        assert!(line.contains("client='name with spaces'"));
    }

    #[test]
    fn listening_banner_carries_addr() {
        let addr: SocketAddr = "127.0.0.1:5710".parse().unwrap();
        let banner = format_listening_banner(addr);
        assert!(banner.contains("vbox-server listening on"));
        assert!(banner.contains("127.0.0.1:5710"));
    }

    #[test]
    fn startup_debug_carries_every_field_as_key_value() {
        let line = format_startup_debug("0.0.0.0".parse().unwrap(), 5710, 8);
        // Ops greps for the prefix and individual fields. Pin all three.
        assert!(line.starts_with("debug: "));
        assert!(line.contains("bind=0.0.0.0"));
        assert!(line.contains("port=5710"));
        assert!(line.contains("max_clients=8"));
    }

    #[test]
    fn welcome_carries_protocol_version_and_session_id() {
        let w = build_welcome_for_session(42);
        assert_eq!(w.protocol_version, PROTOCOL_VERSION);
        assert_eq!(w.session_id, 42);
    }

    #[test]
    fn welcome_server_name_advertises_wayland_first() {
        let w = build_welcome_for_session(1);
        assert!(
            w.server_name.starts_with("vbox-server/"),
            "server_name was: {}",
            w.server_name
        );
        assert!(
            w.server_name.contains("Wayland-first"),
            "server_name was: {}",
            w.server_name
        );
    }

    #[test]
    fn accept_with_zero_max_rejects_immediately() {
        // Edge case: `--max-clients 0` accepts nobody. The first
        // connection bounces immediately.
        assert_eq!(
            decide_accept(0, 0),
            AcceptDecision::Reject {
                active_unchanged: 0
            }
        );
    }

    // ---- decide_handle_action ---------------------------------------------
    //
    // Story: `handle()` reads frames in a loop and runs one of seven
    // behaviours per frame. The pure dispatcher pins which behaviour each
    // message kind picks. Operators rely on these mappings (e.g. Goodbye
    // closes the session immediately, ViewRequest hands off to the
    // Wayland compositor) and a typo here would silently flip a feature.

    #[test]
    fn ping_routes_to_pong_with_same_seq() {
        let msg = Message::Ping(Ping {
            seq: 42,
            stamp_ns: 1_000,
        });
        assert_eq!(decide_handle_action(&msg), HandleDecision::Pong { seq: 42 });
    }

    #[test]
    fn goodbye_routes_to_close_with_reason() {
        let msg = Message::Goodbye(Goodbye {
            reason: "client shutdown".into(),
        });
        assert_eq!(
            decide_handle_action(&msg),
            HandleDecision::Close {
                reason: "client shutdown".into()
            }
        );
    }

    #[test]
    fn view_request_routes_to_start_view() {
        let msg = Message::ViewRequest(ViewRequest {
            socket_name: "wayland-0".into(),
            width: 1024,
            height: 768,
        });
        assert_eq!(decide_handle_action(&msg), HandleDecision::StartView);
    }

    #[test]
    fn input_event_routes_to_forward_input() {
        let msg = Message::InputEvent(vbox_proto::InputEvent::PointerMotion { id: 1, x: 0, y: 0 });
        assert_eq!(decide_handle_action(&msg), HandleDecision::ForwardInput);
    }

    #[test]
    fn clipboard_routes_to_forward_clipboard() {
        let msg = Message::Clipboard(vbox_proto::Clipboard {
            origin: vbox_proto::ClipboardOrigin::Host,
            serial: 0,
            text: "hi".into(),
        });
        assert_eq!(decide_handle_action(&msg), HandleDecision::ForwardClipboard);
    }

    #[test]
    fn volume_change_routes_to_forward_volume() {
        let msg = Message::VolumeChange(vbox_proto::VolumeChange {
            level: 0.5,
            muted: false,
        });
        assert_eq!(decide_handle_action(&msg), HandleDecision::ForwardVolume);
    }

    #[test]
    fn unsupported_messages_route_to_ignore_with_kind() {
        // A Pong from a client (impossible in practice, but defensive) is
        // not the kind of frame the server consumes — log and drop.
        let msg = Message::Pong(Pong {
            seq: 1,
            stamp_ns: 0,
        });
        assert_eq!(
            decide_handle_action(&msg),
            HandleDecision::Ignore {
                kind: vbox_proto::Kind::Pong
            }
        );
    }

    // ---- now_ns -----------------------------------------------------------

    #[test]
    fn now_ns_is_monotonically_non_zero_on_modern_clocks() {
        // We can't easily pin the value, but it must be a real Unix
        // nanosecond timestamp — a UNIX epoch read on any machine post-1970
        // gives a non-zero u64. Two consecutive reads on a working clock
        // should never go backwards.
        let a = now_ns();
        let b = now_ns();
        assert!(a > 0, "now_ns should be after the Unix epoch");
        assert!(b >= a, "consecutive now_ns reads must be monotonic-ish");
    }
}
