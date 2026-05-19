// vbox-controld — guest-side control daemon.
//
// Listens for RPC connections from the host CLI (`vbox`) and owns instance
// lifecycle (server spawn, app launch, socket wait, stop). Replaces the
// per-call `ssh ... bash -s` pattern that lost track of state whenever a
// pid_file disappeared — daemon memory is the single source of truth.
//
// Auth:
//   - Shared-secret token (Authenticate RPC). Default, plain TCP.
//   - mTLS (opt-in). When `--tls-cert/--tls-key/--tls-client-ca` are all
//     provided, the listener becomes a TLS server, requires a client cert
//     signed by the configured CA, and skips the token check (the TLS layer
//     has already authenticated the peer). Both modes can coexist between
//     restarts; pick one per deployment.

mod dbus_session;
mod snapshot;
mod socket_wait;
mod state;
mod tls;
mod utils;

use anyhow::{Context, Result, bail};
use clap::Parser;
use state::DaemonState;
use std::fs;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tls::{CtlConn, ServerTls};
use vbox_proto::{
    BootstrapBundle, Goodbye, Message, PROTOCOL_VERSION, Ping, Pong, ProtoError, RpcMethod, RpcOk,
    RpcRequest, RpcResponse, RpcResult, StatusReply, TransportCapabilities, Welcome, read_frame,
    read_prelude, write_frame, write_prelude,
};

const DEFAULT_PORT: u16 = 5711;
const TOKEN_BYTES: usize = 32;

#[derive(Parser, Debug)]
#[command(name = "vbox-controld", version)]
struct Args {
    /// Address to bind. Default 0.0.0.0 so the host can reach the guest over
    /// Parallels host-only network. Tighten via systemd unit for prod.
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// TCP port. 5711 sits next to 5710 (viewer data plane).
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Path to the vbox-server binary this daemon will spawn.
    #[arg(long, default_value = "./target/release/vbox-server")]
    server_bin: PathBuf,

    /// Working directory for daemon-owned logs and runtime files.
    #[arg(long, default_value = "./.vbox-controld")]
    work_dir: PathBuf,

    /// Path to the shared-secret token file. Created with a fresh random
    /// hex token on first start; clients must present it via `Authenticate`
    /// before any other RPC method succeeds. Defaults to `<work_dir>/token`.
    /// Ignored when mTLS is enabled.
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// mTLS: server cert (PEM). When all three TLS flags are set, the
    /// listener requires a client cert signed by --tls-client-ca and the
    /// shared-secret check is skipped.
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    #[arg(long)]
    tls_key: Option<PathBuf>,
    #[arg(long)]
    tls_client_ca: Option<PathBuf>,
}

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn main() -> Result<()> {
    let args = Args::parse();
    let addr = SocketAddr::new(args.bind, args.port);
    let listener = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    let work_dir = args.work_dir.clone();

    let tls_server = match (&args.tls_cert, &args.tls_key, &args.tls_client_ca) {
        (Some(cert), Some(key), Some(ca)) => {
            let tls = ServerTls::load(cert, key, ca).with_context(|| "load TLS server config")?;
            eprintln!(
                "vbox-controld listening on {addr} (mTLS: cert={}, client-ca={})",
                cert.display(),
                ca.display()
            );
            Some(Arc::new(tls))
        }
        (None, None, None) => {
            eprintln!("vbox-controld listening on {addr} (plain TCP + shared-secret)");
            None
        }
        _ => bail!("--tls-cert, --tls-key, --tls-client-ca must all be set or all unset"),
    };

    // Token is required for plain-TCP mode. For mTLS it's irrelevant — TLS
    // verifies the peer cert — but we still create the file so the install
    // helper that always fetches it doesn't break.
    let token_path = resolve_token_path(args.token_file.clone(), &work_dir);
    let token = ensure_token(&token_path)?;
    eprintln!("auth: token file = {}", token_path.display());

    let state = Arc::new(DaemonState::new(args.server_bin, work_dir)?);

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };
        let session_id = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let state = Arc::clone(&state);
        let token = token.clone();
        let tls_server = tls_server.clone();
        std::thread::Builder::new()
            .name(format!("ctl-session-{session_id}"))
            .spawn(move || {
                let conn = match tls_server {
                    Some(tls) => match tls.accept(stream) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("session {session_id}: TLS handshake failed: {e:#}");
                            return;
                        }
                    },
                    None => CtlConn::Plain(stream),
                };
                if let Err(e) = handle_session(session_id, conn, &state, &token) {
                    eprintln!("session {session_id}: {e:#}");
                }
            })
            .context("spawning session thread")?;
    }
    Ok(())
}

