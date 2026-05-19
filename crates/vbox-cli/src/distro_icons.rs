use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::brand;
use crate::context::AppContext;
use crate::{DistroIconsArgs, DistroIconsCommand};

const SOURCES: &[(&str, &str)] = &[
    (
        "fedora",
        "https://commons.wikimedia.org/wiki/Special:FilePath/Fedora_icon_(2021).svg",
    ),
    (
        "ubuntu",
        "https://upload.wikimedia.org/wikipedia/commons/9/9e/UbuntuCoF.svg",
    ),
    (
        "debian",
        "https://upload.wikimedia.org/wikipedia/commons/6/66/Openlogo-debianV2.svg",
    ),
    (
        "arch",
        "https://raw.githubusercontent.com/devicons/devicon/master/icons/archlinux/archlinux-original.svg",
    ),
    (
        "centos",
        "https://upload.wikimedia.org/wikipedia/commons/6/63/CentOS_color_logo.svg",
    ),
    (
        "mint",
        "https://commons.wikimedia.org/wiki/Special:FilePath/Linux_Mint_logo_without_wordmark.svg",
    ),
    (
        "opensuse",
        "https://raw.githubusercontent.com/devicons/devicon/master/icons/opensuse/opensuse-original.svg",
    ),
    (
        "tux",
        "https://upload.wikimedia.org/wikipedia/commons/3/35/Tux.svg",
    ),
];

pub(crate) fn run(ctx: &AppContext, args: DistroIconsArgs) -> Result<()> {
    match args.command {
        Some(DistroIconsCommand::Fetch(args)) => fetch(ctx, args.refresh),
        Some(DistroIconsCommand::Refresh) => fetch(ctx, true),
        Some(DistroIconsCommand::List) => list(ctx),
        Some(DistroIconsCommand::Dir) => {
            println!("{}", dir(ctx).display());
            Ok(())
        }
        None if args.refresh => fetch(ctx, true),
        None => list(ctx),
    }
}

pub(crate) fn ensure(ctx: &AppContext) -> Result<()> {
    let dir = dir(ctx);
    let cache_is_complete = SOURCES.iter().all(|(id, _)| {
        let path = dir.join(format!("{id}.png"));
        path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
    });
    if cache_is_complete {
        return Ok(());
    }
    fetch(ctx, false)
}

fn fetch(ctx: &AppContext, refresh: bool) -> Result<()> {
    let dir = dir(ctx);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let mut fetched = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for (id, url) in SOURCES {
        let svg = dir.join(format!(".{id}.svg"));
        let svg_tmp = dir.join(format!(".{id}.svg.tmp"));
        let png = dir.join(format!("{id}.png"));
        let png_tmp = dir.join(format!("{id}.png.tmp"));
        if !refresh && png.is_file() && png.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            skipped += 1;
            continue;
        }
        if !download(url, &svg_tmp) {
            let _ = fs::remove_file(&svg_tmp);
            failed += 1;
            eprintln!("distro-icons: download failed {id}");
            continue;
        }
        fs::rename(&svg_tmp, &svg).with_context(|| format!("rename {}", svg.display()))?;
        if convert_svg_to_png(&svg, &png_tmp) {
            fs::rename(&png_tmp, &png).with_context(|| format!("rename {}", png.display()))?;
            let _ = fs::remove_file(&svg);
            fetched += 1;
        } else {
            let _ = fs::remove_file(&svg);
            let _ = fs::remove_file(&png_tmp);
            failed += 1;
            eprintln!("distro-icons: convert failed {id} (rsvg-convert/qlmanage missing?)");
        }
    }

    eprintln!(
        "distro-icons: fetched={fetched} skipped={skipped} failed={failed} (cache={})",
        dir.display()
    );
    if fetched == 0 && failed > 0 && skipped == 0 {
        bail!("distro icon fetch failed")
    }
    Ok(())
}

fn list(ctx: &AppContext) -> Result<()> {
    let dir = dir(ctx);
    if !dir.is_dir() {
        return Ok(());
    }
    for (id, _) in SOURCES {
        let path = dir.join(format!("{id}.png"));
        if path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            println!("{id}\t{}", path.display());
        }
    }
    Ok(())
}

pub(crate) fn dir(ctx: &AppContext) -> PathBuf {
    brand::env_os("VBOX_DISTRO_ICON_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.state_dir.join("distro-icons"))
}

fn download(url: &str, out: &Path) -> bool {
    std::process::Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "15",
            "-A",
            "vbox-distro-icon-fetcher/0.1",
            "-o",
        ])
        .arg(out)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn convert_svg_to_png(svg: &Path, png: &Path) -> bool {
    if std::process::Command::new("rsvg-convert")
        .args(["-w", "256", "-h", "256", "-a", "-o"])
        .arg(png)
        .arg(svg)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }

    let Some(parent) = png.parent() else {
        return false;
    };
    let tmp_dir = parent.join(".qlmanage-tmp");
    let _ = fs::remove_dir_all(&tmp_dir);
    if fs::create_dir_all(&tmp_dir).is_err() {
        return false;
    }
    let ok = std::process::Command::new("qlmanage")
        .args(["-t", "-s", "256", "-o"])
        .arg(&tmp_dir)
        .arg(svg)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        let generated = tmp_dir.join(format!(
            "{}.png",
            svg.file_name().unwrap_or_default().to_string_lossy()
        ));
        if generated.is_file() && fs::rename(&generated, png).is_ok() {
            let _ = fs::remove_dir_all(&tmp_dir);
            return true;
        }
    }
    let _ = fs::remove_dir_all(&tmp_dir);
    false
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
            "vbox-distro-icons-test-{}-{}",
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
    fn dir_prefers_explicit_environment_override() {
        let _guard = test_env::lock();
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let icons = dir.path.join("icons");
        test_env::set_var("VBOX_DISTRO_ICON_DIR", &icons);
        assert_eq!(super::dir(&ctx), icons);
        test_env::remove_var("VBOX_DISTRO_ICON_DIR");
    }

    #[test]
    fn list_is_ok_when_cache_directory_is_missing() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        assert!(list(&ctx).is_ok());
    }

    #[test]
    fn fetch_skips_existing_non_empty_pngs_without_network() {
        let dir = tempdir_for_test();
        let ctx = ctx(&dir);
        let icon_dir = super::dir(&ctx);
        fs::create_dir_all(&icon_dir).unwrap();
        for (id, _) in SOURCES {
            fs::write(icon_dir.join(format!("{id}.png")), b"png").unwrap();
        }
        fetch(&ctx, false).unwrap();
    }
}
