//! Command-line parsing for `vbox-client`.
//!
//! Pure clap definitions: every subcommand is dispatched from `main.rs` to
//! a function in `net`, `launch`, or `ctl`. Keeping these types here keeps
//! the binary entrypoint free of argument-shape noise.
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

const DEFAULT_INPUT_WINDOW_ID: u64 = 1;

#[derive(Parser, Debug)]
#[command(name = "vbox-client", version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Cmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Cmd {
    /// Connect, handshake, send one Ping, print round-trip time, exit.
    Ping {
        addr: SocketAddr,
        #[arg(long, default_value_t = 5)]
        timeout_secs: u64,
        #[arg(long, value_enum, default_value = "tcp-only")]
        data_plane: DataPlaneModeArg,
        #[arg(long)]
        quic_addr: Option<SocketAddr>,
        #[arg(long)]
        quic_token: Option<String>,
        #[arg(long)]
        quic_cert_sha256: Option<String>,
    },
    /// Connect, handshake, then read commands from stdin: "ping", "quit".
    Connect { addr: SocketAddr },
    /// Connect, request the Wayland view stream, and display the first toplevel.
    View {
        addr: SocketAddr,
        #[arg(long, default_value = "vbox-0")]
        socket_name: String,
        #[arg(long, default_value_t = 1024)]
        width: u32,
        #[arg(long, default_value_t = 768)]
        height: u32,
        #[arg(long, value_enum, default_value = "tcp-only")]
        data_plane: DataPlaneModeArg,
        #[arg(long)]
        quic_addr: Option<SocketAddr>,
        #[arg(long)]
        quic_token: Option<String>,
        #[arg(long)]
        quic_cert_sha256: Option<String>,
    },
    /// Send a synthetic input event to a Wayland view window id.
    Input {
        addr: SocketAddr,
        #[arg(long, value_enum, default_value = "tcp-only")]
        data_plane: DataPlaneModeArg,
        #[arg(long)]
        quic_addr: Option<SocketAddr>,
        #[arg(long)]
        quic_token: Option<String>,
        #[arg(long)]
        quic_cert_sha256: Option<String>,
        #[arg(long, default_value_t = DEFAULT_INPUT_WINDOW_ID)]
        id: u64,
        #[command(subcommand)]
        event: InputCmd,
    },
    /// Bring up an isolated per-app backend via `vbox prepare-app`,
    /// then run the viewer. Used by the installed launcher .apps; the
    /// binary also enters this mode automatically when invoked from a
    /// launcher bundle with no CLI args (the bundle's Resources/ supplies
    /// the inputs).
    Launch {
        /// Absolute path to the `vbox` helper script.
        #[arg(long)]
        vbox_native: PathBuf,
        /// `xdg_toplevel.app_id` of the guest app, e.g. `org.gnome.Calculator`.
        #[arg(long)]
        app_id: String,
        /// Instance name. Defaults to a sanitized form of `app_id`.
        #[arg(long)]
        instance: Option<String>,
    },
    /// Control-plane RPC client — talk to `vbox-controld` on the guest.
    Ctl {
        #[command(subcommand)]
        cmd: crate::ctl::CtlCmd,
    },
    /// One-shot Mac→Linux volume push. Intended for manual testing of the
    /// volume path; the viewer's `volume` worker covers continuous sync.
    Volume {
        addr: SocketAddr,
        #[arg(long, value_enum, default_value = "tcp-only")]
        data_plane: DataPlaneModeArg,
        #[arg(long)]
        quic_addr: Option<SocketAddr>,
        #[arg(long)]
        quic_token: Option<String>,
        #[arg(long)]
        quic_cert_sha256: Option<String>,
        /// Target level, 0..=100 (mirrors the macOS HUD scale; converted
        /// to the 0.0..=1.0 wire scalar on send).
        #[arg(long)]
        level: u8,
        /// Push mute=true with this frame.
        #[arg(long, conflicts_with = "unmuted")]
        muted: bool,
        /// Push mute=false with this frame.
        #[arg(long, conflicts_with = "muted")]
        unmuted: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DataPlaneModeArg {
    TcpOnly,
    QuicOnly,
    Auto,
}

impl From<DataPlaneModeArg> for vbox_proto::DataPlaneMode {
    fn from(value: DataPlaneModeArg) -> Self {
        match value {
            DataPlaneModeArg::TcpOnly => Self::TcpOnly,
            DataPlaneModeArg::QuicOnly => Self::QuicOnly,
            DataPlaneModeArg::Auto => Self::Auto,
        }
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum InputCmd {
    Motion {
        x: i32,
        y: i32,
    },
    Click {
        x: i32,
        y: i32,
    },
    Drag {
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
    },
    Text {
        text: Vec<String>,
    },
    Preedit {
        text: Vec<String>,
    },
    Key {
        keycode: u32,
    },
}
