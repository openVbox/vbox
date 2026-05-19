//! Per-guest ephemeral D-Bus session setup.
//!
//! Each instance gets a private `dbus-daemon` so GNOME apps see the
//! environment variables (`DBUS_SESSION_BUS_ADDRESS`, etc.) they expect on
//! login. The bus's `<servicedir>` is restricted to a single per-instance
//! stub directory — host services are excluded so dbus-daemon won't burn
//! 120 s auto-activating things like `org.freedesktop.secrets` that don't
//! exist on the ephemeral bus.
//!
//! Two artifacts are written into the instance work dir:
//! 1. [`write_dbus_session_conf`] writes the bus's `session.conf`.
//! 2. [`write_dbus_stub_assets`] writes a Python name-owner for
//!    `org.gnome.Mutter.ServiceChannel` plus the .service file that
//!    auto-activates it. Without that owner, Nautilus's startup probe
//!    aborts with "Failed to initialize display server connection".
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Write the session.conf used by the ephemeral D-Bus bus per-guest. The
/// servicedir list points only at our stub directory — host services are
/// excluded so dbus-daemon won't burn 120 s auto-activating things like
/// org.freedesktop.secrets that don't exist on the ephemeral bus.
pub(crate) fn write_dbus_session_conf(work_dir: &Path) -> Result<()> {
    let path = work_dir.join("dbus-session.conf");
    let services_dir = work_dir.join("dbus-services");
    let body = format!(
        r#"<!DOCTYPE busconfig PUBLIC
 "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <servicedir>{services_dir}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="false"/>
    <allow eavesdrop="false"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        services_dir = services_dir.display()
    );
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))
}

