use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

use crate::context::AppContext;
use crate::machines;
use crate::remote;

#[derive(Debug, Clone, Default)]
pub(crate) struct SshCreds {
    pub(crate) identity_file: Option<PathBuf>,
    pub(crate) keychain_account: Option<String>,
}

/// IR for ssh/scp command-line options.
///
/// `flags` are `-x value` pairs and `env` are environment variables. The two
/// adapters (`apply_to_command` / `to_shell_string`) translate the IR into the
/// shape each call site needs; the option-decision logic lives only in
/// `SshCreds::build_options`.
#[derive(Debug, Default)]
struct SshOptions {
    flags: Vec<(&'static str, OsString)>,
    env: Vec<(&'static str, OsString)>,
}

impl SshOptions {
    fn apply_to_command(self, cmd: &mut Command) {
        for (key, value) in self.env {
            cmd.env(key, value);
        }
        for (flag, value) in self.flags {
            cmd.arg(flag).arg(value);
        }
    }

    fn to_shell_string(self, program: &str) -> String {
        let env_prefix = if self.env.is_empty() {
            String::new()
        } else {
            let joined = self
                .env
                .iter()
                .map(|(k, v)| format!("{k}={}", shell_quote(v)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{joined} ")
        };
        let mut opts = String::new();
        for (flag, value) in &self.flags {
            opts.push(' ');
            opts.push_str(flag);
            opts.push(' ');
            opts.push_str(&shell_quote(value));
        }
        format!("{env_prefix}{program}{opts}")
    }
}

impl SshCreds {
    pub(crate) fn from_context(ctx: &AppContext) -> Self {
        let Some(guest) = ctx.guest.as_deref().filter(|value| !value.is_empty()) else {
            return Self::default();
        };
        match remote::find_by_ssh_target(ctx, guest).ok().flatten() {
            Some(row) if !row.identity_file.is_empty() || row.has_password => Self {
                identity_file: option_path(&row.identity_file),
                keychain_account: row.has_password.then(|| row.id.clone()),
            },
            _ => match machines::find_by_ssh_target(ctx, guest).ok().flatten() {
                Some(row) if !row.identity_file.is_empty() || row.has_password => Self {
                    identity_file: option_path(&row.identity_file),
                    keychain_account: row.has_password.then(|| {
                        machines::MachineKind::from_record(&row).keychain_account()
                    }),
                },
                _ => Self::default(),
            },
        }
    }

    fn build_options(&self, askpass_helper: Option<&OsStr>) -> SshOptions {
        let mut opts = SshOptions::default();
        if let Some(path) = self.identity_file.as_deref() {
            opts.flags.push(("-i", path.as_os_str().to_owned()));
            opts.flags
                .push(("-o", OsString::from("IdentitiesOnly=yes")));
        }
        if let (Some(account), Some(helper)) =
            (self.keychain_account.as_deref(), askpass_helper)
        {
            opts.env.push(("SSH_ASKPASS", helper.to_owned()));
            opts.env
                .push(("SSH_ASKPASS_REQUIRE", OsString::from("force")));
            opts.env
                .push(("VBOX_ASKPASS_ACCOUNT", OsString::from(account)));
            if std::env::var_os("DISPLAY").is_none() {
                opts.env.push(("DISPLAY", OsString::from(":0")));
            }
            opts.flags.push((
                "-o",
                OsString::from(
                    "PreferredAuthentications=password,keyboard-interactive,publickey",
                ),
            ));
            opts.flags
                .push(("-o", OsString::from("NumberOfPasswordPrompts=1")));
            opts.flags
                .push(("-o", OsString::from("PubkeyAuthentication=no")));
        }
        opts
    }
}

fn option_path(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        None
    } else {
        Some(expand_tilde(value))
    }
}

fn expand_tilde(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = dirs_home()
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn askpass_helper(ctx: &AppContext, creds: &SshCreds) -> Option<OsString> {
    creds
        .keychain_account
        .as_ref()
        .map(|_| ctx.cli_path.as_os_str().to_owned())
}

pub(crate) fn ssh_command(ctx: &AppContext) -> Command {
    let mut cmd = Command::new("ssh");
    apply_creds(ctx, &mut cmd);
    cmd
}

pub(crate) fn scp_command(ctx: &AppContext) -> Command {
    let mut cmd = Command::new("scp");
    apply_creds(ctx, &mut cmd);
    cmd
}

fn apply_creds(ctx: &AppContext, cmd: &mut Command) {
    let creds = SshCreds::from_context(ctx);
    let helper = askpass_helper(ctx, &creds);
    creds.build_options(helper.as_deref()).apply_to_command(cmd);
}

pub(crate) fn inline_ssh(ctx: &AppContext) -> String {
    let creds = SshCreds::from_context(ctx);
    let helper = askpass_helper(ctx, &creds);
    creds.build_options(helper.as_deref()).to_shell_string("ssh")
}

fn shell_quote(value: &OsStr) -> String {
    let s = value.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub(crate) fn run_askpass() -> Result<()> {
    let account = std::env::var("VBOX_ASKPASS_ACCOUNT").unwrap_or_default();
    if account.is_empty() {
        return Ok(());
    }
    if let Some(password) = crate::keychain::get_password(&account)? {
        println!("{password}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag_pairs(opts: &SshOptions) -> Vec<(&'static str, String)> {
        opts.flags
            .iter()
            .map(|(k, v)| (*k, v.to_string_lossy().to_string()))
            .collect()
    }

    fn env_keys(opts: &SshOptions) -> Vec<&'static str> {
        opts.env.iter().map(|(k, _)| *k).collect()
    }

    #[test]
    fn empty_creds_produce_no_options() {
        let opts = SshCreds::default().build_options(None);
        assert!(opts.flags.is_empty());
        assert!(opts.env.is_empty());
    }

    #[test]
    fn identity_only_sets_i_flag_and_identities_only() {
        let creds = SshCreds {
            identity_file: Some(PathBuf::from("/path/to/key")),
            keychain_account: None,
        };
        let opts = creds.build_options(None);
        let pairs = flag_pairs(&opts);
        assert_eq!(
            pairs,
            vec![
                ("-i", "/path/to/key".to_string()),
                ("-o", "IdentitiesOnly=yes".to_string()),
            ]
        );
        assert!(opts.env.is_empty());
    }

    #[test]
    fn keychain_with_helper_sets_askpass_env_and_auth_flags() {
        let creds = SshCreds {
            identity_file: None,
            keychain_account: Some("remote:lab".to_string()),
        };
        let helper = OsString::from("/usr/local/bin/vbox");
        let opts = creds.build_options(Some(helper.as_os_str()));

        let keys = env_keys(&opts);
        assert!(keys.contains(&"SSH_ASKPASS"));
        assert!(keys.contains(&"SSH_ASKPASS_REQUIRE"));
        assert!(keys.contains(&"VBOX_ASKPASS_ACCOUNT"));

        let pairs = flag_pairs(&opts);
        assert!(pairs.iter().any(|(_, v)| v == "PubkeyAuthentication=no"));
        assert!(
            pairs
                .iter()
                .any(|(_, v)| v.starts_with("PreferredAuthentications="))
        );
        assert!(pairs.iter().any(|(_, v)| v == "NumberOfPasswordPrompts=1"));
    }

    #[test]
    fn keychain_without_helper_is_no_op() {
        let creds = SshCreds {
            identity_file: None,
            keychain_account: Some("remote:lab".to_string()),
        };
        let opts = creds.build_options(None);
        assert!(opts.flags.is_empty());
        assert!(opts.env.is_empty());
    }

    #[test]
    fn shell_string_quotes_identity_path_with_spaces() {
        let creds = SshCreds {
            identity_file: Some(PathBuf::from("/path with space/key")),
            keychain_account: None,
        };
        let s = creds.build_options(None).to_shell_string("ssh");
        assert!(
            s.contains("ssh -i '/path with space/key' -o 'IdentitiesOnly=yes'"),
            "got: {s}"
        );
    }

    #[test]
    fn shell_string_quotes_single_quotes_in_path() {
        let creds = SshCreds {
            identity_file: Some(PathBuf::from("/it's/here")),
            keychain_account: None,
        };
        let s = creds.build_options(None).to_shell_string("ssh");
        assert!(s.contains(r"-i '/it'\''s/here'"), "got: {s}");
    }
}
