//! On-disk snapshot of live instances, persisted to `state.bin` (postcard).
//!
//! `DaemonState::new` calls [`load_snapshot`] at startup so the daemon can
//! reconcile instances whose pids survived a restart (kept alive by the
//! kernel, but no longer tracked in memory). After the reconcile pass we
//! drop the on-disk handle and only the live in-memory state matters; the
//! file is rewritten whenever DaemonState mutates so a future restart sees
//! the current set.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Snapshot {
    pub(crate) instances: Vec<InstanceSnapshot>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct InstanceSnapshot {
    pub(crate) instance: String,
    pub(crate) port: u16,
    pub(crate) server_pid: u32,
    pub(crate) app_pids: Vec<u32>,
}

pub(crate) fn load_snapshot(path: &Path) -> Result<Option<Snapshot>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let snap: Snapshot = postcard::from_bytes(&bytes)
        .with_context(|| format!("decode snapshot {}", path.display()))?;
    Ok(Some(snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // The daemon's startup story for snapshots is short:
    //   1. First-ever boot: state.bin does not exist → Ok(None) so we treat
    //      "no prior state" as the empty set, not an error.
    //   2. Clean shutdown then restart: state.bin exists with valid postcard
    //      bytes → we decode it and recover the instance list.
    //   3. Crash before first flush: state.bin exists but is zero bytes →
    //      Ok(None), again the empty set.
    //   4. Corruption (postcard returns Err): bubble up so the operator sees
    //      it rather than silently losing track of running children.

    #[test]
    fn load_snapshot_returns_none_when_file_missing() {
        let dir = tempdir_for_test();
        let path = dir.path.join("never-written.bin");

        let result = load_snapshot(&path).expect("missing file is not an error");

        assert!(result.is_none(), "missing file → Ok(None)");
    }

    #[test]
    fn load_snapshot_returns_none_when_file_empty() {
        // Simulates an interrupted first flush: file created but never
        // populated. We must not try to decode zero bytes — postcard would
        // return an obscure Eof, but the operator just wants "no state".
        let dir = tempdir_for_test();
        let path = dir.path.join("empty.bin");
        fs::write(&path, b"").unwrap();

        let result = load_snapshot(&path).expect("empty file is not an error");

        assert!(result.is_none(), "empty file → Ok(None)");
    }

    #[test]
    fn load_snapshot_round_trips_a_typical_two_instance_snapshot() {
        // Mirrors the real layout: the daemon owns two instances ("dev",
        // "scratch"), each with a server pid and a couple of guest app
        // pids. Encode → write → load → compare.
        let dir = tempdir_for_test();
        let path = dir.path.join("state.bin");
        let snap = Snapshot {
            instances: vec![
                InstanceSnapshot {
                    instance: "dev".to_owned(),
                    port: 5710,
                    server_pid: 1234,
                    app_pids: vec![1235, 1236],
                },
                InstanceSnapshot {
                    instance: "scratch".to_owned(),
                    port: 5712,
                    server_pid: 7777,
                    app_pids: vec![],
                },
            ],
        };
        let bytes = postcard::to_allocvec(&snap).unwrap();
        fs::write(&path, &bytes).unwrap();

        let restored = load_snapshot(&path).unwrap().expect("snapshot should load");

        assert_eq!(restored.instances.len(), 2);
        assert_eq!(restored.instances[0].instance, "dev");
        assert_eq!(restored.instances[0].port, 5710);
        assert_eq!(restored.instances[0].server_pid, 1234);
        assert_eq!(restored.instances[0].app_pids, vec![1235, 1236]);
        assert_eq!(restored.instances[1].instance, "scratch");
        assert_eq!(restored.instances[1].app_pids, Vec::<u32>::new());
    }

    #[test]
    fn load_snapshot_surfaces_corruption_as_error() {
        // If state.bin gets truncated or someone else writes garbage to it,
        // we want a loud failure at startup — silently dropping instance
        // state would orphan running guest apps.
        let dir = tempdir_for_test();
        let path = dir.path.join("corrupt.bin");
        fs::write(&path, b"this is not postcard").unwrap();

        let err = load_snapshot(&path).expect_err("corrupt file must error");

        let chain = format!("{err:#}");
        assert!(
            chain.contains("decode snapshot"),
            "error chain should name the file we were decoding, got: {chain}"
        );
    }

    // Trivial tempdir helper; see utils.rs for the same pattern. Kept local
    // so each module is independently testable without pulling in tempfile.
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
            "vbox-controld-snapshot-{}-{}",
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
