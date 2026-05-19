//! Wait for a Wayland socket to appear under `$XDG_RUNTIME_DIR`.
//!
//! The instance-start flow creates the wayland-socket via `vbox-server`
//! before app processes spawn; this module is what the daemon uses to
//! "wait for the socket to materialise" before declaring the instance
//! ready.
//!
//! Two strategies:
//! - [`wait_socket_poll`] is portable and always available — `path.exists()`
//!   on a 50ms tick until the deadline.
//! - [`wait_socket_inotify`] is Linux-only and watches the parent dir for
//!   `IN_CREATE | IN_MOVED_TO` events filtered to the target filename. It
//!   returns `None` when inotify itself is unavailable so the caller can
//!   fall back to polling.
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use anyhow::anyhow;
use anyhow::{Result, bail};

pub(crate) fn wait_socket_poll(path: &Path, timeout: Duration) -> Result<()> {
    wait_socket_poll_with_step(path, timeout, Duration::from_millis(50))
}

/// Same logic as [`wait_socket_poll`] but with the poll interval injected.
/// Tests use a short step (≤5 ms) so they exit in tens of milliseconds even
/// on a slow CI runner; production keeps the original 50 ms cadence.
fn wait_socket_poll_with_step(path: &Path, timeout: Duration, step: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("missing socket: {} (timeout)", path.display());
        }
        std::thread::sleep(step);
    }
}

/// Linux inotify wait. Returns `None` if inotify itself is unavailable (so
/// the caller can fall back to polling). Returns `Some(Ok(()))` when the
/// socket appears, `Some(Err(_))` for hard failures (watch on missing parent
/// dir, fatal poll error, deadline elapsed).
///
/// Race ordering matters: arm the watch BEFORE the existence re-check, so a
/// socket that appears in the gap between the caller's first `path.exists()`
/// and our watch setup still wakes us.
#[cfg(target_os = "linux")]
pub(crate) fn wait_socket_inotify(
    runtime: &Path,
    socket: &str,
    path: &Path,
    timeout: Duration,
) -> Option<Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;

    // SAFETY: inotify_init1 returns -1 on failure; we wrap the fd immediately.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if fd < 0 {
        return None; // fall back to polling
    }
    // RAII: close fd on any exit path.
    let _inotify = unsafe { std::fs::File::from_raw_fd(fd) };

    let parent_c = match CString::new(runtime.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(e) => return Some(Err(anyhow!("CString from runtime: {e}"))),
    };
    let wd = unsafe {
        libc::inotify_add_watch(fd, parent_c.as_ptr(), libc::IN_CREATE | libc::IN_MOVED_TO)
    };
    if wd < 0 {
        return Some(Err(anyhow!(
            "inotify_add_watch({}): {}",
            runtime.display(),
            std::io::Error::last_os_error()
        )));
    }

    // Re-check after arming the watch — covers the race where the socket
    // appears between the caller's existence probe and the watch setup.
    if path.exists() {
        return Some(Ok(()));
    }

    let target_name = socket.as_bytes();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Some(Err(anyhow!(
                "missing socket: {} (timeout {}ms)",
                path.display(),
                timeout.as_millis()
            )));
        }
        let remaining = deadline - now;
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd is valid pointer to a single pollfd; libc::poll signature.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Some(Err(anyhow!("poll inotify fd: {err}")));
        }
        if rc == 0 {
            return Some(Err(anyhow!(
                "missing socket: {} (timeout {}ms)",
                path.display(),
                timeout.as_millis()
            )));
        }

        // Drain events. Any event whose name matches our socket wins.
        loop {
            // SAFETY: read fills our owned stack buffer; we bound the length.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                return Some(Err(anyhow!("read inotify fd: {err}")));
            }
            if n == 0 {
                break;
            }
            let n = n as usize;
            const HDR: usize = std::mem::size_of::<libc::inotify_event>();
            let mut off = 0;
            while off + HDR <= n {
                // SAFETY: `buf[off..off+HDR]` is initialised by `read`; align fine
                // for inotify_event (4-byte alignment guaranteed by the kernel).
                let ev: &libc::inotify_event =
                    unsafe { &*(buf.as_ptr().add(off) as *const libc::inotify_event) };
                let name_len = ev.len as usize;
                let name_start = off + HDR;
                let name_end = name_start + name_len;
                if name_end > n {
                    break;
                }
                let name_raw = &buf[name_start..name_end];
                // Names are NUL-padded to align; take the prefix before the
                // first NUL.
                let name = name_raw.split(|b| *b == 0).next().unwrap_or(&[]);
                if name == target_name && path.exists() {
                    return Some(Ok(()));
                }
                off = name_end;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // Story: the daemon waits for $XDG_RUNTIME_DIR/wayland-N to appear after
    // spawning vbox-server. The polling helper drives that wait. We exercise
    // three flows a real operator might see:
    //   1. Socket already exists by the time we ask → immediate Ok.
    //   2. Socket appears in the middle of the wait → Ok before the deadline.
    //   3. Socket never appears → timeout error names the path so logs are
    //      grep-friendly.

    #[test]
    fn returns_immediately_when_socket_already_exists() {
        let dir = tempdir_for_test();
        let path = dir.path.join("wayland-0");
        fs::write(&path, b"").unwrap();

        let start = Instant::now();
        wait_socket_poll_with_step(&path, Duration::from_secs(5), Duration::from_millis(50))
            .expect("existing socket should resolve immediately");

        assert!(
            start.elapsed() < Duration::from_millis(50),
            "should not have slept at all"
        );
    }

    #[test]
    fn returns_ok_when_socket_appears_during_wait() {
        // Background thread creates the socket ~30 ms after we start
        // waiting. The poll helper must wake up and return Ok well before
        // the 2 s timeout. Mirrors the vbox-server-spawn-then-bind flow.
        let dir = tempdir_for_test();
        let path = dir.path.join("wayland-1");
        let writer_path = path.clone();
        let creator = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            fs::write(&writer_path, b"").unwrap();
        });

        let outcome =
            wait_socket_poll_with_step(&path, Duration::from_secs(2), Duration::from_millis(5));
        creator.join().unwrap();

        outcome.expect("socket created during wait → Ok");
    }

    #[test]
    fn errors_when_socket_never_appears() {
        let dir = tempdir_for_test();
        let path = dir.path.join("wayland-missing");

        let err =
            wait_socket_poll_with_step(&path, Duration::from_millis(60), Duration::from_millis(5))
                .expect_err("missing socket past the deadline must error");

        let msg = format!("{err}");
        assert!(msg.contains("missing socket"));
        assert!(
            msg.contains("wayland-missing"),
            "error must name the path so operators can find it in logs, got: {msg}"
        );
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
            "vbox-controld-socketwait-{}-{}",
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
