use std::ffi::OsStr;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub(crate) fn run(mut cmd: Command) -> Result<()> {
    let display = format!("{cmd:?}");
    let status = cmd
        .status()
        .with_context(|| format!("spawn command {display}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("command failed ({status}): {display}")
    }
}

pub(crate) fn output(mut cmd: Command) -> Result<String> {
    let display = format!("{cmd:?}");
    let out = cmd
        .output()
        .with_context(|| format!("spawn command {display}"))?;
    if !out.status.success() {
        bail!("command failed ({}): {display}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

pub(crate) fn shell(script: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(script);
    cmd
}

pub(crate) fn piped_shell(script: &str) -> Command {
    let mut cmd = shell(script);
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_constructs_requested_program() {
        let cmd = command("printf");
        assert_eq!(cmd.get_program(), OsStr::new("printf"));
    }

    #[test]
    fn shell_wraps_script_with_bash_lc() {
        let cmd = shell("printf ok");
        let args = cmd.get_args().collect::<Vec<_>>();
        assert_eq!(cmd.get_program(), OsStr::new("bash"));
        assert_eq!(args, vec![OsStr::new("-lc"), OsStr::new("printf ok")]);
    }

    #[test]
    fn output_returns_stdout_for_successful_command() {
        let mut cmd = command("printf");
        cmd.arg("hello");
        assert_eq!(output(cmd).unwrap(), "hello");
    }

    #[test]
    fn run_surfaces_non_zero_status() {
        let err = run(shell("exit 7")).unwrap_err().to_string();
        assert!(err.contains("command failed"), "err was: {err}");
    }

    #[test]
    fn output_surfaces_non_zero_status() {
        let err = output(shell("exit 9")).unwrap_err().to_string();
        assert!(err.contains("command failed"), "err was: {err}");
    }
}
