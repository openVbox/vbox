// vbox-client — runs on the macOS host.
//
// Connect to a `vbox-server` on a TCP endpoint
// (typically reached through `ssh -L 5710:localhost:5710 USER@HOST`), do the
// prelude + Hello/Welcome handshake, run a Ping loop on stdin commands.
//
// Subcommands:
//   ping <addr>            single round-trip then exit (handy for `doctor`)
//   connect <addr>         interactive: stdin "ping" lines emit Ping frames
//   view <addr>            request the Wayland view stream and display frames
//   input <addr> ...       send input events to a remote window id
//   volume <addr> --level N [--muted|--unmuted]
//                          push one master-volume change to the guest sink

mod app_icon;
mod brand;
mod cli;
mod clipboard;
mod ctl;
mod data_plane;
mod launch;
mod net;
mod viewer;
mod volume;

#[cfg(test)]
mod test_env {
    use std::ffi::OsStr;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap()
    }

    pub(crate) fn set_var<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) {
        // SAFETY: callers hold ENV_LOCK while mutating process-wide environment.
        unsafe { std::env::set_var(key, value) };
    }

    pub(crate) fn remove_var<K: AsRef<OsStr>>(key: K) {
        // SAFETY: callers hold ENV_LOCK while mutating process-wide environment.
        unsafe { std::env::remove_var(key) };
    }
}

use anyhow::Result;
use app_icon::sanitize_app_id;
use clap::Parser;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::{Cli, Cmd};
use crate::launch::{bundle_launch_args, run_bundle_launch, run_launch};
use crate::net::{
    CommandTransport, ViewTransport, interactive, one_shot_ping_with_transport, send_input_command,
    send_volume_command, view_with_transport,
};

fn main() -> Result<()> {
    // macOS Launchpad runs the bundle's `CFBundleExecutable` with no CLI args.
    // When we *are* that executable, the bundle's `Resources/` tells us which
    // GNOME app and helper script to drive. We probe for that first so the
    // launcher .app doesn't have to ship a separate wrapper script.
    let probed = if std::env::args_os().len() == 1 {
        bundle_launch_args()
    } else {
        None
    };
    if should_enter_bundle_launch(std::env::args_os().len(), probed.is_some()) {
        let args = probed.expect("guarded by should_enter_bundle_launch");
        return run_bundle_launch(&args);
    }
    let cli = Cli::parse();
    match cli.command {
        Cmd::Ping {
            addr,
            timeout_secs,
            data_plane,
            quic_addr,
            quic_token,
            quic_cert_sha256,
        } => one_shot_ping_with_transport(
            addr,
            command_transport(data_plane.into(), quic_addr, quic_token, quic_cert_sha256),
            timeout_secs,
        ),
        Cmd::Connect { addr } => interactive(addr),
        Cmd::View {
            addr,
            socket_name,
            width,
            height,
            data_plane,
            quic_addr,
            quic_token,
            quic_cert_sha256,
        } => view_with_transport(
            addr,
            socket_name,
            width,
            height,
            ViewTransport {
                mode: data_plane.into(),
                quic_addr,
                quic_token,
                quic_cert_sha256,
            },
        ),
        Cmd::Input {
            addr,
            data_plane,
            quic_addr,
            quic_token,
            quic_cert_sha256,
            id,
            event,
        } => send_input_command(
            addr,
            command_transport(data_plane.into(), quic_addr, quic_token, quic_cert_sha256),
            id,
            event,
        ),
        Cmd::Volume {
            addr,
            data_plane,
            quic_addr,
            quic_token,
            quic_cert_sha256,
            level,
            muted,
            unmuted,
        } => {
            // CLI exposes muted/unmuted as separate flags so the absence
            // of either means "leave default". Map to the Option<bool>
            // the network layer expects.
            send_volume_command(
                addr,
                command_transport(data_plane.into(), quic_addr, quic_token, quic_cert_sha256),
                level,
                resolve_volume_mute_flag(muted, unmuted),
            )
        }
        Cmd::Launch {
            vbox_native,
            app_id,
            instance,
        } => {
            let instance = resolve_launch_instance(instance, &app_id);
            run_launch(&vbox_native, &app_id, &instance)
        }
        Cmd::Ctl { cmd } => ctl::run(cmd),
    }
}

fn command_transport(
    mode: vbox_proto::DataPlaneMode,
    quic_addr: Option<std::net::SocketAddr>,
    quic_token: Option<String>,
    quic_cert_sha256: Option<String>,
) -> CommandTransport {
    CommandTransport {
        mode,
        quic_addr,
        quic_token,
        quic_cert_sha256,
    }
}

