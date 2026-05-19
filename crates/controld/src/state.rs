//! Daemon-owned instance state — the single source of truth for what
//! vbox-server processes are running, on what ports, with which guest
//! apps attached. Replaces the pid_file scheme that lost track of state when
//! the host script crashed mid-cleanup.
//!
//! Persistence: every state-changing operation flushes a snapshot to
//! `work_dir/state.bin` (postcard-encoded). On startup the daemon loads that
//! snapshot and reconciles it against the live process table — instances
//! whose server PID is gone are dropped; instances whose server is still
//! alive are re-attached (no Child handle, but we can still kill by pid).
//! Without this, a systemd-managed daemon restart would orphan every
//! vbox-server it spawned previously.

use crate::dbus_session::{write_dbus_session_conf, write_dbus_stub_assets};
use crate::snapshot::{InstanceSnapshot, Snapshot, load_snapshot};
#[cfg(target_os = "linux")]
use crate::socket_wait::wait_socket_inotify;
use crate::socket_wait::wait_socket_poll;
use crate::utils::{kill_pid, pid_alive, which_in_path, xdg_runtime_dir};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vbox_proto::InstanceSummary;

pub struct QuicInstanceConfig {
    pub bind: std::net::IpAddr,
    pub port: Option<u16>,
    pub token: String,
}

pub struct DaemonState {
    server_bin: PathBuf,
    work_dir: PathBuf,
    state_path: PathBuf,
    instances: Mutex<HashMap<String, Instance>>,
}

struct Instance {
    port: u16,
    server_pid: u32,
    /// Some when we spawned the server in this daemon process (we own the
    /// Child handle and can wait on it). None for instances reconciled at
    /// startup — we only have the pid, so termination falls back to kill(2).
    server_child: Option<Child>,
    app_pids: Vec<u32>,
}

