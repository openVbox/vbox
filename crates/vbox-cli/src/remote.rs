use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::RemoteArgs;
use crate::RemoteCommand;
use crate::context::AppContext;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct RemoteHosts {
    #[serde(default)]
    hosts: Vec<RemoteHost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemoteHost {
    name: String,
    ssh: String,
    #[serde(default)]
    guest_dir: String,
    #[serde(default = "default_os_kind")]
    os_kind: String,
    #[serde(default)]
    os_raw: String,
    #[serde(default)]
    added_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) ssh_user: String,
    pub(crate) ssh_host: String,
    pub(crate) guest_dir: String,
    pub(crate) os_raw: String,
    pub(crate) os_kind: String,
}

pub(crate) fn run(ctx: &AppContext, args: RemoteArgs) -> Result<()> {
    match args.command {
        Some(RemoteCommand::List(list)) => list_cmd(ctx, list.tsv, list.json),
        Some(RemoteCommand::Add(add)) => add_cmd(
            ctx,
            add.name,
            add.ssh,
            add.dir.map(|p| p.display().to_string()),
            add.os,
            add.os_raw.unwrap_or_default(),
        ),
        Some(RemoteCommand::Remove { name }) => remove_cmd(ctx, &name),
        Some(RemoteCommand::File) => {
            println!("{}", file(ctx).display());
            Ok(())
        }
        None => list_cmd(ctx, args.list.tsv, args.list.json),
    }
}

pub(crate) fn records(ctx: &AppContext) -> Result<Vec<RemoteRecord>> {
    Ok(load(ctx)?
        .hosts
        .into_iter()
        .filter_map(|host| {
            let (user, ssh_host) = host.ssh.split_once('@')?;
            let guest_dir = host.guest_dir.clone();
            let name = host.name.clone();
            Some(RemoteRecord {
                id: format!("remote:{}", sanitize_id(&name)),
                name,
                ssh_user: user.to_string(),
                ssh_host: ssh_host.to_string(),
                guest_dir,
                os_raw: host.os_raw,
                os_kind: host.os_kind,
            })
        })
        .collect())
}

pub(crate) fn remove_name(ctx: &AppContext, name: &str) -> Result<bool> {
    let mut data = load(ctx)?;
    let before = data.hosts.len();
    data.hosts.retain(|host| host.name != name);
    save(ctx, &data)?;
    Ok(data.hosts.len() != before)
}

fn list_cmd(ctx: &AppContext, tsv: bool, json: bool) -> Result<()> {
    if json {
        print!("{}", serde_json::to_string_pretty(&load(ctx)?)?);
        println!();
        return Ok(());
    }
    let rows = records(ctx)?;
    if tsv {
        for row in rows {
            println!(
                "{}\t{}\tremote\t{}\t{}\t{}\t{}\t{}\t{}",
                row.id,
                row.name,
                if row.ssh_host.is_empty() {
                    "-"
                } else {
                    &row.ssh_host
                },
                row.os_raw,
                row.os_kind,
                row.ssh_user,
                row.ssh_host,
                row.guest_dir
            );
        }
    } else {
        println!("{:<32}  {:<30}  DIR", "NAME", "SSH");
        println!("{:<32}  {:<30}  ---", "----", "---");
        for row in rows {
            println!(
                "{:<32}  {:<30}  {}",
                row.name,
                format!("{}@{}", row.ssh_user, row.ssh_host),
                row.guest_dir
            );
        }
    }
    Ok(())
}

fn add_cmd(
    ctx: &AppContext,
    name: String,
    ssh: String,
    guest_dir: Option<String>,
    os_kind: String,
    os_raw: String,
) -> Result<()> {
    if name.trim().is_empty() {
        bail!("--name required");
    }
    if !ssh.contains('@') {
        bail!("--ssh must be user@host");
    }
    let guest_dir = match guest_dir {
        Some(guest_dir) => guest_dir,
        None => ctx.guest_dir()?.display().to_string(),
    };
    let mut data = load(ctx)?;
    data.hosts.retain(|host| host.name != name);
    data.hosts.push(RemoteHost {
        name: name.clone(),
        ssh: ssh.clone(),
        guest_dir,
        os_kind,
        os_raw,
        added_at: now_string(),
    });
    save(ctx, &data)?;
    println!("added remote: {name} ({ssh})");
    Ok(())
}

