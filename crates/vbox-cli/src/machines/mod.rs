use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::context::AppContext;
use crate::remote;
use crate::{MachineListArgs, MachineStatus, MachinesArgs, MachinesCommand};

mod kind;
mod password;

pub(crate) use kind::MachineKind;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MachineRecord {
    pub(crate) uuid: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) ip: String,
    pub(crate) os_raw: String,
    pub(crate) os_kind: String,
    pub(crate) ssh_user: String,
    pub(crate) ssh_host: String,
    pub(crate) guest_dir: String,
    #[serde(default)]
    pub(crate) identity_file: String,
    #[serde(default)]
    pub(crate) has_password: bool,
}

pub(crate) fn run(ctx: &AppContext, args: MachinesArgs) -> Result<()> {
    match args.command {
        Some(MachinesCommand::List(list)) => list_cmd(ctx, list),
        Some(MachinesCommand::Info { target }) => info_cmd(ctx, &target),
        Some(MachinesCommand::Start { target }) => control_cmd(ctx, &target, "start"),
        Some(MachinesCommand::Stop { target }) => control_cmd(ctx, &target, "stop"),
        Some(MachinesCommand::Delete(args)) => delete_cmd(ctx, &args.target),
        Some(MachinesCommand::Config(args)) => config_cmd(ctx, &args.target, args.json),
        Some(MachinesCommand::Set { target, key, value }) => set_cmd(ctx, &target, &key, &value),
        Some(MachinesCommand::Unset { target, key }) => unset_cmd(ctx, &target, &key),
        None => list_cmd(ctx, args.list),
    }
}

fn list_cmd(ctx: &AppContext, args: MachineListArgs) -> Result<()> {
    let mut rows = records(ctx)?;
    let status = resolve_status(&args);
    if status != "all" {
        rows.retain(|row| row.status == status);
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if args.tsv {
        for row in rows {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                row.uuid,
                row.name,
                row.status,
                row.ip,
                row.os_raw,
                row.os_kind,
                row.ssh_user,
                row.ssh_host,
                row.guest_dir
            );
        }
    } else {
        println!(
            "{:<36}  {:<8}  {:<15}  {:<7}  NAME",
            "UUID", "STATUS", "IP", "OS"
        );
        println!(
            "{:<36}  {:<8}  {:<15}  {:<7}  ----",
            "----", "------", "--", "--"
        );
        for row in rows {
            println!(
                "{:<36}  {:<8}  {:<15}  {:<7}  {}",
                row.uuid, row.status, row.ip, row.os_kind, row.name
            );
        }
    }
    Ok(())
}

fn info_cmd(ctx: &AppContext, target: &str) -> Result<()> {
    let row = find_one(ctx, target)?;
    println!("uuid:      {}", row.uuid);
    println!("name:      {}", row.name);
    println!("status:    {}", row.status);
    println!("ip:        {}", row.ip);
    println!("os:        {} ({})", row.os_raw, row.os_kind);
    println!("ssh_user:  {}", row.ssh_user);
    println!("ssh_host:  {}", row.ssh_host);
    println!("guest_dir: {}", row.guest_dir);
    Ok(())
}

fn control_cmd(ctx: &AppContext, target: &str, action: &str) -> Result<()> {
    let row = find_one(ctx, target)?;
    if matches!(MachineKind::from_record(&row), MachineKind::Remote { .. }) {
        bail!(
            "remote machines cannot be controlled with prlctl: {}",
            row.name
        );
    }
    let status = Command::new("prlctl")
        .arg(action)
        .arg(format!("{{{}}}", row.uuid))
        .status()
        .with_context(|| format!("run prlctl {action} {}", row.uuid))?;
    if !status.success() {
        bail!("prlctl {action} failed for {}", row.name);
    }
    Ok(())
}

fn delete_cmd(ctx: &AppContext, target: &str) -> Result<()> {
    let row = find_one(ctx, target)?;
    if matches!(MachineKind::from_record(&row), MachineKind::Remote { .. }) {
        if remote::remove_name(ctx, &row.name)? {
            println!("removed remote: {}", row.name);
            return Ok(());
        }
        bail!("remote not found: {}", row.name);
    }
    if row.status == "running" {
        bail!("stop machine before deleting: {}", row.name);
    }
    let status = Command::new("prlctl")
        .args(["delete", &format!("{{{}}}", row.uuid)])
        .status()
        .with_context(|| format!("run prlctl delete {}", row.uuid))?;
    if !status.success() {
        bail!("prlctl delete failed for {}", row.name);
    }
    drop_override(ctx, &row.uuid)?;
    println!("deleted machine: {}", row.name);
    Ok(())
}