impl DaemonState {
    pub fn new(server_bin: PathBuf, work_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&work_dir).with_context(|| format!("create {}", work_dir.display()))?;
        let state_path = work_dir.join("state.bin");
        write_dbus_stub_assets(&work_dir)?;
        write_dbus_session_conf(&work_dir)?;
        let state = Self {
            server_bin,
            work_dir,
            state_path,
            instances: Mutex::new(HashMap::new()),
        };
        state.reconcile_on_startup()?;
        Ok(state)
    }

    fn dbus_session_conf(&self) -> PathBuf {
        self.work_dir.join("dbus-session.conf")
    }

    /// Read the on-disk snapshot and recover only the instances whose server
    /// PID is still alive. Dead instances are dropped silently (their disk
    /// log files remain for forensics). Called once from `new()`.
    fn reconcile_on_startup(&self) -> Result<()> {
        let Some(snap) = load_snapshot(&self.state_path)? else {
            eprintln!("reconcile: no state.bin yet");
            return Ok(());
        };
        eprintln!(
            "reconcile: loaded {} instance(s) from {}",
            snap.instances.len(),
            self.state_path.display()
        );
        let mut map = self.instances.lock().expect("instances mutex");
        let mut recovered = 0;
        for inst in snap.instances {
            let alive = pid_alive(inst.server_pid);
            eprintln!(
                "reconcile:   {} server_pid={} alive={}",
                inst.instance, inst.server_pid, alive
            );
            let outcome = decide_reconcile(&inst, |pid| pid_alive(pid));
            match outcome {
                ReconcileOutcome::Drop => continue,
                ReconcileOutcome::Recover { app_pids } => {
                    map.insert(
                        inst.instance,
                        Instance {
                            port: inst.port,
                            server_pid: inst.server_pid,
                            server_child: None,
                            app_pids,
                        },
                    );
                    recovered += 1;
                }
            }
        }
        drop(map);
        eprintln!(
            "reconcile: recovered {recovered} live instance(s) from {}",
            self.state_path.display()
        );
        // Rewrite the snapshot so dead entries don't accumulate.
        let _ = self.flush();
        Ok(())
    }

    pub fn summaries(&self) -> Vec<InstanceSummary> {
        let mut map = self.instances.lock().expect("instances mutex");
        // Reap whatever exited under our nose. Use Child::try_wait when we
        // have the handle (only for instances we spawned); fall back to
        // kill -0 for reconciled instances.
        let mut need_flush = reap_dead_instances(&mut map);
        for inst in map.values_mut() {
            need_flush |= retain_live_app_pids(&mut inst.app_pids, pid_alive);
        }
        let out: Vec<InstanceSummary> = map
            .iter()
            .map(|(name, inst)| InstanceSummary {
                instance: name.clone(),
                port: inst.port,
                server_pid: inst.server_pid,
                app_pids: inst.app_pids.clone(),
            })
            .collect();
        drop(map);
        if need_flush {
            let _ = self.flush();
        }
        out
    }

    pub fn start_instance(
        &self,
        instance: String,
        port: u16,
        debug: bool,
        quic: Option<QuicInstanceConfig>,
    ) -> Result<u32> {
        let mut map = self.instances.lock().expect("instances mutex");
        let reaped_dead = reap_dead_instances(&mut map);
        if let Some((owner, pid)) = port_conflict_owner(&map, &instance, port) {
            drop(map);
            if reaped_dead {
                let _ = self.flush();
            }
            bail!("port {port} already used by instance {owner} (server_pid={pid})");
        }

        if let Some(existing) = map.remove(&instance) {
            terminate(existing);
        }

        let log_dir = self.work_dir.join("logs").join(&instance);
        fs::create_dir_all(&log_dir).with_context(|| format!("create {}", log_dir.display()))?;
        let log_path = log_dir.join("server.log");
        let log = fs::File::create(&log_path)
            .with_context(|| format!("create {}", log_path.display()))?;
        let log_err = log.try_clone().context("clone log fd")?;

        // Spawn vbox-server. setsid() in pre_exec detaches it into a new
        // process group so systemd's KillMode=process (in the controld unit)
        // is enough to keep it alive when controld restarts: SIGTERM only
        // hits controld's main pid, not the whole pgroup, and the cgroup
        // membership alone doesn't propagate kills.
        let mut cmd = Command::new(&self.server_bin);
        cmd.arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        if let Some(quic) = quic {
            if quic.token.is_empty() {
                bail!("quic token must be non-empty when --quic-bind is set");
            }
            let cert = ensure_quic_cert_material(&self.work_dir, &instance)?;
            cmd.arg("--quic-bind")
                .arg(quic.bind.to_string())
                .arg("--quic-port")
                .arg(quic.port.unwrap_or(port).to_string())
                .arg("--quic-token")
                .arg(quic.token)
                .arg("--quic-cert")
                .arg(cert.cert_path)
                .arg("--quic-key")
                .arg(cert.key_path);
        }
        if debug {
            cmd.env("VBOX_DEBUG", "1");
        }
        cmd.env("RUST_BACKTRACE", "1");
        // SAFETY: pre_exec runs after fork and before exec, in a forked
        // child where async-signal-safety matters. setsid() is on POSIX's
        // async-signal-safe list. We don't allocate or touch any shared
        // state here.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", self.server_bin.display()))?;
        std::thread::sleep(Duration::from_millis(150));
        if let Ok(Some(status)) = child.try_wait() {
            bail!("vbox-server failed to start (status={status})");
        }
        let pid = child.id();
        map.insert(
            instance,
            Instance {
                port,
                server_pid: pid,
                server_child: Some(child),
                app_pids: vec![],
            },
        );
        drop(map);
        let _ = self.flush();
        Ok(pid)
    }

    pub fn stop_instance(&self, instance: &str) -> Result<()> {
        let inst = self
            .instances
            .lock()
            .expect("instances mutex")
            .remove(instance)
            .ok_or_else(|| anyhow!("no such instance: {instance}"))?;
        terminate(inst);
        let _ = self.flush();
        Ok(())
    }

    pub fn launch_app(
        &self,
        instance: &str,
        socket: &str,
        argv: &[String],
        wait_ready: Duration,
    ) -> Result<u32> {
        if argv.is_empty() {
            bail!("launch_app: empty argv");
        }
        let runtime = xdg_runtime_dir()?;
        let socket_path = runtime.join(socket);
        if !socket_path.exists() {
            bail!("Wayland socket not found: {}", socket_path.display());
        }

        let log_dir = self.work_dir.join("logs").join(instance);
        fs::create_dir_all(&log_dir).with_context(|| format!("create {}", log_dir.display()))?;
        let log_path = log_dir.join("app.log");
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("open {}", log_path.display()))?;
        let log_err = log.try_clone().context("clone log fd")?;

        // Wrap each guest spawn in a private D-Bus session bus so GNOME apps
        // can't single-instance-route the "new window" onto the guest's own
        // desktop. Falls back to direct exec when dbus-run-session is absent.
        let use_dbus_isolation = which_in_path("dbus-run-session").is_some();
        let mut cmd = if use_dbus_isolation {
            // --config-file points the ephemeral bus at our own (empty)
            // servicedir so it won't auto-activate host services like
            // xdg-desktop-portal and burn 120 s per missing service.
            let mut c = Command::new("dbus-run-session");
            let cfg = self.dbus_session_conf();
            c.arg("--config-file")
                .arg(&cfg)
                .arg("--")
                .arg(&argv[0])
                .args(&argv[1..]);
            c
        } else {
            eprintln!(
                "warn: dbus-run-session not found in PATH — GNOME single-instance \
                 routing may send this app to the guest's own desktop"
            );
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        apply_guest_env(&mut cmd, socket, use_dbus_isolation);
        // Detach into a fresh session so the guest app outlives this daemon
        // (mirrors the vbox-server spawn). Otherwise systemd's KillMode=process
        // only protects the server pid, not its sibling app pids.
        // SAFETY: setsid is async-signal-safe and we don't touch shared state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", argv.join(" ")))?;
        // Phase 1 (fast): catches "binary missing", "exec format error", and
        // most spawn-time failures within 120ms.
        std::thread::sleep(Duration::from_millis(120));
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "guest app exited immediately (status={status}); see {}",
                log_path.display()
            );
        }
        // Phase 2 (optional, caller-controlled): poll try_wait at 100ms steps
        // for `wait_ready`. Wayland handshake failures and dbus connect errors
        // typically kill the app within ~200-500ms; this window catches them
        // before the daemon hands a "successful" pid back to the host.
        if wait_ready > Duration::ZERO {
            let deadline = Instant::now() + wait_ready;
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(Some(status)) = child.try_wait() {
                    bail!(
                        "guest app exited during wait-ready (status={status}, after {}ms); see {}",
                        wait_ready.as_millis(),
                        log_path.display()
                    );
                }
            }
        }
        let pid = child.id();
        let mut map = self.instances.lock().expect("instances mutex");
        if let Some(inst) = map.get_mut(instance) {
            inst.app_pids.push(pid);
        }
        // We don't track the Child handle long-term — the gnome app outlives
        // its scope, and we control it later by pid. Forgetting the handle
        // also prevents Drop from killing it when this function returns.
        std::mem::forget(child);
        drop(map);
        let _ = self.flush();
        Ok(pid)
    }

    // Method on DaemonState for API consistency with start/stop/launch even
    // though it doesn't yet need shared state.
    #[allow(clippy::unused_self)]
    pub fn wait_socket(&self, socket: &str, timeout: Duration) -> Result<()> {
        let runtime = xdg_runtime_dir()?;
        let path = runtime.join(socket);
        if path.exists() {
            return Ok(());
        }
        // Linux inotify on the parent dir wakes us the instant the socket
        // appears — no 50ms polling tax, no jittery first-frame latency on
        // small apps that bind their Wayland socket within a few ms of spawn.
        // The poll-based fallback stays in place for non-Linux builds and the
        // rare case where inotify is unavailable (containers, fs limits).
        #[cfg(target_os = "linux")]
        {
            if let Some(res) = wait_socket_inotify(&runtime, socket, &path, timeout) {
                return res;
            }
        }
        wait_socket_poll(&path, timeout)
    }

    pub fn quic_cert_sha256(&self, instance: &str) -> Result<Option<String>> {
        let cert_path = quic_cert_dir(&self.work_dir, instance).join("server.pem");
        if !cert_path.exists() {
            return Ok(None);
        }
        Ok(Some(sha256_hex(&load_first_cert_der(&cert_path)?)))
    }

    fn flush(&self) -> Result<()> {
        let map = self.instances.lock().expect("instances mutex");
        let snap = Snapshot {
            instances: map
                .iter()
                .map(|(name, inst)| InstanceSnapshot {
                    instance: name.clone(),
                    port: inst.port,
                    server_pid: inst.server_pid,
                    app_pids: inst.app_pids.clone(),
                })
                .collect(),
        };
        drop(map);
        let bytes = postcard::to_allocvec(&snap).context("encode snapshot")?;
        let tmp = self.state_path.with_extension("bin.tmp");
        fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &self.state_path).with_context(|| {
            format!("rename {} -> {}", tmp.display(), self.state_path.display())
        })?;
        Ok(())
    }
}

