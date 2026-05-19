//! macOS Launchpad bundle integration.
//!
//! `vbox-client` ships as a `.app` per launcher; Launchpad invokes the
//! `CFBundleExecutable` with no CLI args. The bundle's `Resources/` carries
//! the `AppID`, `VBoxNativePath`, and `Instance` strings that would otherwise
//! sit on the command line. [`bundle_launch_args`] reads those probes. The
//! main entrypoint calls [`run_launch`] which drives `vbox prepare-app`,
//! spawns the `app-id` watcher thread, and hands off to the viewer.
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use vbox_proto::DataPlaneMode;

use crate::app_icon::{sanitize_app_id, set_icon_dir};
use crate::net::{ViewTransport, view_with_transport};

pub(crate) struct BundleLaunchArgs {
    pub(crate) vbox_native: PathBuf,
    pub(crate) app_id: String,
    pub(crate) instance: String,
    pub(crate) command_env: Vec<(String, String)>,
    pub(crate) icon_dir: Option<PathBuf>,
}

/// Inspect the bundle the running binary lives in (`<exe>/../../Resources`)
/// for the launcher metadata that `build_launcher_app` writes. Returns `None`
/// for non-bundled invocations (development builds).
pub(crate) fn bundle_launch_args() -> Option<BundleLaunchArgs> {
    let exe = std::env::current_exe().ok()?;
    let resources = exe.parent()?.parent()?.join("Resources");
    let app_id = read_resource(&resources, "AppID.txt")?;
    let vbox_native = read_resource(&resources, "VBoxNativePath.txt")
        .or_else(|| read_resource(&resources, "CohNativePath.txt"))?;
    if app_id.is_empty() || vbox_native.is_empty() {
        return None;
    }
    let instance = read_resource(&resources, "Instance.txt")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| sanitize_app_id(&app_id));
    let command_env = bundle_command_env(&resources);
    let icon_dir = read_resource(&resources, "IconDir.txt").map(PathBuf::from);
    Some(BundleLaunchArgs {
        vbox_native: PathBuf::from(vbox_native),
        app_id,
        instance,
        command_env,
        icon_dir,
    })
}

fn read_resource(resources: &Path, name: &str) -> Option<String> {
    fs::read_to_string(resources.join(name))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn bundle_command_env(resources: &Path) -> Vec<(String, String)> {
    [
        ("Root.txt", "VBOX_ROOT"),
        ("StateDir.txt", "VBOX_STATE_DIR"),
        ("Guest.txt", "VBOX_GUEST"),
        ("GuestDir.txt", "VBOX_GUEST_DIR"),
        ("Port.txt", "VBOX_PORT"),
        ("Width.txt", "VBOX_WIDTH"),
        ("Height.txt", "VBOX_HEIGHT"),
    ]
    .into_iter()
    .filter_map(|(file, key)| read_resource(resources, file).map(|value| (key.to_string(), value)))
    .collect()
}

#[derive(Debug)]
struct PreparedInstance {
    port: u16,
    socket: String,
    width: u32,
    height: u32,
    transport: ViewTransport,
}

pub(crate) fn run_launch(vbox_native: &Path, app_id: &str, instance: &str) -> Result<()> {
    run_launch_with_env(vbox_native, app_id, instance, &[], None)
}

pub(crate) fn run_bundle_launch(args: &BundleLaunchArgs) -> Result<()> {
    run_launch_with_env(
        &args.vbox_native,
        &args.app_id,
        &args.instance,
        &args.command_env,
        args.icon_dir.as_deref(),
    )
}

fn run_launch_with_env(
    vbox_native: &Path,
    app_id: &str,
    instance: &str,
    command_env: &[(String, String)],
    icon_dir: Option<&Path>,
) -> Result<()> {
    let prepared = run_prepare_app_with_env(vbox_native, app_id, instance, command_env)
        .with_context(|| format!("prepare-app failed for {app_id} (instance {instance})"))?;

    // The host CLI writes cached guest icons next to the executable. Configure
    // the viewer before view() so the Dock/window icon resolves to the GNOME
    // PNG that `fetch_guest_icon` cached.
    if let Some(dir) = icon_dir
        .map(PathBuf::from)
        .or_else(|| vbox_icon_dir_for(vbox_native))
    {
        set_icon_dir(dir);
    }

    // `app-id` polls the guest's Wayland socket and launches the GNOME app
    // once the socket exists. The socket is created by the viewer's
    // ViewRequest, so this has to race in parallel with view() — running it
    // before view() would deadlock on `wait_socket`.
    {
        let vbox_native = vbox_native.to_path_buf();
        let app_id = app_id.to_string();
        let instance = instance.to_string();
        let command_env = command_env.to_vec();
        std::thread::Builder::new()
            .name("vbox-app-id".into())
            .spawn(move || {
                let argv = app_id_helper_argv(&instance, &app_id);
                let status = Command::new(&vbox_native)
                    .args(argv.iter().map(String::as_str))
                    .envs(command_env.iter().map(|(key, value)| (key, value)))
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .status();
                match status {
                    Ok(s) if !s.success() => eprintln!("app-id exited with {s}"),
                    Err(e) => eprintln!("app-id failed: {e}"),
                    _ => {}
                }
            })
            .context("spawning app-id thread")?;
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], prepared.port));
    let view_result = view_with_transport(
        addr,
        prepared.socket,
        prepared.width,
        prepared.height,
        prepared.transport,
    );

    // Tear down the backend stack for this instance. Best-effort: a non-zero
    // exit here would shadow any error from view(), and the user can always
    // run `./vbox --instance <inst> stop` to clean up by hand.
    let argv = stop_helper_argv(instance);
    let _ = Command::new(vbox_native)
        .args(argv.iter().map(String::as_str))
        .envs(command_env.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    view_result
}