/// Read the token from `path`, or generate a fresh one if the file doesn't
/// exist. Token file is chmod 0600 so the daemon can authenticate any client
/// that already has shell access to the same user account on this host.
fn ensure_token(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if path.exists() {
        let s = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let trimmed = s.trim().to_owned();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    let token = random_hex_token(TOKEN_BYTES)?;
    fs::write(path, &token).with_context(|| format!("write {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod {}", path.display()))?;
    Ok(token)
}

fn random_hex_token(byte_len: usize) -> Result<String> {
    use std::fmt::Write as _;
    use std::io::Read as _;

    let mut buf = vec![0u8; byte_len];
    let mut f = fs::File::open("/dev/urandom").context("open /dev/urandom")?;
    f.read_exact(&mut buf).context("read /dev/urandom")?;
    let mut hex = String::with_capacity(byte_len * 2);
    for b in &buf {
        write!(hex, "{b:02x}").expect("writing to String never fails");
    }
    Ok(hex)
}

fn handle_session(
    session_id: u64,
    mut conn: CtlConn,
    state: &Arc<DaemonState>,
    expected_token: &str,
) -> Result<()> {
    let peer = conn.peer_addr();
    let tls_mode = conn.is_tls();
    eprintln!("session {session_id}: accept from {peer:?} (tls={tls_mode})");

    read_prelude(&mut conn).context("reading client prelude")?;
    write_prelude(&mut conn).context("writing server prelude")?;

    let hello = match read_frame(&mut conn)? {
        Message::Hello(h) => h,
        other => bail!("expected Hello, got {:?}", other.kind()),
    };
    match check_protocol_version(&hello.client_name, hello.protocol_version) {
        HelloDecision::Accept => {}
        HelloDecision::Reject(err) => {
            write_frame(&mut conn, &Message::Error(err))?;
            bail!("protocol mismatch from {}", hello.client_name);
        }
    }
    write_frame(&mut conn, &Message::Welcome(build_welcome(session_id)))?;

    // mTLS handshake already proved the peer holds a client cert signed by our
    // CA. Skip the token check so the host doesn't have to also distribute the
    // shared secret in TLS deployments.
    let mut authenticated = tls_mode;
    loop {
        let msg = match read_frame(&mut conn) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("session {session_id}: read end: {e:#}");
                return Ok(());
            }
        };
        match decide_session_action(&msg) {
            SessionAction::Pong { seq, stamp_ns } => {
                write_frame(&mut conn, &Message::Pong(Pong { seq, stamp_ns }))?;
            }
            SessionAction::Close { reason } => {
                eprintln!("session {session_id}: client said goodbye: {reason}");
                return Ok(());
            }
            SessionAction::Rpc => {
                let Message::RpcRequest(req) = msg else {
                    unreachable!("decide_session_action only returns Rpc for RpcRequest")
                };
                let resp = dispatch(state, req, &mut authenticated, expected_token);
                write_frame(&mut conn, &Message::RpcResponse(resp))?;
            }
            SessionAction::Unexpected { kind } => {
                write_frame(&mut conn, &Message::Error(unexpected_message_error(kind)))?;
            }
        }
    }
}