fn terminate(mut inst: Instance) {
    // Uniform shutdown: SIGTERM every tracked pid, give them 1s to clean up,
    // SIGKILL anyone still around. Previous version sent SIGTERM only to apps
    // (so a misbehaving gnome app could linger as a zombie/stale) and relied
    // on Child::kill() (=SIGKILL) for the server (no graceful exit).
    let pids = pids_to_terminate(&inst);

    for &pid in &pids {
        let _ = kill_pid(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline && pids.iter().any(|&p| pid_alive(p)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    for &pid in &pids {
        if pid_alive(pid) {
            let _ = kill_pid(pid, libc::SIGKILL);
        }
    }

    // Reap the Child handle if we owned one (avoids a zombie until daemon
    // exit). Reconciled instances have no Child to reap.
    if let Some(mut child) = inst.server_child.take() {
        let _ = child.wait();
    }
    // app_pids were drained into `pids`; clear the original copy so the
    // Drop on the Instance struct sees a tidy state. (No-op once we move
    // to fully draining inside this fn.)
    inst.app_pids.clear();
}

/// Build the absolute path the daemon's `wait_socket` RPC checks for.
/// The contract: `<xdg_runtime>/<socket_name>`. Pulling it out lets a
/// test pin the join without touching the real $XDG_RUNTIME_DIR.
#[allow(dead_code)] // exercised by tests
pub(crate) fn wayland_socket_path(runtime: &std::path::Path, socket: &str) -> std::path::PathBuf {
    runtime.join(socket)
}

/// Plan the env vars `start_instance` adds to its vbox-server spawn.
/// Two key/value pairs: `VBOX_DEBUG=1` only when debug is on, and
/// `RUST_BACKTRACE=1` unconditionally so panics from the server bubble
/// up with a usable trace. Splitting from the Command setup lets tests
/// pin the exact env keys without spawning a child.
#[allow(dead_code)] // exercised by tests; production inlines cmd.env() calls
pub(crate) fn server_spawn_env(debug: bool) -> Vec<(&'static str, &'static str)> {
    let mut env = Vec::with_capacity(2);
    if debug {
        env.push(("VBOX_DEBUG", "1"));
    }
    env.push(("RUST_BACKTRACE", "1"));
    env
}

/// Encode a snapshot to postcard bytes the way `flush()` writes them.
/// Splitting the encode step out of the lock-and-write flow lets a test
/// pin the serialization (and thus the on-disk format) without owning a
/// DaemonState mutex.
#[allow(dead_code)] // exercised by tests; production goes through flush()
pub(crate) fn encode_snapshot(snap: &Snapshot) -> Result<Vec<u8>> {
    postcard::to_allocvec(snap).context("encode snapshot")
}

/// Build the temp-then-rename path pair the atomic snapshot write uses.
/// Operators care that an interrupted write doesn't leave a torn
/// `state.bin`; the rename-from-`.bin.tmp` trick is the guarantee.
#[allow(dead_code)] // exercised by tests; production inlines this
pub(crate) fn snapshot_tmp_path(state_path: &std::path::Path) -> std::path::PathBuf {
    state_path.with_extension("bin.tmp")
}

/// Order the pids `terminate()` should signal in shutdown order: every
/// guest app first, then the server. Splitting this out lets tests pin
/// the ordering without spawning processes — operators have asked for
/// "apps shut down before the vbox-server they live inside" so they get
/// a chance to flush state to disk before their compositor goes away.
fn pids_to_terminate(inst: &Instance) -> Vec<u32> {
    let mut pids: Vec<u32> = inst.app_pids.clone();
    pids.push(inst.server_pid);
    pids
}

struct QuicCertMaterial {
    cert_path: PathBuf,
    key_path: PathBuf,
}

fn ensure_quic_cert_material(work_dir: &Path, instance: &str) -> Result<QuicCertMaterial> {
    use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose};
    use std::os::unix::fs::PermissionsExt;

    let dir = quic_cert_dir(work_dir, instance);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let cert_path = dir.join("server.pem");
    let key_path = dir.join("server.key.pem");
    if cert_path.exists() && key_path.exists() {
        let _ = load_first_cert_der(&cert_path)?;
        return Ok(QuicCertMaterial {
            cert_path,
            key_path,
        });
    }

    let key = KeyPair::generate().context("generate QUIC certificate key")?;
    let mut params = CertificateParams::new(vec!["vbox-server".to_owned(), "localhost".to_owned()])
        .context("QUIC certificate params")?;
    params
        .distinguished_name
        .push(DnType::CommonName, "vbox-server");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    let cert = params
        .self_signed(&key)
        .context("self-sign QUIC certificate")?;
    fs::write(&cert_path, cert.pem()).with_context(|| format!("write {}", cert_path.display()))?;
    fs::write(&key_path, key.serialize_pem())
        .with_context(|| format!("write {}", key_path.display()))?;
    let mut perms = fs::metadata(&key_path)
        .with_context(|| format!("stat {}", key_path.display()))?
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&key_path, perms)
        .with_context(|| format!("chmod 0600 {}", key_path.display()))?;

    Ok(QuicCertMaterial {
        cert_path,
        key_path,
    })
}

fn quic_cert_dir(work_dir: &Path, instance: &str) -> PathBuf {
    work_dir
        .join("quic-certs")
        .join(safe_instance_component(instance))
}

fn safe_instance_component(instance: &str) -> String {
    instance
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn load_first_cert_der(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut rdr = BufReader::new(file);
    let cert = rustls_pemfile::certs(&mut rdr)
        .next()
        .transpose()
        .with_context(|| format!("parse certificate {}", path.display()))?
        .ok_or_else(|| anyhow!("no certificate in {}", path.display()))?;
    Ok(cert.as_ref().to_vec())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(out, "{byte:02x}").expect("writing to String never fails");
    }
    out
}

fn retain_live_app_pids(app_pids: &mut Vec<u32>, pid_alive: impl Fn(u32) -> bool) -> bool {
    let before = app_pids.len();
    app_pids.retain(|&pid| pid_alive(pid));
    app_pids.len() != before
}

fn reap_dead_instances(map: &mut HashMap<String, Instance>) -> bool {
    let dead: Vec<String> = map
        .iter_mut()
        .filter_map(|(name, inst)| {
            let alive = match inst.server_child.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => pid_alive(inst.server_pid),
            };
            if alive { None } else { Some(name.clone()) }
        })
        .collect();
    let changed = !dead.is_empty();
    for name in dead {
        map.remove(&name);
    }
    changed
}