pub(crate) fn client_name() -> String {
    format_client_name(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Pure formatter for the Hello.client_name string. The viewer surfaces
/// this in server logs and `vbox-controld` status output, so the
/// shape is part of the user-facing contract — test it.
fn format_client_name(version: &str, host_os: &str, host_arch: &str) -> String {
    format!("vbox-client/{version} ({host_os} {host_arch})")
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

pub(crate) fn debug_enabled() -> bool {
    debug_flag_from(crate::brand::env_var("VBOX_DEBUG").as_deref())
}

/// Same parse contract as the server's debug helper, kept duplicated here
/// because the binary doesn't share code with the server crate.
fn debug_flag_from(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "TRUE" | "yes" | "on"))
}

/// CLI-level decision for "should we go into bundle-launch mode?". The
/// real `main()` checks `args_os().len() == 1`, but that's hard to test
/// because the launcher unit-tests can't fake argv length without
/// invoking the process. This pure helper takes the arg count instead,
/// so tests can pin every (count, bundle-args-present?) combination.
fn should_enter_bundle_launch(arg_count: usize, bundle_args_present: bool) -> bool {
    arg_count == 1 && bundle_args_present
}

/// Map the Volume subcommand's --muted/--unmuted flag pair into the
/// `Option<bool>` the network layer expects. (true, false) → Some(true),
/// (false, true) → Some(false), neither flag → None.
fn resolve_volume_mute_flag(muted: bool, unmuted: bool) -> Option<bool> {
    match (muted, unmuted) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// Pick the instance name a `Launch` invocation should use. The CLI
/// flag is optional; the default is `sanitize_app_id(app_id)`. Splitting
/// the fallback into its own helper lets tests pin both branches without
/// reaching into clap.
fn resolve_launch_instance(explicit: Option<String>, app_id: &str) -> String {
    explicit.unwrap_or_else(|| sanitize_app_id(app_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_name_includes_version_os_and_arch() {
        let s = format_client_name("9.9.9", "linux", "aarch64");
        assert_eq!(s, "vbox-client/9.9.9 (linux aarch64)");
    }

    #[test]
    fn debug_flag_unset_is_off() {
        assert!(!debug_flag_from(None));
    }

    #[test]
    fn debug_flag_recognises_documented_truthy_values() {
        for v in ["1", "true", "TRUE", "yes", "on"] {
            assert!(
                debug_flag_from(Some(v)),
                "VBOX_DEBUG={v:?} should turn debug on"
            );
        }
    }

    #[test]
    fn debug_flag_rejects_unknown_values() {
        for v in ["", "0", "false", "no", "off", "True", "Yes"] {
            assert!(
                !debug_flag_from(Some(v)),
                "VBOX_DEBUG={v:?} should stay off"
            );
        }
    }

    #[test]
    fn bundle_launch_only_engages_when_argc_one_and_bundle_present() {
        // Story: Launchpad invokes the .app's CFBundleExecutable with no
        // CLI args (argc==1). We probe the bundle's Resources/. If the
        // probe found bundle metadata we should enter bundle mode; if it
        // didn't, we fall through to clap parsing.
        assert!(should_enter_bundle_launch(1, true));
        assert!(!should_enter_bundle_launch(1, false));
        // Any CLI argument means the user invoked us explicitly — never
        // enter bundle mode regardless of whether the probe succeeded.
        assert!(!should_enter_bundle_launch(2, true));
        assert!(!should_enter_bundle_launch(5, true));
    }

    #[test]
    fn volume_mute_flag_resolves_to_some_true_when_only_muted_passed() {
        assert_eq!(resolve_volume_mute_flag(true, false), Some(true));
    }

    #[test]
    fn volume_mute_flag_resolves_to_some_false_when_only_unmuted_passed() {
        assert_eq!(resolve_volume_mute_flag(false, true), Some(false));
    }

    #[test]
    fn volume_mute_flag_returns_none_when_neither_flag_passed() {
        // Mirrors the CLI default — operator runs `vbox volume 60`
        // without --muted/--unmuted, so the wire frame's mute bit keeps
        // whatever the server-side default is.
        assert_eq!(resolve_volume_mute_flag(false, false), None);
    }

    #[test]
    fn volume_mute_flag_treats_both_flags_as_none() {
        // clap's `conflicts_with` already rejects this at parse time, but
        // the helper handles the case defensively so an API caller bypassing
        // clap can't end up with a contradictory state.
        assert_eq!(resolve_volume_mute_flag(true, true), None);
    }

    // ---- resolve_launch_instance ----------------------------------------
    //
    // Story: `vbox-client launch --app-id org.gnome.Calculator` with
    // no --instance flag defaults to a sanitized app id. Operators can
    // override with --instance dev when they want two instances of the
    // same app open at once.

    #[test]
    fn launch_instance_uses_explicit_flag_when_provided() {
        let out = resolve_launch_instance(Some("dev".into()), "org.gnome.Calculator");
        assert_eq!(out, "dev");
    }

    #[test]
    fn launch_instance_falls_back_to_sanitized_app_id() {
        // The sanitize step folds dots — make sure the helper goes
        // through it rather than passing the raw app_id (which would
        // produce a different on-disk cache path than the host CLI).
        let out = resolve_launch_instance(None, "org.gnome.Calculator");
        assert_eq!(out, sanitize_app_id("org.gnome.Calculator"));
    }

    #[test]
    fn launch_instance_explicit_empty_string_is_taken_verbatim() {
        // An operator passing --instance "" is unusual but possible —
        // we treat the flag as authoritative rather than substituting
        // the default. (clap doesn't enforce non-empty for this arg.)
        assert_eq!(resolve_launch_instance(Some(String::new()), "x"), "");
    }
}