fn run_prepare_app_with_env(
    vbox_native: &Path,
    app_id: &str,
    instance: &str,
    command_env: &[(String, String)],
) -> Result<PreparedInstance> {
    let argv = prepare_app_bundle_helper_argv(instance, app_id);
    let output = Command::new(vbox_native)
        .args(argv.iter().map(String::as_str))
        .envs(command_env.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("spawning {}", vbox_native.display()))?;
    if !output.status.success() {
        bail!("prepare-app exited with status {}", output.status);
    }
    let stdout = std::str::from_utf8(&output.stdout).context("prepare-app stdout not utf-8")?;
    parse_prepare_app_stdout(stdout)
}

/// Build the argv for the `prepare-app` helper invocation. Mirror of
/// [`app_id_helper_argv`] and [`stop_helper_argv`] — pulled out so all
/// three argv shapes are pinned by tests in one place.
#[allow(dead_code)] // compatibility shape for the documented four-field helper contract.
pub(crate) fn prepare_app_helper_argv(instance: &str, app_id: &str) -> [String; 5] {
    [
        "--no-build".into(),
        "--instance".into(),
        instance.into(),
        "prepare-app".into(),
        app_id.into(),
    ]
}

pub(crate) fn prepare_app_bundle_helper_argv(instance: &str, app_id: &str) -> [String; 6] {
    [
        "--no-build".into(),
        "--instance".into(),
        instance.into(),
        "prepare-app".into(),
        "--bundle".into(),
        app_id.into(),
    ]
}

/// Resolve the icon-cache directory that `VBOX_ICON_DIR` should point at
/// when the launcher fires the viewer. The host CLI writes icons to
/// `<vbox_native_dir>/.vbox/icons` — we mirror that path here. Returns
/// `None` when the CLI path lives at filesystem root (no parent),
/// matching the production behaviour of "skip the VBOX_ICON_DIR export".
pub(crate) fn vbox_icon_dir_for(vbox_native: &Path) -> Option<PathBuf> {
    vbox_native.parent().map(|root| root.join(".vbox/icons"))
}

/// Build the argv for the `app-id` helper invocation. Splitting this out
/// of `run_launch` makes the "which CLI flags do we pass to the bash
/// helper?" decision testable without spawning a child process. Order
/// matters: `--no-build` and `--instance <name>` are global options,
/// `app-id <app_id>` is the subcommand + positional.
pub(crate) fn app_id_helper_argv(instance: &str, app_id: &str) -> [String; 5] {
    [
        "--no-build".into(),
        "--instance".into(),
        instance.into(),
        "app-id".into(),
        app_id.into(),
    ]
}

