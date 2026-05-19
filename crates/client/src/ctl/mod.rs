//! Host-side RPC client for `vbox-controld`. Pairs with the daemon
//! defined in `crates/controld`.
//!
//! Each subcommand opens a fresh TCP (or TLS) connection, does the proto
//! handshake, sends one RpcRequest, prints the result, and exits. Connection
//! pooling comes later if call latency becomes a bottleneck.
//!
//! Auth modes:
//!   - Shared-secret: default. Token from VBOX_CONTROL_TOKEN[_FILE].
//!   - mTLS: opt-in. When VBOX_TLS_CA/CERT/KEY are all set, the client wraps
//!     the TCP stream in a rustls ClientConnection that presents its own cert
//!     to the daemon, and skips the Authenticate RPC.
//!
//! ## Module map
//!
//! - [`call`] — full RPC lifecycle (connect → handshake → method → goodbye).
//! - [`tls`] — rustls config loading and the `tls-bootstrap` cert-minting helper.
//! - [`token`] — shared-secret token loader (env var or file).
//! - [`transport`] — `CtlClient` enum that unifies plain TCP and rustls streams.
//! - [`print`] — human-readable rendering of successful RPC replies.

mod call;
mod print;
mod tls;
mod token;
mod transport;

use anyhow::{Result, bail};
use clap::Subcommand;
use std::net::SocketAddr;
use std::path::PathBuf;
use vbox_proto::RpcMethod;

#[derive(Subcommand, Debug)]
pub enum CtlCmd {
    /// Show the daemon pid and running instances.
    Status { addr: SocketAddr },
    /// Spawn a vbox-server inside the daemon under INSTANCE on PORT.
    StartInstance {
        addr: SocketAddr,
        instance: String,
        port: u16,
        #[arg(long)]
        debug: bool,
        #[arg(long)]
        quic_bind: Option<std::net::IpAddr>,
        #[arg(long)]
        quic_port: Option<u16>,
        #[arg(long)]
        quic_token: Option<String>,
    },
    /// Terminate INSTANCE and any apps it launched.
    StopInstance { addr: SocketAddr, instance: String },
    /// Launch a guest app inside INSTANCE's Wayland session.
    LaunchApp {
        addr: SocketAddr,
        instance: String,
        socket: String,
        /// Extra fail-fast window in milliseconds on top of the daemon's
        /// built-in 120ms. The daemon polls try_wait during this window so
        /// Wayland/dbus handshake failures (which typically kill the app
        /// inside ~500ms) surface as an error instead of a phantom pid.
        /// Default 0 = keep the original behaviour.
        #[arg(long, default_value_t = 0)]
        wait_ready_ms: u64,
        /// Argv for the guest app (use `--` to terminate flag parsing).
        argv: Vec<String>,
    },
    /// Block until the Wayland socket appears (or TIMEOUT_MS elapses).
    WaitSocket {
        addr: SocketAddr,
        socket: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },
    /// Build a versioned data-plane bootstrap bundle for hybrid viewers.
    PrepareDataPlane {
        addr: SocketAddr,
        #[arg(long)]
        instance: Option<String>,
        tcp_addr: SocketAddr,
        #[arg(long)]
        quic_addr: Option<SocketAddr>,
        #[arg(long)]
        session_token: String,
    },
    /// Generate a CA + server cert + client cert for mTLS deployments.
    /// Idempotent: re-running reuses the CA and only refreshes leaf certs.
    TlsBootstrap {
        /// Hostnames/IPs the server cert should cover (SAN entries). Pass the
        /// guest's hostname and/or address. Example:
        ///   `--san vbox-guest --san 10.211.55.5`
        #[arg(long = "san", required = true)]
        sans: Vec<String>,
        /// Output directory (will be created). Server cert/key/CA are written
        /// here; copy them to the guest. Client cert/key/CA also live here for
        /// the host side.
        #[arg(long, default_value = ".vbox/tls")]
        out_dir: PathBuf,
    },
}

pub fn run(cmd: CtlCmd) -> Result<()> {
    let routed = route_ctl_cmd(cmd)?;
    let result = match routed {
        RoutedCmd::Rpc(addr, method) => call::call(addr, method),
        RoutedCmd::TlsBootstrap { sans, out_dir } => {
            tls::tls_bootstrap(&sans, &out_dir)?;
            return Ok(());
        }
    };
    match result {
        Ok(ok) => {
            print::print_ok(&ok);
            Ok(())
        }
        Err(e) => {
            let msg = format_ctl_error(&e);
            eprintln!("{msg}");
            std::process::exit(1);
        }
    }
}

