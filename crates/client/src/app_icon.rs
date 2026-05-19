//! Resolve and apply per-app icons for the macOS viewer.
//!
//! `xdg_toplevel.app_id` arrives from the guest. The host already caches the
//! corresponding Linux app icon under `$VBOX_ICON_DIR/<sanitized-app-id>.<ext>`
//! (see `vbox` `fetch_guest_icon`). On macOS we load the bytes once per
//! app_id and stamp them onto the running `NSApplication` so the Dock and
//! `Cmd-Tab` switcher display the guest app instead of the vbox viewer.

use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use winit::window::Icon;

static CONFIGURED_ICON_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub(crate) fn set_icon_dir(dir: PathBuf) {
    if let Ok(mut configured) = CONFIGURED_ICON_DIR.lock() {
        *configured = Some(dir);
    }
}

/// Match `sanitize_id` in the host `vbox` CLI: keep alnum/`.`/`_`/`-`,
/// fold every other byte to `-`, then trim and collapse runs of `-` so we
/// share the cache path the CLI writes.
pub(crate) fn sanitize_app_id(app_id: &str) -> String {
    let mut out = String::with_capacity(app_id.len());
    let mut last_was_dash = true;
    for ch in app_id.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            ch
        } else {
            '-'
        };
        if mapped == '-' {
            if !last_was_dash {
                out.push('-');
                last_was_dash = true;
            }
        } else {
            out.push(mapped);
            last_was_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn icon_dir() -> Option<PathBuf> {
    CONFIGURED_ICON_DIR
        .lock()
        .ok()
        .and_then(|configured| configured.clone())
        .or_else(|| crate::brand::env_os("VBOX_ICON_DIR").map(PathBuf::from))
}

fn find_icon_file(app_id: &str) -> Option<PathBuf> {
    let dir = icon_dir()?;
    find_icon_file_in(&dir, app_id, |p| p.is_file())
}

/// Pure search for a cached icon under `dir` matching `app_id`. The
/// `exists` predicate is injected so tests can drive the lookup without
/// writing real files. Mirrors `find_icon_file` but with explicit IO.
fn find_icon_file_in(
    dir: &std::path::Path,
    app_id: &str,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<PathBuf> {
    if app_id.is_empty() {
        return None;
    }
    let safe = sanitize_app_id(app_id);
    if safe.is_empty() {
        return None;
    }
    for ext in ["png", "jpg", "jpeg", "tiff", "tif", "icns"] {
        let candidate = dir.join(format!("{safe}.{ext}"));
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

struct DecodedIcon {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

fn decode_png(path: &std::path::Path) -> Option<DecodedIcon> {
    let file = fs::File::open(path).ok()?;
    let mut reader = png::Decoder::new(file).read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let (width, height) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(buf.len() / 3 * 4);
            for chunk in buf.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(0xff);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(buf.len() * 2);
            for chunk in buf.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(buf.len() * 4);
            for &g in &buf {
                out.extend_from_slice(&[g, g, g, 0xff]);
            }
            out
        }
        png::ColorType::Indexed => return None,
    };
    Some(DecodedIcon {
        rgba,
        width,
        height,
    })
}

/// Per-process cache keyed by app_id. Decoding a PNG and minting an NSImage is
/// cheap, but apps can map a flood of toplevels (popups, secondary windows)
/// for the same app_id and we don't want to re-stamp the Dock each time.
#[derive(Default)]
pub struct AppIconCache {
    last_applied: Option<String>,
}

impl AppIconCache {
    pub fn apply_for(&mut self, app_id: &str) -> Option<Arc<Icon>> {
        let path = find_icon_file(app_id)?;

        #[cfg(target_os = "macos")]
        {
            if self.last_applied.as_deref() != Some(app_id)
                && macos::set_application_icon(&path).is_some()
            {
                self.last_applied = Some(app_id.to_string());
            }
        }

        if path.extension().and_then(OsStr::to_str)? != "png" {
            return None;
        }
        let decoded = decode_png(&path)?;
        let icon = Icon::from_rgba(decoded.rgba, decoded.width, decoded.height).ok()?;
        Some(Arc::new(icon))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::ClassType;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSData;
    use std::fs;
    use std::path::Path;

    pub fn set_application_icon(path: &Path) -> Option<()> {
        let bytes = fs::read(path).ok()?;
        unsafe {
            let data = NSData::with_bytes(&bytes);
            let image: Option<Retained<NSImage>> = NSImage::initWithData(NSImage::alloc(), &data);
            let image = image?;
            // NSApplication::sharedApplication requires the main thread marker, which
            // would force the caller through MainThreadMarker plumbing. We're already
            // on the main thread (winit's event loop) so call the Objective-C selector
            // directly to keep the call sites simple.
            let cls = objc2::class!(NSApplication);
            let app: *mut AnyObject = msg_send![cls, sharedApplication];
            if app.is_null() {
                return None;
            }
            let _: () = msg_send![app, setApplicationIconImage: &*image];
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn sanitize_matches_bash_helper() {
        assert_eq!(
            sanitize_app_id("org.gnome.Calculator"),
            "org.gnome.Calculator"
        );
        assert_eq!(sanitize_app_id("foo bar"), "foo-bar");
        assert_eq!(
            sanitize_app_id("--leading--inner---trailing--"),
            "leading-inner-trailing"
        );
        assert_eq!(sanitize_app_id(""), "");
    }

    #[test]
    fn sanitize_preserves_dots_underscores_and_dashes_inside_identifiers() {
        // Real-world app_ids: GNOME's reverse-DNS-style, plus the
        // occasional underscore in third-party flatpak names.
        assert_eq!(
            sanitize_app_id("org.freedesktop.Foo_Bar"),
            "org.freedesktop.Foo_Bar"
        );
    }

    #[test]
    fn sanitize_collapses_runs_of_unsafe_chars_into_single_dash() {
        // `foo!!bar` is one run of unsafe chars — folds to a single
        // dash. Without the collapse we'd write `foo--bar` and the
        // cached-icon path on disk wouldn't match the host CLI's
        // output.
        assert_eq!(sanitize_app_id("foo!!bar"), "foo-bar");
    }

    // ---- find_icon_file_in ------------------------------------------------
    //
    // Story: on each guest window we look for a cached icon under
    // $VBOX_ICON_DIR/<sanitized>.<ext>. The loader walks the same
    // extension list as the host CLI. We pin every observable shape:
    //   1. empty app_id → no lookup, no result
    //   2. app_id sanitizes to empty → no lookup
    //   3. no matching file → None
    //   4. png match → first hit wins
    //   5. extension order matters — png beats jpg beats jpeg

    #[test]
    fn find_icon_returns_none_for_empty_app_id() {
        let dir = PathBuf::from("/cache");
        assert!(find_icon_file_in(&dir, "", |_| true).is_none());
    }

    #[test]
    fn find_icon_returns_none_when_sanitized_is_empty() {
        // app_id that's all-unsafe chars — the host CLI would write the
        // empty file name too, so we refuse to look it up.
        let dir = PathBuf::from("/cache");
        assert!(find_icon_file_in(&dir, "!!!!", |_| true).is_none());
    }

    #[test]
    fn find_icon_returns_none_when_no_extension_matches() {
        let dir = PathBuf::from("/cache");
        let out = find_icon_file_in(&dir, "org.gnome.Calculator", |_| false);
        assert!(out.is_none());
    }

    #[test]
    fn find_icon_returns_first_extension_match() {
        // The lookup walks "png, jpg, jpeg, tiff, tif, icns" in order.
        // When only the `tiff` candidate exists, we return that path —
        // proving the search continued past png/jpg/jpeg.
        let dir = PathBuf::from("/cache");
        let want = dir.join("org.gnome.Calculator.tiff");
        let found = find_icon_file_in(&dir, "org.gnome.Calculator", |p: &Path| p == want)
            .expect("tiff candidate should be found");
        assert_eq!(found, want);
    }

    #[test]
    fn find_icon_prefers_png_when_multiple_extensions_exist() {
        // When both png and jpg exist, png wins. The host CLI writes
        // png by default; matching its priority means the host viewer
        // and the CLI-driven cache stay in sync.
        let dir = PathBuf::from("/cache");
        let png = dir.join("org.gnome.Calculator.png");
        let jpg = dir.join("org.gnome.Calculator.jpg");
        let found = find_icon_file_in(&dir, "org.gnome.Calculator", |p: &Path| {
            p == png || p == jpg
        })
        .unwrap();
        assert_eq!(found, png);
    }
}
