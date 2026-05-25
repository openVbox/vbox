#[cfg(target_os = "macos")]
use {
    anyhow::Context,
    std::{
        io::Write,
        process::{Command, Stdio},
    },
};

use anyhow::{Result, bail};

#[cfg(target_os = "macos")]
const SERVICE: &str = "vbox-remote";

#[cfg(target_os = "macos")]
pub(crate) fn set_password(account: &str, password: &str) -> Result<()> {
    let mut child = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            SERVICE,
            "-a",
            account,
            "-w",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn `security add-generic-password`")?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .context("open stdin to security command")?;
        stdin
            .write_all(password.as_bytes())
            .context("write password to security stdin")?;
    }
    let output = child
        .wait_with_output()
        .context("wait for security command")?;
    if !output.status.success() {
        bail!(
            "keychain store failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn get_password(account: &str) -> Result<Option<String>> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", SERVICE, "-a", account, "-w"])
        .output()
        .context("spawn `security find-generic-password`")?;
    if !output.status.success() {
        return Ok(None);
    }
    let mut text = String::from_utf8(output.stdout).context("decode keychain output")?;
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(Some(text))
}

#[cfg(target_os = "macos")]
pub(crate) fn delete_password(account: &str) -> Result<()> {
    let status = Command::new("security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", account])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("spawn `security delete-generic-password`")?;
    let _ = status;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_password(_account: &str, _password: &str) -> Result<()> {
    bail!("password-based SSH auth is only supported on macOS hosts")
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn get_password(_account: &str) -> Result<Option<String>> {
    Ok(None)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn delete_password(_account: &str) -> Result<()> {
    Ok(())
}
