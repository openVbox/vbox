use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

mod brand;
mod context;
mod distro_icons;
mod keychain;
mod library_ui;
mod machines;
mod process;
mod remote;
mod runtime;
mod ssh;
mod symlink;

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

use context::{AppContext, GlobalOptions};

#[derive(Parser, Debug)]
#[command(name = "vbox", version, about = "Wayland-first vbox runner")]
struct Cli {
    #[arg(long, global = true, value_name = "USER@HOST")]
    guest: Option<String>,
    #[arg(long, global = true, value_name = "PATH")]
    guest_dir: Option<PathBuf>,
    #[arg(long, global = true, value_name = "NAME")]
    instance: Option<String>,
    #[arg(long, global = true, value_parser = clap::value_parser!(u16), value_name = "PORT")]
    port: Option<u16>,
    #[arg(long, global = true, value_name = "NAME")]
    socket: Option<String>,
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(1..), value_name = "PX")]
    width: Option<u32>,
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(1..), value_name = "PX")]
    height: Option<u32>,
    #[arg(long, global = true)]
    debug: bool,
    #[arg(long, global = true)]
    no_build: bool,
    #[command(subcommand)]
    command: Option<VboxCommand>,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
enum VboxCommand {
    InstallCli(InstallCliArgs),
    UninstallCli(UninstallCliArgs),
    Run(TrailingArgs),
    Debug(TrailingArgs),
    Memo(TrailingArgs),
    View,
    Launch(TrailingArgs),
    LaunchId {
        app_id: String,
    },
    PrepareApp(PrepareAppArgs),
    BenchDataPlane(SamplesArg),
    StabilityTest(SecondsArg),
    AppId {
        app_id: String,
    },
    ControldInstall(ControldInstallArgs),
    ControldUninstall,
    #[command(alias = "controld-status")]
    Status,
    ControldFailure,
    #[command(alias = "audio-bootstrap")]
    Bootstrap,
    #[command(alias = "audio-doctor")]
    Doctor,
    TlsBootstrap(TlsBootstrapArgs),
    App(RequiredTrailingArgs),
    Library(LibraryArgs),
    LibraryUi,
    LibraryPicker,
    CacheIcons(CacheIconsArgs),
    InstallApps(InstallAppsArgs),
    InstallLauncher {
        app_id: String,
    },
    RemoveApps(FilterArgs),
    RemoveLauncher {
        app_id: String,
    },
    LaunchpadCheck(FilterArgs),
    Suffix(SuffixArgs),
    Click {
        x: i32,
        y: i32,
    },
    Drag {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
    },
    Type(TextArgs),
    Preedit(TextArgs),
    Key {
        keycode: u16,
    },
    Volume(VolumeArgs),
    #[command(alias = "ps")]
    Processes,
    KillPid {
        #[arg(required = true)]
        pids: Vec<u32>,
    },
    Windows,
    Logs(LogsArgs),
    Stop,
    Sync,
    Build,
    #[command(alias = "debug-bundle")]
    Bundle,
    #[command(alias = "guests")]
    Machines(MachinesArgs),
    DistroIcons(DistroIconsArgs),
    Remote(RemoteArgs),
    #[command(name = "_askpass", hide = true)]
    Askpass,
}

