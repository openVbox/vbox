//! Single-shot RPC call against `vbox-controld`.
//!
//! `call()` owns the full lifecycle: TCP connect → optional TLS wrap → prelude
//! → Hello/Welcome → optional shared-secret Authenticate → user RPC → Goodbye.
//! `rpc_round_trip` is the inner request/response unit reused by both the
//! Authenticate step and the user-visible method.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use vbox_proto::{
    Goodbye, Hello, Message, PROTOCOL_VERSION, RpcMethod, RpcOk, RpcRequest, RpcResult, read_frame,
    read_prelude, write_frame, write_prelude,
};

use rustls::{ClientConnection, StreamOwned};

use super::tls::load_client_tls;
use super::token::load_token;
use super::transport::CtlClient;

pub(super) fn call(addr: SocketAddr, method: RpcMethod) -> Result<RpcOk> {
    let stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).context("controld connect")?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let tls_cfg = load_client_tls()?;
    let mut conn = if let Some((cfg, server_name)) = tls_cfg {
        let client = ClientConnection::new(cfg, server_name).context("new ClientConnection")?;
        CtlClient::Tls(Box::new(StreamOwned::new(client, stream)))
    } else {
        CtlClient::Plain(stream)
    };
    let tls_mode = conn.is_tls();

    write_prelude(&mut conn)?;
    read_prelude(&mut conn)?;
    write_frame(&mut conn, &Message::Hello(build_ctl_hello()))?;
    match read_frame(&mut conn)? {
        Message::Welcome(_) => {}
        Message::Error(e) => bail!(
            "daemon error during handshake: {} (code {})",
            e.message,
            e.code
        ),
        other => bail!("expected Welcome, got {:?}", other.kind()),
    }

    // mTLS already authenticated us via the client cert; skip the token. In
    // plain mode the daemon refuses everything else until Authenticate passes.
    if !tls_mode {
        let token = load_token()?;
        rpc_round_trip(&mut conn, 1, RpcMethod::Authenticate { token })?;
    }

    let ok = rpc_round_trip(&mut conn, 2, method)?;
    let _ = write_frame(
        &mut conn,
        &Message::Goodbye(Goodbye {
            reason: "ctl done".to_owned(),
        }),
    );
    Ok(ok)
}

/// Build the Hello the ctl client sends to the daemon. The
/// `client_name` field follows the documented "vbox-client/<ver> (ctl)"
/// format — operator log scrapers grep for it.
fn build_ctl_hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: format!("vbox-client/{} (ctl)", env!("CARGO_PKG_VERSION")),
    }
}

