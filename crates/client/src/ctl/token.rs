//! Shared-secret token loader for non-TLS control connections.

use anyhow::{Context, Result, bail};

/// Read the shared-secret token: VBOX_CONTROL_TOKEN env wins, else read from
/// the file at VBOX_CONTROL_TOKEN_FILE. Errors if neither is set.
pub(super) fn load_token() -> Result<String> {
    resolve_token(
        crate::brand::env_var("VBOX_CONTROL_TOKEN").as_deref(),
        crate::brand::env_var("VBOX_CONTROL_TOKEN_FILE").as_deref(),
        |path| std::fs::read_to_string(path).with_context(|| format!("read token file {path}")),
    )
}

/// Pure resolver: env value wins; else read the file path; else error.
/// The file-read step is injected so tests can pin every outcome without
/// touching the filesystem or process env.
fn resolve_token<F>(
    env_value: Option<&str>,
    file_path: Option<&str>,
    read_file: F,
) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    if let Some(v) = env_value {
        let t = v.trim().to_owned();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    if let Some(path) = file_path {
        let s = read_file(path)?;
        let t = s.trim().to_owned();
        if t.is_empty() {
            bail!("token file {path} is empty");
        }
        return Ok(t);
    }
    bail!("set VBOX_CONTROL_TOKEN or VBOX_CONTROL_TOKEN_FILE (run `vbox controld-install` first)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_read(_: &str) -> Result<String> {
        panic!("read_file should not be called when VBOX_CONTROL_TOKEN wins");
    }

    // Story: ops sets a token one of three ways and we must obey the
    // documented precedence. Each test pins one branch of `resolve_token`.

    #[test]
    fn env_var_wins_over_file_path() {
        let token = resolve_token(Some("hex-token"), Some("/etc/vbox/token"), never_read).unwrap();
        assert_eq!(token, "hex-token");
    }

    #[test]
    fn env_var_is_trimmed() {
        // A common .env mistake — trailing whitespace from copy-paste.
        let token = resolve_token(Some("  hex-token  \n"), None, never_read).unwrap();
        assert_eq!(token, "hex-token");
    }

    #[test]
    fn falls_back_to_file_when_env_blank() {
        // env var present but empty/whitespace → keep looking for the file.
        let token = resolve_token(Some("   "), Some("/etc/vbox/token"), |path| {
            assert_eq!(path, "/etc/vbox/token", "should pass the configured path");
            Ok("file-token\n".into())
        })
        .unwrap();
        assert_eq!(token, "file-token");
    }

    #[test]
    fn reads_from_file_when_env_unset() {
        let token = resolve_token(None, Some("/etc/vbox/token"), |path| {
            assert_eq!(path, "/etc/vbox/token");
            Ok("on-disk".into())
        })
        .unwrap();
        assert_eq!(token, "on-disk");
    }

    #[test]
    fn errors_when_file_is_blank() {
        let err = resolve_token(None, Some("/etc/vbox/token"), |_| Ok("\n  \n".into()))
            .expect_err("blank file must error");
        let msg = format!("{err}");
        assert!(msg.contains("token file"), "got: {msg}");
        assert!(msg.contains("/etc/vbox/token"));
        assert!(msg.contains("empty"));
    }

    #[test]
    fn errors_when_no_env_and_no_file() {
        let err = resolve_token(None, None, never_read).expect_err("neither set → error");
        let msg = format!("{err}");
        assert!(msg.contains("VBOX_CONTROL_TOKEN"));
        assert!(msg.contains("VBOX_CONTROL_TOKEN_FILE"));
    }

    #[test]
    fn propagates_read_file_error() {
        // The operator points at /etc/vbox/token but the file isn't
        // readable. The wrapping context must reach the user verbatim.
        let err = resolve_token(None, Some("/etc/vbox/token"), |_| {
            Err(anyhow::anyhow!("permission denied"))
        })
        .expect_err("read failure must propagate");
        assert!(format!("{err:#}").contains("permission denied"));
    }
}