fn remove_cmd(ctx: &AppContext, name: &str) -> Result<()> {
    if remove_name(ctx, name)? {
        println!("removed remote: {name}");
        Ok(())
    } else {
        bail!("no such remote: {name}")
    }
}

fn load(ctx: &AppContext) -> Result<RemoteHosts> {
    let path = file(ctx);
    if !path.exists() {
        return Ok(RemoteHosts::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

fn save(ctx: &AppContext, data: &RemoteHosts) -> Result<()> {
    let path = file(ctx);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(data)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
}

fn file(ctx: &AppContext) -> PathBuf {
    ctx.state_dir.join("remote-hosts.json")
}

fn default_os_kind() -> String {
    "linux".to_string()
}

fn sanitize_id(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    let mut out = String::new();
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "host".to_string()
    } else {
        out
    }
}

fn now_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AppContext;
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
        let path = std::env::temp_dir().join(format!(
            "vbox-remote-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn ctx(dir: &TempDir) -> AppContext {
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
    fn sanitize_id_keeps_stable_remote_prefix_shape() {
        assert_eq!(sanitize_id("Fedora Workstation"), "fedora_workstation");
        assert_eq!(sanitize_id("__!!!__"), "host");
        assert_eq!(sanitize_id("host.example_1"), "host.example_1");
    }

    #[test]
    fn missing_remote_file_loads_empty_hosts() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        assert!(load(&ctx).unwrap().hosts.is_empty());
        assert!(records(&ctx).unwrap().is_empty());
    }

    #[test]
    fn save_and_records_round_trip_valid_hosts_only() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        save(
            &ctx,
            &RemoteHosts {
                hosts: vec![
                    RemoteHost {
                        name: "Fedora Lab".to_string(),
                        ssh: "alice@fedora.local".to_string(),
                        guest_dir: "/srv/vbox".to_string(),
                        os_kind: "linux".to_string(),
                        os_raw: "Fedora".to_string(),
                        added_at: "unix:1".to_string(),
                    },
                    RemoteHost {
                        name: "broken".to_string(),
                        ssh: "missing-at".to_string(),
                        guest_dir: "/ignored".to_string(),
                        os_kind: "linux".to_string(),
                        os_raw: String::new(),
                        added_at: "unix:2".to_string(),
                    },
                ],
            },
        )
        .unwrap();

        let rows = records(&ctx).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "remote:fedora_lab");
        assert_eq!(rows[0].ssh_user, "alice");
        assert_eq!(rows[0].ssh_host, "fedora.local");
        assert_eq!(rows[0].guest_dir, "/srv/vbox");
    }

    #[test]
    fn add_replaces_existing_remote_and_uses_context_guest_dir() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        add_cmd(
            &ctx,
            "lab".to_string(),
            "alice@old".to_string(),
            Some("/old".to_string()),
            "linux".to_string(),
            String::new(),
        )
        .unwrap();
        add_cmd(
            &ctx,
            "lab".to_string(),
            "bob@new".to_string(),
            None,
            "linux".to_string(),
            "Ubuntu".to_string(),
        )
        .unwrap();

        let rows = records(&ctx).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ssh_user, "bob");
        assert_eq!(rows[0].ssh_host, "new");
        assert_eq!(rows[0].guest_dir, "/home/alice/vbox");
        assert_eq!(rows[0].os_raw, "Ubuntu");
    }

    #[test]
    fn remove_name_reports_whether_record_changed() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        add_cmd(
            &ctx,
            "lab".to_string(),
            "alice@host".to_string(),
            Some("/home/alice/vbox".to_string()),
            "linux".to_string(),
            String::new(),
        )
        .unwrap();
        assert!(remove_name(&ctx, "lab").unwrap());
        assert!(!remove_name(&ctx, "lab").unwrap());
    }
}