fn config_cmd(ctx: &AppContext, target: &str, as_json: bool) -> Result<()> {
    let row = find_one(ctx, target)?;
    let overrides = load_overrides(ctx)?;
    let mut value = overrides
        .get(&row.uuid)
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(map) = value.as_object_mut()
        && matches!(MachineKind::from_record(&row), MachineKind::Remote { .. })
    {
        if !map.contains_key("identity_file") && !row.identity_file.is_empty() {
            map.insert(
                "identity_file".to_string(),
                Value::String(row.identity_file.clone()),
            );
        }
        if !map.contains_key("has_password") && row.has_password {
            map.insert("has_password".to_string(), Value::Bool(true));
        }
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if let Some(map) = value.as_object() {
        for (key, value) in map {
            println!("{key}={}", printable_json_value(value));
        }
    }
    Ok(())
}

fn set_cmd(ctx: &AppContext, target: &str, key: &str, value: &str) -> Result<()> {
    validate_config_key(key)?;
    let row = find_one(ctx, target)?;
    if key == "password" {
        return password::set(ctx, &row, value);
    }
    let mut overrides = load_overrides(ctx)?;
    let entry = overrides
        .entry(row.uuid)
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(map) = entry.as_object_mut() else {
        bail!("override record is not an object");
    };
    map.insert(key.to_string(), Value::String(value.to_string()));
    save_overrides(ctx, &overrides)
}

fn unset_cmd(ctx: &AppContext, target: &str, key: &str) -> Result<()> {
    validate_config_key(key)?;
    let row = find_one(ctx, target)?;
    if key == "password" {
        return password::clear(ctx, &row);
    }
    let mut overrides = load_overrides(ctx)?;
    if let Some(value) = overrides.get_mut(&row.uuid).and_then(Value::as_object_mut) {
        value.remove(key);
    }
    save_overrides(ctx, &overrides)
}

fn find_one(ctx: &AppContext, target: &str) -> Result<MachineRecord> {
    records(ctx)?
        .into_iter()
        .find(|row| row.uuid == target || row.name == target)
        .with_context(|| format!("machine not found: {target}"))
}

pub(crate) fn find_by_ssh_target(ctx: &AppContext, ssh: &str) -> Result<Option<MachineRecord>> {
    Ok(records(ctx)?
        .into_iter()
        .find(|row| format!("{}@{}", row.ssh_user, row.ssh_host) == ssh))
}

pub(crate) fn records(ctx: &AppContext) -> Result<Vec<MachineRecord>> {
    let overrides = load_overrides(ctx)?;
    let mut out = Vec::new();
    let list_json = prlctl_json(["list", "--all", "--full", "--json"])?;
    let machines = list_json.as_array().cloned().unwrap_or_default();
    let default_user = default_user(ctx.guest.as_deref())?;
    let default_dir = default_guest_dir(ctx, &default_user);

    for machine in machines {
        let uuid_braced = machine
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let uuid = uuid_braced.trim_matches(['{', '}']).to_string();
        if uuid.is_empty() {
            continue;
        }
        let name = machine
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = machine
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let ip = machine
            .get("ip_configured")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
            .to_string();
        let info = prlctl_json(["list", "--info", "--json", uuid_braced.as_str()])
            .unwrap_or_else(|_| json!({}));
        let info_obj = info
            .as_array()
            .and_then(|items| items.first())
            .unwrap_or(&info);
        let os_raw = info_obj
            .get("OS")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let os_kind = classify_os(&os_raw, &name);
        let ov = overrides.get(&uuid).and_then(Value::as_object);
        let default_ssh_host = if ip == "-" { String::new() } else { ip.clone() };
        out.push(MachineRecord {
            uuid: uuid.clone(),
            name,
            status,
            ip,
            os_raw,
            os_kind,
            ssh_user: ov
                .and_then(|m| m.get("ssh_user"))
                .and_then(Value::as_str)
                .unwrap_or(&default_user)
                .to_string(),
            ssh_host: ov
                .and_then(|m| m.get("ssh_host"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(&default_ssh_host)
                .to_string(),
            guest_dir: ov
                .and_then(|m| m.get("guest_dir"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(&default_dir)
                .to_string(),
            identity_file: ov
                .and_then(|m| m.get("identity_file"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            has_password: ov
                .and_then(|m| m.get("has_password"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }

    for row in remote::records(ctx)? {
        out.push(MachineRecord {
            uuid: row.id,
            name: row.name,
            status: "remote".to_string(),
            ip: if row.ssh_host.is_empty() {
                "-".to_string()
            } else {
                row.ssh_host.clone()
            },
            os_raw: row.os_raw,
            os_kind: row.os_kind,
            ssh_user: row.ssh_user,
            ssh_host: row.ssh_host,
            guest_dir: row.guest_dir,
            identity_file: row.identity_file,
            has_password: row.has_password,
        });
    }
    Ok(out)
}


fn prlctl_json<const N: usize>(args: [&str; N]) -> Result<Value> {
    let Ok(output) = Command::new("prlctl").args(args).output() else {
        return Ok(json!([]));
    };
    if !output.status.success() {
        return Ok(json!([]));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!([])))
}

fn load_overrides(ctx: &AppContext) -> Result<BTreeMap<String, Value>> {
    let path = overrides_file(ctx);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

fn save_overrides(ctx: &AppContext, overrides: &BTreeMap<String, Value>) -> Result<()> {
    let path = overrides_file(ctx);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(overrides)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
}

fn drop_override(ctx: &AppContext, uuid: &str) -> Result<()> {
    let mut overrides = load_overrides(ctx)?;
    overrides.remove(uuid);
    save_overrides(ctx, &overrides)
}

fn overrides_file(ctx: &AppContext) -> PathBuf {
    ctx.state_dir.join("machines.json")
}

fn resolve_status(args: &MachineListArgs) -> &'static str {
    if args.running {
        "running"
    } else if args.stopped {
        "stopped"
    } else if args.invalid {
        "invalid"
    } else if args.all {
        "all"
    } else {
        match args.status {
            Some(MachineStatus::Running) => "running",
            Some(MachineStatus::Stopped) => "stopped",
            Some(MachineStatus::Suspended) => "suspended",
            Some(MachineStatus::Paused) => "paused",
            Some(MachineStatus::Invalid) => "invalid",
            Some(MachineStatus::Remote) => "remote",
            Some(MachineStatus::All) | None => "all",
        }
    }
}

fn default_user(guest: Option<&str>) -> Result<String> {
    if let Some((user, _)) = guest
        .and_then(|value| value.split_once('@'))
        .filter(|(user, _)| !user.is_empty())
    {
        return Ok(user.to_string());
    }
    if let Some(user) = host_user() {
        return Ok(user);
    }
    bail!(
        "could not infer default SSH user; use --guest USER@HOST, VBOX_GUEST, or `vbox machines set <target> ssh_user <user>`"
    )
}

fn default_guest_dir(ctx: &AppContext, user: &str) -> String {
    ctx.guest_dir
        .as_deref()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("/home/{user}/vbox"))
}

fn host_user() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty())
        .or_else(|| {
            std::env::var("LOGNAME")
                .ok()
                .filter(|user| !user.is_empty())
        })
}

fn classify_os(os_raw: &str, name: &str) -> String {
    let hay = format!("{os_raw} {name}").to_ascii_lowercase();
    if ["windows", "win-", "win10", "win11"]
        .iter()
        .any(|needle| hay.contains(needle))
    {
        "windows".to_string()
    } else if ["macos", "darwin", "osx"]
        .iter()
        .any(|needle| hay.contains(needle))
    {
        "macos".to_string()
    } else if [
        "fedora", "ubuntu", "debian", "linux", "centos", "rhel", "arch", "suse", "mint",
    ]
    .iter()
    .any(|needle| hay.contains(needle))
    {
        "linux".to_string()
    } else {
        "unknown".to_string()
    }
}

fn validate_config_key(key: &str) -> Result<()> {
    match key {
        "ssh_user" | "ssh_host" | "ssh_port" | "identity_file" | "guest_dir" | "port"
        | "socket" | "width" | "height" | "debug" | "tls" | "notes" | "password" => Ok(()),
        _ => bail!("unsupported config key: {key}"),
    }
}

fn printable_json_value(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AppContext;
    use crate::test_env;

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
            "vbox-machines-test-{}-{}",
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
    fn resolve_status_prefers_legacy_short_flags() {
        assert_eq!(
            resolve_status(&MachineListArgs {
                running: true,
                ..MachineListArgs::default()
            }),
            "running"
        );
        assert_eq!(
            resolve_status(&MachineListArgs {
                stopped: true,
                ..MachineListArgs::default()
            }),
            "stopped"
        );
        assert_eq!(
            resolve_status(&MachineListArgs {
                invalid: true,
                ..MachineListArgs::default()
            }),
            "invalid"
        );
        assert_eq!(
            resolve_status(&MachineListArgs {
                all: true,
                status: Some(MachineStatus::Running),
                ..MachineListArgs::default()
            }),
            "all"
        );
    }

    #[test]
    fn resolve_status_uses_enum_when_no_short_flag_is_set() {
        assert_eq!(
            resolve_status(&MachineListArgs {
                status: Some(MachineStatus::Remote),
                ..MachineListArgs::default()
            }),
            "remote"
        );
        assert_eq!(resolve_status(&MachineListArgs::default()), "all");
    }

    #[test]
    fn default_user_prefers_guest_user_before_environment() {
        let _guard = test_env::lock();
        test_env::set_var("USER", "envuser");
        assert_eq!(default_user(Some("guest@example.test")).unwrap(), "guest");
        test_env::remove_var("USER");
    }

    #[test]
    fn default_user_falls_back_to_login_env() {
        let _guard = test_env::lock();
        test_env::remove_var("USER");
        test_env::set_var("LOGNAME", "logname");
        assert_eq!(default_user(None).unwrap(), "logname");
        test_env::remove_var("LOGNAME");
    }

    #[test]
    fn classify_os_uses_raw_os_and_name_hints() {
        assert_eq!(classify_os("Windows 11", "dev"), "windows");
        assert_eq!(classify_os("", "macOS test"), "macos");
        assert_eq!(classify_os("Ubuntu Linux", "dev"), "linux");
        assert_eq!(classify_os("Solaris", "lab"), "unknown");
    }

    #[test]
    fn validate_config_key_accepts_documented_keys_only() {
        assert!(validate_config_key("guest_dir").is_ok());
        assert!(validate_config_key("notes").is_ok());
        assert!(validate_config_key("unknown").is_err());
    }

    #[test]
    fn overrides_round_trip_and_drop_single_uuid() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let mut overrides = BTreeMap::new();
        overrides.insert("uuid-1".to_string(), json!({"ssh_user":"alice"}));
        overrides.insert("uuid-2".to_string(), json!({"ssh_user":"bob"}));
        save_overrides(&ctx, &overrides).unwrap();
        assert_eq!(load_overrides(&ctx).unwrap(), overrides);

        drop_override(&ctx, "uuid-1").unwrap();
        let loaded = load_overrides(&ctx).unwrap();
        assert!(!loaded.contains_key("uuid-1"));
        assert_eq!(loaded.get("uuid-2"), Some(&json!({"ssh_user":"bob"})));
    }

    #[test]
    fn records_include_remote_hosts_when_prlctl_is_unavailable() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        fs::create_dir_all(&ctx.state_dir).unwrap();
        fs::write(
            ctx.state_dir.join("remote-hosts.json"),
            r#"{"hosts":[{"name":"Remote Lab","ssh":"alice@host.test","guest_dir":"/srv/vbox","os_kind":"linux","os_raw":"Ubuntu","added_at":"unix:1"}]}"#,
        )
        .unwrap();

        let rows = records(&ctx)
            .unwrap()
            .into_iter()
            .filter(|row| row.uuid.starts_with("remote:"))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].uuid, "remote:remote_lab");
        assert_eq!(rows[0].status, "remote");
        assert_eq!(rows[0].ssh_user, "alice");
        assert_eq!(rows[0].ssh_host, "host.test");
    }

    #[test]
    fn printable_json_value_keeps_strings_unquoted() {
        assert_eq!(printable_json_value(&json!("hello")), "hello");
        assert_eq!(printable_json_value(&json!(42)), "42");
    }
}