#[derive(Args, Debug)]
struct InstallCliArgs {
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[derive(Args, Debug)]
struct UninstallCliArgs {
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct TrailingArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
}

#[derive(Args, Debug)]
struct RequiredTrailingArgs {
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
}

#[derive(Args, Debug)]
struct PrepareAppArgs {
    #[arg(long)]
    bundle: bool,
    app_id: String,
}

#[derive(Args, Debug)]
struct SamplesArg {
    #[arg(value_parser = clap::value_parser!(u32).range(1..), value_name = "N")]
    samples: Option<u32>,
}

#[derive(Args, Debug)]
struct SecondsArg {
    #[arg(value_parser = clap::value_parser!(u64).range(1..), value_name = "SECONDS")]
    seconds: Option<u64>,
}

#[derive(Args, Debug)]
struct ControldInstallArgs {
    #[arg(long)]
    with_tls: bool,
    /// Path to vbox-controld on the guest. Defaults to <guest_dir>/target/release/vbox-controld.
    #[arg(long, value_name = "PATH")]
    controld_bin: Option<PathBuf>,
    /// Path to vbox-server on the guest. Defaults to <guest_dir>/target/release/vbox-server.
    #[arg(long, value_name = "PATH")]
    server_bin: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct TlsBootstrapArgs {
    #[arg(long = "san", value_name = "HOST")]
    sans: Vec<String>,
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct LibraryArgs {
    #[arg(long)]
    refresh: bool,
    #[arg(value_name = "FILTER")]
    filters: Vec<String>,
}

#[derive(Args, Debug)]
struct CacheIconsArgs {
    #[arg(long)]
    refresh: bool,
}

#[derive(Args, Debug)]
struct InstallAppsArgs {
    #[arg(long)]
    refresh: bool,
    #[arg(value_name = "FILTER")]
    filters: Vec<String>,
}

#[derive(Args, Debug)]
struct FilterArgs {
    #[arg(value_name = "FILTER")]
    filters: Vec<String>,
}

#[derive(Args, Debug)]
struct SuffixArgs {
    #[arg(long, conflicts_with = "value")]
    clear: bool,
    #[arg(value_name = "VALUE", allow_hyphen_values = true)]
    value: Vec<OsString>,
}

#[derive(Args, Debug)]
struct TextArgs {
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    text: Vec<OsString>,
}

#[derive(Args, Debug)]
struct VolumeArgs {
    #[arg(value_parser = clap::value_parser!(u8).range(0..=100), value_name = "LEVEL")]
    level: u8,
    #[arg(long, conflicts_with = "unmute")]
    mute: bool,
    #[arg(long, conflicts_with = "mute")]
    unmute: bool,
}

#[derive(Args, Debug)]
struct LogsArgs {
    #[arg(short = 'f', long = "follow", conflicts_with = "mode")]
    follow: bool,
    #[arg(value_enum, value_name = "MODE")]
    mode: Option<LogsMode>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogsMode {
    Input,
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
struct MachinesArgs {
    #[command(subcommand)]
    command: Option<MachinesCommand>,
    #[command(flatten)]
    list: MachineListArgs,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
enum MachinesCommand {
    List(MachineListArgs),
    Info {
        target: String,
    },
    Start {
        target: String,
    },
    Stop {
        target: String,
    },
    Delete(MachineDeleteArgs),
    Config(MachineConfigArgs),
    Set {
        target: String,
        key: String,
        value: String,
    },
    Unset {
        target: String,
        key: String,
    },
}

#[derive(Args, Debug, Default)]
struct MachineListArgs {
    #[arg(long, conflicts_with_all = ["tsv", "table"])]
    json: bool,
    #[arg(long, conflicts_with_all = ["json", "table"])]
    tsv: bool,
    #[arg(long, conflicts_with_all = ["json", "tsv"])]
    table: bool,
    #[arg(long, conflicts_with_all = ["status", "stopped", "invalid", "all"])]
    running: bool,
    #[arg(long, conflicts_with_all = ["status", "running", "invalid", "all"])]
    stopped: bool,
    #[arg(long, conflicts_with_all = ["status", "running", "stopped", "all"])]
    invalid: bool,
    #[arg(long, conflicts_with_all = ["status", "running", "stopped", "invalid"])]
    all: bool,
    #[arg(long, value_enum)]
    status: Option<MachineStatus>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum MachineStatus {
    Running,
    Stopped,
    Suspended,
    Paused,
    Invalid,
    Remote,
    All,
}

#[derive(Args, Debug)]
struct MachineDeleteArgs {
    target: String,
    #[arg(long, short = 'y', required = true)]
    yes: bool,
}

#[derive(Args, Debug)]
struct MachineConfigArgs {
    target: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
struct DistroIconsArgs {
    #[command(subcommand)]
    command: Option<DistroIconsCommand>,
    #[arg(long)]
    refresh: bool,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
enum DistroIconsCommand {
    Fetch(CacheIconsArgs),
    Refresh,
    List,
    Dir,
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
struct RemoteArgs {
    #[command(subcommand)]
    command: Option<RemoteCommand>,
    #[command(flatten)]
    list: RemoteListArgs,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "kebab-case")]
enum RemoteCommand {
    List(RemoteListArgs),
    Add(RemoteAddArgs),
    #[command(alias = "rm")]
    Remove {
        name: String,
    },
    File,
}

#[derive(Args, Debug, Default)]
struct RemoteListArgs {
    #[arg(long, conflicts_with = "json")]
    tsv: bool,
    #[arg(long, conflicts_with = "tsv")]
    json: bool,
}

#[derive(Args, Debug)]
struct RemoteAddArgs {
    #[arg(long)]
    name: String,
    #[arg(long, value_parser = parse_ssh_target)]
    ssh: String,
    #[arg(long = "dir", alias = "guest-dir", value_name = "PATH")]
    dir: Option<PathBuf>,
    #[arg(long, default_value = "linux")]
    os: String,
    #[arg(long)]
    os_raw: Option<String>,
    #[arg(long, value_name = "PATH")]
    identity_file: Option<PathBuf>,
    #[arg(long)]
    password_stdin: bool,
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    let Some(command) = cli.command.take() else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    let ctx = AppContext::from_globals(&GlobalOptions::from(&cli))?;
    dispatch(&ctx, command)
}

fn dispatch(ctx: &AppContext, command: VboxCommand) -> Result<()> {
    match command {
        VboxCommand::InstallCli(args) => symlink::install(ctx, args.dir, args.force),
        VboxCommand::UninstallCli(args) => symlink::uninstall(ctx, args.dir),
        VboxCommand::Run(args) => runtime::run_app(ctx, args, false),
        VboxCommand::Debug(args) => runtime::run_app(ctx, args, true),
        VboxCommand::Memo(args) => runtime::memo(ctx, args),
        VboxCommand::View => runtime::view(ctx),
        VboxCommand::Launch(args) => runtime::launch(ctx, args),
        VboxCommand::LaunchId { app_id } => runtime::launch_id(ctx, app_id),
        VboxCommand::PrepareApp(args) => runtime::prepare_app(ctx, args),
        VboxCommand::BenchDataPlane(args) => runtime::bench_data_plane(ctx, args),
        VboxCommand::StabilityTest(args) => runtime::stability_test(ctx, args),
        VboxCommand::AppId { app_id } => runtime::app_id(ctx, app_id),
        VboxCommand::ControldInstall(args) => runtime::controld_install(ctx, args),
        VboxCommand::ControldUninstall => runtime::controld_uninstall(ctx),
        VboxCommand::Status => runtime::status(ctx),
        VboxCommand::ControldFailure => runtime::controld_failure(ctx),
        VboxCommand::Bootstrap => runtime::bootstrap(ctx),
        VboxCommand::Doctor => runtime::doctor(ctx),
        VboxCommand::TlsBootstrap(args) => runtime::tls_bootstrap(ctx, args.sans, args.out_dir),
        VboxCommand::App(args) => runtime::app(ctx, args.args),
        VboxCommand::Library(args) => runtime::library(ctx, args),
        VboxCommand::LibraryUi => runtime::library_ui(ctx),
        VboxCommand::LibraryPicker => runtime::library_picker(ctx),
        VboxCommand::CacheIcons(args) => runtime::cache_icons(ctx, args.refresh),
        VboxCommand::InstallApps(args) => runtime::install_apps(ctx, args),
        VboxCommand::InstallLauncher { app_id } => runtime::install_launcher(ctx, app_id),
        VboxCommand::RemoveApps(args) => runtime::remove_apps(ctx, args),
        VboxCommand::RemoveLauncher { app_id } => runtime::remove_launcher(ctx, app_id),
        VboxCommand::LaunchpadCheck(args) => runtime::launchpad_check(ctx, args),
        VboxCommand::Suffix(args) => runtime::suffix(ctx, args),
        VboxCommand::Click { x, y } => runtime::input(
            ctx,
            "click",
            vec![x.to_string().into(), y.to_string().into()],
        ),
        VboxCommand::Drag { x1, y1, x2, y2 } => runtime::input(
            ctx,
            "drag",
            vec![
                x1.to_string().into(),
                y1.to_string().into(),
                x2.to_string().into(),
                y2.to_string().into(),
            ],
        ),
        VboxCommand::Type(args) => runtime::text_input(ctx, "text", args),
        VboxCommand::Preedit(args) => runtime::text_input(ctx, "preedit", args),
        VboxCommand::Key { keycode } => {
            runtime::input(ctx, "key", vec![keycode.to_string().into()])
        }
        VboxCommand::Volume(args) => runtime::volume(ctx, args),
        VboxCommand::Processes => runtime::processes(ctx),
        VboxCommand::KillPid { pids } => runtime::kill_pid(ctx, pids),
        VboxCommand::Windows => runtime::windows(ctx),
        VboxCommand::Logs(args) => runtime::logs(ctx, args),
        VboxCommand::Stop => runtime::stop(ctx),
        VboxCommand::Sync => runtime::sync(ctx),
        VboxCommand::Build => runtime::build(ctx),
        VboxCommand::Bundle => runtime::debug_bundle(ctx),
        VboxCommand::Machines(args) => machines::run(ctx, args),
        VboxCommand::DistroIcons(args) => distro_icons::run(ctx, args),
        VboxCommand::Remote(args) => remote::run(ctx, args),
        VboxCommand::Askpass => ssh::run_askpass(),
    }
}

impl From<&Cli> for GlobalOptions {
    fn from(cli: &Cli) -> Self {
        Self {
            guest: cli.guest.clone(),
            guest_dir: cli.guest_dir.clone(),
            instance: cli.instance.clone(),
            port: cli.port,
            socket: cli.socket.clone(),
            width: cli.width,
            height: cli.height,
            debug: cli.debug,
            no_build: cli.no_build,
        }
    }
}

fn parse_ssh_target(value: &str) -> std::result::Result<String, String> {
    if value.contains('@') {
        Ok(value.to_owned())
    } else {
        Err("expected USER@HOST".to_owned())
    }
}