fn port_conflict_owner(
    map: &HashMap<String, Instance>,
    instance: &str,
    port: u16,
) -> Option<(String, u32)> {
    map.iter()
        .find(|(name, inst)| name.as_str() != instance && inst.port == port)
        .map(|(name, inst)| (name.clone(), inst.server_pid))
}

/// What `reconcile_on_startup` should do with a single on-disk
/// snapshot entry. `Drop` means the server pid is gone and the
/// instance is dead; `Recover` means we re-attach (with only the
/// app pids that are still alive).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReconcileOutcome {
    Drop,
    Recover { app_pids: Vec<u32> },
}

/// Build the InstanceSummary vector the daemon hands to `Status` RPC.
/// Pure projection over the in-memory map; pulling it out of
/// `summaries()` lets us pin the field order and naming without locking
/// a mutex in tests. (`summaries()` itself still runs the reap pass and
/// flushes — those are I/O-bound.)
#[allow(dead_code)] // exercised by tests; production path remains inlined
pub(crate) fn instance_summaries_from_map(
    map: &HashMap<String, (u16, u32, Vec<u32>)>,
) -> Vec<InstanceSummary> {
    let mut out: Vec<InstanceSummary> = map
        .iter()
        .map(|(name, (port, server_pid, app_pids))| InstanceSummary {
            instance: name.clone(),
            port: *port,
            server_pid: *server_pid,
            app_pids: app_pids.clone(),
        })
        .collect();
    // Sort for deterministic order in the operator-visible output —
    // HashMap iteration order changes per build otherwise.
    out.sort_by(|a, b| a.instance.cmp(&b.instance));
    out
}

/// Pure decision for `reconcile_on_startup`. Takes a snapshot entry and
/// a "is this pid alive?" probe and returns the action to perform.
/// Splitting this out lets tests pin the recover/drop logic without
/// touching the kernel pid table.
pub(crate) fn decide_reconcile(
    snap: &InstanceSnapshot,
    pid_alive: impl Fn(u32) -> bool,
) -> ReconcileOutcome {
    if !pid_alive(snap.server_pid) {
        return ReconcileOutcome::Drop;
    }
    // Orphan-app cleanup: filter to apps still in the process table. If
    // the server is alive but every guest app died, we still recover —
    // a future LaunchApp can re-fill the list.
    let app_pids: Vec<u32> = snap
        .app_pids
        .iter()
        .copied()
        .filter(|&pid| pid_alive(pid))
        .collect();
    ReconcileOutcome::Recover { app_pids }
}

