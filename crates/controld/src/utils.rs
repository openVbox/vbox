//! Misc POSIX/XDG helpers shared by the daemon lifecycle code.
//!
//! Two unrelated concerns kept together because both are thin wrappers over
//! `libc` / `std::env`:
//! - pid signalling: [`kill_pid`] (real signal), [`pid_alive`] (signal-0 probe).
//! - environment lookup: [`which_in_path`] (PATH search), [`xdg_runtime_dir`]
//!   (`$XDG_RUNTIME_DIR` with `/run/user/$UID` fallback).
//!
//! Each public wrapper delegates to a small "pure" helper that takes the
//! environment as arguments. Tests target the pure helpers — they never look
//! at the real process's `$PATH` or `$XDG_RUNTIME_DIR`, so they stay
//! deterministic regardless of how cargo test is launched.
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;

pub(crate) fn kill_pid(pid: u32, sig: i32) -> io::Result<()> {
    // SAFETY: kill() accepts any pid; errno reports whether the pid existed.
    // pid_t is i32 on Linux; PID_MAX_LIMIT is 2^22 so the cast never wraps in
    // practice, but use try_from to surface the impossible case as ESRCH.
    let pid_t =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from_raw_os_error(libc::ESRCH))?;
    let rc = unsafe { libc::kill(pid_t, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // kill(pid, 0) returns 0 if the process exists and we can signal it;
    // ESRCH means the pid doesn't exist; EPERM means it does (different user).
    let rc = unsafe { libc::kill(pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let err = io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

/// Return the absolute path of `name` if found under any directory listed in
/// $PATH, else None. Used to decide whether to wrap guest app spawns with
/// `dbus-run-session`.
pub(crate) fn which_in_path(name: &str) -> Option<PathBuf> {
    which_in_paths(name, std::env::var_os("PATH"), |p| p.is_file())
}

/// Same as [`which_in_path`] but takes the PATH string and the "is this an
/// executable file?" predicate as arguments. Lets tests pin a fixed PATH
/// without poking the process-wide env, and lets them stub the filesystem
/// check so they don't need to chmod +x a real file.
fn which_in_paths<F>(name: &str, path_env: Option<OsString>, is_match: F) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let env = path_env?;
    for dir in std::env::split_paths(&env) {
        let candidate = dir.join(name);
        if is_match(&candidate) {
            return Some(candidate);
        }
    }
    None
}

// Result return keeps the door open for future `env::var` propagation; callers
// already use `?`.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn xdg_runtime_dir() -> Result<PathBuf> {
    // SAFETY: getuid() never fails.
    let uid = unsafe { libc::getuid() };
    Ok(resolve_xdg_runtime_dir(
        std::env::var("XDG_RUNTIME_DIR").ok(),
        uid,
    ))
}

/// Pure resolver: choose $XDG_RUNTIME_DIR when non-empty, else
/// `/run/user/<uid>`. Empty/whitespace strings are treated as unset — they
/// arrive as the empty string from broken init scripts and the fallback is
/// always more useful than `PathBuf::new()`.
fn resolve_xdg_runtime_dir(xdg: Option<String>, uid: u32) -> PathBuf {
    match xdg {
        Some(s) if !s.trim().is_empty() => PathBuf::from(s),
        _ => PathBuf::from(format!("/run/user/{uid}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    // ---------- pid_alive / kill_pid scenarios ----------
    //
    // Story: the daemon needs to ask "is this server pid still running?"
    // before deciding to skip a restart. The cheapest way is kill(pid, 0)
    // — that's what pid_alive does. Our own process is the one pid we can
    // always count on existing.

    #[test]
    fn pid_alive_says_yes_for_our_own_process() {
        let me = std::process::id();
        assert!(pid_alive(me), "current process must look alive to itself");
    }

    #[test]
    fn pid_alive_says_no_for_pid_zero() {
        // pid 0 is reserved (the calling process's process group); the
        // daemon should never treat it as a real instance.
        assert!(!pid_alive(0));
    }

    #[test]
    fn pid_alive_says_no_for_unreachable_pid() {
        // pid 2^22 is above Linux's PID_MAX_LIMIT default — guaranteed not to
        // exist. We use this to prove the ESRCH branch flips alive→false.
        assert!(!pid_alive(4_194_304));
    }

    #[test]
    fn kill_pid_with_signal_zero_succeeds_for_self() {
        // signal 0 is a permission/existence probe — never delivered. Used
        // here just to confirm kill_pid wraps libc::kill without surprises.
        let me = std::process::id();
        kill_pid(me, 0).expect("signal 0 to self should succeed");
    }

    #[test]
    fn kill_pid_with_huge_pid_reports_esrch() {
        // Above PID_MAX_LIMIT we hit the try_from arm; the manifest contract
        // is "surface as ESRCH so the caller sees a familiar error code".
        let err = kill_pid(u32::MAX, 0).expect_err("u32::MAX is not a real pid");
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
    }

    // ---------- which_in_path scenarios ----------
    //
    // Story: the daemon needs to decide whether to wrap guest spawns in
    // dbus-run-session. The decision is "is dbus-run-session on $PATH?".
    // The pure helper takes an explicit PATH so the test doesn't depend on
    // whatever PATH cargo inherited.

    #[test]
    fn which_in_paths_finds_executable_in_listed_dir() {
        let tmp = tempdir_for_test();
        let bin_dir = tmp.path.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("dbus-run-session");
        std::fs::write(&exe, b"#!/bin/sh\nexec true\n").unwrap();
        let path_env = OsString::from(bin_dir.to_str().unwrap());

        let found = which_in_paths("dbus-run-session", Some(path_env), |p| p.is_file());

        assert_eq!(found, Some(exe));
    }

    #[test]
    fn which_in_paths_returns_none_when_no_match() {
        let tmp = tempdir_for_test();
        let path_env = OsString::from(tmp.path.to_str().unwrap());

        let found = which_in_paths("does-not-exist-anywhere", Some(path_env), |p| p.is_file());

        assert_eq!(found, None);
    }

    #[test]
    fn which_in_paths_returns_none_when_path_env_unset() {
        let found = which_in_paths::<_>("anything", None, |_| true);
        assert!(found.is_none());
    }

    #[test]
    fn which_in_paths_walks_entries_in_order() {
        let tmp = tempdir_for_test();
        let a = tmp.path.join("a");
        let b = tmp.path.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("tool"), b"").unwrap();
        let path_env = OsString::from(format!("{}:{}", a.display(), b.display()));

        let found = which_in_paths("tool", Some(path_env), |p| p.is_file()).unwrap();

        // First entry has no "tool"; second does. Returned path must live in
        // the second dir, not the first.
        assert!(found.starts_with(&b));
    }

    // ---------- resolve_xdg_runtime_dir scenarios ----------
    //
    // Story: ephemeral GNOME apps care about $XDG_RUNTIME_DIR — under systemd
    // it's already set to /run/user/$UID, but a stripped-env spawn can lose
    // it. The resolver picks the env value when usable, falls back to the
    // canonical /run/user/$UID otherwise.

    #[test]
    fn resolve_xdg_uses_env_value_when_set() {
        let dir = resolve_xdg_runtime_dir(Some("/run/user/1000".into()), 1000);
        assert_eq!(dir, PathBuf::from("/run/user/1000"));
    }

    #[test]
    fn resolve_xdg_falls_back_when_env_unset() {
        let dir = resolve_xdg_runtime_dir(None, 1000);
        assert_eq!(dir, PathBuf::from("/run/user/1000"));
    }

    #[test]
    fn resolve_xdg_falls_back_when_env_empty_string() {
        // Broken systemd unit files sometimes export XDG_RUNTIME_DIR="". The
        // daemon must treat that as "missing" so we don't end up joining
        // socket names onto an empty path.
        let dir = resolve_xdg_runtime_dir(Some(String::new()), 42);
        assert_eq!(dir, PathBuf::from("/run/user/42"));
    }

    #[test]
    fn resolve_xdg_falls_back_when_env_only_whitespace() {
        let dir = resolve_xdg_runtime_dir(Some("   ".into()), 7);
        assert_eq!(dir, PathBuf::from("/run/user/7"));
    }

    // Tiny tempdir helper so tests don't reach for an external crate. RAII
    // wrapper that wipes its tree on drop.
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
            "vbox-controld-utils-{}-{}",
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