/// Result of validating + reshaping a CtlCmd before it hits the network.
/// `run()` produces this, then either spends it on an RPC call or on the
/// local TLS-bootstrap helper. Splitting the validation out lets tests
/// pin every shape (empty argv, all happy paths) without standing up a
/// `controld` listener.
#[derive(Debug, PartialEq, Eq)]
enum RoutedCmd {
    Rpc(std::net::SocketAddr, RpcMethod),
    TlsBootstrap { sans: Vec<String>, out_dir: PathBuf },
}

fn route_ctl_cmd(cmd: CtlCmd) -> Result<RoutedCmd> {
    let routed = match cmd {
        CtlCmd::Status { addr } => RoutedCmd::Rpc(addr, RpcMethod::Status),
        CtlCmd::StartInstance {
            addr,
            instance,
            port,
            debug,
            quic_bind,
            quic_port,
            quic_token,
        } => RoutedCmd::Rpc(
            addr,
            RpcMethod::StartInstance {
                instance,
                port,
                debug,
                quic_bind,
                quic_port,
                quic_token,
            },
        ),
        CtlCmd::StopInstance { addr, instance } => {
            RoutedCmd::Rpc(addr, RpcMethod::StopInstance { instance })
        }
        CtlCmd::LaunchApp {
            addr,
            instance,
            socket,
            wait_ready_ms,
            argv,
        } => {
            if argv.is_empty() {
                bail!("launch-app requires at least one argv element");
            }
            RoutedCmd::Rpc(
                addr,
                RpcMethod::LaunchApp {
                    instance,
                    socket,
                    argv,
                    wait_ready_ms,
                },
            )
        }
        CtlCmd::WaitSocket {
            addr,
            socket,
            timeout_ms,
        } => RoutedCmd::Rpc(addr, RpcMethod::WaitSocket { socket, timeout_ms }),
        CtlCmd::PrepareDataPlane {
            addr,
            instance,
            tcp_addr,
            quic_addr,
            session_token,
        } => RoutedCmd::Rpc(
            addr,
            RpcMethod::PrepareDataPlane {
                instance,
                tcp_addr,
                quic_addr,
                session_token,
            },
        ),
        CtlCmd::TlsBootstrap { sans, out_dir } => RoutedCmd::TlsBootstrap { sans, out_dir },
    };
    Ok(routed)
}

