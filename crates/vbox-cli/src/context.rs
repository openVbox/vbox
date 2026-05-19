use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::brand;

#[derive(Debug, Clone)]
pub(crate) struct GlobalOptions {
    pub(crate) guest: Option<String>,
    pub(crate) guest_dir: Option<PathBuf>,
    pub(crate) instance: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) socket: Option<String>,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) debug: bool,
    pub(crate) no_build: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct AppContext {
    pub(crate) root: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) cli_path: PathBuf,
    pub(crate) guest: Option<String>,
    pub(crate) guest_dir: Option<PathBuf>,
    pub(crate) instance: String,
    pub(crate) port: u16,
    pub(crate) socket: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) debug: bool,
    pub(crate) build: bool,
}

impl AppContext {
    pub(crate) fn from_globals(opts: &GlobalOptions) -> Result<Self> {
        let root = repo_root()?;
        let state_dir = brand::state_dir(&root);
        let cli_path = env::current_exe().unwrap_or_else(|_| root.join("target/release/vbox"));
        let guest = opts.guest.clone().or_else(|| brand::env_var("VBOX_GUEST"));
        let guest_dir = opts
            .guest_dir
            .clone()
            .or_else(|| brand::env_os("VBOX_GUEST_DIR").map(PathBuf::from));
        let instance = opts
            .instance
            .clone()
            .or_else(|| brand::env_var("VBOX_INSTANCE"))
            .unwrap_or_else(|| "default".to_string());
        let port = opts
            .port
            .or_else(|| brand::env_var("VBOX_PORT").and_then(|v| v.parse().ok()))
            .unwrap_or(5710);
        let socket = opts
            .socket
            .clone()
            .or_else(|| brand::env_var("VBOX_SOCKET"))
            .unwrap_or_else(|| {
                if instance == "default" {
                    "vbox-0".to_string()
                } else {
                    format!("vbox-{}", sanitize_socket_name(&instance))
                }
            });
        let width = opts
            .width
            .or_else(|| brand::env_var("VBOX_WIDTH").and_then(|v| v.parse().ok()))
            .unwrap_or(1024);
        let height = opts
            .height
            .or_else(|| brand::env_var("VBOX_HEIGHT").and_then(|v| v.parse().ok()))
            .unwrap_or(768);
        let debug = opts.debug || brand::env_var("VBOX_DEBUG").as_deref() == Some("1");
        let build = !opts.no_build && brand::env_var("VBOX_BUILD").as_deref() != Some("0");

        Ok(Self {
            root,
            state_dir,
            cli_path,
            guest,
            guest_dir,
            instance,
            port,
            socket,
            width,
            height,
            debug,
            build,
        })
    }

