use anyhow::Result;
use serde_json::Value;

use super::MachineInfo;

/// Stable plain-text format. Existing 9 lines are preserved so that
/// scripts piping `vbox machines info <target> | grep ssh_host` keep
/// working; new data goes into optional sections below.
pub(crate) fn render_text(info: &MachineInfo) -> String {
    let r = &info.record;
    let mut out = String::new();
    out.push_str(&format!("uuid:      {}\n", r.uuid));
    out.push_str(&format!("name:      {}\n", r.name));
    out.push_str(&format!("status:    {}\n", r.status));
    out.push_str(&format!("ip:        {}\n", r.ip));
    out.push_str(&format!("os:        {} ({})\n", r.os_raw, r.os_kind));
    out.push_str(&format!("ssh_user:  {}\n", r.ssh_user));
    out.push_str(&format!("ssh_host:  {}\n", r.ssh_host));
    out.push_str(&format!("guest_dir: {}\n", r.guest_dir));

    if !r.identity_file.is_empty() {
        out.push_str(&format!("identity:  {}\n", r.identity_file));
    }
    if r.has_password {
        out.push_str("password:  (saved in Keychain)\n");
    }

    if !info.overrides.is_empty() {
        out.push_str("\noverrides:\n");
        for (key, value) in &info.overrides {
            out.push_str(&format!("  {key} = {}\n", display_value(value)));
        }
    }

    if let Some(p) = &info.probe {
        out.push_str("\nprobe:\n");
        out.push_str(&format!("  ssh.reachable = {}\n", p.ssh.reachable));
        if let Some(rtt) = p.ssh.rtt_ms {
            out.push_str(&format!("  ssh.rtt_ms    = {rtt}\n"));
        }
        if let Some(err) = &p.ssh.error {
            out.push_str(&format!("  ssh.error     = {err}\n"));
        }
        if let Some(g) = &p.guest
            && let Some(uptime) = &g.uptime
        {
            out.push_str(&format!("  uptime        = {uptime}\n"));
        }
        out.push_str(&format!("  probed_at     = {}\n", p.probed_at));
    }
    out
}

pub(crate) fn render_json(info: &MachineInfo) -> Result<String> {
    Ok(serde_json::to_string_pretty(info)?)
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineRecord;
    use std::collections::BTreeMap;

    fn record() -> MachineRecord {
        MachineRecord {
            uuid: "abc".to_string(),
            name: "VM".to_string(),
            status: "stopped".to_string(),
            ip: "-".to_string(),
            os_raw: "Ubuntu".to_string(),
            os_kind: "linux".to_string(),
            ssh_user: "alice".to_string(),
            ssh_host: "host".to_string(),
            guest_dir: "/home/alice/vbox".to_string(),
            identity_file: String::new(),
            has_password: false,
        }
    }

    fn info_with(record: MachineRecord, overrides: BTreeMap<String, Value>) -> MachineInfo {
        MachineInfo {
            record,
            overrides,
            probe: None,
        }
    }

    #[test]
    fn legacy_nine_lines_are_preserved_when_no_extras() {
        let info = info_with(record(), BTreeMap::new());
        let out = render_text(&info);
        assert_eq!(out.lines().count(), 8);
        assert!(out.starts_with("uuid:      abc"));
        assert!(out.ends_with("guest_dir: /home/alice/vbox\n"));
        assert!(!out.contains("identity:"));
        assert!(!out.contains("password:"));
        assert!(!out.contains("overrides:"));
    }

    #[test]
    fn identity_and_password_lines_appear_only_when_set() {
        let mut rec = record();
        rec.identity_file = "/home/alice/.ssh/id_ed25519".to_string();
        rec.has_password = true;
        let info = info_with(rec, BTreeMap::new());
        let out = render_text(&info);
        assert!(out.contains("identity:  /home/alice/.ssh/id_ed25519"));
        assert!(out.contains("password:  (saved in Keychain)"));
    }

    #[test]
    fn overrides_section_renders_simple_key_value_pairs() {
        let mut overrides = BTreeMap::new();
        overrides.insert("notes".to_string(), Value::String("test".to_string()));
        overrides.insert("port".to_string(), Value::String("5710".to_string()));
        let info = info_with(record(), overrides);
        let out = render_text(&info);
        assert!(out.contains("overrides:"));
        assert!(out.contains("  notes = test"));
        assert!(out.contains("  port = 5710"));
    }

    #[test]
    fn json_round_trips_record_and_overrides_under_named_keys() {
        let mut overrides = BTreeMap::new();
        overrides.insert("notes".to_string(), Value::String("hi".to_string()));
        let info = info_with(record(), overrides);
        let out = render_json(&info).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["record"]["uuid"], "abc");
        assert_eq!(parsed["overrides"]["notes"], "hi");
    }
}