/// Format an RPC failure the way `run()` writes it to stderr. The trick
/// is that daemon-side errors arrive as a single anyhow chain that may
/// already encode `\n  caused by:` lines (see `format_err_chain` on the
/// controld side); we must preserve that verbatim. Locally-raised errors
/// (TCP connect, TLS handshake) carry their own chain so we flatten them
/// with `{e:#}`. Pure formatter so tests can pin the exact bytes the
/// operator sees in their terminal.
fn format_ctl_error(err: &anyhow::Error) -> String {
    let msg = err.to_string();
    if msg.contains("\n  caused by:") {
        format!("controld error: {msg}")
    } else {
        format!("controld error: {err:#}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::str::FromStr;

    fn addr() -> SocketAddr {
        SocketAddr::from_str("127.0.0.1:5711").unwrap()
    }

    // Story: every `vbox ctl <verb>` invocation lands in `run()`. Before
    // it can hit the network we have to (a) translate the CtlCmd into an
    // RpcMethod, (b) validate the few fields clap can't (empty argv on
    // launch-app), and (c) for tls-bootstrap, route to the local helper
    // instead of a TCP call. Each test pins one of those branches.

    #[test]
    fn route_status_to_rpc_status() {
        let routed = route_ctl_cmd(CtlCmd::Status { addr: addr() }).unwrap();
        assert_eq!(routed, RoutedCmd::Rpc(addr(), RpcMethod::Status));
    }

    #[test]
    fn route_start_instance_carries_all_fields() {
        let routed = route_ctl_cmd(CtlCmd::StartInstance {
            addr: addr(),
            instance: "dev".into(),
            port: 5710,
            debug: true,
            quic_bind: None,
            quic_port: None,
            quic_token: None,
        })
        .unwrap();
        assert_eq!(
            routed,
            RoutedCmd::Rpc(
                addr(),
                RpcMethod::StartInstance {
                    instance: "dev".into(),
                    port: 5710,
                    debug: true,
                    quic_bind: None,
                    quic_port: None,
                    quic_token: None,
                }
            )
        );
    }

    #[test]
    fn route_stop_instance_carries_instance() {
        let routed = route_ctl_cmd(CtlCmd::StopInstance {
            addr: addr(),
            instance: "dev".into(),
        })
        .unwrap();
        assert_eq!(
            routed,
            RoutedCmd::Rpc(
                addr(),
                RpcMethod::StopInstance {
                    instance: "dev".into()
                }
            )
        );
    }

    #[test]
    fn route_launch_app_rejects_empty_argv() {
        // clap doesn't enforce non-empty argv (it's a trailing Vec<String>);
        // route_ctl_cmd is the one place that catches the operator typo
        // `vbox ctl launch-app addr inst sock` (no exec specified).
        let err = route_ctl_cmd(CtlCmd::LaunchApp {
            addr: addr(),
            instance: "dev".into(),
            socket: "wayland-0".into(),
            wait_ready_ms: 0,
            argv: vec![],
        })
        .expect_err("empty argv must fail validation");
        assert!(format!("{err}").contains("requires at least one argv element"));
    }

    #[test]
    fn route_launch_app_passes_through_when_argv_non_empty() {
        let routed = route_ctl_cmd(CtlCmd::LaunchApp {
            addr: addr(),
            instance: "dev".into(),
            socket: "wayland-0".into(),
            wait_ready_ms: 500,
            argv: vec!["gnome-calculator".into()],
        })
        .unwrap();
        assert_eq!(
            routed,
            RoutedCmd::Rpc(
                addr(),
                RpcMethod::LaunchApp {
                    instance: "dev".into(),
                    socket: "wayland-0".into(),
                    argv: vec!["gnome-calculator".into()],
                    wait_ready_ms: 500,
                }
            )
        );
    }

    #[test]
    fn route_wait_socket_carries_timeout() {
        let routed = route_ctl_cmd(CtlCmd::WaitSocket {
            addr: addr(),
            socket: "wayland-3".into(),
            timeout_ms: 10_000,
        })
        .unwrap();
        assert_eq!(
            routed,
            RoutedCmd::Rpc(
                addr(),
                RpcMethod::WaitSocket {
                    socket: "wayland-3".into(),
                    timeout_ms: 10_000,
                }
            )
        );
    }

    #[test]
    fn route_tls_bootstrap_to_local_helper() {
        // tls-bootstrap is the one verb that doesn't talk to controld —
        // it generates certs locally. The routed shape must reflect
        // that so `run()` calls the right entry point.
        let routed = route_ctl_cmd(CtlCmd::TlsBootstrap {
            sans: vec!["vbox-guest".into()],
            out_dir: PathBuf::from(".vbox/tls"),
        })
        .unwrap();
        assert_eq!(
            routed,
            RoutedCmd::TlsBootstrap {
                sans: vec!["vbox-guest".into()],
                out_dir: PathBuf::from(".vbox/tls"),
            }
        );
    }

    // ---- format_ctl_error -------------------------------------------------

    #[test]
    fn ctl_error_for_local_failure_uses_alt_format() {
        // A TCP connect failure wraps a couple of anyhow contexts via `?`
        // but doesn't carry the newline-bullet daemon format. The
        // formatter must use the `{:#}` alt-display to flatten it onto
        // one line for the user.
        let local: anyhow::Error = anyhow::anyhow!("connection refused")
            .context("connect 127.0.0.1:5711")
            .context("controld connect");
        let out = format_ctl_error(&local);
        assert!(out.starts_with("controld error: "));
        // The flattened form contains the outer context.
        assert!(out.contains("controld connect"));
        // No multi-line bullet form when the alt-display flattens it.
        assert!(!out.contains("\n  caused by:"));
    }

    #[test]
    fn ctl_error_for_daemon_chain_keeps_multiline_form() {
        // A daemon-side failure arrives pre-formatted (controld's
        // `format_err_chain` joins lines with "\n  caused by:"). The
        // host-side formatter must preserve that wording verbatim so the
        // operator sees the same lines the daemon logged.
        let daemon_msg = "start_instance(...) failed\n  caused by: spawn /usr/bin/vbox-server\n  caused by: No such file or directory";
        let err = anyhow::Error::msg(daemon_msg.to_string());
        let out = format_ctl_error(&err);
        assert_eq!(out, format!("controld error: {daemon_msg}"));
    }
}