fn rpc_round_trip<S: Read + Write>(stream: &mut S, id: u64, method: RpcMethod) -> Result<RpcOk> {
    write_frame(stream, &Message::RpcRequest(RpcRequest { id, method }))?;
    let resp = match read_frame(stream)? {
        Message::RpcResponse(r) => r,
        Message::Error(e) => bail!("daemon error: {} (code {})", e.message, e.code),
        other => bail!("expected RpcResponse, got {:?}", other.kind()),
    };
    match resp.result {
        RpcResult::Ok(ok) => Ok(ok),
        RpcResult::Err(msg) => Err(anyhow!(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vbox_proto::{ProtoError, RpcResponse, StatusReply};

    // ---- rpc_round_trip behaviour against scripted server bytes ----------
    //
    // Story: `vbox ctl status` writes one RpcRequest, then reads exactly one
    // RpcResponse off the same stream. We exercise four cases the daemon can
    // produce in real deployments:
    //   1. Ok response → returns the contained RpcOk.
    //   2. Err response → surfaces the daemon's error message.
    //   3. Daemon emits a top-level Message::Error before the response →
    //      surfaces "daemon error" with the code.
    //   4. Daemon emits some other frame (e.g. Goodbye) → surfaces a
    //      "expected RpcResponse, got X" error.

    /// In-memory full-duplex stream: reads from a pre-baked script, captures
    /// writes for later inspection. The script is the bytes the daemon would
    /// have emitted in response to our request.
    struct MockStream {
        script: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl MockStream {
        fn new(server_reply: Vec<u8>) -> Self {
            Self {
                script: std::io::Cursor::new(server_reply),
                written: Vec::new(),
            }
        }
    }

    impl Read for MockStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.script.read(buf)
        }
    }

    impl Write for MockStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.written.flush()
        }
    }

    fn encode_frame(msg: &Message) -> Vec<u8> {
        let mut buf = Vec::new();
        write_frame(&mut buf, msg).unwrap();
        buf
    }

    #[test]
    fn ok_response_unwraps_rpc_ok() {
        let response = Message::RpcResponse(RpcResponse {
            id: 7,
            result: RpcResult::Ok(RpcOk::Status(StatusReply {
                daemon_pid: 999,
                instances: vec![],
            })),
        });
        let mut stream = MockStream::new(encode_frame(&response));

        let ok = rpc_round_trip(&mut stream, 7, RpcMethod::Status).unwrap();

        assert!(matches!(ok, RpcOk::Status(s) if s.daemon_pid == 999));
        // Sanity: the request frame must have actually been written.
        assert!(
            !stream.written.is_empty(),
            "rpc_round_trip should have written the request before reading"
        );
    }

    #[test]
    fn err_response_surfaces_daemon_message() {
        let response = Message::RpcResponse(RpcResponse {
            id: 3,
            result: RpcResult::Err("no such instance: dev".to_owned()),
        });
        let mut stream = MockStream::new(encode_frame(&response));

        let err = rpc_round_trip(
            &mut stream,
            3,
            RpcMethod::StopInstance {
                instance: "dev".to_owned(),
            },
        )
        .expect_err("RpcResult::Err must propagate");

        assert_eq!(format!("{err}"), "no such instance: dev");
    }

    #[test]
    fn top_level_error_frame_surfaces_code_and_message() {
        let error = Message::Error(ProtoError {
            code: 426,
            message: "protocol version mismatch".into(),
        });
        let mut stream = MockStream::new(encode_frame(&error));

        let err = rpc_round_trip(&mut stream, 1, RpcMethod::Status)
            .expect_err("daemon Error frame must propagate");

        let msg = format!("{err}");
        assert!(msg.contains("daemon error"));
        assert!(msg.contains("protocol version mismatch"));
        assert!(msg.contains("426"));
    }

    // ---- build_ctl_hello -------------------------------------------------
    //
    // Story: every ctl invocation opens with a Hello carrying the
    // documented "vbox-client/<ver> (ctl)" client_name. The daemon logs
    // this verbatim, so operators rely on the "(ctl)" suffix to tell
    // RPC clients apart from viewer clients in the controld log.

    #[test]
    fn ctl_hello_carries_protocol_version() {
        let hello = build_ctl_hello();
        assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn ctl_hello_client_name_distinguishes_from_viewer() {
        let hello = build_ctl_hello();
        assert!(
            hello.client_name.starts_with("vbox-client/"),
            "client_name was: {}",
            hello.client_name
        );
        // The "(ctl)" suffix is the marker scrapers grep for.
        assert!(
            hello.client_name.ends_with("(ctl)"),
            "client_name must end with (ctl), got: {}",
            hello.client_name
        );
    }

    #[test]
    fn unexpected_frame_kind_surfaces_with_kind_name() {
        // The daemon hangs up early with a Goodbye instead of a response.
        // The client surfaces a "expected RpcResponse" error so logs
        // pinpoint the protocol divergence.
        let goodbye = Message::Goodbye(vbox_proto::Goodbye {
            reason: "boom".into(),
        });
        let mut stream = MockStream::new(encode_frame(&goodbye));

        let err = rpc_round_trip(&mut stream, 1, RpcMethod::Status)
            .expect_err("unexpected frame must error");

        let msg = format!("{err}");
        assert!(msg.contains("expected RpcResponse"), "got: {msg}");
    }
}