/// Build the argv for the `stop` teardown that `run_launch` runs after
/// `view()` returns. Mirror of [`app_id_helper_argv`] for the cleanup
/// path. Best-effort: a non-zero exit here just goes to stderr.
pub(crate) fn stop_helper_argv(instance: &str) -> [String; 4] {
    [
        "--no-build".into(),
        "--instance".into(),
        instance.into(),
        "stop".into(),
    ]
}

/// Parse the `<port> <socket> <width> <height>` summary line `vbox
/// prepare-app` writes to stdout. The helper script's contract is:
/// - everything informational goes to stderr,
/// - exactly one summary line lands on stdout (possibly followed by a
///   trailing newline),
/// - the four fields are whitespace-separated and the port/width/height
///   parse as integers.
///
/// Split out from [`run_prepare_app`] so tests can exercise the parser
/// without spawning a real bash process.
fn parse_prepare_app_stdout(stdout: &str) -> Result<PreparedInstance> {
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|s| !s.is_empty())
        .context("prepare-app produced no stdout")?;
    if line.starts_with('{') {
        return parse_prepare_app_bundle(line);
    }
    let mut parts = line.split_whitespace();
    let (Some(port), Some(socket), Some(width), Some(height), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        bail!("prepare-app stdout malformed: {line:?}");
    };
    Ok(PreparedInstance {
        port: port.parse().context("parse port")?,
        socket: socket.to_string(),
        width: width.parse().context("parse width")?,
        height: height.parse().context("parse height")?,
        transport: ViewTransport::tcp(),
    })
}

#[derive(Debug, Deserialize)]
struct PrepareAppBundle {
    version: u8,
    port: u16,
    socket: String,
    width: u32,
    height: u32,
    data_plane: Option<PrepareAppDataPlane>,
}

#[derive(Debug, Deserialize)]
struct PrepareAppDataPlane {
    mode: String,
    quic_addr: Option<SocketAddr>,
    session_token: Option<String>,
    quic_server_cert_sha256: Option<String>,
}

fn parse_prepare_app_bundle(line: &str) -> Result<PreparedInstance> {
    let bundle: PrepareAppBundle =
        serde_json::from_str(line).context("parse prepare-app bundle")?;
    if bundle.version != 1 {
        bail!("unsupported prepare-app bundle version {}", bundle.version);
    }
    let transport = match bundle.data_plane {
        Some(data_plane) => prepare_app_transport(data_plane)?,
        None => ViewTransport::tcp(),
    };
    Ok(PreparedInstance {
        port: bundle.port,
        socket: bundle.socket,
        width: bundle.width,
        height: bundle.height,
        transport,
    })
}

