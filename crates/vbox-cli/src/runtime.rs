use std::ffi::{OsStr, OsString};
use std::fs;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;

use crate::brand;
use crate::context::AppContext;
use crate::process;
use crate::{
    ControldInstallArgs, FilterArgs, InstallAppsArgs, LibraryArgs, LogsArgs, LogsMode,
    PrepareAppArgs, SamplesArg, SecondsArg, SuffixArgs, TextArgs, TrailingArgs, VolumeArgs,
};

const INSTANCE_PORT_ALLOC_FILE: &str = "instance-ports.tsv";
const INSTANCE_PORT_OFFSET: u32 = 2;
const INSTANCE_PORT_SPAN: u32 = 2048;

pub(crate) fn build(ctx: &AppContext) -> Result<()> {
    eprintln!("[vbox] build host CLI + client");
    let mut cmd = process::command("cargo");
    cmd.current_dir(&ctx.root)
        .args(["build", "--release", "-p", "vbox-cli", "-p", "vbox-client"]);
    process::run(cmd)?;
    sync(ctx)?;
    build_guest(ctx)
}

pub(crate) fn sync(ctx: &AppContext) -> Result<()> {
    let guest = ctx.guest()?;
    let guest_dir = ctx.guest_dir()?;
    eprintln!("[vbox] sync -> {}:{}", guest, guest_dir.display());
    let remote = shell_quote(guest_dir.as_os_str());
    let script = format!(
        "set -euo pipefail; cd {}; COPYFILE_DISABLE=1 tar --no-xattrs --exclude='./target' --exclude='./.vbox' --exclude='./.icebox' --exclude='./.git' -czf - . | ssh {} \"mkdir -p {remote} && tar -xzf - -C {remote}\"",
        shell_quote(ctx.root.as_os_str()),
        shell_quote(OsStr::new(guest)),
    );
    process::run(process::piped_shell(&script))
}

pub(crate) fn run_app(ctx: &AppContext, args: TrailingArgs, debug: bool) -> Result<()> {
    let argv = default_app_args(args.args);
    let mut ctx = ctx.clone();
    if debug {
        ctx.debug = true;
    }
    start_stack(&ctx)?;
    start_viewer_bg(&ctx)?;
    wait_socket(&ctx)?;
    launch_app(&ctx, argv)?;
    wait_viewer(&ctx)?;
    if brand::env_var("VBOX_KEEP").as_deref() != Some("1") {
        stop(&ctx)?;
    }
    Ok(())
}

pub(crate) fn memo(ctx: &AppContext, args: TrailingArgs) -> Result<()> {
    let mut argv = vec![
        OsString::from("gnome-text-editor"),
        OsString::from("--standalone"),
        OsString::from("--ignore-session"),
    ];
    argv.extend(args.args);
    run_app(ctx, TrailingArgs { args: argv }, true)
}

pub(crate) fn view(ctx: &AppContext) -> Result<()> {
    start_stack(ctx)?;
    start_viewer_bg(ctx)?;
    wait_socket(ctx)?;
    eprintln!("[vbox] viewer is open; launch apps with: vbox app gnome-calculator");
    wait_viewer(ctx)?;
    if brand::env_var("VBOX_KEEP").as_deref() != Some("1") {
        stop(ctx)?;
    }
    Ok(())
}

pub(crate) fn launch(ctx: &AppContext, args: TrailingArgs) -> Result<()> {
    let argv = default_app_args(args.args);
    start_stack(ctx)?;
    start_viewer_bg(ctx)?;
    wait_socket(ctx)?;
    launch_app(ctx, argv)
}

pub(crate) fn launch_id(ctx: &AppContext, app_id: String) -> Result<()> {
    let record = find_app_record(ctx, &app_id)?;
    start_stack(ctx)?;
    start_viewer_bg(ctx)?;
    wait_socket(ctx)?;
    launch_app(ctx, record.argv)
}

pub(crate) fn prepare_app(ctx: &AppContext, args: PrepareAppArgs) -> Result<()> {
    let record = find_app_record(ctx, &args.app_id)?;
    let app_ctx = instance_context(ctx, &sanitize_id(&record.id))?;
    start_stack(&app_ctx)?;
    if args.bundle {
        println!(
            "{{\"version\":1,\"port\":{},\"socket\":\"{}\",\"width\":{},\"height\":{},\"data_plane\":{{\"mode\":\"tcp-only\"}}}}",
            app_ctx.port, app_ctx.socket, app_ctx.width, app_ctx.height
        );
    } else {
        println!(
            "{} {} {} {}",
            app_ctx.port, app_ctx.socket, app_ctx.width, app_ctx.height
        );
    }
    Ok(())
}

pub(crate) fn app_id(ctx: &AppContext, app_id: String) -> Result<()> {
    let record = find_app_record(ctx, &app_id)?;
    wait_socket(ctx)?;
    launch_app(ctx, record.argv)
}

pub(crate) fn app(ctx: &AppContext, args: Vec<OsString>) -> Result<()> {
    if args.is_empty() {
        bail!("app requires a command");
    }
    wait_socket(ctx)?;
    launch_app(ctx, args)
}

pub(crate) fn bench_data_plane(ctx: &AppContext, args: SamplesArg) -> Result<()> {
    let samples = args.samples.unwrap_or(10);
    start_stack(ctx)?;
    println!("transport\tsamples\tok\tfail");
    let mut ok = 0;
    let mut fail = 0;
    for _ in 0..samples {
        if ping(ctx, 2).is_ok() {
            ok += 1;
        } else {
            fail += 1;
        }
    }
    println!("tcp_ssh\t{samples}\t{ok}\t{fail}");
    Ok(())
}

pub(crate) fn stability_test(ctx: &AppContext, args: SecondsArg) -> Result<()> {
    let seconds = args.seconds.unwrap_or(60);
    start_stack(ctx)?;
    let mut ok = 0;
    let mut fail = 0;
    for _ in 0..seconds {
        if ping(ctx, 2).is_ok() {
            ok += 1;
        } else {
            fail += 1;
        }
        thread::sleep(Duration::from_secs(1));
    }
    println!("duration_s={seconds} ping_ok={ok} ping_fail={fail}");
    Ok(())
}

pub(crate) fn controld_install(ctx: &AppContext, args: ControldInstallArgs) -> Result<()> {
    if ctx.build {
        build(ctx)?;
    }
    let tls_flags = if args.with_tls {
        tls_bootstrap_default(ctx)?;
        let tls_dir = ctx.state_dir.join("tls");
        let guest = ctx.guest()?;
        let guest_tls_dir = ctx.guest_dir()?.join(".vbox-controld/tls");
        ssh(
            ctx,
            &format!(
                "mkdir -p {} && chmod 700 {}",
                shell_quote(guest_tls_dir.as_os_str()),
                shell_quote(guest_tls_dir.as_os_str())
            ),
        )?;
        let mut scp = process::command("scp");
        scp.arg("-q")
            .arg(tls_dir.join("server.pem"))
            .arg(tls_dir.join("server.key.pem"))
            .arg(tls_dir.join("ca.pem"))
            .arg(format!("{}:{}/", guest, guest_tls_dir.display()));
        process::run(scp)?;
        format!(
            "--tls-cert {}/server.pem --tls-key {}/server.key.pem --tls-client-ca {}/ca.pem",
            guest_tls_dir.display(),
            guest_tls_dir.display(),
            guest_tls_dir.display()
        )
    } else {
        String::new()
    };

    let port = brand::env_var("VBOX_CONTROL_PORT").unwrap_or_else(|| "5711".to_string());
    let controld_override = args
        .controld_bin
        .as_ref()
        .map(|p| p.as_os_str())
        .unwrap_or(OsStr::new(""));
    let server_override = args
        .server_bin
        .as_ref()
        .map(|p| p.as_os_str())
        .unwrap_or(OsStr::new(""));
    let remote_script = format!(
        r#"set -euo pipefail
dir={dir}
port={port}
tls_flags={tls_flags}
controld_bin={controld_bin}
server_bin={server_bin}
controld_bin=${{controld_bin:-$(command -v vbox-controld 2>/dev/null || echo "$dir/target/release/vbox-controld")}}
server_bin=${{server_bin:-$(command -v vbox-server 2>/dev/null || echo "$dir/target/release/vbox-server")}}
mkdir -p "$HOME/.config/systemd/user"
mkdir -p "$dir/.vbox-controld"
pkill -x vbox-controld 2>/dev/null || true
cat > "$HOME/.config/systemd/user/vbox-controld.service" <<UNIT
[Unit]
Description=vbox control daemon
After=default.target

[Service]
Type=simple
WorkingDirectory=$dir
ExecStart=$controld_bin --bind 127.0.0.1 --port $port --work-dir $dir/.vbox-controld --server-bin $server_bin --token-file $dir/.vbox-controld/token $tls_flags
Restart=always
RestartSec=2
KillMode=process
StandardOutput=append:$dir/.vbox-controld/daemon.log
StandardError=append:$dir/.vbox-controld/daemon.log

[Install]
WantedBy=default.target
UNIT
systemctl --user daemon-reload
systemctl --user reset-failed vbox-controld.service 2>/dev/null || true
systemctl --user enable vbox-controld.service
systemctl --user restart vbox-controld.service
for _ in {{1..40}}; do [[ -s "$dir/.vbox-controld/token" ]] && break; sleep 0.1; done
systemctl --user is-active --quiet vbox-controld.service
    "#,
        dir = shell_quote(ctx.guest_dir()?.as_os_str()),
        port = shell_quote(OsStr::new(&port)),
        tls_flags = shell_quote(OsStr::new(&tls_flags)),
        controld_bin = shell_quote(controld_override),
        server_bin = shell_quote(server_override),
    );
    ssh(ctx, &remote_script)?;
    fetch_control_token(ctx)?;
    Ok(())
}

