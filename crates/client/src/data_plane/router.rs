use std::sync::mpsc;

use vbox_proto::Message;

/// Shared outbound channel shape for TCP fallback and QUIC reliable streams.
pub(crate) type OutboundRx = mpsc::Receiver<Message>;
