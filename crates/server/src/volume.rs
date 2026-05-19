//! Apply host-pushed volume changes to the guest's PipeWire default sink.
//!
//! Sits next to [`crate::wayland_session`] but is **not** part of it: the
//! Wayland compositor doesn't know or care about audio routing, and
//! `wpctl` is just a small synchronous subprocess that talks to
//! PipeWire's IPC. Keeping this module separate lets the wayland session
//! stay focused on surfaces / input, and lets volume work even before a
//! ViewRequest has bound a compositor.
//!
//! ## Why `wpctl` instead of native PipeWire IPC?
//!
//! `wpctl` is already on every PipeWire-shipping distro (it's the
//! reference CLI in `wireplumber`) and uses the same `WirePlumber`
//! policy as desktop sessions — including the "default sink" routing
//! that follows the user's last selection. Spawning a process per
//! change is heavier than holding a PipeWire connection, but the
//! client-side throttle keeps the rate to ~20 Hz max, which is well
//! inside the budget of fork+exec on modern hardware (~1ms).
//!
//! ## Failure policy
//!
//! Volume control is best-effort: a missing `wpctl` binary, a Dummy
//! Output sink, or a transient WirePlumber restart all cause the
//! command to fail. We log at debug level and continue — there is no
//! sensible "abort the session" outcome for a missed volume tick.

use std::process::{Command, Stdio};

use vbox_proto::VolumeChange;

/// Apply one `VolumeChange` to the guest's default audio sink. Always
/// returns; errors are surfaced via stderr only (no propagation up the
/// TCP loop because the host doesn't expect an ack for a control push).
///
/// `debug` is the server's existing flag — when set we narrate every
/// step so the operator can correlate host drags with guest applies.
pub fn apply_volume(change: VolumeChange, debug: bool) {
    let level = clamp_level(change.level);
    apply_level(level, debug);
    apply_mute(change.muted, debug);
}

/// Clamp the wire scalar into the `[0.0, 1.0]` range. macOS reports
/// scalar volume already in this range, but a malicious or buggy peer
/// could send arbitrary `f32`s including NaN — clamping below into a
/// finite default keeps `wpctl` from getting a string like "NaN%".
fn clamp_level(raw: f32) -> f32 {
    if raw.is_nan() {
        // NaN is the only finite-vs-non-finite case we care about. Inf
        // also exists but clamps cleanly via `min/max`.
        return 0.0;
    }
    raw.clamp(0.0, 1.0)
}

/// Spawn `wpctl set-volume @DEFAULT_AUDIO_SINK@ <level>`. PipeWire's
/// volume is a linear scalar matching ours, so no conversion is needed.
fn apply_level(level: f32, debug: bool) {
    // wpctl accepts the bare scalar (no "%" suffix) when the value is
    // a decimal — keeps locale variation (comma vs period) out of the
    // command line by formatting with the C-locale-friendly `{:.4}`.
    let formatted = format!("{level:.4}");
    let result = Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &formatted])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match result {
        Ok(out) if out.status.success() => {
            if debug {
                eprintln!("debug: wpctl set-volume {formatted} ok");
            }
        }
        Ok(out) => {
            eprintln!(
                "volume: wpctl set-volume {formatted} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            eprintln!("volume: wpctl set-volume spawn failed: {e}");
        }
    }
}

/// Spawn `wpctl set-mute @DEFAULT_AUDIO_SINK@ <0|1>`. Absolute, not a
/// toggle — matches the wire contract.
fn apply_mute(muted: bool, debug: bool) {
    let flag = if muted { "1" } else { "0" };
    let result = Command::new("wpctl")
        .args(["set-mute", "@DEFAULT_AUDIO_SINK@", flag])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match result {
        Ok(out) if out.status.success() => {
            if debug {
                eprintln!("debug: wpctl set-mute {flag} ok");
            }
        }
        Ok(out) => {
            eprintln!(
                "volume: wpctl set-mute {flag} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            eprintln!("volume: wpctl set-mute spawn failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_handles_nan() {
        assert_eq!(clamp_level(f32::NAN), 0.0);
    }

    #[test]
    fn clamp_handles_infinity() {
        assert_eq!(clamp_level(f32::INFINITY), 1.0);
        assert_eq!(clamp_level(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn clamp_pass_through_in_range() {
        assert_eq!(clamp_level(0.0), 0.0);
        assert_eq!(clamp_level(0.5), 0.5);
        assert_eq!(clamp_level(1.0), 1.0);
    }

    #[test]
    fn clamp_clips_out_of_range() {
        assert_eq!(clamp_level(-0.1), 0.0);
        assert_eq!(clamp_level(1.1), 1.0);
    }
}