pub(crate) fn controld_uninstall(ctx: &AppContext) -> Result<()> {
    ssh(
        ctx,
        "systemctl --user disable --now vbox-controld.service >/dev/null 2>&1 || true; rm -f \"$HOME/.config/systemd/user/vbox-controld.service\"; systemctl --user daemon-reload >/dev/null 2>&1 || true",
    )
}

pub(crate) fn status(ctx: &AppContext) -> Result<()> {
    let _ = ssh_status(
        ctx,
        "systemctl --user status vbox-controld.service --no-pager",
    );
    let started_tunnel = start_control_tunnel(ctx)?;
    let mut cmd = client_ctl_command(ctx);
    cmd.args(["ctl", "status", &ctx.control_addr()]);
    let result = process::run(cmd);
    if started_tunnel {
        kill_pid_file(&run_dir(ctx).join("control_tunnel.pid"));
    }
    result
}

pub(crate) fn controld_failure(ctx: &AppContext) -> Result<()> {
    let guest_dir = shell_quote(ctx.guest_dir()?.as_os_str());
    ssh_status(
        ctx,
        &format!(
            "systemctl --user status vbox-controld.service --no-pager | tail -n 20; journalctl --user -u vbox-controld.service -n 30 --no-pager || true; tail -30 {guest_dir}/.vbox-controld/daemon.log 2>/dev/null || true; pgrep -af vbox-controld || true"
        ),
    )
}

pub(crate) fn bootstrap(ctx: &AppContext) -> Result<()> {
    ssh(
        ctx,
        "loginctl enable-linger \"$USER\" 2>/dev/null || true; for g in audio render; do if getent group \"$g\" >/dev/null 2>&1; then sudo usermod -aG \"$g\" \"$USER\"; fi; done",
    )
}

pub(crate) fn doctor(ctx: &AppContext) -> Result<()> {
    let guest = ctx.guest()?;
    let guest_dir = ctx.guest_dir()?;
    println!("== host ==");
    println!("root={}", ctx.root.display());
    println!("guest={guest}");
    println!("guest_dir={}", guest_dir.display());
    println!(
        "instance={} port={} socket={}",
        ctx.instance, ctx.port, ctx.socket
    );
    let mut cargo = process::command("cargo");
    cargo.arg("--version");
    let _ = process::run(cargo);
    println!("== guest ==");
    ssh_status(
        ctx,
        "id; command -v cargo >/dev/null && cargo --version || echo 'cargo: missing'; command -v rustc >/dev/null && rustc --version || echo 'rustc: missing'; systemctl --user is-active vbox-controld.service || true",
    )
}

pub(crate) fn tls_bootstrap(
    ctx: &AppContext,
    sans: Vec<String>,
    out_dir: Option<PathBuf>,
) -> Result<()> {
    let mut cmd = process::command(ctx.client_bin());
    cmd.args(["ctl", "tls-bootstrap"]);
    let sans = if sans.is_empty() {
        let guest = ctx.guest()?;
        vec![
            "vbox-controld".to_string(),
            guest
                .split('@')
                .next_back()
                .unwrap_or("127.0.0.1")
                .to_string(),
            "127.0.0.1".to_string(),
        ]
    } else {
        sans
    };
    for san in sans {
        cmd.arg("--san").arg(san);
    }
    cmd.arg("--out-dir")
        .arg(out_dir.unwrap_or_else(|| ctx.state_dir.join("tls")));
    process::run(cmd)
}

pub(crate) fn library(ctx: &AppContext, args: LibraryArgs) -> Result<()> {
    if args.refresh || !app_cache(ctx).is_file() {
        refresh_app_library(ctx)?;
    }
    let rows = read_app_cache(ctx)?;
    println!("{:<42}  {:<32}  EXEC", "APP_ID", "NAME");
    println!("{:<42}  {:<32}  ----", "------", "----");
    for row in rows {
        if selected(&row.id, &row.name, &args.filters) {
            println!("{:<42}  {:<32}  {}", row.id, row.name, row.exec);
        }
    }
    Ok(())
}

pub(crate) fn library_ui(ctx: &AppContext) -> Result<()> {
    if !app_cache(ctx).is_file() {
        if let Err(err) = refresh_app_library(ctx) {
            eprintln!("[vbox] warning: app library refresh failed: {err:#}");
        }
    }
    crate::library_ui::open(ctx, &app_cache(ctx), &launcher_dir())
}

pub(crate) fn library_picker(ctx: &AppContext) -> Result<()> {
    library_ui(ctx)
}

pub(crate) fn cache_icons(ctx: &AppContext, refresh: bool) -> Result<()> {
    if refresh || !app_cache(ctx).is_file() {
        refresh_app_library(ctx)?;
    }
    let count = fetch_guest_icons(ctx, refresh)?;
    eprintln!(
        "[vbox] cached {count} icon(s) into {}",
        icon_cache_dir(ctx).display()
    );
    Ok(())
}

pub(crate) fn install_apps(ctx: &AppContext, args: InstallAppsArgs) -> Result<()> {
    if args.refresh || !app_cache(ctx).is_file() {
        refresh_app_library(ctx)?;
    }
    if ctx.build {
        let mut cmd = process::command("cargo");
        cmd.current_dir(&ctx.root).args([
            "build",
            "--release",
            "-p",
            "vbox-cli",
            "-p",
            "vbox-client",
        ]);
        process::run(cmd)?;
        sync(ctx)?;
        build_guest(ctx)?;
    }
    let main_app =
        crate::library_ui::build_main_library_app(ctx, &app_cache(ctx), &launcher_dir())?;
    eprintln!("[vbox] installed: vbox -> {}", main_app.display());
    if let Err(err) = fetch_guest_icons(ctx, false) {
        eprintln!("[vbox] warning: app icon cache refresh failed: {err:#}");
    }
    let rows = read_app_cache(ctx)?;
    let mut count = 0;
    for row in rows {
        if selected(&row.id, &row.name, &args.filters) {
            let path = build_launcher_app(ctx, &row)?;
            eprintln!("[vbox] installed: {} -> {}", row.name, path.display());
            count += 1;
        }
    }
    if count == 0 {
        bail!("no apps matched");
    }
    eprintln!(
        "[vbox] installed {count} launcher(s) into {}",
        launcher_dir().display()
    );
    Ok(())
}

