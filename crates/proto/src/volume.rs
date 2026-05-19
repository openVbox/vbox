//! Mac→Linux master volume sync (unidirectional, push-only).
//!
//! macOS owns the truth: when the host's default-output volume or mute
//! state changes, the client sends one [`VolumeChange`] frame and the
//! server applies it to PipeWire's default sink. Guest→host is **not**
//! implemented — the protocol only flows one way, which sidesteps the
//! echo-loop problem clipboard has and keeps the server's
//! responsibilities purely "apply what the host says".
//!
//! Why a dedicated frame instead of multiplexing into an existing channel?
//! Volume is a control-plane signal — small, lossy-OK, and ratelimited
//! independently of input or window events. A separate variant lets us
//! debounce/throttle on the client side without coupling that policy to
//! the input pipeline (where every frame matters and ordering is sacred).
//!
//! ## Rate-limiting contract
//!
//! macOS fires its CoreAudio property listener once per HID tick during
//! a slider drag — roughly 60–120 Hz. Forwarding every tick would spawn
//! the same many `wpctl` processes on the guest, which is wasteful and
//! triggers audible volume-step staircases. Clients **must** coalesce
//! changes to no more than [`MAX_UPDATES_PER_SEC`] frames per second by
//! keeping only the most recent value during the debounce window.

use serde::{Deserialize, Serialize};

/// One discrete volume-state push.
///
/// `level` is the canonical linear scalar (0.0 = silent, 1.0 = unity);
/// the server clamps to `[0.0, 1.0]` before invoking the mixer so a
/// hostile or buggy client can't drive `wpctl` into rejection. `muted`
/// is the absolute mute state, not a toggle — the server always sets
/// the sink to exactly this value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolumeChange {
    /// Linear scalar 0.0..=1.0. Servers must clamp; values outside the
    /// range are not an error but should be treated as the nearest bound.
    pub level: f32,
    /// Absolute mute state for the default sink.
    pub muted: bool,
}

/// Upper bound on the wire frame rate the client should emit during a
/// continuous change (e.g. slider drag). 20 Hz keeps the visible volume
/// step smooth on the receiver while bounding the guest's `wpctl` spawn
/// rate to one process per 50 ms — well under what PipeWire's IPC layer
/// can absorb. Servers don't enforce this; it's a client-side budget.
pub const MAX_UPDATES_PER_SEC: u32 = 20;

/// Smallest level delta worth sending. macOS reports scalar volume as an
/// `f32` and even a paused listener can see sub-millivolt noise on the
/// same logical step — without a deadband we'd emit redundant frames
/// every poll. 0.005 (~0.5%) is below the visible threshold on every
/// stock macOS volume HUD increment (which step at 1/16 = 6.25%).
pub const LEVEL_EPSILON: f32 = 0.005;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epsilon_is_sub_step() {
        // macOS volume HUD steps are 1/16 ≈ 0.0625. Our epsilon must be
        // strictly smaller so we never collapse two distinct user steps
        // into one. Const-evaluated so a regression fails compilation.
        const _: () = assert!(LEVEL_EPSILON < 1.0 / 16.0);
    }

    #[test]
    fn rate_cap_is_realistic() {
        // 20 Hz × 50 ms per frame == 1 s. If anyone bumps the constant
        // without thinking, the const_assert forces them to read this
        // comment about the wpctl-per-tick budget on the guest.
        const _: () = assert!(MAX_UPDATES_PER_SEC >= 10 && MAX_UPDATES_PER_SEC <= 60);
    }
}