/// Apply GTK/Qt/GNOME env vars to a guest-app spawn. Each key compensates
/// for a specific guest-side default that breaks under vbox's nested
/// Wayland session: GDK/Qt platforms, software GL, locked GNOME identity,
/// GTK portal bypass, and (under dbus-run-session) a fresh per-app bus.
fn apply_guest_env(cmd: &mut Command, socket: &str, use_dbus_isolation: bool) {
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let plan = guest_env_plan(GuestEnvInputs {
        socket,
        use_dbus_isolation,
        xdg_runtime: xdg_runtime.as_deref(),
        home: home.as_deref(),
        check_path: |p| p.exists(),
    });
    for (k, v) in &plan.set {
        cmd.env(k, v);
    }
    for k in &plan.unset {
        cmd.env_remove(k);
    }
}

/// Inputs to [`guest_env_plan`]. Kept as a struct so the test wrappers can
/// pass a fixed `check_path` predicate without poking the real filesystem.
pub(crate) struct GuestEnvInputs<'a, F: Fn(&std::path::Path) -> bool> {
    pub socket: &'a str,
    pub use_dbus_isolation: bool,
    pub xdg_runtime: Option<&'a str>,
    pub home: Option<&'a std::path::Path>,
    pub check_path: F,
}

/// Result of guest-env planning: which env vars to set, which to clear.
/// `set` is ordered so callers iterating in insertion order get a
/// deterministic environment (useful for tests and log diffing).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct GuestEnvPlan {
    pub set: Vec<(String, String)>,
    pub unset: Vec<&'static str>,
}