fn prepare_app_transport(data_plane: PrepareAppDataPlane) -> Result<ViewTransport> {
    let mode = match data_plane.mode.as_str() {
        "tcp-only" => DataPlaneMode::TcpOnly,
        "auto" => DataPlaneMode::Auto,
        "quic-only" => DataPlaneMode::QuicOnly,
        other => bail!("unknown prepare-app data-plane mode {other:?}"),
    };
    if mode == DataPlaneMode::TcpOnly {
        return Ok(ViewTransport::tcp());
    }
    let quic_addr = data_plane
        .quic_addr
        .context("prepare-app bundle missing quic_addr")?;
    let quic_token = data_plane
        .session_token
        .context("prepare-app bundle missing session_token")?;
    let quic_cert_sha256 = data_plane
        .quic_server_cert_sha256
        .context("prepare-app bundle missing quic_server_cert_sha256")?;
    Ok(ViewTransport {
        mode,
        quic_addr: Some(quic_addr),
        quic_token: Some(quic_token),
        quic_cert_sha256: Some(quic_cert_sha256),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---- parse_prepare_app_stdout -----------------------------------------
    //
    // Story: the launch flow invokes `./vbox prepare-app` which prints a
    // single summary line. We pin every shape the bash script is allowed to
    // emit (with/without trailing newline, surrounded by blank lines), and
    // assert error wording on the malformed cases an operator might hit
    // when they replace the helper with a misbehaving stub.

    #[test]
    fn parses_typical_summary_line() {
        // Real-world output: one tidy line, no preamble.
        let out = "5723 wayland-3 1280 800\n";
        let prepared = parse_prepare_app_stdout(out).expect("typical line parses");
        assert_eq!(prepared.port, 5723);
        assert_eq!(prepared.socket, "wayland-3");
        assert_eq!(prepared.width, 1280);
        assert_eq!(prepared.height, 800);
        assert_eq!(prepared.transport, ViewTransport::tcp());
    }

    #[test]
    fn parses_versioned_prepare_app_bundle_with_quic_transport() {
        let out = r#"{"version":1,"port":5723,"socket":"wayland-3","width":1280,"height":800,"data_plane":{"mode":"auto","quic_addr":"192.0.2.10:5723","session_token":"tok","quic_server_cert_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}"#;
        let prepared = parse_prepare_app_stdout(out).expect("bundle parses");

        assert_eq!(prepared.port, 5723);
        assert_eq!(prepared.socket, "wayland-3");
        assert_eq!(prepared.transport.mode, DataPlaneMode::Auto);
        assert_eq!(
            prepared.transport.quic_addr,
            Some("192.0.2.10:5723".parse().unwrap())
        );
        assert_eq!(prepared.transport.quic_token.as_deref(), Some("tok"));
        assert_eq!(
            prepared.transport.quic_cert_sha256.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn picks_last_non_empty_line_when_trailing_blank_lines_exist() {
        // A future helper version may append a trailing newline pair. The
        // parser keeps working by taking the last non-empty line.
        let out = "5710 wayland-0 1024 768\n\n";
        let prepared = parse_prepare_app_stdout(out).unwrap();
        assert_eq!(prepared.port, 5710);
        assert_eq!(prepared.socket, "wayland-0");
    }

    #[test]
    fn errors_when_stdout_is_empty() {
        let err = parse_prepare_app_stdout("\n\n").expect_err("empty stdout must error");
        assert!(format!("{err:#}").contains("prepare-app produced no stdout"));
    }

    #[test]
    fn errors_when_summary_has_too_few_fields() {
        let err = parse_prepare_app_stdout("5710 wayland-0 1024\n")
            .expect_err("3-field stdout must error");
        assert!(format!("{err:#}").contains("malformed"));
    }

    #[test]
    fn errors_when_summary_has_extra_field() {
        // Defensive parse — strictly four whitespace-separated fields. An
        // extra column likely means the helper changed its contract; we
        // would rather error loudly than silently ignore the new column.
        let err = parse_prepare_app_stdout("5710 wayland-0 1024 768 extra\n")
            .expect_err("5-field stdout must error");
        assert!(format!("{err:#}").contains("malformed"));
    }

    #[test]
    fn errors_when_port_is_not_a_number() {
        let err = parse_prepare_app_stdout("nope wayland-0 1024 768\n")
            .expect_err("non-numeric port must error");
        assert!(format!("{err:#}").contains("parse port"));
    }

    #[test]
    fn errors_when_width_is_not_a_number() {
        let err = parse_prepare_app_stdout("5710 wayland-0 wide 768\n")
            .expect_err("non-numeric width must error");
        assert!(format!("{err:#}").contains("parse width"));
    }

    // ---- bundle_launch_args (filesystem flow) -----------------------------
    //
    // Story: Launchpad invokes the bundled binary with no CLI args. We
    // probe `<exe>/../../Resources/{AppID,VBoxNativePath,Instance}.txt`.
    // Because bundle_launch_args() is hard-coded to current_exe(), we
    // unit-test the *underlying* logic via a parallel helper that takes
    // the resources path explicitly.

    fn read_bundle_args_from(resources: &Path) -> Option<BundleLaunchArgs> {
        // Mirror bundle_launch_args() but with an injected resources dir.
        let app_id = read_resource(resources, "AppID.txt")?;
        let vbox_native = read_resource(resources, "VBoxNativePath.txt")
            .or_else(|| read_resource(resources, "CohNativePath.txt"))?;
        if app_id.is_empty() || vbox_native.is_empty() {
            return None;
        }
        let instance = read_resource(resources, "Instance.txt")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| sanitize_app_id(&app_id));
        Some(BundleLaunchArgs {
            vbox_native: PathBuf::from(vbox_native),
            app_id,
            instance,
            command_env: bundle_command_env(resources),
            icon_dir: read_resource(resources, "IconDir.txt").map(PathBuf::from),
        })
    }

    #[test]
    fn bundle_args_present_when_resources_has_complete_set() {
        let dir = tempdir_for_test();
        std::fs::write(dir.path.join("AppID.txt"), "org.gnome.Calculator\n").unwrap();
        std::fs::write(
            dir.path.join("VBoxNativePath.txt"),
            "/usr/local/share/vbox/vbox\n",
        )
        .unwrap();
        std::fs::write(dir.path.join("Instance.txt"), "calc\n").unwrap();

        let args = read_bundle_args_from(&dir.path).expect("complete bundle must parse");

        assert_eq!(args.app_id, "org.gnome.Calculator");
        assert_eq!(
            args.vbox_native,
            PathBuf::from("/usr/local/share/vbox/vbox")
        );
        assert_eq!(args.instance, "calc");
    }

    #[test]
    fn bundle_args_carries_guest_environment_and_icon_dir() {
        let dir = tempdir_for_test();
        std::fs::write(dir.path.join("AppID.txt"), "org.gnome.Calculator\n").unwrap();
        std::fs::write(dir.path.join("VBoxNativePath.txt"), "/opt/vbox/vbox\n").unwrap();
        std::fs::write(dir.path.join("Guest.txt"), "alice@example.test\n").unwrap();
        std::fs::write(dir.path.join("GuestDir.txt"), "/home/alice/vbox\n").unwrap();
        std::fs::write(dir.path.join("IconDir.txt"), "/tmp/vbox-icons\n").unwrap();

        let args = read_bundle_args_from(&dir.path).expect("complete bundle must parse");

        assert!(
            args.command_env
                .contains(&("VBOX_GUEST".to_string(), "alice@example.test".to_string()))
        );
        assert!(
            args.command_env
                .contains(&("VBOX_GUEST_DIR".to_string(), "/home/alice/vbox".to_string()))
        );
        assert_eq!(args.icon_dir, Some(PathBuf::from("/tmp/vbox-icons")));
    }

    #[test]
    fn bundle_args_accepts_legacy_native_path_resource() {
        let dir = tempdir_for_test();
        std::fs::write(dir.path.join("AppID.txt"), "org.gnome.Calculator\n").unwrap();
        std::fs::write(dir.path.join("CohNativePath.txt"), "/opt/vbox/vbox\n").unwrap();

        let args = read_bundle_args_from(&dir.path).expect("legacy bundle must parse");

        assert_eq!(args.vbox_native, PathBuf::from("/opt/vbox/vbox"));
    }

    #[test]
    fn bundle_args_default_instance_from_app_id_when_instance_blank() {
        // Blank Instance.txt → derive a sanitized instance name from
        // app_id. Mirrors what the production helper does.
        let dir = tempdir_for_test();
        std::fs::write(dir.path.join("AppID.txt"), "org.gnome.Calculator").unwrap();
        std::fs::write(dir.path.join("VBoxNativePath.txt"), "/opt/vbox/vbox").unwrap();
        std::fs::write(dir.path.join("Instance.txt"), "   \n").unwrap();

        let args = read_bundle_args_from(&dir.path).unwrap();

        assert_eq!(args.instance, sanitize_app_id("org.gnome.Calculator"));
    }

    #[test]
    fn bundle_args_returns_none_when_app_id_missing() {
        let dir = tempdir_for_test();
        std::fs::write(dir.path.join("VBoxNativePath.txt"), "/opt/vbox/vbox").unwrap();
        std::fs::write(dir.path.join("Instance.txt"), "calc").unwrap();

        let args = read_bundle_args_from(&dir.path);

        assert!(
            args.is_none(),
            "missing AppID.txt → no bundle args (caller falls back to clap)"
        );
    }

    // ---- vbox_icon_dir_for ------------------------------------------------
    //
    // Story: the host `vbox` CLI writes guest icons under
    // `/path/to/.vbox/icons`. The launcher passes that path to the viewer
    // via `VBOX_ICON_DIR` so `app_icon::find_icon_file_in` can pick them
    // up. We pin the path-derivation rule rather than the export itself
    // (which goes through std::env::set_var, racy in a multi-thread test).

    #[test]
    fn icon_dir_anchored_at_helper_parent() {
        let path = vbox_icon_dir_for(Path::new("/opt/vbox/vbox")).unwrap();
        assert_eq!(path, PathBuf::from("/opt/vbox/.vbox/icons"));
    }

    #[test]
    fn icon_dir_none_when_helper_at_root() {
        // `/vbox` has no parent we can hang `.vbox/icons` off; the helper
        // returns None so run_launch skips the export.
        assert!(vbox_icon_dir_for(Path::new("/")).is_none());
    }

    #[test]
    fn icon_dir_uses_relative_parent_when_helper_is_relative() {
        // Operators sometimes call `./vbox` from a project root; the
        // relative parent is "." which becomes "./.vbox/icons".
        let path = vbox_icon_dir_for(Path::new("./vbox")).unwrap();
        assert_eq!(path, PathBuf::from("./.vbox/icons"));
    }

    // ---- app_id_helper_argv / stop_helper_argv ----------------------------
    //
    // Story: the launcher invokes `vbox` with a tight argv shape the
    // host CLI expects. Pin the exact ordering so a future
    // refactor (or accidental --instance/--app-id swap) is caught.

    #[test]
    fn app_id_argv_is_exact_shape_helper_expects() {
        let argv = app_id_helper_argv("calc", "org.gnome.Calculator");
        assert_eq!(
            argv,
            [
                "--no-build".to_string(),
                "--instance".to_string(),
                "calc".to_string(),
                "app-id".to_string(),
                "org.gnome.Calculator".to_string(),
            ]
        );
    }

    #[test]
    fn stop_argv_is_exact_shape_helper_expects() {
        let argv = stop_helper_argv("calc");
        assert_eq!(
            argv,
            [
                "--no-build".to_string(),
                "--instance".to_string(),
                "calc".to_string(),
                "stop".to_string(),
            ]
        );
    }

    #[test]
    fn helper_argvs_carry_instance_names_with_dashes() {
        // Sanitized instance names can contain dashes (`org-gnome-calculator`);
        // the helper bash script handles them, our argv must pass them
        // through verbatim with no quoting tricks.
        let app_id = app_id_helper_argv("org-gnome-calculator", "org.gnome.Calculator");
        let stop = stop_helper_argv("org-gnome-calculator");
        assert!(app_id.contains(&"org-gnome-calculator".to_string()));
        assert!(stop.contains(&"org-gnome-calculator".to_string()));
    }

    #[test]
    fn prepare_app_argv_is_exact_shape_helper_expects() {
        let argv = prepare_app_helper_argv("calc", "org.gnome.Calculator");
        assert_eq!(
            argv,
            [
                "--no-build".to_string(),
                "--instance".to_string(),
                "calc".to_string(),
                "prepare-app".to_string(),
                "org.gnome.Calculator".to_string(),
            ]
        );
    }

    #[test]
    fn prepare_app_bundle_argv_requests_versioned_bundle() {
        let argv = prepare_app_bundle_helper_argv("calc", "org.gnome.Calculator");
        assert_eq!(
            argv,
            [
                "--no-build".to_string(),
                "--instance".to_string(),
                "calc".to_string(),
                "prepare-app".to_string(),
                "--bundle".to_string(),
                "org.gnome.Calculator".to_string(),
            ]
        );
    }

    #[test]
    fn prepare_app_argv_differs_from_app_id_argv_only_in_subcommand() {
        // Sanity: the only difference between the three argv builders
        // is the subcommand token. If a future refactor accidentally
        // changes the global-flags prefix on one of them, this test
        // catches it.
        let prep = prepare_app_helper_argv("calc", "org.x");
        let appid = app_id_helper_argv("calc", "org.x");
        assert_eq!(prep[0..3], appid[0..3]);
        assert_eq!(prep[3], "prepare-app");
        assert_eq!(appid[3], "app-id");
        assert_eq!(prep[4], appid[4]);
    }

    #[test]
    fn bundle_args_returns_none_when_app_id_blank() {
        // An empty AppID.txt is structurally present but unusable. The
        // probe must reject it so we don't end up calling prepare-app with
        // an empty app_id and producing a confusing downstream error.
        let dir = tempdir_for_test();
        std::fs::write(dir.path.join("AppID.txt"), "\n").unwrap();
        std::fs::write(dir.path.join("VBoxNativePath.txt"), "/opt/vbox/vbox").unwrap();

        assert!(read_bundle_args_from(&dir.path).is_none());
    }

    struct TempDir {
        path: PathBuf,
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    fn tempdir_for_test() -> TempDir {
        let base = std::env::temp_dir();
        let suffix = format!(
            "vbox-client-launch-{}-{}",
            std::process::id(),
            unique_counter()
        );
        let path = base.join(suffix);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    fn unique_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }
}
