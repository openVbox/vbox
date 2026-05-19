//! TCP data-plane compatibility module.
//!
//! The production accept loop still lives in `main.rs` for now so the
//! rollout keeps behavior stable. This module marks the TCP path as the
//! baseline implementation beside the new QUIC listener.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TcpDataPlane {
    pub(crate) enabled: bool,
}

impl TcpDataPlane {
    #[allow(dead_code)]
    pub(crate) const fn enabled() -> Self {
        Self { enabled: true }
    }
}
