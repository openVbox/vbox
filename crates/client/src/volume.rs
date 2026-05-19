//! macOS master-volume → guest PipeWire bridge (push-only).
//!
//! Polls the default output device's `kAudioDevicePropertyVolumeScalar`
//! and `kAudioDevicePropertyMute` on a worker thread; whenever the
//! scalar moves more than [`vbox_proto::LEVEL_EPSILON`] or the mute
//! state flips, the new state goes out as one `Message::VolumeChange`.
//!
//! ## Why polling instead of CoreAudio property listeners?
//!
//! A real property listener is delivered on whatever CoreAudio thread
//! it pleases and (worse) doesn't help here — we still need to debounce
//! the slider-drag firehose, which means joining a worker channel
//! anyway. A 50 ms poll on a dedicated thread is structurally the same
//! shape as `clipboard.rs` (200 ms NSPasteboard poll), zero CoreAudio
//! threading surprises, and the constant CPU cost is well under 0.1%.
//!
//! ## Echo-loop policy
//!
//! Volume is one-way (host → guest). The guest never sends a volume
//! frame back, so there's no equivalent of the clipboard's
//! `last_installed` cache. The only echo concern is *self-echo* if
//! the user somehow drove the host volume from inside the guest (e.g.
//! via a VM passthrough toolbar); we don't address that today —
//! callers who care can disable the bridge by not calling [`start`].
//!
//! ## CoreAudio FFI
//!
//! We hand-roll the small handful of CoreAudio HAL symbols this needs
//! rather than depending on `coreaudio-sys` (which is a bindgen-driven
//! 5k-line build dep for the same five function signatures we list
//! here). The ABI has been stable since macOS 10.6.

use std::sync::mpsc;
use std::time::Duration;

use vbox_proto::{LEVEL_EPSILON, MAX_UPDATES_PER_SEC, Message, VolumeChange};

/// How often to sample CoreAudio. 50 ms gives us 20 Hz max throughput —
/// the same ceiling [`MAX_UPDATES_PER_SEC`] advertises on the wire.
/// Slider drags fire at HID rate (~120 Hz); sampling at 50 ms naturally
/// coalesces 6 ticks into one frame, which is the whole point.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Spawn the macOS volume bridge.
///
/// `outbound` is the same client→server `Message` channel input and
/// clipboard ride on — multiplexing keeps connection count at one and
/// avoids a second handshake round trip.
///
/// Returns `()` on non-mac and after a successful spawn on mac; the
/// worker thread runs for the process lifetime (there is no equivalent
/// of NSPasteboard's installer side here because we never apply).
#[cfg(target_os = "macos")]
pub(crate) fn start(outbound: mpsc::Sender<Message>) {
    use std::thread;
    thread::Builder::new()
        .name("vbox-volume-poller".into())
        .spawn(move || run_poll_loop(outbound))
        .expect("spawn volume poller thread");
    // Static rate-cap assertion — guards against someone bumping the
    // proto constant without lowering the poll interval (or vice versa).
    // `POLL_INTERVAL.as_millis() * MAX_UPDATES_PER_SEC <= 1000` enforces
    // "polling no faster than the advertised wire rate".
    debug_assert!(
        (POLL_INTERVAL.as_millis() as u32).saturating_mul(MAX_UPDATES_PER_SEC) <= 1_000,
        "POLL_INTERVAL and MAX_UPDATES_PER_SEC drifted"
    );
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn start(_outbound: mpsc::Sender<Message>) {
    // No CoreAudio on non-mac hosts; volume sync is a host-driven feature.
}

#[cfg(target_os = "macos")]
fn run_poll_loop(outbound: mpsc::Sender<Message>) {
    let mut last_sent: Option<VolumeChange> = None;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let Some(current) = read_current_volume() else {
            // CoreAudio is transiently unavailable (device unplug / hot-
            // swap mid-poll). Wait a tick and retry — no logging at info
            // level since this happens every time the user yanks AirPods.
            continue;
        };
        if !worth_sending(&current, last_sent.as_ref()) {
            continue;
        }
        if outbound.send(Message::VolumeChange(current)).is_err() {
            // Receiver dropped — viewer is shutting down; exit cleanly.
            return;
        }
        last_sent = Some(current);
    }
}

/// Decide whether `current` differs from the last sent state enough to
/// warrant a wire frame. Split out so the rate-policy is independently
/// testable on every platform.
fn worth_sending(current: &VolumeChange, last: Option<&VolumeChange>) -> bool {
    let Some(prev) = last else {
        return true;
    };
    if prev.muted != current.muted {
        return true;
    }
    (prev.level - current.level).abs() >= LEVEL_EPSILON
}

#[cfg(target_os = "macos")]
fn read_current_volume() -> Option<VolumeChange> {
    let device = core_audio::default_output_device()?;
    let level = core_audio::volume_scalar(device)?;
    let muted = core_audio::is_muted(device).unwrap_or(false);
    Some(VolumeChange { level, muted })
}

#[cfg(target_os = "macos")]
mod core_audio {
    //! Minimal CoreAudio HAL bindings — just the property reads we need.
    //!
    //! All the property selectors here are FourCC constants documented
    //! in `<CoreAudio/AudioHardware.h>`. We compute them at compile time
    //! from their byte form to keep the relationship between the
    //! Apple-side name and the constant value visible in source.
    //!
    //! `AudioObjectGetPropertyData` is a zero-allocation read API — we
    //! pass a pointer to the output struct directly, so the only place
    //! a foreign pointer ever lives is on this thread's stack.
    use std::mem::{MaybeUninit, size_of};
    use std::os::raw::{c_int, c_void};

