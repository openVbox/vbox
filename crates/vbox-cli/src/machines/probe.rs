use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::MachineRecord;

/// `--probe` 결과. 정적 정보와 분리되어 있어 시간 비용 큰 항목만 들어간다.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProbeReport {
    pub(crate) ssh: SshReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) guest: Option<GuestReport>,
    pub(crate) probed_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SshReport {
    pub(crate) reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtt_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GuestReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uptime: Option<String>,
}

pub(crate) fn probe(record: &MachineRecord) -> ProbeReport {
    let ssh = probe_ssh(record);
    let guest = if ssh.reachable {
        probe_guest(record)
    } else {
        None
    };
    ProbeReport {
        ssh,
        guest,
        probed_at: now_unix(),
    }
}

/// SSH 도달성 + RTT. BatchMode=yes 로 인터랙티브 인증을 거부 — probe는
/// "키/SSH agent 로 자동 접속 되는가?" 를 묻는다. 비밀번호 필요한
/// 호스트는 reachable=false + error="Permission denied" 로 나오므로
/// 사용자가 키체인 비밀번호 설정 여부를 알 수 있다.
fn probe_ssh(record: &MachineRecord) -> SshReport {
    let target = format!("{}@{}", record.ssh_user, record.ssh_host);
    let start = Instant::now();
    let output = Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=3",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
        ])
        .arg(&target)
        .arg("true")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(out) if out.status.success() => SshReport {
            reachable: true,
            rtt_ms: Some(start.elapsed().as_millis() as u64),
            error: None,
        },
        Ok(out) => SshReport {
            reachable: false,
            rtt_ms: None,
            error: Some(last_meaningful_line(&out.stderr)),
        },
        Err(e) => SshReport {
            reachable: false,
            rtt_ms: None,
            error: Some(e.to_string()),
        },
    }
}

fn probe_guest(record: &MachineRecord) -> Option<GuestReport> {
    let target = format!("{}@{}", record.ssh_user, record.ssh_host);
    let output = Command::new("ssh")
        .args(["-o", "ConnectTimeout=3", "-o", "BatchMode=yes"])
        .arg(&target)
        .arg("uptime")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(GuestReport {
        uptime: if text.is_empty() { None } else { Some(text) },
    })
}

fn last_meaningful_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or("")
        .to_string()
}

fn now_unix() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_meaningful_line_skips_blank_and_returns_final_message() {
        let bytes = b"\nLoading...\nPermission denied (publickey).\n\n";
        assert_eq!(
            last_meaningful_line(bytes),
            "Permission denied (publickey)."
        );
    }

    #[test]
    fn last_meaningful_line_handles_empty_input() {
        assert_eq!(last_meaningful_line(b""), "");
    }
}
