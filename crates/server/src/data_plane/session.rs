use std::sync::atomic::{AtomicU64, Ordering};

#[allow(dead_code)]
static QUIC_SESSION_COUNTER: AtomicU64 = AtomicU64::new(10_000);

#[allow(dead_code)]
pub(crate) fn next_quic_session_id() -> u64 {
    QUIC_SESSION_COUNTER.fetch_add(1, Ordering::SeqCst)
}