impl GuestEnvPlan {
    #[cfg(test)]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.set
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Pure planner — decides which env vars belong on a guest-app spawn given
/// the socket name, whether we're under dbus-run-session, and the host's
/// XDG/HOME values. Splitting this out from [`apply_guest_env`] lets us
/// assert the resulting map without ever spawning a process.
pub(crate) fn guest_env_plan<F: Fn(&std::path::Path) -> bool>(
    inputs: GuestEnvInputs<'_, F>,
) -> GuestEnvPlan {
    let mut plan = GuestEnvPlan::default();
    let base: &[(&str, &str)] = &[
        ("WAYLAND_DISPLAY", inputs.socket),
        ("GDK_BACKEND", "wayland"),
        ("GSK_RENDERER", "cairo"),
        ("G_APPLICATION_NON_UNIQUE", "1"),
        ("LIBGL_ALWAYS_SOFTWARE", "1"),
        ("QT_QPA_PLATFORM", "wayland"),
        ("XDG_SESSION_TYPE", "wayland"),
        ("XDG_CURRENT_DESKTOP", "GNOME"),
        ("DESKTOP_SESSION", "gnome"),
        ("GNOME_DESKTOP_SESSION_ID", "vbox"),
        // Skip xdg-desktop-portal init: GTK apps would otherwise wait ~120s
        // per missing portal service on the ephemeral bus before rendering.
        ("GTK_USE_PORTAL", "0"),
    ];
    for (k, v) in base {
        plan.set.push(((*k).to_owned(), (*v).to_owned()));
    }

    // Audio routing — without explicit PIPEWIRE_REMOTE/PULSE_SERVER, the
    // dbus-run-session ephemeral bus hides the host's audio service from
    // the guest. Point at the absolute unix socket paths instead.
    if let Some(runtime) = inputs.xdg_runtime {
        let runtime_path = std::path::Path::new(runtime);
        let pipewire_sock = runtime_path.join("pipewire-0");
        if (inputs.check_path)(&pipewire_sock) {
            plan.set.push((
                "PIPEWIRE_REMOTE".to_owned(),
                pipewire_sock.display().to_string(),
            ));
        }
        let pulse_sock = runtime_path.join("pulse").join("native");
        if (inputs.check_path)(&pulse_sock) {
            plan.set.push((
                "PULSE_SERVER".to_owned(),
                format!("unix:{}", pulse_sock.display()),
            ));
        }
        // PulseAudio auth cookie — without it `pa_context_connect` fails
        // AUTH and the guest app goes silent on a cookie-enforcing server.
        if let Some(home) = inputs.home {
            let cookie = home.join(".config").join("pulse").join("cookie");
            if (inputs.check_path)(&cookie) {
                plan.set
                    .push(("PULSE_COOKIE".to_owned(), cookie.display().to_string()));
            }
        }
        // PipeWire client libs fall back to XDG_RUNTIME_DIR when neither
        // socket env var resolves — propagate it always.
        plan.set
            .push(("XDG_RUNTIME_DIR".to_owned(), runtime.to_owned()));
    }

    if inputs.use_dbus_isolation {
        // dbus-run-session reuses any inherited DBUS_SESSION_BUS_ADDRESS;
        // clear it so the ephemeral bus wins. Bypass mode keeps the
        // inherited value — that's the host session bus Nautilus needs.
        plan.unset.push("DBUS_SESSION_BUS_ADDRESS");
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    // The flow we're modelling: launch_app() calls apply_guest_env() right
    // before spawning a guest app under dbus-run-session. We can't easily
    // test the live spawn from cargo (it needs a real Wayland socket and a
    // child binary), but the *decision* about which env vars to set lives
    // in guest_env_plan(). That function is pure — given the inputs, we
    // know the entire output. These tests pin the contract so a future
    // refactor can't drop a critical env var without flipping red.

    fn deny_all_paths(_: &std::path::Path) -> bool {
        // Default predicate for tests: pretend no audio sockets exist so we
        // get the minimum plan. Individual tests override per-key.
        false
    }

    fn tempdir_for_test() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "vbox-controld-state-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn base_env_always_includes_wayland_and_gnome_identity() {
        // No XDG_RUNTIME_DIR, no HOME, no dbus isolation — bare minimum.
        // The base set must still carry the Wayland display + GNOME identity
        // hints, otherwise GTK apps would launch with the host's GDM
        // session leaking through.
        let plan = guest_env_plan(GuestEnvInputs {
            socket: "wayland-3",
            use_dbus_isolation: false,
            xdg_runtime: None,
            home: None,
            check_path: deny_all_paths,
        });

        assert_eq!(plan.get("WAYLAND_DISPLAY"), Some("wayland-3"));
        assert_eq!(plan.get("GDK_BACKEND"), Some("wayland"));
        assert_eq!(plan.get("QT_QPA_PLATFORM"), Some("wayland"));
        assert_eq!(plan.get("XDG_CURRENT_DESKTOP"), Some("GNOME"));
        assert_eq!(plan.get("GTK_USE_PORTAL"), Some("0"));
        // No XDG_RUNTIME_DIR known → no audio + no propagation.
        assert!(plan.get("PIPEWIRE_REMOTE").is_none());
        assert!(plan.get("PULSE_SERVER").is_none());
        assert!(plan.get("XDG_RUNTIME_DIR").is_none());
        // No dbus isolation requested → keep the inherited host bus.
        assert!(plan.unset.is_empty());
    }

    #[test]
    fn dbus_isolation_clears_inherited_bus_address() {
        // When vbox wraps the guest in `dbus-run-session`, the ephemeral bus
        // must win — inherited DBUS_SESSION_BUS_ADDRESS would take priority
        // and defeat the isolation, so we mark it for removal.
        let plan = guest_env_plan(GuestEnvInputs {
            socket: "wayland-0",
            use_dbus_isolation: true,
            xdg_runtime: None,
            home: None,
            check_path: deny_all_paths,
        });

        assert_eq!(plan.unset, vec!["DBUS_SESSION_BUS_ADDRESS"]);
    }

    #[test]
    fn audio_envs_appear_when_pipewire_and_pulse_sockets_exist() {
        // Operator's PipeWire is up: $XDG_RUNTIME_DIR/pipewire-0 and
        // $XDG_RUNTIME_DIR/pulse/native both exist. We expect the planner
        // to point the guest app at both unix sockets.
        let runtime = "/run/user/1000";
        let plan = guest_env_plan(GuestEnvInputs {
            socket: "wayland-0",
            use_dbus_isolation: true,
            xdg_runtime: Some(runtime),
            home: Some(std::path::Path::new("test-home")),
            check_path: |p| {
                let s = p.to_str().unwrap_or("");
                s == "/run/user/1000/pipewire-0" || s == "/run/user/1000/pulse/native"
            },
        });

        assert_eq!(
            plan.get("PIPEWIRE_REMOTE"),
            Some("/run/user/1000/pipewire-0")
        );
        assert_eq!(
            plan.get("PULSE_SERVER"),
            Some("unix:/run/user/1000/pulse/native")
        );
        // PULSE_COOKIE not set because the cookie file's path predicate
        // returns false for everything except the two sockets.
        assert!(plan.get("PULSE_COOKIE").is_none());
        assert_eq!(plan.get("XDG_RUNTIME_DIR"), Some(runtime));
    }

    #[test]
    fn pulse_cookie_added_when_home_dot_config_cookie_exists() {
        // The cookie is required when PulseAudio's server enforces auth
        // (GNOME default). Recreate the scenario where only the cookie
        // file matches — no sockets — and assert just the cookie env.
        let runtime = "/run/user/1000";
        let home = "test-home";
        let cookie = "test-home/.config/pulse/cookie";
        let plan = guest_env_plan(GuestEnvInputs {
            socket: "wayland-0",
            use_dbus_isolation: false,
            xdg_runtime: Some(runtime),
            home: Some(std::path::Path::new(home)),
            check_path: |p| p.to_str() == Some(cookie),
        });

        assert_eq!(plan.get("PULSE_COOKIE"), Some(cookie));
        // Sockets did NOT exist → no PipeWire/Pulse server env.
        assert!(plan.get("PIPEWIRE_REMOTE").is_none());
        assert!(plan.get("PULSE_SERVER").is_none());
    }

    // ---- pids_to_terminate -----------------------------------------------
    //
    // Story: when an instance shuts down, we SIGTERM every tracked pid.
    // The order matters: apps first, then the server. If we kill the
    // server first, the guest apps lose their compositor and may crash
    // (instead of saving state); doing apps first lets each gnome app
    // see the SIGTERM, flush, and exit before the compositor goes away.

    #[test]
    fn pids_to_terminate_lists_apps_first_then_server() {
        let inst = Instance {
            port: 5710,
            server_pid: 100,
            server_child: None,
            app_pids: vec![201, 202, 203],
        };
        assert_eq!(pids_to_terminate(&inst), vec![201, 202, 203, 100]);
    }

    #[test]
    fn pids_to_terminate_handles_instance_with_no_apps() {
        // A freshly started instance has no guest apps yet. The only
        // pid to signal is the server.
        let inst = Instance {
            port: 5710,
            server_pid: 100,
            server_child: None,
            app_pids: vec![],
        };
        assert_eq!(pids_to_terminate(&inst), vec![100]);
    }

    #[test]
    fn retain_live_app_pids_drops_dead_entries_and_reports_change() {
        let mut app_pids = vec![201, 202, 203];
        let changed = retain_live_app_pids(&mut app_pids, |pid| matches!(pid, 201 | 203));

        assert!(changed);
        assert_eq!(app_pids, vec![201, 203]);
    }

    #[test]
    fn retain_live_app_pids_keeps_stable_list_without_change() {
        let mut app_pids = vec![201, 202];
        let changed = retain_live_app_pids(&mut app_pids, |_| true);

        assert!(!changed);
        assert_eq!(app_pids, vec![201, 202]);
    }

    #[test]
    fn port_conflict_owner_reports_other_instance_on_same_port() {
        let mut map = HashMap::new();
        map.insert(
            "calendar".to_owned(),
            Instance {
                port: 5805,
                server_pid: 100,
                server_child: None,
                app_pids: vec![],
            },
        );
        map.insert(
            "settings".to_owned(),
            Instance {
                port: 5995,
                server_pid: 200,
                server_child: None,
                app_pids: vec![],
            },
        );

        assert_eq!(
            port_conflict_owner(&map, "colors", 5805),
            Some(("calendar".to_owned(), 100))
        );
    }

    #[test]
    fn port_conflict_owner_allows_restart_of_same_instance() {
        let mut map = HashMap::new();
        map.insert(
            "calendar".to_owned(),
            Instance {
                port: 5805,
                server_pid: 100,
                server_child: None,
                app_pids: vec![],
            },
        );

        assert_eq!(port_conflict_owner(&map, "calendar", 5805), None);
    }

    // ---- decide_reconcile -------------------------------------------------
    //
    // Story: on startup, the daemon reads state.bin and walks each entry.
    // The decision tree is: "is the server pid still alive?". If not,
    // the instance is dead — drop it. If yes, re-attach but filter
    // app_pids to only those still alive (orphan apps are normal).

    fn snap_with(server_pid: u32, app_pids: Vec<u32>) -> InstanceSnapshot {
        InstanceSnapshot {
            instance: "dev".into(),
            port: 5710,
            server_pid,
            app_pids,
        }
    }

    #[test]
    fn reconcile_drops_when_server_pid_dead() {
        let snap = snap_with(100, vec![201, 202]);
        // Probe says nobody is alive.
        assert_eq!(decide_reconcile(&snap, |_| false), ReconcileOutcome::Drop);
    }

    #[test]
    fn reconcile_recovers_when_server_alive_with_all_apps_alive() {
        let snap = snap_with(100, vec![201, 202]);
        let alive = |pid| matches!(pid, 100 | 201 | 202);
        assert_eq!(
            decide_reconcile(&snap, alive),
            ReconcileOutcome::Recover {
                app_pids: vec![201, 202]
            }
        );
    }

    #[test]
    fn reconcile_recovers_filtering_dead_app_pids() {
        // Server is still alive but one of the gnome apps died in the
        // gap. We recover the instance but drop the dead app from the
        // tracked list.
        let snap = snap_with(100, vec![201, 202, 203]);
        let alive = |pid| matches!(pid, 100 | 201 | 203);
        assert_eq!(
            decide_reconcile(&snap, alive),
            ReconcileOutcome::Recover {
                app_pids: vec![201, 203]
            }
        );
    }

    // ---- instance_summaries_from_map ------------------------------------
    //
    // Story: `vbox ctl status` shows the current instances. The
    // serializer must produce deterministic output (HashMap iteration
    // is unspecified) and round-trip every field.

    // ---- encode_snapshot / snapshot_tmp_path ----------------------------

    // ---- server_spawn_env -----------------------------------------------
    //
    // Story: every `vbox-server` spawn must always carry
    // `RUST_BACKTRACE=1` so a panic in the guest compositor produces
    // an actionable trace. `VBOX_DEBUG=1` is a toggle from the
    // operator's StartInstance flag.

    // ---- wayland_socket_path -------------------------------------------
    //
    // Story: `wait_socket` resolves the Wayland socket under
    // `$XDG_RUNTIME_DIR`. The path join is trivial but the contract
    // (always the runtime dir + socket name, no separator surprises)
    // is something tests can pin once and for all.

    #[test]
    fn wayland_path_joins_runtime_and_socket() {
        let path = wayland_socket_path(std::path::Path::new("/run/user/1000"), "wayland-0");
        assert_eq!(path, std::path::PathBuf::from("/run/user/1000/wayland-0"));
    }

    #[test]
    fn wayland_path_works_with_alternate_socket_names() {
        let path = wayland_socket_path(std::path::Path::new("/run/user/1000"), "vbox-dev");
        assert_eq!(path, std::path::PathBuf::from("/run/user/1000/vbox-dev"));
    }

    #[test]
    fn server_env_always_includes_rust_backtrace() {
        let env = server_spawn_env(false);
        assert!(env.iter().any(|(k, v)| *k == "RUST_BACKTRACE" && *v == "1"));
    }

    #[test]
    fn server_env_omits_debug_when_not_requested() {
        let env = server_spawn_env(false);
        assert!(env.iter().all(|(k, _)| *k != "VBOX_DEBUG"));
    }

    #[test]
    fn server_env_includes_debug_when_requested() {
        let env = server_spawn_env(true);
        let debug = env
            .iter()
            .find(|(k, _)| *k == "VBOX_DEBUG")
            .expect("VBOX_DEBUG must be set when debug=true");
        assert_eq!(debug.1, "1");
    }

    #[test]
    fn server_env_keeps_backtrace_and_debug_distinct() {
        // Sanity: the two keys never collide. Operators rely on both
        // being inspectable in `ps -E <pid>` output.
        let env = server_spawn_env(true);
        let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"VBOX_DEBUG"));
        assert!(keys.contains(&"RUST_BACKTRACE"));
    }

    #[test]
    fn encode_snapshot_round_trips_via_postcard() {
        // Stage a snapshot, encode it, decode it. Bytes between writes
        // must reproduce field-for-field — the on-disk schema is what
        // a restarted daemon depends on.
        let snap = Snapshot {
            instances: vec![InstanceSnapshot {
                instance: "dev".into(),
                port: 5710,
                server_pid: 1000,
                app_pids: vec![1001, 1002],
            }],
        };
        let bytes = encode_snapshot(&snap).expect("encode must succeed");
        let decoded: Snapshot = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.instances.len(), 1);
        assert_eq!(decoded.instances[0].instance, "dev");
        assert_eq!(decoded.instances[0].port, 5710);
        assert_eq!(decoded.instances[0].server_pid, 1000);
        assert_eq!(decoded.instances[0].app_pids, vec![1001, 1002]);
    }

    #[test]
    fn snapshot_tmp_path_replaces_extension_with_bin_tmp() {
        // The atomic-write contract is "write to <state_path>.tmp then
        // rename(2)". The temp path must be `.bin.tmp` (not `.tmp`) so
        // a future operator who greps `*.bin.tmp` in the work_dir
        // catches half-written snapshots.
        let p = snapshot_tmp_path(std::path::Path::new("/var/lib/vbox/state.bin"));
        assert_eq!(p, std::path::PathBuf::from("/var/lib/vbox/state.bin.tmp"));
    }

    #[test]
    fn snapshot_tmp_path_works_on_relative_paths() {
        let p = snapshot_tmp_path(std::path::Path::new(".vbox-controld/state.bin"));
        assert_eq!(p, std::path::PathBuf::from(".vbox-controld/state.bin.tmp"));
    }

    #[test]
    fn quic_cert_material_is_reused_and_hashable() {
        let dir = tempdir_for_test();
        let first = ensure_quic_cert_material(&dir, "org.gnome.Calculator").unwrap();
        let first_pin = sha256_hex(&load_first_cert_der(&first.cert_path).unwrap());
        let second = ensure_quic_cert_material(&dir, "org.gnome.Calculator").unwrap();
        let second_pin = sha256_hex(&load_first_cert_der(&second.cert_path).unwrap());

        assert_eq!(first.cert_path, second.cert_path);
        assert_eq!(first.key_path, second.key_path);
        assert_eq!(first_pin.len(), 64);
        assert_eq!(first_pin, second_pin);
    }

    #[test]
    fn summaries_render_each_instance_with_its_fields() {
        let mut map = HashMap::new();
        map.insert("dev".to_owned(), (5710u16, 1000u32, vec![1001u32, 1002]));
        map.insert("scratch".to_owned(), (5712, 2000, vec![]));

        let out = instance_summaries_from_map(&map);

        assert_eq!(out.len(), 2);
        // Sorted alphabetically → "dev" comes before "scratch".
        assert_eq!(out[0].instance, "dev");
        assert_eq!(out[0].port, 5710);
        assert_eq!(out[0].server_pid, 1000);
        assert_eq!(out[0].app_pids, vec![1001, 1002]);
        assert_eq!(out[1].instance, "scratch");
        assert_eq!(out[1].app_pids, Vec::<u32>::new());
    }

    #[test]
    fn summaries_are_sorted_so_status_output_is_stable() {
        // Insert in reverse alphabetic order; output must still come
        // back alphabetically so `vbox ctl status` doesn't shuffle
        // between calls.
        let mut map = HashMap::new();
        for name in ["zeta", "alpha", "middle"] {
            map.insert(name.to_owned(), (5710u16, 1u32, Vec::<u32>::new()));
        }

        let out = instance_summaries_from_map(&map);

        let names: Vec<&str> = out.iter().map(|s| s.instance.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zeta"]);
    }

    #[test]
    fn summaries_empty_map_produces_empty_vec() {
        // No running instances → no rows. Mirrors a fresh daemon that
        // hasn't seen a StartInstance yet.
        let map: HashMap<String, (u16, u32, Vec<u32>)> = HashMap::new();
        assert!(instance_summaries_from_map(&map).is_empty());
    }

    #[test]
    fn reconcile_recovers_with_empty_app_list_when_all_apps_died() {
        // Server alive, every guest app died — instance is still useful,
        // a future LaunchApp can re-fill it.
        let snap = snap_with(100, vec![201, 202]);
        let alive = |pid| pid == 100;
        assert_eq!(
            decide_reconcile(&snap, alive),
            ReconcileOutcome::Recover { app_pids: vec![] }
        );
    }

    #[test]
    fn xdg_runtime_dir_is_propagated_even_without_audio_sockets() {
        // The PipeWire client libs use XDG_RUNTIME_DIR as a last-resort
        // discovery path. We propagate it whenever XDG is known, regardless
        // of whether the explicit sockets resolved.
        let plan = guest_env_plan(GuestEnvInputs {
            socket: "wayland-0",
            use_dbus_isolation: false,
            xdg_runtime: Some("/run/user/1000"),
            home: None,
            check_path: deny_all_paths,
        });

        assert_eq!(plan.get("XDG_RUNTIME_DIR"), Some("/run/user/1000"));
        assert!(plan.get("PIPEWIRE_REMOTE").is_none());
    }
}