fn dispatch(
    state: &DaemonState,
    req: RpcRequest,
    authenticated: &mut bool,
    expected_token: &str,
) -> RpcResponse {
    // Authenticate is the only method allowed pre-auth. Other methods get a
    // canned error so a peer that lacks the token can't read instance state
    // or spawn processes.
    let result = match req.method {
        RpcMethod::Authenticate { token } => {
            let outcome = authenticate(token.as_bytes(), expected_token.as_bytes());
            if outcome.is_ok() {
                *authenticated = true;
            }
            outcome
        }
        _ if !*authenticated => RpcResult::Err("not authenticated".to_owned()),
        RpcMethod::Status => RpcResult::Ok(RpcOk::Status(StatusReply {
            daemon_pid: std::process::id(),
            instances: state.summaries(),
        })),
        RpcMethod::StartInstance {
            instance,
            port,
            debug,
            quic_bind,
            quic_port,
            quic_token,
        } => {
            let label = format!("start_instance(instance={instance}, port={port})");
            match state.start_instance(
                instance,
                port,
                debug,
                quic_bind.map(|bind| state::QuicInstanceConfig {
                    bind,
                    port: quic_port,
                    token: quic_token.clone().unwrap_or_default(),
                }),
            ) {
                Ok(pid) => RpcResult::Ok(RpcOk::InstanceStarted { pid }),
                Err(e) => RpcResult::Err(format_err_chain(&e.context(label))),
            }
        }
        RpcMethod::StopInstance { instance } => {
            let label = format!("stop_instance(instance={instance})");
            match state.stop_instance(&instance) {
                Ok(()) => RpcResult::Ok(RpcOk::InstanceStopped),
                Err(e) => RpcResult::Err(format_err_chain(&e.context(label))),
            }
        }
        RpcMethod::LaunchApp {
            instance,
            socket,
            argv,
            wait_ready_ms,
        } => {
            let label = format!(
                "launch_app(instance={instance}, socket={socket}, argv0={:?}, wait_ready_ms={wait_ready_ms})",
                argv.first().map_or("", String::as_str)
            );
            let wait_ready = Duration::from_millis(wait_ready_ms);
            match state.launch_app(&instance, &socket, &argv, wait_ready) {
                Ok(pid) => RpcResult::Ok(RpcOk::AppLaunched { pid }),
                Err(e) => RpcResult::Err(format_err_chain(&e.context(label))),
            }
        }
        RpcMethod::WaitSocket { socket, timeout_ms } => {
            let label = format!("wait_socket(socket={socket}, timeout_ms={timeout_ms})");
            match state.wait_socket(&socket, Duration::from_millis(timeout_ms)) {
                Ok(()) => RpcResult::Ok(RpcOk::SocketReady),
                Err(e) => RpcResult::Err(format_err_chain(&e.context(label))),
            }
        }
        RpcMethod::PrepareDataPlane {
            instance,
            tcp_addr,
            quic_addr,
            session_token,
        } => RpcResult::Ok(RpcOk::DataPlanePrepared(BootstrapBundle {
            quic_server_cert_sha256: instance
                .as_deref()
                .and_then(|instance| state.quic_cert_sha256(instance).ok().flatten()),
            tcp_addr,
            quic_addr,
            session_token,
            capabilities: if quic_addr.is_some() {
                TransportCapabilities::quic_phase1()
            } else {
                TransportCapabilities::tcp()
            },
        })),
    };
    RpcResponse { id: req.id, result }
}

/// Build the operator-visible listening banner the daemon prints at
/// startup. Two variants — mTLS and plain. Pure formatter so a test
/// pins the wire log shape ops scripts grep.
#[allow(dead_code)] // exercised by tests; production inlines eprintln!
fn format_listening_banner(addr: SocketAddr, mode: &TlsListenMode) -> String {
    match mode {
        TlsListenMode::Mtls { cert, ca } => {
            format!(
                "vbox-controld listening on {addr} (mTLS: cert={}, client-ca={})",
                cert.display(),
                ca.display()
            )
        }
        TlsListenMode::Plain => {
            format!("vbox-controld listening on {addr} (plain TCP + shared-secret)")
        }
    }
}

/// Reduce TlsFlagsOutcome to the just-enough mode the banner formatter
/// needs. mTLS keeps cert + CA paths; plain carries no extras.
#[allow(dead_code)] // exercised by tests
pub(crate) enum TlsListenMode<'a> {
    Mtls { cert: &'a Path, ca: &'a Path },
    Plain,
}

/// Outcome of validating the daemon's TLS flag triple. `Mtls` means all
/// three flags are set; `Plain` means none of them are. Partial set is
/// surfaced as an error so a half-config can't quietly downgrade the
/// daemon to plain TCP.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TlsFlagsOutcome<'a> {
    Mtls {
        cert: &'a PathBuf,
        key: &'a PathBuf,
        ca: &'a PathBuf,
    },
    Plain,
    PartiallySet,
}

#[allow(dead_code)] // exercised by tests; production inlines the match
pub(crate) fn classify_tls_flags<'a>(
    cert: Option<&'a PathBuf>,
    key: Option<&'a PathBuf>,
    ca: Option<&'a PathBuf>,
) -> TlsFlagsOutcome<'a> {
    match (cert, key, ca) {
        (Some(cert), Some(key), Some(ca)) => TlsFlagsOutcome::Mtls { cert, key, ca },
        (None, None, None) => TlsFlagsOutcome::Plain,
        _ => TlsFlagsOutcome::PartiallySet,
    }
}

/// Resolve where the shared-secret token file should live: prefer the
/// `--token-file` CLI override, fall back to `<work_dir>/token`. Splitting
/// this out lets a test pin the precedence without poking the real
/// filesystem.
fn resolve_token_path(explicit: Option<PathBuf>, work_dir: &Path) -> PathBuf {
    explicit.unwrap_or_else(|| work_dir.join("token"))
}