    type AudioObjectID = u32;
    type OSStatus = c_int;
    type UInt32 = u32;

    const K_AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
    /// Convenience constant: every selector path uses Global scope and
    /// `kAudioObjectPropertyElementMain` (= 0). Element 0 is the
    /// "main" of the device — same value Apple alias-renamed from
    /// `Master` in macOS 12; we keep the legacy 0 because the
    /// numeric value is what the kernel sees.
    const ELEMENT_MAIN: UInt32 = 0;

    /// FourCC helper: turns 'aBcD' into the matching `UInt32` constant.
    /// `const fn` so we can keep the byte form in source.
    const fn fourcc(s: &[u8; 4]) -> UInt32 {
        ((s[0] as UInt32) << 24)
            | ((s[1] as UInt32) << 16)
            | ((s[2] as UInt32) << 8)
            | (s[3] as UInt32)
    }

    const K_SCOPE_GLOBAL: UInt32 = fourcc(b"glob");
    const K_SCOPE_OUTPUT: UInt32 = fourcc(b"outp");

    const K_DEFAULT_OUTPUT_DEVICE: UInt32 = fourcc(b"dOut");
    const K_VOLUME_SCALAR: UInt32 = fourcc(b"volm");
    const K_MUTE: UInt32 = fourcc(b"mute");

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        m_selector: UInt32,
        m_scope: UInt32,
        m_element: UInt32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            in_object: AudioObjectID,
            in_address: *const AudioObjectPropertyAddress,
            in_qualifier_data_size: UInt32,
            in_qualifier_data: *const c_void,
            io_data_size: *mut UInt32,
            out_data: *mut c_void,
        ) -> OSStatus;
    }

    /// Read the system's current default output device.
    pub(super) fn default_output_device() -> Option<AudioObjectID> {
        let addr = AudioObjectPropertyAddress {
            m_selector: K_DEFAULT_OUTPUT_DEVICE,
            m_scope: K_SCOPE_GLOBAL,
            m_element: ELEMENT_MAIN,
        };
        get_property::<AudioObjectID>(K_AUDIO_OBJECT_SYSTEM_OBJECT, &addr)
    }

    /// Read the device's master output volume scalar (0.0..=1.0).
    pub(super) fn volume_scalar(device: AudioObjectID) -> Option<f32> {
        let addr = AudioObjectPropertyAddress {
            m_selector: K_VOLUME_SCALAR,
            m_scope: K_SCOPE_OUTPUT,
            m_element: ELEMENT_MAIN,
        };
        get_property::<f32>(device, &addr)
    }

    /// Read the device's master mute state. Returns None if the device
    /// doesn't expose a mute property (some external DACs don't).
    pub(super) fn is_muted(device: AudioObjectID) -> Option<bool> {
        let addr = AudioObjectPropertyAddress {
            m_selector: K_MUTE,
            m_scope: K_SCOPE_OUTPUT,
            m_element: ELEMENT_MAIN,
        };
        let v: UInt32 = get_property(device, &addr)?;
        Some(v != 0)
    }

    /// Wrapper around `AudioObjectGetPropertyData` that turns a typed
    /// stack slot into the in/out buffer the C API expects. Returns
    /// None on any OSStatus error — callers treat that as "device
    /// transient or unsupported" and retry next tick.
    fn get_property<T: Copy>(
        object: AudioObjectID,
        addr: &AudioObjectPropertyAddress,
    ) -> Option<T> {
        let mut size = size_of::<T>() as UInt32;
        let mut out = MaybeUninit::<T>::uninit();
        // SAFETY: `out` is exactly `size` bytes, `addr` is a valid
        // pointer pinned on this thread's stack, qualifier args are
        // null/zero (we never use the qualified-read overload).
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                addr,
                0,
                std::ptr::null(),
                &mut size,
                out.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return None;
        }
        if (size as usize) != size_of::<T>() {
            // Some devices return a different size than expected
            // (e.g. older HALs report mute as `UInt32` even when caller
            // asked for `bool`). Defensive bail instead of misreading.
            return None;
        }
        // SAFETY: the kernel just wrote `size` bytes into `out`; we
        // verified size matches `T` above.
        Some(unsafe { out.assume_init() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worth_sending_first_frame() {
        let c = VolumeChange {
            level: 0.5,
            muted: false,
        };
        assert!(worth_sending(&c, None));
    }

    #[test]
    fn worth_sending_below_epsilon_skipped() {
        let prev = VolumeChange {
            level: 0.500,
            muted: false,
        };
        let next = VolumeChange {
            level: 0.501,
            muted: false,
        };
        assert!(!worth_sending(&next, Some(&prev)));
    }

    #[test]
    fn worth_sending_above_epsilon_emitted() {
        let prev = VolumeChange {
            level: 0.5,
            muted: false,
        };
        let next = VolumeChange {
            level: 0.5 + LEVEL_EPSILON + 0.001,
            muted: false,
        };
        assert!(worth_sending(&next, Some(&prev)));
    }

    #[test]
    fn worth_sending_mute_flip_always_emitted() {
        let prev = VolumeChange {
            level: 0.5,
            muted: false,
        };
        let next = VolumeChange {
            level: 0.5,
            muted: true,
        };
        assert!(worth_sending(&next, Some(&prev)));
    }
}
