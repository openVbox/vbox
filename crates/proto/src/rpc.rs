//! Control plane RPC (daemon).
//!
//! The control daemon (`vbox-controld`) listens on its own TCP port and
//! answers request/response RPCs from the host CLI. Both sides reuse the
//! existing framing + Hello/Welcome handshake; control traffic is just two new
//! Message variants (RpcRequest / RpcResponse) that wrap method-specific
//! payloads. The daemon owns instance lifecycle as the single source of truth,
//! replacing per-call `ssh ... bash -s` invocations that lose state when
//! pid_files go missing.

use serde::{Deserialize, Serialize};

use crate::data_plane::BootstrapBundle;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcRequest {
    pub id: u64,
    pub method: RpcMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcResponse {
    pub id: u64,
    pub result: RpcResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RpcMethod {
    /// First call on every connection. Must succeed before the daemon will
    /// accept any other method. Phase 2: replace with mTLS.
    Authenticate {
        token: String,
    },
    Status,
    StartInstance {
        instance: String,
        port: u16,
        debug: bool,
        quic_bind: Option<std::net::IpAddr>,
        quic_port: Option<u16>,
        quic_token: Option<String>,
    },
    StopInstance {
        instance: String,
    },
    LaunchApp {
        instance: String,
        socket: String,
        argv: Vec<String>,
        /// Extend the post-spawn fail-fast window from the default 120ms to
        /// 120ms + this many ms. Lets the daemon catch apps that connect to
        /// Wayland, fail handshake, and exit within a few hundred ms — a much
        /// more decisive "did the launch succeed" signal than the raw
        /// spawn-then-return path. Set to 0 to keep the original fast behavior.
        wait_ready_ms: u64,
    },
    WaitSocket {
        socket: String,
        timeout_ms: u64,
    },
    PrepareDataPlane {
        instance: Option<String>,
        tcp_addr: std::net::SocketAddr,
        quic_addr: Option<std::net::SocketAddr>,
        session_token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RpcResult {
    Ok(RpcOk),
    Err(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RpcOk {
    Authenticated,
    Status(StatusReply),
    InstanceStarted { pid: u32 },
    InstanceStopped,
    AppLaunched { pid: u32 },
    SocketReady,
    DataPlanePrepared(BootstrapBundle),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusReply {
    pub daemon_pid: u32,
    pub instances: Vec<InstanceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceSummary {
    pub instance: String,
    pub port: u16,
    pub server_pid: u32,
    pub app_pids: Vec<u32>,
}