/// Build the "context label" each RPC handler attaches to its anyhow
/// chain on failure. Pinning the format lets the host's `vbox ctl
/// <verb>` failure messages stay stable across daemon versions; the
/// labels show up in operator playbooks.
#[allow(dead_code)] // exercised by tests; production inlines the format! calls
fn rpc_failure_label(method: &RpcMethod) -> String {
    match method {
        RpcMethod::Authenticate { .. } => "authenticate".to_owned(),
        RpcMethod::Status => "status".to_owned(),
        RpcMethod::StartInstance { instance, port, .. } => {
            format!("start_instance(instance={instance}, port={port})")
        }
        RpcMethod::StopInstance { instance } => {
            format!("stop_instance(instance={instance})")
        }
        RpcMethod::LaunchApp {
            instance,
            socket,
            argv,
            wait_ready_ms,
        } => {
            format!(
                "launch_app(instance={instance}, socket={socket}, argv0={:?}, wait_ready_ms={wait_ready_ms})",
                argv.first().map_or("", String::as_str)
            )
        }
        RpcMethod::WaitSocket { socket, timeout_ms } => {
            format!("wait_socket(socket={socket}, timeout_ms={timeout_ms})")
        }
        RpcMethod::PrepareDataPlane { tcp_addr, .. } => {
            format!("prepare_data_plane(tcp_addr={tcp_addr})")
        }
    }
}

