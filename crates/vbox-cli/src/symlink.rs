use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::brand;
use crate::context::AppContext;
use crate::process;

pub(crate) fn install(ctx: &AppContext, dir: Option<PathBuf>, force: bool) -> Result<()> {
    let dir = dir.unwrap_or_else(default_install_dir);
    if !ctx.cli_path.is_file() {
        let mut cmd = process::command("cargo");
        cmd.current_dir(&ctx.root)
            .args(["build", "--release", "-p", "vbox-cli"]);
        process::run(cmd)?;
    }

    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    install_link(&dir.join("vbox"), &ctx.cli_path, force, "cli")?;

    if !path_contains(&dir) {
        eprintln!("[vbox] PATH does not include {}", dir.display());
        eprintln!(
            "[vbox] add this to your shell profile: export PATH=\"{}:$PATH\"",
            dir.display()
        );
    }
    Ok(())
}

fn install_link(link: &Path, target: &Path, force: bool, label: &str) -> Result<()> {
    if link.exists() || link.symlink_metadata().is_ok() {
        if resolves_to(link, target) {
            eprintln!(
                "[vbox] {label} already installed: {} -> {}",
                link.display(),
                target.display()
            );
        } else if force {
            let meta = fs::symlink_metadata(&link)
                .with_context(|| format!("inspect existing {}", link.display()))?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                bail!("refusing to replace directory: {}", link.display());
            }
            fs::remove_file(&link).with_context(|| format!("remove {}", link.display()))?;
            symlink(target, link)
                .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))?;
            eprintln!(
                "[vbox] replaced {label} symlink: {} -> {}",
                link.display(),
                target.display()
            );
        } else {
            bail!(
                "{} already exists and does not point to {}; use --force to replace it",
                link.display(),
                target.display()
            );
        }
    } else {
        symlink(target, link)
            .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))?;
        eprintln!(
            "[vbox] installed {label}: {} -> {}",
            link.display(),
            target.display()
        );
    }
    Ok(())
}

pub(crate) fn uninstall(ctx: &AppContext, dir: Option<PathBuf>) -> Result<()> {
    let dir = dir.unwrap_or_else(default_install_dir);
    let link = dir.join("vbox");
    if !remove_link_if_owned(&link, &ctx.cli_path)? {
        eprintln!("[vbox] cli not installed: {}", dir.display());
    }
    Ok(())
}

fn remove_link_if_owned(link: &Path, target: &Path) -> Result<bool> {
    if !link.exists() && link.symlink_metadata().is_err() {
        return Ok(false);
    }
    let meta =
        fs::symlink_metadata(&link).with_context(|| format!("inspect {}", link.display()))?;
    if !meta.file_type().is_symlink() {
        bail!("refusing to remove non-symlink: {}", link.display());
    }
    if !resolves_to(link, target) {
        let current_target = fs::read_link(link)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".to_string());
        bail!(
            "refusing to remove symlink not owned by this checkout: {} -> {}",
            link.display(),
            current_target
        );
    }
    fs::remove_file(&link).with_context(|| format!("remove {}", link.display()))?;
    eprintln!("[vbox] removed cli: {}", link.display());
    Ok(true)
}

fn default_install_dir() -> PathBuf {
    brand::env_os("VBOX_CLI_INSTALL_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolves_to(link: &Path, target: &Path) -> bool {
    let Ok(link_real) = link.canonicalize() else {
        return false;
    };
    let Ok(target_real) = target.canonicalize() else {
        return false;
    };
    link_real == target_real
}

fn path_contains(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p == dir))
        .unwrap_or(false)
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
            "vbox-symlink-test-{}-{}",
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
        let cli_path = dir.path.join("target/release/vbox");
        fs::create_dir_all(cli_path.parent().unwrap()).unwrap();
        fs::write(&cli_path, b"fake cli").unwrap();
        AppContext {
            root: dir.path.clone(),
            state_dir: dir.path.join(".vbox"),
            cli_path,
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
    fn resolves_to_requires_both_paths_to_exist_and_match() {
        let dir = tempdir_for_test();
        let target = dir.path.join("target");
        let link = dir.path.join("link");
        assert!(!resolves_to(&link, &target));
        fs::write(&target, b"target").unwrap();
        symlink(&target, &link).unwrap();
        assert!(resolves_to(&link, &target));
    }

    #[test]
    fn install_and_uninstall_manage_owned_symlink() {
        let _guard = test_env::lock();
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let install_dir = dir.path.join("bin");
        test_env::set_var("PATH", &install_dir);

        install(&ctx, Some(install_dir.clone()), false).unwrap();
        let link = install_dir.join("vbox");
        assert!(resolves_to(&link, &ctx.cli_path));
        install(&ctx, Some(install_dir.clone()), false).unwrap();

        uninstall(&ctx, Some(install_dir.clone())).unwrap();
        assert!(!link.exists());
        test_env::remove_var("PATH");
    }

    #[test]
    fn install_refuses_to_replace_directory_even_with_force() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let install_dir = dir.path.join("bin");
        fs::create_dir_all(install_dir.join("vbox")).unwrap();
        let err = install(&ctx, Some(install_dir), true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("refusing to replace directory"),
            "err was: {err}"
        );
    }

    #[test]
    fn path_contains_checks_split_path_entries() {
        let _guard = test_env::lock();
        let dir = tempdir_for_test();
        let first = dir.path.join("one");
        let second = dir.path.join("two");
        let joined = std::env::join_paths([first.as_path(), second.as_path()]).unwrap();
        test_env::set_var("PATH", joined);
        assert!(path_contains(&second));
        assert!(!path_contains(&dir.path.join("missing")));
        test_env::remove_var("PATH");
    }
}