    pub(crate) fn client_bin(&self) -> PathBuf {
        brand::env_os("VBOX_CLIENT_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.join("target/release/vbox-client"))
    }

    pub(crate) fn control_addr(&self) -> String {
        let host = brand::env_var("VBOX_CONTROL_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        let port = brand::env_var("VBOX_CONTROL_PORT").unwrap_or_else(|| "5711".to_string());
        format!("{host}:{port}")
    }

    pub(crate) fn local_addr(&self) -> String {
        let host = brand::env_var("VBOX_LOCAL_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        format!("{host}:{}", self.port)
    }

    pub(crate) fn guest(&self) -> Result<&str> {
        self.guest
            .as_deref()
            .filter(|guest| !guest.is_empty())
            .context("guest is not configured; set --guest USER@HOST or VBOX_GUEST")
    }

    pub(crate) fn guest_dir(&self) -> Result<&Path> {
        self.guest_dir
            .as_deref()
            .context("guest dir is not configured; set --guest-dir PATH or VBOX_GUEST_DIR")
    }
}

pub(crate) fn repo_root() -> Result<PathBuf> {
    if let Some(root) = brand::env_os("VBOX_ROOT") {
        return Ok(PathBuf::from(root));
    }
    if let Some(root) = repo_root_from_current_exe() {
        return Ok(root);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|crates| crates.parent())
        .map(PathBuf::from)
        .context("could not resolve repository root")
}

fn repo_root_from_current_exe() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let profile = exe.parent()?;
    let target = profile.parent()?;
    if target.file_name()? != "target" {
        return None;
    }
    target.parent().map(PathBuf::from)
}

fn sanitize_socket_name(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            ch
        } else {
            '-'
        };
        if mapped == '-' {
            if !last_dash && !out.is_empty() {
                out.push(mapped);
            }
            last_dash = true;
        } else {
            out.push(mapped);
            last_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env;
    use std::fs;

    struct TempDir {
        path: PathBuf,
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn tempdir_for_test() -> TempDir {
        let path = env::temp_dir().join(format!(
            "vbox-context-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn clear_vbox_env() {
        for key in [
            "VBOX_ROOT",
            "VBOX_STATE_DIR",
            "VBOX_GUEST",
            "VBOX_GUEST_DIR",
            "VBOX_INSTANCE",
            "VBOX_PORT",
            "VBOX_SOCKET",
            "VBOX_WIDTH",
            "VBOX_HEIGHT",
            "VBOX_DEBUG",
            "VBOX_BUILD",
            "VBOX_CLIENT_BIN",
            "VBOX_CONTROL_HOST",
            "VBOX_CONTROL_PORT",
            "VBOX_LOCAL_HOST",
        ] {
            test_env::remove_var(key);
        }
    }

    fn opts() -> GlobalOptions {
        GlobalOptions {
            guest: Some("alice@example.test".to_string()),
            guest_dir: Some(PathBuf::from("/guest/workdir")),
            instance: None,
            port: None,
            socket: None,
            width: None,
            height: None,
            debug: false,
            no_build: false,
        }
    }

    #[test]
    fn sanitize_socket_name_collapses_and_trims_unsafe_runs() {
        assert_eq!(sanitize_socket_name("dev box!!west"), "dev-box-west");
        assert_eq!(sanitize_socket_name("--"), "default");
        assert_eq!(sanitize_socket_name("name_1-ok"), "name_1-ok");
    }

    #[test]
    fn from_globals_uses_explicit_values_before_environment() {
        let _guard = test_env::lock();
        clear_vbox_env();
        let root = tempdir_for_test();
        test_env::set_var("VBOX_ROOT", &root.path);
        test_env::set_var("VBOX_GUEST", "env@example.test");
        test_env::set_var("VBOX_GUEST_DIR", "/env/vbox");
        test_env::set_var("VBOX_INSTANCE", "env-instance");
        test_env::set_var("VBOX_PORT", "6000");
        test_env::set_var("VBOX_SOCKET", "env-socket");
        test_env::set_var("VBOX_WIDTH", "900");
        test_env::set_var("VBOX_HEIGHT", "700");
        test_env::set_var("VBOX_DEBUG", "1");
        test_env::set_var("VBOX_BUILD", "0");

        let mut explicit = opts();
        explicit.instance = Some("dev box".to_string());
        explicit.port = Some(7001);
        explicit.socket = Some("explicit-socket".to_string());
        explicit.width = Some(1440);
        explicit.height = Some(900);
        explicit.debug = false;
        explicit.no_build = false;

        let ctx = AppContext::from_globals(&explicit).unwrap();
        assert_eq!(ctx.root, root.path);
        assert_eq!(ctx.guest.as_deref(), Some("alice@example.test"));
        assert_eq!(ctx.guest_dir, Some(PathBuf::from("/guest/workdir")));
        assert_eq!(ctx.instance, "dev box");
        assert_eq!(ctx.port, 7001);
        assert_eq!(ctx.socket, "explicit-socket");
        assert_eq!(ctx.width, 1440);
        assert_eq!(ctx.height, 900);
        assert!(ctx.debug);
        assert!(!ctx.build);
        clear_vbox_env();
    }

    #[test]
    fn from_globals_derives_socket_for_named_instance() {
        let _guard = test_env::lock();
        clear_vbox_env();
        let root = tempdir_for_test();
        test_env::set_var("VBOX_ROOT", &root.path);
        let mut explicit = opts();
        explicit.instance = Some("dev box!!west".to_string());
        explicit.socket = None;
        let ctx = AppContext::from_globals(&explicit).unwrap();
        assert_eq!(ctx.socket, "vbox-dev-box-west");
        clear_vbox_env();
    }

    #[test]
    fn guest_accessors_report_missing_configuration() {
        let _guard = test_env::lock();
        clear_vbox_env();
        let root = tempdir_for_test();
        test_env::set_var("VBOX_ROOT", &root.path);
        let mut explicit = opts();
        explicit.guest = None;
        explicit.guest_dir = None;
        let ctx = AppContext::from_globals(&explicit).unwrap();
        assert!(ctx.guest().is_err());
        assert!(ctx.guest_dir().is_err());
        clear_vbox_env();
    }

    #[test]
    fn default_addresses_use_expected_loopback_ports() {
        let _guard = test_env::lock();
        clear_vbox_env();
        let root = tempdir_for_test();
        test_env::set_var("VBOX_ROOT", &root.path);
        let ctx = AppContext::from_globals(&opts()).unwrap();
        assert_eq!(ctx.control_addr(), "127.0.0.1:5711");
        assert_eq!(ctx.local_addr(), "127.0.0.1:5710");
        assert_eq!(
            ctx.client_bin(),
            root.path.join("target/release/vbox-client")
        );
        clear_vbox_env();
    }
}
