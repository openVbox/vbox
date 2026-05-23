use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::context::AppContext;
use crate::process;

pub(crate) fn open(ctx: &AppContext, app_cache: &Path, launcher_root: &Path) -> Result<()> {
    let app = build_main_library_app(ctx, app_cache, launcher_root)?;
    let mut cmd = process::command("/usr/bin/open");
    cmd.arg("-n").arg(app);
    process::run(cmd)
}

pub(crate) fn build_main_library_app(
    ctx: &AppContext,
    app_cache: &Path,
    launcher_root: &Path,
) -> Result<PathBuf> {
    if std::env::consts::OS != "macos" {
        bail!("library-ui is only supported on macOS");
    }

    let app = launcher_root.join("vbox.app");
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;

    let distro_icon_dir = crate::distro_icons::dir(ctx);
    if let Err(err) = crate::distro_icons::ensure(ctx) {
        eprintln!("[vbox] warning: distro icon cache refresh failed: {err:#}");
    }

    copy_best_icon(ctx, &resources.join("AppIcon.icns"));
    write_resource(&resources, "Root.txt", &ctx.root.display().to_string())?;
    write_resource(
        &resources,
        "StateDir.txt",
        &ctx.state_dir.display().to_string(),
    )?;
    write_resource(
        &resources,
        "LauncherDir.txt",
        &launcher_root.display().to_string(),
    )?;
    write_resource(&resources, "AppCache.txt", &app_cache.display().to_string())?;
    write_resource(
        &resources,
        "CliPath.txt",
        &ctx.cli_path.display().to_string(),
    )?;
    write_resource(
        &resources,
        "IconCacheDir.txt",
        &ctx.state_dir.join("icons").display().to_string(),
    )?;
    write_resource(
        &resources,
        "DistroIconDir.txt",
        &distro_icon_dir.display().to_string(),
    )?;
    write_resource(
        &resources,
        "Guest.txt",
        ctx.guest.as_deref().unwrap_or_default(),
    )?;
    write_resource(
        &resources,
        "GuestDir.txt",
        &ctx.guest_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    )?;
    write_resource(&resources, "Instance.txt", &ctx.instance)?;
    write_resource(&resources, "Port.txt", &ctx.port.to_string())?;
    write_resource(&resources, "Socket.txt", &ctx.socket)?;
    write_resource(&resources, "Width.txt", &ctx.width.to_string())?;
    write_resource(&resources, "Height.txt", &ctx.height.to_string())?;
    write_resource(&resources, "Suffix.txt", &launcher_suffix(ctx))?;

    let source = ctx.state_dir.join("run/VBoxLibrary.swift");
    if let Some(parent) = source.parent() {
        fs::create_dir_all(parent)?;
    }
    let source_changed = write_if_changed(&source, VBOX_LIBRARY_SWIFT)?;
    let binary = macos.join("VBoxLibrary");
    if source_changed || !binary.is_file() {
        let mut cmd = process::command("swiftc");
        cmd.arg("-parse-as-library")
            .arg("-O")
            .arg(&source)
            .arg("-o")
            .arg(&binary);
        process::run(cmd).context("build SwiftUI vbox library app")?;
        let _ = Command::new("chmod").arg("+x").arg(&binary).status();
    }

    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>VBoxLibrary</string>
  <key>CFBundleIdentifier</key><string>local.vbox.native.library</string>
  <key>CFBundleName</key><string>vbox</string>
  <key>CFBundleDisplayName</key><string>vbox</string>
  <key>CFBundleIconFile</key><string>{}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.1</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
            if resources.join("AppIcon.icns").is_file() {
                "AppIcon"
            } else {
                ""
            }
        ),
    )?;

    let _ = Command::new("/usr/bin/plutil")
        .arg("-lint")
        .arg(contents.join("Info.plist"))
        .status();
    let _ = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(&binary)
        .status();
    let _ = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(&app)
        .status();
    let _ = Command::new("/usr/bin/touch").arg(&app).status();
    let _ = Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
    .arg("-f")
    .arg(&app)
    .status();

    Ok(app)
}

fn write_resource(resources: &Path, name: &str, value: &str) -> Result<()> {
    fs::write(resources.join(name), format!("{value}\n"))?;
    Ok(())
}

fn write_if_changed(path: &Path, value: &str) -> Result<bool> {
    if fs::read_to_string(path).ok().as_deref() == Some(value) {
        return Ok(false);
    }
    fs::write(path, value)?;
    Ok(true)
}

fn copy_best_icon(ctx: &AppContext, out: &Path) {
    let mut candidates = vec![
        ctx.root.join("AppIcon.icns"),
        ctx.root.join(".vbox/AppIcon.icns"),
        ctx.state_dir.join("AppIcon.icns"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join("Documents/vbox-icon.icns"));
    }
    for candidate in candidates {
        if candidate.is_file() && fs::copy(candidate, out).is_ok() {
            return;
        }
    }
}

const VBOX_LIBRARY_SWIFT: &str =
    include_str!("../../../vbox-swift/Sources/VBoxLibrary/VBoxLibrary.swift");

fn launcher_suffix(ctx: &AppContext) -> String {
    if let Ok(value) = std::env::var("VBOX_LAUNCHER_SUFFIX") {
        return value;
    }
    fs::read_to_string(ctx.state_dir.join("launcher-suffix.txt"))
        .ok()
        .and_then(|value| value.lines().next().map(str::to_string))
        .unwrap_or_default()
}