pub(crate) fn install_launcher(ctx: &AppContext, app_id: String) -> Result<()> {
    let row = find_app_record(ctx, &app_id)?;
    let path = build_launcher_app(ctx, &row)?;
    eprintln!(
        "[vbox] Launchpad enabled: {} -> {}",
        row.name,
        path.display()
    );
    Ok(())
}

pub(crate) fn remove_apps(ctx: &AppContext, args: FilterArgs) -> Result<()> {
    if !app_cache(ctx).is_file() {
        refresh_app_library(ctx)?;
    }
    let mut count = 0;
    for row in read_app_cache(ctx)? {
        if selected(&row.id, &row.name, &args.filters) {
            remove_launcher_app(&row)?;
            eprintln!("[vbox] removed: {}", row.name);
            count += 1;
        }
    }
    if count == 0 {
        bail!("no apps matched");
    }
    Ok(())
}

pub(crate) fn remove_launcher(ctx: &AppContext, app_id: String) -> Result<()> {
    let row = find_app_record(ctx, &app_id)?;
    remove_launcher_app(&row)?;
    eprintln!("[vbox] Launchpad disabled: {}", row.name);
    Ok(())
}

pub(crate) fn launchpad_check(ctx: &AppContext, args: FilterArgs) -> Result<()> {
    if !app_cache(ctx).is_file() {
        refresh_app_library(ctx)?;
    }
    println!("{:<8}  {:<32}  DETAIL", "STATUS", "APP");
    println!("{:<8}  {:<32}  ------", "------", "---");
    for row in read_app_cache(ctx)? {
        if selected(&row.id, &row.name, &args.filters) {
            let path = launcher_app_path(&row);
            if path.is_dir() {
                println!("{:<8}  {:<32}  path={}", "OK", row.name, path.display());
            } else {
                println!(
                    "{:<8}  {:<32}  missing launcher bundle",
                    "MISSING", row.name
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn suffix(ctx: &AppContext, args: SuffixArgs) -> Result<()> {
    let path = ctx.state_dir.join("launcher-suffix.txt");
    if args.clear {
        let _ = fs::remove_file(&path);
        eprintln!("[vbox] launcher suffix cleared; reverting to default");
        return Ok(());
    }
    if args.value.is_empty() {
        if let Ok(value) = fs::read_to_string(&path) {
            print!("{value}");
        } else {
            println!("(none - installed names will have no suffix)");
        }
        return Ok(());
    }
    fs::create_dir_all(&ctx.state_dir)?;
    let value = args
        .value
        .iter()
        .map(|v| v.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    fs::write(&path, format!("{value}\n"))?;
    eprintln!("[vbox] launcher suffix set to: {value}");
    Ok(())
}

pub(crate) fn input(ctx: &AppContext, event: &str, args: Vec<OsString>) -> Result<()> {
    let id = brand::env_var("VBOX_WINDOW_ID").unwrap_or_else(|| "1".to_string());
    let mut cmd = process::command(ctx.client_bin());
    cmd.arg("input")
        .arg(ctx.local_addr())
        .arg("--id")
        .arg(id)
        .arg(event)
        .args(args);
    process::run(cmd)
}

pub(crate) fn text_input(ctx: &AppContext, event: &str, args: TextArgs) -> Result<()> {
    input(ctx, event, args.text)
}

pub(crate) fn volume(ctx: &AppContext, args: VolumeArgs) -> Result<()> {
    let mut cmd = process::command(ctx.client_bin());
    cmd.arg("volume")
        .arg(ctx.local_addr())
        .arg("--level")
        .arg(args.level.to_string());
    if args.mute {
        cmd.arg("--muted");
    }
    if args.unmute {
        cmd.arg("--unmuted");
    }
    process::run(cmd)
}

pub(crate) fn processes(ctx: &AppContext) -> Result<()> {
    ssh_status(
        ctx,
        "ps -eo pid,lstart,comm,args | grep -E 'WAYLAND|gnome|vbox|vbox' || true",
    )
}

pub(crate) fn kill_pid(ctx: &AppContext, pids: Vec<u32>) -> Result<()> {
    let joined = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    ssh(ctx, &format!("kill {joined} 2>/dev/null || true"))
}

pub(crate) fn windows(ctx: &AppContext) -> Result<()> {
    let viewer_pid = run_dir(ctx).join("viewer.pid");
    if let Ok(pid) = fs::read_to_string(&viewer_pid) {
        let _ = Command::new("kill").arg("-USR1").arg(pid.trim()).status();
    }
    ssh_status(
        ctx,
        "for pid in $(pgrep -f 'target/release/vbox-server' 2>/dev/null); do kill -USR1 \"$pid\" 2>/dev/null || true; done",
    )
}

pub(crate) fn logs(ctx: &AppContext, args: LogsArgs) -> Result<()> {
    let log_dir = log_dir(ctx);
    if args.follow {
        let guest = ctx.guest()?;
        let guest_dir = ctx.guest_dir()?;
        let script = format!(
            "tail -F {}/*.log 2>/dev/null & local_pid=$!; ssh -t {} 'cd {} && tail -F .vbox/logs/{}/server.log .vbox/logs/{}/app.log' || true; kill $local_pid 2>/dev/null || true",
            shell_quote(log_dir.as_os_str()),
            shell_quote(OsStr::new(guest)),
            shell_quote(guest_dir.as_os_str()),
            shell_quote(OsStr::new(&ctx.instance)),
            shell_quote(OsStr::new(&ctx.instance)),
        );
        return process::run(process::piped_shell(&script));
    }
    let needle = matches!(args.mode, Some(LogsMode::Input));
    for name in ["client.log", "tunnel.log", "ping.log"] {
        let path = log_dir.join(name);
        if path.is_file() {
            println!("== local:{} ==", path.display());
            let text = fs::read_to_string(&path).unwrap_or_default();
            for line in text
                .lines()
                .rev()
                .take(120)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                if !needle
                    || line.contains("input")
                    || line.contains("ime")
                    || line.contains("keyboard")
                {
                    println!("{line}");
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn stop(ctx: &AppContext) -> Result<()> {
    kill_pid_file(&run_dir(ctx).join("viewer.pid"));
    kill_pid_file(&run_dir(ctx).join("tunnel.pid"));
    let _ = start_control_tunnel(ctx);
    let mut cmd = client_ctl_command(ctx);
    cmd.args(["ctl", "stop-instance", &ctx.control_addr(), &ctx.instance]);
    let _ = process::run(cmd);
    kill_pid_file(&run_dir(ctx).join("control_tunnel.pid"));
    Ok(())
}

fn kill_pid_file(path: &std::path::Path) {
    if let Ok(pid) = fs::read_to_string(path) {
        let _ = Command::new("kill")
            .arg(pid.trim())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_file(path);
    }
}

fn client_ctl_command(ctx: &AppContext) -> Command {
    let mut cmd = process::command(ctx.client_bin());
    add_default_tls_env(ctx, &mut cmd);
    cmd
}

fn add_default_tls_env(ctx: &AppContext, cmd: &mut Command) {
    let tls_dir = ctx.state_dir.join("tls");
    let ca = tls_dir.join("ca.pem");
    let cert = tls_dir.join("client.pem");
    let key = tls_dir.join("client.key.pem");
    if !(ca.is_file() && cert.is_file() && key.is_file()) {
        return;
    }
    if brand::env_os("VBOX_TLS_CA").is_none() {
        cmd.env("VBOX_TLS_CA", ca);
    }
    if brand::env_os("VBOX_TLS_CERT").is_none() {
        cmd.env("VBOX_TLS_CERT", cert);
    }
    if brand::env_os("VBOX_TLS_KEY").is_none() {
        cmd.env("VBOX_TLS_KEY", key);
    }
    if brand::env_os("VBOX_TLS_SERVER_NAME").is_none() {
        cmd.env("VBOX_TLS_SERVER_NAME", "vbox-controld");
    }
}

pub(crate) fn debug_bundle(ctx: &AppContext) -> Result<()> {
    fs::create_dir_all(&ctx.state_dir)?;
    let out = ctx.state_dir.join("debug-bundle.txt");
    fs::write(
        &out,
        format!(
            "root={}\nguest={}\nguest_dir={}\ninstance={}\nport={}\nsocket={}\n",
            ctx.root.display(),
            ctx.guest.as_deref().unwrap_or("<unset>"),
            ctx.guest_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unset>".to_string()),
            ctx.instance,
            ctx.port,
            ctx.socket
        ),
    )?;
    eprintln!("[vbox] debug bundle: {}", out.display());
    Ok(())
}

fn start_stack(ctx: &AppContext) -> Result<()> {
    if ctx.build {
        build(ctx)?;
    }
    start_control_tunnel(ctx)?;
    start_server(ctx)?;
    start_tunnel(ctx)?;
    wait_ping(ctx)
}

fn start_server(ctx: &AppContext) -> Result<()> {
    let mut cmd = client_ctl_command(ctx);
    cmd.args([
        "ctl",
        "start-instance",
        &ctx.control_addr(),
        &ctx.instance,
        &ctx.port.to_string(),
    ]);
    if ctx.debug {
        cmd.arg("--debug");
    }
    process::run(cmd)
}

fn start_tunnel(ctx: &AppContext) -> Result<()> {
    let guest = ctx.guest()?;
    let dir = log_dir(ctx);
    fs::create_dir_all(&dir)?;
    let log = fs::File::create(dir.join("tunnel.log"))?;
    let child = Command::new("ssh")
        .arg("-N")
        .arg("-L")
        .arg(format!("127.0.0.1:{}:localhost:{}", ctx.port, ctx.port))
        .arg(guest)
        .stdout(log.try_clone()?)
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .context("start ssh tunnel")?;
    fs::create_dir_all(run_dir(ctx))?;
    fs::write(run_dir(ctx).join("tunnel.pid"), child.id().to_string())?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

fn start_control_tunnel(ctx: &AppContext) -> Result<bool> {
    if TcpStream::connect(ctx.control_addr()).is_ok() {
        return Ok(false);
    }
    let guest = ctx.guest()?;
    let dir = log_dir(ctx);
    fs::create_dir_all(&dir)?;
    kill_pid_file(&run_dir(ctx).join("control_tunnel.pid"));
    let port = brand::env_var("VBOX_CONTROL_PORT").unwrap_or_else(|| "5711".to_string());
    let log = fs::File::create(dir.join("control_tunnel.log"))?;
    let child = Command::new("ssh")
        .arg("-N")
        .arg("-L")
        .arg(format!("127.0.0.1:{port}:localhost:{port}"))
        .arg(guest)
        .stdout(log.try_clone()?)
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .context("start control ssh tunnel")?;
    fs::create_dir_all(run_dir(ctx))?;
    fs::write(
        run_dir(ctx).join("control_tunnel.pid"),
        child.id().to_string(),
    )?;
    thread::sleep(Duration::from_millis(500));
    Ok(true)
}

fn wait_ping(ctx: &AppContext) -> Result<()> {
    for _ in 0..40 {
        if ping(ctx, 1).is_ok() {
            eprintln!("[vbox] server ping ok");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    bail!("server did not answer ping")
}

fn ping(ctx: &AppContext, timeout_secs: u64) -> Result<()> {
    let mut cmd = process::command(ctx.client_bin());
    cmd.args([
        "ping",
        &ctx.local_addr(),
        "--timeout-secs",
        &timeout_secs.to_string(),
    ]);
    process::run(cmd)
}

fn start_viewer_bg(ctx: &AppContext) -> Result<()> {
    let dir = log_dir(ctx);
    fs::create_dir_all(&dir)?;
    let log = fs::File::create(dir.join("client.log"))?;
    let child = Command::new(ctx.client_bin())
        .arg("view")
        .arg(ctx.local_addr())
        .arg("--socket-name")
        .arg(&ctx.socket)
        .arg("--width")
        .arg(ctx.width.to_string())
        .arg("--height")
        .arg(ctx.height.to_string())
        .env("VBOX_DEBUG", if ctx.debug { "1" } else { "0" })
        .stdout(log.try_clone()?)
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .context("start viewer")?;
    fs::create_dir_all(run_dir(ctx))?;
    fs::write(run_dir(ctx).join("viewer.pid"), child.id().to_string())?;
    thread::sleep(Duration::from_millis(300));
    Ok(())
}

fn wait_socket(ctx: &AppContext) -> Result<()> {
    let mut cmd = client_ctl_command(ctx);
    cmd.args([
        "ctl",
        "wait-socket",
        &ctx.control_addr(),
        &ctx.socket,
        "--timeout-ms",
        "5000",
    ]);
    process::run(cmd)
}

fn launch_app(ctx: &AppContext, argv: Vec<OsString>) -> Result<()> {
    let mut argv = argv;
    if argv.is_empty() {
        argv.push("gnome-calculator".into());
    }
    let mut cmd = client_ctl_command(ctx);
    cmd.args([
        "ctl",
        "launch-app",
        &ctx.control_addr(),
        "--wait-ready-ms",
        &brand::env_var("VBOX_LAUNCH_WAIT_READY_MS").unwrap_or_else(|| "500".to_string()),
        &ctx.instance,
        &ctx.socket,
        "--",
    ])
    .args(argv);
    process::run(cmd)
}

fn wait_viewer(ctx: &AppContext) -> Result<()> {
    let path = run_dir(ctx).join("viewer.pid");
    let pid = fs::read_to_string(&path).unwrap_or_default();
    if pid.trim().is_empty() {
        return Ok(());
    }
    while pid_alive_non_zombie(pid.trim()) {
        thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

fn pid_alive_non_zombie(pid: &str) -> bool {
    if !Command::new("kill")
        .arg("-0")
        .arg(pid)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return false;
    }
    let Ok(output) = Command::new("ps").args(["-o", "stat=", "-p", pid]).output() else {
        return true;
    };
    !String::from_utf8_lossy(&output.stdout)
        .trim()
        .starts_with('Z')
}

fn build_guest(ctx: &AppContext) -> Result<()> {
    ssh(
        ctx,
        &format!(
            "cd {} && cargo build --release -p vbox-server -p vbox-controld",
            shell_quote(ctx.guest_dir()?.as_os_str())
        ),
    )
}

fn fetch_control_token(ctx: &AppContext) -> Result<()> {
    let token_file = brand::env_os("VBOX_CONTROL_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.state_dir.join("control.token"));
    if let Some(parent) = token_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let output = process::output({
        let mut cmd = process::command("ssh");
        let guest_dir = ctx.guest_dir()?;
        cmd.arg(ctx.guest()?)
            .arg(format!("cat {}/.vbox-controld/token", guest_dir.display()));
        cmd
    })?;
    fs::write(&token_file, output)?;
    Ok(())
}

fn tls_bootstrap_default(ctx: &AppContext) -> Result<()> {
    tls_bootstrap(ctx, Vec::new(), None)
}

fn ssh(ctx: &AppContext, script: &str) -> Result<()> {
    let mut cmd = process::command("ssh");
    cmd.arg(ctx.guest()?)
        .arg(format!("bash -lc {}", shell_quote(OsStr::new(script))));
    process::run(cmd)
}

fn ssh_status(ctx: &AppContext, script: &str) -> Result<()> {
    let mut cmd = process::command("ssh");
    cmd.arg(ctx.guest()?)
        .arg(format!("bash -lc {}", shell_quote(OsStr::new(script))));
    let _ = process::run(cmd);
    Ok(())
}

fn default_app_args(args: Vec<OsString>) -> Vec<OsString> {
    if args.is_empty() {
        vec![OsString::from("gnome-calculator")]
    } else {
        args
    }
}

fn run_dir(ctx: &AppContext) -> PathBuf {
    ctx.state_dir.join("run").join(&ctx.instance)
}

fn log_dir(ctx: &AppContext) -> PathBuf {
    ctx.state_dir.join("logs").join(&ctx.instance)
}

fn app_cache(ctx: &AppContext) -> PathBuf {
    ctx.state_dir.join("app-library.tsv")
}

fn machine_id(ctx: &AppContext) -> Option<String> {
    ctx.guest
        .as_deref()
        .filter(|guest| !guest.trim().is_empty())
        .map(sanitize_guest_id)
}

fn machine_dir(ctx: &AppContext) -> Option<PathBuf> {
    machine_id(ctx).map(|id| ctx.state_dir.join("machines").join(id))
}

fn machine_app_cache(ctx: &AppContext) -> Option<PathBuf> {
    machine_dir(ctx).map(|dir| dir.join("app-library.tsv"))
}

fn icon_cache_dir(ctx: &AppContext) -> PathBuf {
    ctx.state_dir.join("icons")
}

fn machine_icon_dir(ctx: &AppContext) -> Option<PathBuf> {
    machine_dir(ctx).map(|dir| dir.join("icons"))
}

fn icon_cache_dirs(ctx: &AppContext) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = machine_icon_dir(ctx) {
        dirs.push(dir);
    }
    dirs.push(icon_cache_dir(ctx));
    dirs
}

#[derive(Debug, Clone)]
struct AppRecord {
    id: String,
    name: String,
    exec: String,
    icon: String,
    argv_b64: String,
    argv: Vec<OsString>,
}

fn read_app_cache(ctx: &AppContext) -> Result<Vec<AppRecord>> {
    let text = fs::read_to_string(app_cache(ctx)).unwrap_or_default();
    Ok(text
        .lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            if parts.len() < 7 {
                return None;
            }
            let argv = decode_argv_b64(parts[6]).unwrap_or_else(|| vec![OsString::from(parts[2])]);
            Some(AppRecord {
                id: parts[0].to_string(),
                name: parts[1].to_string(),
                exec: parts[2].to_string(),
                icon: parts[3].to_string(),
                argv_b64: parts[6].to_string(),
                argv,
            })
        })
        .collect())
}

fn find_app_record(ctx: &AppContext, query: &str) -> Result<AppRecord> {
    if !app_cache(ctx).is_file() {
        refresh_app_library(ctx)?;
    }
    read_app_cache(ctx)?
        .into_iter()
        .find(|row| selected(&row.id, &row.name, &[query.to_string()]))
        .with_context(|| format!("unknown app id/name: {query}"))
}

fn refresh_app_library(ctx: &AppContext) -> Result<()> {
    let mut cmd = process::command("ssh");
    cmd.arg(ctx.guest()?).arg(format!(
        "python3 -c {}",
        shell_quote(OsStr::new(APP_LIBRARY_PY))
    ));
    let out = process::output(cmd)?;
    write_app_cache(ctx, &out)?;
    Ok(())
}

fn write_app_cache(ctx: &AppContext, text: &str) -> Result<()> {
    let path = app_cache(ctx);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, text)?;
    if let Some(path) = machine_app_cache(ctx) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
    }
    Ok(())
}

fn fetch_guest_icons(ctx: &AppContext, refresh: bool) -> Result<usize> {
    let rows = read_app_cache(ctx)?;
    if rows.is_empty() {
        return Ok(0);
    }

    let mut cmd = process::command("ssh");
    cmd.arg(ctx.guest()?).arg(format!(
        "python3 -c {}",
        shell_quote(OsStr::new(APP_ICON_EXPORT_PY))
    ));
    let out = process::output(cmd)?;

    let dirs = icon_cache_dirs(ctx);
    for dir in &dirs {
        fs::create_dir_all(dir)?;
    }
    let primary = icon_cache_dir(ctx);
    let mut cached = 0;

    for line in out.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(app_id), Some(ext), Some(encoded)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !rows.iter().any(|row| row.id == app_id) {
            continue;
        }
        let safe = sanitize_icon_id(app_id);
        if safe.is_empty() {
            continue;
        }
        if !refresh
            && dirs
                .iter()
                .any(|dir| dir.join(format!("{safe}.png")).is_file())
        {
            cached += 1;
            continue;
        }

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .with_context(|| format!("decode icon for {app_id}"))?;
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        let source_tmp = primary.join(format!(".{safe}.source.{ext}.tmp"));
        let png_tmp = primary.join(format!(".{safe}.png.tmp"));
        let png_path = primary.join(format!("{safe}.png"));
        let icns_tmp = primary.join(format!(".{safe}.tmp.icns"));
        let icns_path = primary.join(format!("{safe}.icns"));

        fs::write(&source_tmp, bytes)?;
        let converted = convert_guest_icon_to_png(&source_tmp, &ext, &png_tmp);
        let _ = fs::remove_file(&source_tmp);
        if !converted {
            let _ = fs::remove_file(&png_tmp);
            eprintln!("[vbox] warning: icon convert failed for {app_id}");
            continue;
        }
        fs::rename(&png_tmp, &png_path)?;
        if convert_png_to_icns(&png_path, &icns_tmp) {
            let _ = fs::rename(&icns_tmp, &icns_path);
        } else {
            let _ = fs::remove_file(&icns_tmp);
        }

        for dir in dirs.iter().filter(|dir| **dir != primary) {
            fs::create_dir_all(dir)?;
            let _ = fs::copy(&png_path, dir.join(format!("{safe}.png")));
            if icns_path.is_file() {
                let _ = fs::copy(&icns_path, dir.join(format!("{safe}.icns")));
            }
        }
        cached += 1;
    }

    Ok(cached)
}

fn convert_guest_icon_to_png(input: &Path, ext: &str, png: &Path) -> bool {
    match ext {
        "png" => fs::copy(input, png).is_ok(),
        "svg" => convert_svg_to_png(input, png),
        _ => std::process::Command::new("sips")
            .args(["-s", "format", "png"])
            .arg(input)
            .arg("--out")
            .arg(png)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
    }
}

fn convert_svg_to_png(svg: &Path, png: &Path) -> bool {
    if std::process::Command::new("rsvg-convert")
        .args(["-w", "256", "-h", "256", "-a", "-o"])
        .arg(png)
        .arg(svg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    let Some(parent) = png.parent() else {
        return false;
    };
    let tmp_dir = parent.join(format!(
        ".qlmanage-icon-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp_dir);
    if fs::create_dir_all(&tmp_dir).is_err() {
        return false;
    }
    let ok = std::process::Command::new("qlmanage")
        .args(["-t", "-s", "256", "-o"])
        .arg(&tmp_dir)
        .arg(svg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        let generated = tmp_dir.join(format!(
            "{}.png",
            svg.file_name().unwrap_or_default().to_string_lossy()
        ));
        if generated.is_file() && fs::rename(&generated, png).is_ok() {
            let _ = fs::remove_dir_all(&tmp_dir);
            return true;
        }
    }
    let _ = fs::remove_dir_all(&tmp_dir);
    false
}

fn convert_png_to_icns(png: &Path, icns: &Path) -> bool {
    let Some(parent) = icns.parent() else {
        return false;
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let iconset = parent.join(format!(".vbox-icon-{}-{stamp}.iconset", std::process::id()));
    let _ = fs::remove_dir_all(&iconset);
    if fs::create_dir_all(&iconset).is_err() {
        return false;
    }
    let sizes = [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ];
    for (name, size) in sizes {
        let ok = std::process::Command::new("sips")
            .args(["-z", &size.to_string(), &size.to_string()])
            .arg(png)
            .arg("--out")
            .arg(iconset.join(name))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            let _ = fs::remove_dir_all(&iconset);
            return false;
        }
    }
    let ok = std::process::Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&iconset)
        .arg("-o")
        .arg(icns)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = fs::remove_dir_all(&iconset);
    ok
}

fn selected(id: &str, name: &str, filters: &[String]) -> bool {
    filters.is_empty()
        || filters.iter().any(|f| {
            let f = f.to_ascii_lowercase();
            id.to_ascii_lowercase().contains(&f) || name.to_ascii_lowercase().contains(&f)
        })
}

fn decode_argv_b64(_value: &str) -> Option<Vec<OsString>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(_value)
        .ok()?;
    let argv: Vec<String> = serde_json::from_slice(&bytes).ok()?;
    Some(argv.into_iter().map(OsString::from).collect())
}

fn instance_context(ctx: &AppContext, instance: &str) -> Result<AppContext> {
    let mut out = ctx.clone();
    out.instance = instance.to_string();
    out.socket = if instance == "default" {
        "vbox-0".to_string()
    } else {
        format!("vbox-{}", sanitize_id(instance))
    };
    out.port = instance_port(ctx, instance)?;
    Ok(out)
}

fn instance_port(ctx: &AppContext, instance: &str) -> Result<u16> {
    if instance == "default" {
        return Ok(ctx.port);
    }

    let path = ctx.state_dir.join(INSTANCE_PORT_ALLOC_FILE);
    let mut entries = read_instance_ports(&path)?;
    if let Some((_, port)) = entries.iter().find(|(name, _)| name == instance) {
        return Ok(*port);
    }

    let reserved = reserved_ports(ctx);
    let start = (u32::from(ctx.port) + INSTANCE_PORT_OFFSET).max(1024);
    let end = (start + INSTANCE_PORT_SPAN - 1).min(u32::from(u16::MAX));
    if start > u32::from(u16::MAX) {
        bail!(
            "no free instance port available after base port {}",
            ctx.port
        );
    }
    for raw in start..=end {
        let candidate = raw as u16;
        if reserved.contains(&candidate) {
            continue;
        }
        if entries.iter().any(|(_, port)| *port == candidate) {
            continue;
        }
        entries.push((instance.to_string(), candidate));
        write_instance_ports(&path, &entries)?;
        return Ok(candidate);
    }

    bail!("no free instance port in {}-{} for {instance}", start, end)
}

fn reserved_ports(ctx: &AppContext) -> Vec<u16> {
    let mut ports = vec![ctx.port];
    let control = brand::env_var("VBOX_CONTROL_PORT")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(5711);
    if !ports.contains(&control) {
        ports.push(control);
    }
    ports
}

fn read_instance_ports(path: &Path) -> Result<Vec<(String, u16)>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let mut entries = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(instance) = fields.next() else {
            continue;
        };
        let Some(port) = fields.next() else {
            bail!(
                "malformed {} line {}: missing port",
                path.display(),
                idx + 1
            );
        };
        if fields.next().is_some() {
            bail!(
                "malformed {} line {}: expected instance and port",
                path.display(),
                idx + 1
            );
        }
        entries.push((
            instance.to_string(),
            port.parse()
                .with_context(|| format!("parse {} line {} port", path.display(), idx + 1))?,
        ));
    }
    Ok(entries)
}

fn write_instance_ports(path: &Path, entries: &[(String, u16)]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    for (instance, port) in entries {
        out.push_str(instance);
        out.push('\t');
        out.push_str(&port.to_string());
        out.push('\n');
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, out).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn sanitize_icon_id(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_was_dash = true;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            ch
        } else {
            '-'
        };
        if mapped == '-' {
            if !last_was_dash {
                out.push('-');
                last_was_dash = true;
            }
        } else {
            out.push(mapped);
            last_was_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn sanitize_guest_id(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut previous_was_underscore = false;
    for ch in value.replace('@', "_at_").chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
            previous_was_underscore = false;
        } else if !previous_was_underscore {
            out.push('_');
            previous_was_underscore = true;
        }
    }
    out
}

fn shell_quote(value: &OsStr) -> String {
    let s = value.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn launcher_dir() -> PathBuf {
    brand::env_os("VBOX_LAUNCHER_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications/vbox"))
        })
        .unwrap_or_else(|| PathBuf::from("Applications/vbox"))
}

fn launcher_app_path(row: &AppRecord) -> PathBuf {
    launcher_dir().join(format!(
        "{}.app",
        safe_app_filename(&launcher_display_name(&row.name))
    ))
}

fn launcher_display_name(name: &str) -> String {
    if let Some(suffix) = brand::env_var("VBOX_LAUNCHER_SUFFIX") {
        if !suffix.is_empty() {
            return format!("{name} ({suffix})");
        }
    }
    name.to_string()
}

fn safe_app_filename(value: &str) -> String {
    value.replace(['/', ':'], "-").trim().to_string()
}

fn build_launcher_app(ctx: &AppContext, row: &AppRecord) -> Result<PathBuf> {
    if std::env::consts::OS != "macos" {
        bail!("app launchers are only supported on macOS");
    }
    let app = launcher_app_path(row);
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    let _ = fs::remove_file(resources.join("Socket.txt"));
    let has_icon = copy_launcher_icon(ctx, row, &resources)?;
    let safe_id = sanitize_id(&row.id);
    let app_ctx = instance_context(ctx, &safe_id)?;
    let executable = format!("vbox-client-{safe_id}");
    let bundle_id = format!("local.vbox.native.app.{safe_id}");
    let icon_plist = if has_icon {
        "  <key>CFBundleIconFile</key><string>AppIcon</string>\n"
    } else {
        ""
    };
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>{}</string>
  <key>CFBundleIdentifier</key><string>{}</string>
  <key>CFBundleName</key><string>{}</string>
  <key>CFBundleDisplayName</key><string>{}</string>
{}  <key>LSUIElement</key><false/>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
            plist_escape(&executable),
            plist_escape(&bundle_id),
            plist_escape(&row.name),
            plist_escape(&launcher_display_name(&row.name)),
            icon_plist
        ),
    )?;
    fs::copy(ctx.client_bin(), macos.join(&executable))
        .with_context(|| format!("copy {}", ctx.client_bin().display()))?;
    let _ = Command::new("chmod")
        .arg("+x")
        .arg(macos.join(&executable))
        .status();
    fs::write(resources.join("AppID.txt"), format!("{}\n", row.id))?;
    fs::write(resources.join("AppName.txt"), format!("{}\n", row.name))?;
    fs::write(resources.join("Exec.txt"), format!("{}\n", row.exec))?;
    fs::write(resources.join("Argv.b64"), format!("{}\n", row.argv_b64))?;
    fs::write(
        resources.join("VBoxNativePath.txt"),
        format!("{}\n", ctx.cli_path.display()),
    )?;
    fs::write(
        resources.join("Root.txt"),
        format!("{}\n", ctx.root.display()),
    )?;
    fs::write(
        resources.join("StateDir.txt"),
        format!("{}\n", ctx.state_dir.display()),
    )?;
    fs::write(
        resources.join("Guest.txt"),
        format!("{}\n", ctx.guest.as_deref().unwrap_or_default()),
    )?;
    fs::write(
        resources.join("GuestDir.txt"),
        format!(
            "{}\n",
            ctx.guest_dir
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        ),
    )?;
    fs::write(resources.join("Port.txt"), format!("{}\n", app_ctx.port))?;
    fs::write(resources.join("Width.txt"), format!("{}\n", ctx.width))?;
    fs::write(resources.join("Height.txt"), format!("{}\n", ctx.height))?;
    fs::write(
        resources.join("IconDir.txt"),
        format!("{}\n", icon_cache_dir(ctx).display()),
    )?;
    fs::write(resources.join("Instance.txt"), format!("{safe_id}\n"))?;
    let _ = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(&app)
        .status();
    Ok(app)
}

fn copy_launcher_icon(ctx: &AppContext, row: &AppRecord, resources: &Path) -> Result<bool> {
    if let Some(icns) = cached_icon_path(ctx, row, &["icns"]) {
        fs::copy(icns, resources.join("AppIcon.icns"))?;
        return Ok(true);
    }
    if let Some(png) = cached_icon_path(ctx, row, &["png"]) {
        fs::copy(&png, resources.join("AppIcon.png"))?;
        if convert_png_to_icns(&png, &resources.join("AppIcon.icns")) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cached_icon_path(ctx: &AppContext, row: &AppRecord, exts: &[&str]) -> Option<PathBuf> {
    let mut names = vec![sanitize_icon_id(&row.id)];
    let icon = sanitize_icon_id(&row.icon);
    if !icon.is_empty() && !names.iter().any(|name| name == &icon) {
        names.push(icon);
    }
    for dir in icon_cache_dirs(ctx) {
        for name in &names {
            if name.is_empty() {
                continue;
            }
            for ext in exts {
                let path = dir.join(format!("{name}.{ext}"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn remove_launcher_app(row: &AppRecord) -> Result<()> {
    let app = launcher_app_path(row);
    if app.is_dir() {
        fs::remove_dir_all(&app)?;
    }
    Ok(())
}

fn plist_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AppContext;
    use crate::test_env;
    use crate::{LogsArgs, SuffixArgs};

    struct TempDir {
        path: PathBuf,
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn tempdir_for_test() -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "vbox-runtime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn ctx(dir: &TempDir) -> AppContext {
        let client_bin = dir.path.join("target/release/vbox-client");
        fs::create_dir_all(client_bin.parent().unwrap()).unwrap();
        fs::write(&client_bin, b"fake client").unwrap();
        AppContext {
            root: dir.path.clone(),
            state_dir: dir.path.join(".vbox"),
            cli_path: dir.path.join("target/release/vbox"),
            guest: Some("alice@example.test".to_string()),
            guest_dir: Some(PathBuf::from("/home/alice/vbox")),
            instance: "default".to_string(),
            port: 5710,
            socket: "vbox-0".to_string(),
            width: 1024,
            height: 768,
            debug: false,
            build: false,
        }
    }

    #[test]
    fn default_app_args_supplies_calculator_only_for_empty_args() {
        assert_eq!(
            default_app_args(Vec::new()),
            vec![OsString::from("gnome-calculator")]
        );
        assert_eq!(
            default_app_args(vec![OsString::from("gedit")]),
            vec![OsString::from("gedit")]
        );
    }

    #[test]
    fn runtime_dirs_are_instance_scoped() {
        let dir = tempdir_for_test();
        let mut ctx = ctx(&dir);
        ctx.instance = "dev".to_string();
        assert_eq!(run_dir(&ctx), ctx.state_dir.join("run/dev"));
        assert_eq!(log_dir(&ctx), ctx.state_dir.join("logs/dev"));
        assert_eq!(app_cache(&ctx), ctx.state_dir.join("app-library.tsv"));
    }

    #[test]
    fn read_app_cache_skips_short_rows_and_decodes_argv() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        fs::create_dir_all(&ctx.state_dir).unwrap();
        let argv = serde_json::to_vec(&vec!["/usr/bin/app", "--flag"]).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(argv);
        fs::write(
            app_cache(&ctx),
            format!(
                "too\tshort\norg.app\tApp\t/usr/bin/app\ticon\t/app.desktop\tUtility\t{encoded}\norg.bad\tBad\tbad\ticon\tbad.desktop\tUtility\tnot-base64\n"
            ),
        )
        .unwrap();

        let rows = read_app_cache(&ctx).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].argv,
            vec![OsString::from("/usr/bin/app"), OsString::from("--flag")]
        );
        assert_eq!(rows[1].argv, vec![OsString::from("bad")]);
    }

    #[test]
    fn selected_matches_empty_filters_or_case_insensitive_substrings() {
        assert!(selected("org.gnome.Calculator", "Calculator", &[]));
        assert!(selected(
            "org.gnome.Calculator",
            "Calculator",
            &["calc".to_string()]
        ));
        assert!(!selected(
            "org.gnome.Calculator",
            "Calculator",
            &["terminal".to_string()]
        ));
    }

    #[test]
    fn decode_argv_b64_rejects_invalid_payloads() {
        let argv = serde_json::to_vec(&vec!["app", "--verbose"]).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(argv);
        assert_eq!(
            decode_argv_b64(&encoded).unwrap(),
            vec![OsString::from("app"), OsString::from("--verbose")]
        );
        assert!(decode_argv_b64("not base64").is_none());
    }

    #[test]
    fn instance_context_resets_socket_for_default_and_sanitizes_named_instances() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let default = instance_context(&ctx, "default").unwrap();
        assert_eq!(default.socket, "vbox-0");
        assert_eq!(default.port, 5710);
        let named = instance_context(&ctx, "dev box!!west").unwrap();
        assert_eq!(named.instance, "dev box!!west");
        assert_eq!(named.socket, "vbox-dev-box--west");
        assert_eq!(named.port, 5712);
    }

    #[test]
    fn instance_context_reuses_stable_distinct_ports_for_named_instances() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);

        let calc = instance_context(&ctx, "org-gnome-Calculator").unwrap();
        let maps = instance_context(&ctx, "org-gnome-Maps").unwrap();
        let calc_again = instance_context(&ctx, "org-gnome-Calculator").unwrap();

        assert_eq!(calc.port, 5712);
        assert_eq!(maps.port, 5713);
        assert_eq!(calc_again.port, calc.port);
        assert_eq!(
            fs::read_to_string(ctx.state_dir.join(INSTANCE_PORT_ALLOC_FILE)).unwrap(),
            "org-gnome-Calculator\t5712\norg-gnome-Maps\t5713\n"
        );
    }

    #[test]
    fn shell_quote_and_plist_escape_cover_special_characters() {
        assert_eq!(shell_quote(OsStr::new("a'b")), "'a'\\''b'");
        assert_eq!(plist_escape("A&B<C>"), "A&amp;B&lt;C&gt;");
    }

    #[test]
    fn launcher_paths_apply_suffix_and_safe_filename() {
        let _guard = test_env::lock();
        let dir = tempdir_for_test();
        test_env::set_var("VBOX_LAUNCHER_DIR", dir.path.join("apps"));
        test_env::set_var("VBOX_LAUNCHER_SUFFIX", "Guest");
        let row = AppRecord {
            id: "org.example.App".to_string(),
            name: "Example/App: Editor ".to_string(),
            exec: "example".to_string(),
            icon: "org.example.App".to_string(),
            argv_b64: String::new(),
            argv: vec![OsString::from("example")],
        };
        assert_eq!(launcher_display_name("Files"), "Files (Guest)");
        assert_eq!(safe_app_filename(" Example/App: "), "Example-App-");
        assert_eq!(
            launcher_app_path(&row),
            dir.path.join("apps/Example-App- Editor  (Guest).app")
        );
        test_env::remove_var("VBOX_LAUNCHER_DIR");
        test_env::remove_var("VBOX_LAUNCHER_SUFFIX");
    }

    #[test]
    fn build_and_remove_launcher_app_create_expected_bundle_files() {
        let _guard = test_env::lock();
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let launcher_root = dir.path.join("launchers");
        test_env::set_var("VBOX_LAUNCHER_DIR", &launcher_root);
        test_env::set_var(
            "VBOX_CLIENT_BIN",
            ctx.root.join("target/release/vbox-client"),
        );

        let row = AppRecord {
            id: "org.example.App".to_string(),
            name: "Example".to_string(),
            exec: "example --flag".to_string(),
            icon: "org.example.App".to_string(),
            argv_b64: "YXJndg==".to_string(),
            argv: vec![OsString::from("example")],
        };
        let app = build_launcher_app(&ctx, &row).unwrap();
        assert!(app.join("Contents/Info.plist").is_file());
        assert_eq!(
            fs::read_to_string(app.join("Contents/Resources/AppID.txt")).unwrap(),
            "org.example.App\n"
        );
        assert_eq!(
            fs::read_to_string(app.join("Contents/Resources/Instance.txt")).unwrap(),
            "org-example-App\n"
        );
        assert_eq!(
            fs::read_to_string(app.join("Contents/Resources/Port.txt")).unwrap(),
            "5712\n"
        );
        remove_launcher_app(&row).unwrap();
        assert!(!app.exists());
        test_env::remove_var("VBOX_LAUNCHER_DIR");
        test_env::remove_var("VBOX_CLIENT_BIN");
    }

    #[test]
    fn suffix_writes_reads_and_clears_state_file() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        suffix(
            &ctx,
            SuffixArgs {
                clear: false,
                value: vec![OsString::from("Guest"), OsString::from("Apps")],
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(ctx.state_dir.join("launcher-suffix.txt")).unwrap(),
            "Guest Apps\n"
        );
        suffix(
            &ctx,
            SuffixArgs {
                clear: true,
                value: Vec::new(),
            },
        )
        .unwrap();
        assert!(!ctx.state_dir.join("launcher-suffix.txt").exists());
    }

    #[test]
    fn logs_reads_recent_local_logs_without_following() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let log_dir = log_dir(&ctx);
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(log_dir.join("client.log"), "plain\nkeyboard event\n").unwrap();
        logs(
            &ctx,
            LogsArgs {
                follow: false,
                mode: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn debug_bundle_serializes_context_even_when_guest_is_missing() {
        let dir = tempdir_for_test();
        let mut ctx = ctx(&dir);
        ctx.guest = None;
        ctx.guest_dir = None;
        debug_bundle(&ctx).unwrap();
        let text = fs::read_to_string(ctx.state_dir.join("debug-bundle.txt")).unwrap();
        assert!(text.contains("guest=<unset>"));
        assert!(text.contains("guest_dir=<unset>"));
    }
}

const APP_LIBRARY_PY: &str = r#"
import base64
import configparser
import json
import locale
import os
from pathlib import Path
import shlex
import shutil

locale_name = (locale.getlocale()[0] or os.environ.get("LANG") or "").split(".")[0]
locale_short = locale_name.split("_")[0] if locale_name else ""
app_dirs = [
    Path.home() / ".local/share/applications",
    Path("/usr/local/share/applications"),
    Path("/usr/share/applications"),
    Path.home() / ".local/share/flatpak/exports/share/applications",
    Path("/var/lib/flatpak/exports/share/applications"),
]
field_codes = {"%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v", "%m"}

def clean(value):
    return " ".join(str(value).replace("\t", " ").split())

def localized(group, key):
    for candidate in (
        f"{key}[{locale_name}]" if locale_name else "",
        f"{key}[{locale_short}]" if locale_short else "",
        key,
    ):
        if candidate and candidate in group:
            return group.get(candidate)
    return ""

def truthy(value):
    return str(value).strip().lower() == "true"

def parse_exec(value):
    try:
        parts = shlex.split(value)
    except ValueError:
        return []
    out = []
    for part in parts:
        if part in field_codes or part.startswith("%") or part.startswith("@@"):
            continue
        out.append(part.replace("%%", "%"))
    if not out:
        return []
    command = out[0]
    if "/" in command:
        if not Path(command).exists():
            return []
    elif shutil.which(command) is None:
        return []
    return out

def normalize_argv(app_id, argv):
    if not argv:
        return argv
    command = Path(argv[0]).name
    if app_id == "org.gnome.Ptyxis" and command == "ptyxis":
        if not any(arg in ("-s", "--standalone") for arg in argv[1:]):
            return [argv[0], "--standalone", *argv[1:]]
    return argv

def load_desktop(path):
    parser = configparser.ConfigParser(interpolation=None, strict=False)
    try:
        parser.read(path, encoding="utf-8")
    except Exception:
        return None
    if not parser.has_section("Desktop Entry"):
        return None
    group = parser["Desktop Entry"]
    if group.get("Type", "Application") != "Application":
        return None
    if truthy(group.get("NoDisplay", "")) or truthy(group.get("Hidden", "")) or truthy(group.get("Terminal", "")):
        return None
    name = localized(group, "Name")
    exec_line = group.get("Exec", "")
    app_id = path.name[:-8] if path.name.endswith(".desktop") else path.stem
    argv = normalize_argv(app_id, parse_exec(exec_line))
    if not name or not argv:
        return None
    return {
        "id": clean(app_id),
        "name": clean(name),
        "exec": " ".join(shlex.quote(part) for part in argv),
        "icon": clean(group.get("Icon", "")),
        "desktop": clean(str(path)),
        "categories": clean(group.get("Categories", "")),
        "argv_b64": base64.b64encode(json.dumps(argv, ensure_ascii=False).encode()).decode(),
    }

seen = set()
apps = []
for directory in app_dirs:
    if not directory.is_dir():
        continue
    for path in sorted(directory.glob("*.desktop")):
        app = load_desktop(path)
        if not app or app["id"] in seen:
            continue
        seen.add(app["id"])
        apps.append(app)
apps.sort(key=lambda item: (item["name"].casefold(), item["id"].casefold()))
for app in apps:
    print("\t".join(app[key] for key in ("id", "name", "exec", "icon", "desktop", "categories", "argv_b64")))
"#;

const APP_ICON_EXPORT_PY: &str = r#"
import base64
import configparser
import os
import re
from pathlib import Path

home = Path.home()
app_dirs = [
    home / ".local/share/applications",
    Path("/usr/local/share/applications"),
    Path("/usr/share/applications"),
    Path("/var/lib/flatpak/exports/share/applications"),
    home / ".local/share/flatpak/exports/share/applications",
]
icon_roots = [
    home / ".local/share/icons",
    home / ".icons",
    Path("/usr/local/share/icons"),
    Path("/usr/share/icons"),
    Path("/usr/local/share/pixmaps"),
    Path("/usr/share/pixmaps"),
]
exts = ["png", "svg", "xpm", "jpg", "jpeg"]

def truthy(value):
    return value.strip().lower() in ("1", "true", "yes")

def clean(value):
    return " ".join(value.replace("\t", " ").split())

def load_desktop(path):
    parser = configparser.ConfigParser(interpolation=None, strict=False)
    try:
        parser.read(path, encoding="utf-8")
    except Exception:
        return None
    if not parser.has_section("Desktop Entry"):
        return None
    group = parser["Desktop Entry"]
    if group.get("Type", "Application") != "Application":
        return None
    if truthy(group.get("NoDisplay", "")) or truthy(group.get("Hidden", "")) or truthy(group.get("Terminal", "")):
        return None
    app_id = path.name[:-8] if path.name.endswith(".desktop") else path.stem
    icon = clean(group.get("Icon", ""))
    if not icon:
        return None
    return clean(app_id), icon

def icon_score(path):
    text = str(path)
    suffix = path.suffix.lower().lstrip(".")
    ext_score = {"png": 3000, "svg": 2000, "jpg": 1000, "jpeg": 1000, "xpm": 500}.get(suffix, 0)
    size_score = 0
    match = re.search(r"([0-9]{2,4})x([0-9]{2,4})", text)
    if match:
        size_score = max(int(match.group(1)), int(match.group(2)))
    elif "scalable" in path.parts:
        size_score = 384
    hicolor = 100 if "hicolor" in path.parts else 0
    return ext_score + size_score + hicolor

def resolve_icon(icon):
    raw = icon.strip()
    if not raw:
        return None
    direct = Path(os.path.expanduser(raw))
    if direct.is_absolute() and direct.is_file():
        return direct
    name = Path(raw).name
    suffix = Path(name).suffix.lower().lstrip(".")
    patterns = [name] if suffix in exts else [f"{name}.{ext}" for ext in exts]
    candidates = []
    for root in icon_roots:
        if not root.is_dir():
            continue
        for pattern in patterns:
            direct = root / pattern
            if direct.is_file():
                candidates.append(direct)
    for root in icon_roots:
        if not root.is_dir():
            continue
        for pattern in patterns:
            try:
                candidates.extend(path for path in root.rglob(pattern) if path.is_file())
            except Exception:
                pass
    if not candidates:
        return None
    candidates.sort(key=lambda path: (icon_score(path), -len(str(path))), reverse=True)
    return candidates[0]

seen = set()
for directory in app_dirs:
    if not directory.is_dir():
        continue
    for desktop in sorted(directory.glob("*.desktop")):
        loaded = load_desktop(desktop)
        if not loaded:
            continue
        app_id, icon = loaded
        if app_id in seen:
            continue
        path = resolve_icon(icon)
        if not path:
            continue
        try:
            data = path.read_bytes()
        except Exception:
            continue
        if not data or len(data) > 4 * 1024 * 1024:
            continue
        seen.add(app_id)
        ext = path.suffix.lower().lstrip(".") or "bin"
        print(f"{app_id}\t{ext}\t{base64.b64encode(data).decode()}")
"#;