/// Stub D-Bus name owners that GNOME apps demand on an otherwise empty bus.
/// `org.gnome.Mutter.ServiceChannel` is the one Nautilus probes on startup —
/// our stub claims the name and idles so the proxy-presence check passes.
pub(crate) fn write_dbus_stub_assets(work_dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let services_dir = work_dir.join("dbus-services");
    fs::create_dir_all(&services_dir)
        .with_context(|| format!("create {}", services_dir.display()))?;

    // Stub Python script — name-only owner. dbus-daemon spawns one per
    // session bus on demand and it exits when the bus shuts down.
    let stub_script = work_dir.join("dbus-stub-mutter-service-channel.py");
    let stub_body = r#"#!/usr/bin/env python3
"""Own org.gnome.Mutter.ServiceChannel on a vbox ephemeral session bus.

Nautilus uses dbus_proxy_new_*() with G_DBUS_PROXY_FLAGS_DO_NOT_AUTO_START to
ask Mutter for a display-server connection; if the name has no owner, the
proxy check fails and Nautilus aborts startup with "Failed to initialize
display server connection". We don't actually expose Mutter's interface — we
just claim the name so the proxy presence check succeeds. Nautilus's
subsequent method call returns NoReply / UnknownMethod, which it tolerates.
"""
import sys

try:
    import dbus
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib
except ImportError as exc:
    sys.stderr.write(f"vbox dbus stub: missing python3-dbus / gi: {exc}\n")
    sys.exit(0)  # exit cleanly; lack of stub is non-fatal for non-Nautilus apps

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
result = bus.request_name("org.gnome.Mutter.ServiceChannel")
sys.stderr.write(f"vbox dbus stub: requested Mutter.ServiceChannel -> {result}\n")
GLib.MainLoop().run()
"#;
    fs::write(&stub_script, stub_body)
        .with_context(|| format!("write {}", stub_script.display()))?;
    fs::set_permissions(&stub_script, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod {}", stub_script.display()))?;

    // D-Bus service file that auto-activates the stub on first reference.
    let service_file = services_dir.join("org.gnome.Mutter.ServiceChannel.service");
    let service_body = format!(
        "[D-BUS Service]\nName=org.gnome.Mutter.ServiceChannel\nExec={script}\n",
        script = stub_script.display()
    );
    fs::write(&service_file, service_body)
        .with_context(|| format!("write {}", service_file.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    // Both helpers run during DaemonState::new(), so the user-visible flow is
    // "first daemon start" → "work_dir contains the dbus config and stub
    // assets ready for `dbus-run-session --config-file`". The tests check the
    // *contract* — file presence, paths embedded inside, exec bits — rather
    // than the exact wording of the XML, so a future cosmetic change to the
    // template (newlines, indentation) doesn't break them.

    #[test]
    fn session_conf_lists_the_per_instance_servicedir() {
        // dbus-run-session can auto-activate any host service if servicedir
        // isn't pinned — that's the 120s startup tax we want to avoid. The
        // generated config must hand dbus-daemon exactly one servicedir,
        // pointing at our work_dir/dbus-services tree.
        let dir = tempdir_for_test();

        write_dbus_session_conf(&dir.path).unwrap();

        let body = fs::read_to_string(dir.path.join("dbus-session.conf")).unwrap();
        let expected_servicedir = dir.path.join("dbus-services");
        assert!(
            body.contains(&format!(
                "<servicedir>{}</servicedir>",
                expected_servicedir.display()
            )),
            "config must pin servicedir to {}; full body was: {body}",
            expected_servicedir.display()
        );
        assert!(body.contains("<type>session</type>"));
    }

    #[test]
    fn session_conf_can_be_rewritten_idempotently() {
        // DaemonState::new runs both write helpers on every start; restarting
        // controld with the same work_dir must not error or corrupt the
        // existing config.
        let dir = tempdir_for_test();
        write_dbus_session_conf(&dir.path).unwrap();
        let first = fs::read_to_string(dir.path.join("dbus-session.conf")).unwrap();

        write_dbus_session_conf(&dir.path).unwrap();
        let second = fs::read_to_string(dir.path.join("dbus-session.conf")).unwrap();

        assert_eq!(first, second, "second write must produce the same bytes");
    }

    #[test]
    fn stub_assets_write_executable_python_and_service_file() {
        // Three artifacts the daemon depends on at app-spawn time:
        //   1. dbus-services/  (the dir we point session.conf at)
        //   2. dbus-stub-mutter-service-channel.py  (must be executable so
        //      dbus-daemon can exec it)
        //   3. dbus-services/org.gnome.Mutter.ServiceChannel.service  (the
        //      .service file pointing at the script)
        let dir = tempdir_for_test();

        write_dbus_stub_assets(&dir.path).unwrap();

        let services_dir = dir.path.join("dbus-services");
        assert!(services_dir.is_dir(), "services dir must be created");

        let script = dir.path.join("dbus-stub-mutter-service-channel.py");
        let perms = fs::metadata(&script).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o755,
            "stub script must be chmod 0755 so dbus-daemon can exec it"
        );
        let script_body = fs::read_to_string(&script).unwrap();
        assert!(script_body.contains("org.gnome.Mutter.ServiceChannel"));
        assert!(script_body.starts_with("#!/usr/bin/env python3"));

        let service_file = services_dir.join("org.gnome.Mutter.ServiceChannel.service");
        let svc = fs::read_to_string(&service_file).unwrap();
        assert!(svc.contains("[D-BUS Service]"));
        assert!(svc.contains("Name=org.gnome.Mutter.ServiceChannel"));
        // The Exec= line must reference the script we just wrote — absolute
        // path inside the same work_dir, otherwise dbus-daemon can't find
        // it on activation.
        assert!(
            svc.contains(&format!("Exec={}", script.display())),
            "service file must point at the absolute path of the stub script, got: {svc}"
        );
    }

    #[test]
    fn stub_assets_can_be_rewritten_idempotently() {
        // Same idempotence story as the session.conf helper. We rerun and
        // expect both the script body and the .service file to be byte-for-
        // byte identical.
        let dir = tempdir_for_test();
        write_dbus_stub_assets(&dir.path).unwrap();
        let script_a = fs::read(dir.path.join("dbus-stub-mutter-service-channel.py")).unwrap();
        let svc_a = fs::read(
            dir.path
                .join("dbus-services/org.gnome.Mutter.ServiceChannel.service"),
        )
        .unwrap();

        write_dbus_stub_assets(&dir.path).unwrap();
        let script_b = fs::read(dir.path.join("dbus-stub-mutter-service-channel.py")).unwrap();
        let svc_b = fs::read(
            dir.path
                .join("dbus-services/org.gnome.Mutter.ServiceChannel.service"),
        )
        .unwrap();

        assert_eq!(script_a, script_b);
        assert_eq!(svc_a, svc_b);
    }

    struct TempDir {
        path: PathBuf,
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
    fn tempdir_for_test() -> TempDir {
        let base = std::env::temp_dir();
        let suffix = format!(
            "vbox-controld-dbus-{}-{}",
            std::process::id(),
            unique_counter()
        );
        let path = base.join(suffix);
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    fn unique_counter() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }
}