/// Serialise an anyhow chain so the host CLI can show every cause when it
/// prints the failure. `format!("{e:#}")` joins with ": " on one line, which
/// turns a 4-deep chain into a single hard-to-skim sentence; this writes the
/// root cause on the first line and each subsequent cause on its own line.
fn format_err_chain(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for (i, cause) in e.chain().enumerate() {
        if i == 0 {
            out.push_str(&cause.to_string());
        } else {
            out.push_str("\n  caused by: ");
            out.push_str(&cause.to_string());
        }
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Decide whether a client-supplied token matches the expected one. Pure,
/// constant-time comparison + the success/error mapping `dispatch` wires
/// into the RPC response. Splitting this out lets tests cover the two
/// branches without standing up a `DaemonState`.
fn authenticate(supplied: &[u8], expected: &[u8]) -> RpcResult {
    if constant_time_eq(supplied, expected) {
        RpcResult::Ok(RpcOk::Authenticated)
    } else {
        RpcResult::Err("authentication failed".to_owned())
    }
}

/// Outcome of the Hello protocol-version check.
#[derive(Debug, PartialEq, Eq)]
enum HelloDecision {
    Accept,
    Reject(ProtoError),
}

/// What the session main loop should do with the next inbound message.
/// Splitting the decision out of the giant match keeps `handle_session`
/// readable and lets a test pin every branch without standing up a
/// `DaemonState` + TCP listener.
#[derive(Debug, PartialEq, Eq)]
enum SessionAction {
    /// Reply with `Message::Pong { seq, stamp_ns }`.
    Pong { seq: u64, stamp_ns: u64 },
    /// Client sent Goodbye — close the session cleanly.
    Close { reason: String },
    /// Client sent an RpcRequest — dispatch and reply.
    Rpc,
    /// Anything else — emit an Error frame and keep the session open.
    Unexpected { kind: vbox_proto::Kind },
}

fn decide_session_action(msg: &Message) -> SessionAction {
    match msg {
        Message::Ping(Ping { seq, stamp_ns }) => SessionAction::Pong {
            seq: *seq,
            stamp_ns: *stamp_ns,
        },
        Message::Goodbye(Goodbye { reason }) => SessionAction::Close {
            reason: reason.clone(),
        },
        Message::RpcRequest(_) => SessionAction::Rpc,
        other => SessionAction::Unexpected { kind: other.kind() },
    }
}

/// Build the 400-coded `Error` frame the daemon writes when the client
/// sends a message that isn't allowed inside a session. The wording is
/// part of the wire contract (the client logs it verbatim); pin it.
fn unexpected_message_error(kind: vbox_proto::Kind) -> ProtoError {
    ProtoError {
        code: 400,
        message: format!("unexpected message kind: {kind:?}"),
    }
}

/// Build the Welcome frame the daemon sends after Hello validation.
/// Pinning the shape (server_name string + session_id passthrough) here
/// lets a test prove the operator-visible greeting doesn't drift on a
/// CARGO_PKG_VERSION bump or a struct-field rename.
fn build_welcome(session_id: u64) -> Welcome {
    Welcome {
        protocol_version: PROTOCOL_VERSION,
        server_name: format!("vbox-controld/{}", env!("CARGO_PKG_VERSION")),
        session_id,
    }
}

/// Compare the client's Hello against our protocol version. Splitting
/// out the version-check decision lets tests pin both sides of the
/// contract (accept and reject) without standing up a TCP stream. The
/// `_client_name` arg is ignored on the wire — it's accepted so future
/// log-format changes can include it without touching the call site.
fn check_protocol_version(_client_name: &str, client_proto: u16) -> HelloDecision {
    if client_proto == PROTOCOL_VERSION {
        HelloDecision::Accept
    } else {
        HelloDecision::Reject(ProtoError {
            code: 426,
            message: format!(
                "protocol version mismatch: client={client_proto}, daemon={PROTOCOL_VERSION}"
            ),
        })
    }
}

trait RpcResultExt {
    fn is_ok(&self) -> bool;
}
impl RpcResultExt for RpcResult {
    fn is_ok(&self) -> bool {
        matches!(self, RpcResult::Ok(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---- constant_time_eq -------------------------------------------------
    //
    // Story: the daemon compares the client-supplied token against the
    // expected token without leaking length information through early-exit
    // branches. The function must (a) only return true on byte-for-byte
    // equality and (b) return false on any difference.

    #[test]
    fn constant_time_eq_matches_identical_bytes() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_rejects_different_length_inputs() {
        // A length mismatch is the one shortcut we permit — different
        // lengths can never be equal regardless of content.
        assert!(!constant_time_eq(b"hello", b"hello-world"));
    }

    #[test]
    fn constant_time_eq_rejects_same_length_differing_bytes() {
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn constant_time_eq_treats_empty_slices_as_equal() {
        // Used when neither client nor daemon set a token. Equality is
        // structurally correct here; the surrounding `Authenticate` handler
        // is responsible for rejecting empty-token deployments.
        assert!(constant_time_eq(&[], &[]));
    }

    // ---- random_hex_token / ensure_token ----------------------------------
    //
    // Story: on first boot the daemon writes a fresh token to disk so the
    // installer can fetch it once. Subsequent boots read the existing token.
    //
    //   1. Token file missing → ensure_token mints one, persists it 0600,
    //      and returns the same string we now find on disk.
    //   2. Token file present and non-empty → ensure_token reads it back
    //      and does not rewrite the file (idempotent).
    //   3. Token file present but blank → ensure_token treats it as missing
    //      and mints a fresh token (so a `: > token` doesn't break the
    //      daemon at boot).

    #[test]
    fn random_hex_token_returns_lowercase_hex_of_expected_length() {
        let token = random_hex_token(TOKEN_BYTES).expect("urandom must be available in tests");
        assert_eq!(token.len(), TOKEN_BYTES * 2, "32 bytes → 64 hex chars");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex must be lowercase digits, got: {token}"
        );
    }

    #[test]
    fn random_hex_token_produces_different_values_across_calls() {
        // Sanity: not a statistical test, but identical calls should not
        // collide when reading from /dev/urandom.
        let a = random_hex_token(TOKEN_BYTES).unwrap();
        let b = random_hex_token(TOKEN_BYTES).unwrap();
        assert_ne!(a, b, "two urandom-derived tokens shouldn't collide");
    }

    #[test]
    fn ensure_token_creates_file_on_first_boot() {
        let dir = tempdir_for_test();
        let path = dir.path.join("token");
        assert!(!path.exists(), "precondition: file does not exist yet");

        let token = ensure_token(&path).expect("first-boot mint should succeed");

        assert!(path.exists(), "ensure_token must persist the token");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, token, "in-memory token must equal on-disk bytes");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(
            mode & 0o777,
            0o600,
            "token file must be 0600 (operator-private)",
        );
    }

    #[test]
    fn ensure_token_is_idempotent_on_subsequent_boots() {
        // After mint, a restart re-runs ensure_token and must read the same
        // string back — never roll a new one and break running clients.
        let dir = tempdir_for_test();
        let path = dir.path.join("token");
        let first = ensure_token(&path).unwrap();

        let second = ensure_token(&path).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn ensure_token_treats_blank_file_as_unwritten() {
        // Operator runs `: > /etc/vbox-controld/token` and restarts the
        // daemon. We mint a fresh token instead of returning the empty
        // string and authenticating every client.
        let dir = tempdir_for_test();
        let path = dir.path.join("token");
        std::fs::write(&path, "   \n").unwrap();

        let token = ensure_token(&path).unwrap();

        assert!(
            !token.trim().is_empty(),
            "blank file should be replaced with a fresh token"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, token);
    }

    #[test]
    fn ensure_token_creates_parent_directory_if_missing() {
        // The installer points --token-file at /etc/vbox-controld/token
        // before that directory exists. ensure_token creates the parent so
        // the operator doesn't need a separate `install -d` step.
        let dir = tempdir_for_test();
        let path = dir.path.join("nested").join("dir").join("token");

        let token = ensure_token(&path).unwrap();

        assert!(path.exists(), "nested directories must be created");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
    }

    // ---- format_err_chain -------------------------------------------------
    //
    // Story: when an RPC handler fails, the daemon serialises the full
    // anyhow chain so the host CLI prints every cause. The function should
    // put the root summary on the first line and each subsequent cause on
    // its own line — `format!("{e:#}")` joins with ": " which looks like a
    // long sentence and is hard to skim.

    #[test]
    fn format_err_chain_single_cause_is_one_line() {
        let e = anyhow::anyhow!("boom");
        let out = format_err_chain(&e);
        assert_eq!(out, "boom");
    }

    // ---- authenticate -----------------------------------------------------
    //
    // Story: dispatch routes the very first RPC (`Authenticate`) through
    // this helper. The two outcomes are the only thing a pre-auth peer can
    // see: Ok(Authenticated) when the bytes match, Err("authentication
    // failed") otherwise. The wording is part of the contract — operator
    // scripts grep the daemon log for it.

    #[test]
    fn authenticate_success_returns_authenticated_ok() {
        match authenticate(b"abcd1234", b"abcd1234") {
            RpcResult::Ok(RpcOk::Authenticated) => {}
            other => panic!("expected Ok(Authenticated), got {other:?}"),
        }
    }

    #[test]
    fn authenticate_failure_returns_canonical_error_message() {
        match authenticate(b"wrong", b"right") {
            RpcResult::Err(msg) => assert_eq!(msg, "authentication failed"),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn authenticate_failure_on_length_mismatch() {
        // constant_time_eq returns false on length difference; the wrapper
        // must also flip it to the Err arm — no panic, no auth bypass.
        match authenticate(b"short", b"longer-than-short") {
            RpcResult::Err(msg) => assert_eq!(msg, "authentication failed"),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    // ---- check_protocol_version ------------------------------------------
    //
    // Story: every connection starts with the client's Hello carrying its
    // protocol_version. If it matches PROTOCOL_VERSION we accept; mismatch
    // produces a 426 Error frame the client surfaces as "server refused".

    // ---- build_welcome ---------------------------------------------------

    // ---- decide_session_action -------------------------------------------
    //
    // Story: the daemon's main loop reads one frame per iteration and
    // either replies, closes, or dispatches to RPC. We pin the mapping
    // so a future variant addition forces an explicit branch update.

    // ---- rpc_failure_label -----------------------------------------------
    //
    // Story: when an RPC handler returns Err, dispatch wraps the
    // anyhow chain with a context line describing the failed call.
    // Operators correlate that prefix with playbook entries — so the
    // format is part of the documented contract.

    // ---- resolve_token_path ---------------------------------------------
    //
    // Story: the operator can pin the token path with `--token-file`, or
    // let the daemon default it to `<work_dir>/token`. The default has
    // to be a documented stable path because the install script grabs
    // it; the override has to win.

    // ---- classify_tls_flags --------------------------------------------
    //
    // Story: when ops starts the daemon, the TLS flag triple has three
    // legal shapes (all set, all unset, partial-set). The first two are
    // the deployment modes; the last is a misconfiguration we want to
    // catch loudly. The classifier is the gate.

    // ---- format_listening_banner ---------------------------------------

    #[test]
    fn listening_banner_includes_addr_and_mtls_paths() {
        let cert = PathBuf::from("/etc/vbox/server.pem");
        let ca = PathBuf::from("/etc/vbox/client-ca.pem");
        let banner = format_listening_banner(
            "127.0.0.1:5711".parse().unwrap(),
            &TlsListenMode::Mtls {
                cert: &cert,
                ca: &ca,
            },
        );
        assert!(banner.contains("127.0.0.1:5711"));
        assert!(banner.contains("mTLS"));
        assert!(banner.contains("/etc/vbox/server.pem"));
        assert!(banner.contains("/etc/vbox/client-ca.pem"));
    }

    #[test]
    fn listening_banner_plain_mode_advertises_shared_secret() {
        let banner =
            format_listening_banner("0.0.0.0:5711".parse().unwrap(), &TlsListenMode::Plain);
        assert!(banner.contains("0.0.0.0:5711"));
        assert!(banner.contains("plain TCP"));
        assert!(banner.contains("shared-secret"));
    }

    #[test]
    fn tls_flags_all_set_is_mtls() {
        let cert = PathBuf::from("/etc/vbox/server.pem");
        let key = PathBuf::from("/etc/vbox/server.key.pem");
        let ca = PathBuf::from("/etc/vbox/client-ca.pem");
        let out = classify_tls_flags(Some(&cert), Some(&key), Some(&ca));
        assert_eq!(
            out,
            TlsFlagsOutcome::Mtls {
                cert: &cert,
                key: &key,
                ca: &ca
            }
        );
    }

    #[test]
    fn tls_flags_all_unset_is_plain() {
        let out = classify_tls_flags(None, None, None);
        assert_eq!(out, TlsFlagsOutcome::Plain);
    }

    #[test]
    fn tls_flags_partial_is_misconfiguration() {
        let cert = PathBuf::from("/etc/vbox/server.pem");
        let key = PathBuf::from("/etc/vbox/server.key.pem");
        // Cert-only, key-only, missing one — every shape is partial.
        for (c, k, ca) in [
            (Some(&cert), None, None),
            (None, Some(&key), None),
            (Some(&cert), Some(&key), None),
            (Some(&cert), None, Some(&cert)),
            (None, Some(&key), Some(&cert)),
        ] {
            assert_eq!(classify_tls_flags(c, k, ca), TlsFlagsOutcome::PartiallySet);
        }
    }

    #[test]
    fn token_path_uses_explicit_override_when_set() {
        let explicit = PathBuf::from("/etc/vbox-controld/token");
        let resolved = resolve_token_path(Some(explicit.clone()), Path::new("/var/lib/vbox"));
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn token_path_defaults_to_work_dir_token() {
        let resolved = resolve_token_path(None, Path::new("/var/lib/vbox"));
        assert_eq!(resolved, PathBuf::from("/var/lib/vbox/token"));
    }

    #[test]
    fn token_path_default_works_with_relative_work_dir() {
        let resolved = resolve_token_path(None, Path::new(".vbox-controld"));
        assert_eq!(resolved, PathBuf::from(".vbox-controld/token"));
    }

    #[test]
    fn label_for_status_is_simple_marker() {
        assert_eq!(rpc_failure_label(&RpcMethod::Status), "status");
    }

    #[test]
    fn label_for_start_instance_carries_instance_and_port() {
        let label = rpc_failure_label(&RpcMethod::StartInstance {
            instance: "dev".into(),
            port: 5710,
            debug: false,
            quic_bind: None,
            quic_port: None,
            quic_token: None,
        });
        assert_eq!(label, "start_instance(instance=dev, port=5710)");
    }

    #[test]
    fn label_for_stop_instance_carries_instance() {
        let label = rpc_failure_label(&RpcMethod::StopInstance {
            instance: "dev".into(),
        });
        assert_eq!(label, "stop_instance(instance=dev)");
    }

    #[test]
    fn label_for_launch_app_includes_first_argv() {
        // Only the first argv element is shown — the rest is usually
        // boilerplate (flags, the app's own --args) and would bloat
        // the operator log.
        let label = rpc_failure_label(&RpcMethod::LaunchApp {
            instance: "dev".into(),
            socket: "wayland-0".into(),
            argv: vec!["gnome-calculator".into(), "--mode=advanced".into()],
            wait_ready_ms: 500,
        });
        assert!(label.contains("instance=dev"));
        assert!(label.contains("socket=wayland-0"));
        assert!(label.contains("argv0=\"gnome-calculator\""));
        assert!(label.contains("wait_ready_ms=500"));
    }

    #[test]
    fn label_for_launch_app_with_empty_argv_uses_empty_string() {
        // Defensive: dispatch refuses empty argv, but if it somehow
        // reaches us we don't want to panic on argv.first().unwrap().
        let label = rpc_failure_label(&RpcMethod::LaunchApp {
            instance: "dev".into(),
            socket: "wayland-0".into(),
            argv: vec![],
            wait_ready_ms: 0,
        });
        assert!(label.contains("argv0=\"\""), "got: {label}");
    }

    #[test]
    fn label_for_wait_socket_includes_timeout() {
        let label = rpc_failure_label(&RpcMethod::WaitSocket {
            socket: "wayland-0".into(),
            timeout_ms: 5_000,
        });
        assert_eq!(label, "wait_socket(socket=wayland-0, timeout_ms=5000)");
    }

    #[test]
    fn label_for_authenticate_is_simple_marker() {
        let label = rpc_failure_label(&RpcMethod::Authenticate {
            token: "secret".into(),
        });
        // Token must NOT leak into the label — the failure log shows
        // it verbatim to the operator and may end up in support
        // channels.
        assert_eq!(label, "authenticate");
    }

    #[test]
    fn session_action_ping_returns_pong_with_same_seq_and_stamp() {
        let msg = Message::Ping(Ping {
            seq: 5,
            stamp_ns: 12345,
        });
        assert_eq!(
            decide_session_action(&msg),
            SessionAction::Pong {
                seq: 5,
                stamp_ns: 12345
            }
        );
    }

    #[test]
    fn session_action_goodbye_returns_close_with_reason() {
        let msg = Message::Goodbye(Goodbye {
            reason: "client done".into(),
        });
        assert_eq!(
            decide_session_action(&msg),
            SessionAction::Close {
                reason: "client done".into()
            }
        );
    }

    #[test]
    fn session_action_rpc_request_returns_rpc() {
        let msg = Message::RpcRequest(RpcRequest {
            id: 1,
            method: RpcMethod::Status,
        });
        assert_eq!(decide_session_action(&msg), SessionAction::Rpc);
    }

    #[test]
    fn session_action_unexpected_pong_returns_unexpected_with_kind() {
        // A Pong from a client is structurally legal but semantically
        // wrong — the daemon never sends Ping. We must surface the
        // anomaly with the kind name so operator logs name the offender.
        let msg = Message::Pong(Pong {
            seq: 0,
            stamp_ns: 0,
        });
        assert_eq!(
            decide_session_action(&msg),
            SessionAction::Unexpected {
                kind: vbox_proto::Kind::Pong
            }
        );
    }

    // ---- unexpected_message_error ----------------------------------------

    #[test]
    fn unexpected_error_carries_400_and_kind_name() {
        let err = unexpected_message_error(vbox_proto::Kind::Pong);
        assert_eq!(err.code, 400);
        assert!(
            err.message.contains("unexpected message kind"),
            "message was: {}",
            err.message
        );
        assert!(err.message.contains("Pong"), "got: {}", err.message);
    }

    #[test]
    fn welcome_includes_protocol_version_and_session_id() {
        let w = build_welcome(42);
        assert_eq!(w.protocol_version, PROTOCOL_VERSION);
        assert_eq!(w.session_id, 42);
    }

    #[test]
    fn welcome_server_name_starts_with_daemon_label() {
        // Operator scripts grep the controld log for "vbox-controld/" so
        // they can correlate client-side errors with the daemon
        // version. Pin the prefix.
        let w = build_welcome(1);
        assert!(
            w.server_name.starts_with("vbox-controld/"),
            "server_name was: {}",
            w.server_name
        );
    }

    #[test]
    fn protocol_check_accepts_matching_version() {
        assert_eq!(
            check_protocol_version("vbox-client/0.1.0", PROTOCOL_VERSION),
            HelloDecision::Accept
        );
    }

    #[test]
    fn protocol_check_rejects_mismatched_version_with_426() {
        match check_protocol_version("vbox-client/0.1.0", PROTOCOL_VERSION.wrapping_sub(1)) {
            HelloDecision::Reject(err) => {
                assert_eq!(err.code, 426);
                assert!(err.message.contains("protocol version mismatch"));
                assert!(err.message.contains(&PROTOCOL_VERSION.to_string()));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn protocol_check_includes_both_versions_in_message() {
        // Operators debugging a version drift need both numbers in the
        // log so they can immediately tell which side is older.
        let result = check_protocol_version("any-client", 99);
        match result {
            HelloDecision::Reject(err) => {
                assert!(err.message.contains("client=99"));
                assert!(err.message.contains(&format!("daemon={PROTOCOL_VERSION}")));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn authenticate_empty_strings_match() {
        // Edge case: the empty-token deployment. The helper says they're
        // equal; the caller (`dispatch`) is the layer that decides whether
        // an empty expected_token is a valid deployment, not us.
        assert!(authenticate(b"", b"").is_ok());
    }

    #[test]
    fn format_err_chain_multi_cause_is_one_cause_per_line() {
        // Three-deep chain: top context + two underlying causes. We expect
        // three lines, the first being the outermost context.
        let inner: anyhow::Error = anyhow::anyhow!("fd=-1");
        let mid = inner.context("open /dev/null");
        let outer = mid.context("spawn worker");

        let out = format_err_chain(&outer);

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "three causes → three lines, got: {out:?}");
        assert_eq!(lines[0], "spawn worker");
        assert!(lines[1].starts_with("  caused by: open /dev/null"));
        assert!(lines[2].starts_with("  caused by: fd=-1"));
    }

    struct TempDir {
        path: PathBuf,
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    fn tempdir_for_test() -> TempDir {
        let base = std::env::temp_dir();
        let suffix = format!(
            "vbox-controld-main-{}-{}",
            std::process::id(),
            unique_counter()
        );
        let path = base.join(suffix);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    fn unique_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }
}
