//! Bind the Wayland listening socket under `$XDG_RUNTIME_DIR`, cleaning up
//! any stale leftover from a previously-crashed vbox-server.
//!
//! Smithay's `ListeningSocket::bind` fails on `EADDRINUSE` even when no
//! live process is using the socket — the `wayland-N` and `wayland-N.lock`
//! files outlive a process killed by `SIGKILL/OOM`. We probe the address
//! first to distinguish a genuine concurrent listener from a stale
//! leftover, then unlink and retry once.
//!
//! [`SocketCleanup`] is the RAII counterpart: keep it alive for the lifetime
//! of the server thread so a normal exit removes the socket file before
//! the next start tries to bind.
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use smithay::reexports::wayland_server::ListeningSocket;

/// Smithay's `ListeningSocket::bind` fails on `EADDRINUSE` even when no
/// live process is using the socket — the `wayland-N` and `wayland-N.lock`
/// exists in `$XDG_RUNTIME_DIR`, even when no live process is using them.
/// We see that whenever a previous vbox-server died without running its
/// `SocketCleanup` Drop (SIGKILL, OOM, crash). Probe the address first — a
/// genuine concurrent listener answers `connect(2)`; a stale leftover gives
/// ECONNREFUSED or ENOENT — and clean up both files before retrying once.
pub(crate) fn bind_wayland_socket(socket_name: &str) -> Result<ListeningSocket> {
    if let Ok(l) = ListeningSocket::bind(socket_name) {
        return Ok(l);
    }

    let (socket_path, lock_path) = socket_lock_paths(socket_name);
    let mut last_err = None;
    for attempt in 1..=5 {
        if is_listener_active(&socket_path) {
            bail!(
                "Wayland socket {socket_name} is in use by another live process \
                 (refusing to clobber)"
            );
        }
        eprintln!(
            "wayland: stale leftover for socket '{socket_name}' — unlinking {} and {} (attempt {attempt}/5)",
            socket_path.display(),
            lock_path.display(),
        );
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&lock_path);
        match ListeningSocket::bind(socket_name) {
            Ok(listener) => return Ok(listener),
            Err(err) => {
                last_err = Some(err);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    Err(last_err.expect("bind retry must record the final error")).with_context(|| {
        format!("binding Wayland socket {socket_name} after stale-leftover cleanup")
    })
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid() is async-signal-safe and always succeeds.
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        })
}

fn socket_lock_paths(socket_name: &str) -> (PathBuf, PathBuf) {
    if socket_name.starts_with('/') {
        (
            PathBuf::from(socket_name),
            PathBuf::from(format!("{socket_name}.lock")),
        )
    } else {
        let runtime = runtime_dir();
        (
            runtime.join(socket_name),
            runtime.join(format!("{socket_name}.lock")),
        )
    }
}

fn is_listener_active(socket_path: &Path) -> bool {
    use std::os::unix::net::UnixStream;
    match UnixStream::connect(socket_path) {
        Ok(_) => true,
        Err(e)
            if e.kind() == std::io::ErrorKind::ConnectionRefused
                || e.raw_os_error() == Some(libc::ENOENT) =>
        {
            false
        }
        Err(_) => false,
    }
}

pub(crate) struct SocketCleanup {
    paths: Vec<PathBuf>,
}

impl SocketCleanup {
    pub(crate) fn new(socket_name: &str) -> Self {
        let paths = if socket_name.contains('/') && !socket_name.starts_with('/') {
            Vec::new()
        } else {
            let (socket, lock) = socket_lock_paths(socket_name);
            vec![socket, lock]
        };
        Self { paths }
    }
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        for path in self.paths.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_cleanup_tracks_socket_and_lock_for_relative_name() {
        let cleanup = SocketCleanup::new("vbox-test");

        assert_eq!(cleanup.paths.len(), 2);
        assert!(cleanup.paths[0].ends_with("vbox-test"));
        assert!(cleanup.paths[1].ends_with("vbox-test.lock"));
    }

    #[test]
    fn socket_cleanup_tracks_socket_and_lock_for_absolute_name() {
        let cleanup = SocketCleanup::new("/tmp/vbox-test");

        assert_eq!(
            cleanup.paths,
            vec![
                PathBuf::from("/tmp/vbox-test"),
                PathBuf::from("/tmp/vbox-test.lock"),
            ]
        );
    }

    #[test]
    fn socket_cleanup_ignores_nested_relative_paths() {
        let cleanup = SocketCleanup::new("nested/vbox-test");

        assert!(cleanup.paths.is_empty());
    }
}
