//! Frame buffer + pixel helpers.
//!
//! All rendering logic that operates on a `Vec<u32>` softbuffer plane lives
//! here, with no dependency on `winit` or `ViewerWindow`. Inputs come in as
//! plain numeric arguments and `FrameTile` (from `vbox-proto`), outputs are
//! plain values — that lets the test suite drive every interesting case
//! without spinning up an event loop.
//!
//! The `FrameBuffer` itself owns the running content-rect / matte average so
//! the viewer can letterbox an undersized inner frame to the host window
//! geometry without flashing the dark default matte during fast resizes.
//!
//! Public surface is intentionally `pub(crate)`: only `viewer::window` and
//! the unit tests under this crate read these types.
//!
//! Constants live next to the types that consume them so the rendering math
//! is self-contained; the `viewer` module re-exports nothing from here.
use anyhow::{Context, Result, bail};
use vbox_proto::{FrameTile, PixelEncoding};

pub(crate) const DARK_MATTE_PIXEL: u32 = 0x00181818;
pub(crate) const LIGHT_MATTE_PIXEL: u32 = 0x00ff_ffff;
pub(crate) const DEFAULT_MATTE_PIXEL: u32 = DARK_MATTE_PIXEL;
pub(crate) const TRANSPARENT_PIXEL: u32 = 0x00000000;
pub(crate) const VISIBLE_ALPHA_THRESHOLD: u8 = 48;
// 0 (not 2): a symmetric trim was clipping GNOME's right-edge close button.
pub(crate) const CONTENT_EDGE_TRIM_PX: u32 = 0;

pub(crate) struct FrameBuffer {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u32>,
    pub(crate) content: FrameRect,
    pub(crate) matte: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

impl FrameBuffer {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let mut frame = Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            content: FrameRect::new(0, 0, 1, 1),
            matte: DEFAULT_MATTE_PIXEL,
        };
        frame.resize(width, height);
        frame
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        // Preserve old pixels + matte across rapid resizes (Split View, drag):
        // clearing here let the viewer flash black before the next tile lands.
        self.replace_buffer_preserving_pixels(width, height);
        self.content = FrameRect::new(0, 0, width.max(1), height.max(1));
        // intentionally: self.matte is preserved
    }

    fn grow_preserving_pixels(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        self.replace_buffer_preserving_pixels(width, height);
    }

    /// Reallocate `self.pixels` to a `(width, height)` buffer, row-copying as
    /// many pixels as fit from the old buffer. Caller decides what to do with
    /// `self.content` afterwards: `resize` resets it, `grow_preserving_pixels`
    /// (driven by `apply_tile`) leaves it for `update_content_rect` to recompute.
    fn replace_buffer_preserving_pixels(&mut self, width: u32, height: u32) {
        let old_pixels = std::mem::take(&mut self.pixels);
        let old_width = self.width;
        let old_height = self.height;
        self.width = width;
        self.height = height;
        self.pixels = vec![TRANSPARENT_PIXEL; width as usize * height as usize];
        let copy_w = old_width.min(width) as usize;
        let copy_h = old_height.min(height) as usize;
        for y in 0..copy_h {
            let src_off = y * old_width as usize;
            let dst_off = y * width as usize;
            self.pixels[dst_off..dst_off + copy_w]
                .copy_from_slice(&old_pixels[src_off..src_off + copy_w]);
        }
    }

    pub(crate) fn apply_tile(&mut self, tile: &FrameTile) -> Result<bool> {
        if tile.encoding != PixelEncoding::RawRgba {
            bail!("unsupported pixel encoding: {:?}", tile.encoding);
        }
        let min_stride = tile.w.checked_mul(4).context("tile stride overflow")?;
        if tile.stride < min_stride {
            bail!("tile stride {} < minimum {}", tile.stride, min_stride);
        }
        let needed = tile
            .stride
            .checked_mul(tile.h)
            .context("tile byte length overflow")? as usize;
        if tile.bytes.len() < needed {
            bail!("tile bytes {} < needed {}", tile.bytes.len(), needed);
        }
        let old_display_size = self.display_size();
        let new_width = self.width.max(tile.x.saturating_add(tile.w)).max(1);
        let new_height = self.height.max(tile.y.saturating_add(tile.h)).max(1);
        if new_width != self.width || new_height != self.height {
            self.grow_preserving_pixels(new_width, new_height);
        }

        for row in 0..tile.h {
            let src_row = (row * tile.stride) as usize;
            let dst_row = ((tile.y + row) * self.width + tile.x) as usize;
            for col in 0..tile.w {
                let src = src_row + (col * 4) as usize;
                let dst = dst_row + col as usize;
                let r = tile.bytes[src] as u32;
                let g = tile.bytes[src + 1] as u32;
                let b = tile.bytes[src + 2] as u32;
                let a = tile.bytes[src + 3] as u32;
                // Treat near-zero alpha as "no contribution" and leave the
                // existing pixel in place. Without this, GTK4 popover
                // open/close animations — which commit the popup surface
                // with progressively transparent top rows — would overwrite
                // the parent surface's visible pixels with alpha=0, and
                // `pixel_for_softbuffer` would render those as matte
                // (white in light mode). User sees "popup with the top
                // chopped off and replaced by white", a.k.a. "팝업메뉴가
                // 프레임별로 깨짐". Skipping the write lets the parent's
                // last opaque pixels show through the popup's transparent
                // edges, the way a real Wayland compositor does.
                //
                // The threshold mirrors `pixel_alpha`/`VISIBLE_ALPHA_THRESHOLD`
                // used at render time so the two halves stay in sync.
                if (a as u8) < VISIBLE_ALPHA_THRESHOLD {
                    continue;
                }
                self.pixels[dst] = rgba_to_pixel(r, g, b, a);
            }
        }
        self.update_content_rect();
        Ok(old_display_size != self.display_size())
    }

    pub(crate) fn display_size(&self) -> (u32, u32) {
        (self.content.w.max(1), self.content.h.max(1))
    }

    fn update_content_rect(&mut self) {
        let mut min_x = self.width;
        let mut min_y = self.height;
        let mut max_x = 0;
        let mut max_y = 0;
        let mut r_sum = 0u64;
        let mut g_sum = 0u64;
        let mut b_sum = 0u64;
        let mut count = 0u64;

        for y in 0..self.height {
            let row = y as usize * self.width as usize;
            for x in 0..self.width {
                let pixel = self.pixels[row + x as usize];
                if pixel_alpha(pixel) < VISIBLE_ALPHA_THRESHOLD {
                    continue;
                }
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                r_sum += u64::from(pixel_r(pixel));
                g_sum += u64::from(pixel_g(pixel));
                b_sum += u64::from(pixel_b(pixel));
                count += 1;
            }
        }

        if count == 0 {
            self.content = FrameRect::new(0, 0, self.width.max(1), self.height.max(1));
            self.matte = DEFAULT_MATTE_PIXEL;
            return;
        }

        let alpha_rect = trim_content_rect(FrameRect::new(
            min_x,
            min_y,
            max_x - min_x + 1,
            max_y - min_y + 1,
        ));
        // GTK toplevels paint their empty area with the theme bg color
        // (opaque white in light mode), which the alpha-bounded bbox above
        // happily includes. That makes the alpha rect equal to the full
        // frame and the host window adopts the oversized geom → fat letterbox
        // margin under the app ("앱 실행시 여백이 보이는" 버그). Strip the
        // trailing uniformly-colored rows/columns so the content rect tracks
        // what was actually rendered.
        self.content = trim_uniform_padding(&self.pixels, self.width, alpha_rect);
        self.matte = rgb_to_pixel(
            (r_sum / count) as u32,
            (g_sum / count) as u32,
            (b_sum / count) as u32,
        );
    }
}

/// Minimum run of uniform rows/cols before we trust it's "padding" vs an
/// intentional solid border. Chosen so a thin (1–8 px) GTK decoration line
/// survives untouched while a big GNOME-style empty bottom area is trimmed.
pub(crate) const UNIFORM_PADDING_MIN_RUN: u32 = 16;

pub(crate) fn trim_uniform_padding(pixels: &[u32], stride: u32, rect: FrameRect) -> FrameRect {
    let mut top = rect.y;
    let mut bottom = rect.y + rect.h;
    let mut left = rect.x;
    let mut right = rect.x + rect.w;

    // Trim from bottom: stop once we see a non-uniform row.
    let mut bottom_run = 0u32;
    while bottom > top + 1 {
        if !is_uniform_row(pixels, stride, bottom - 1, left, right) {
            break;
        }
        bottom -= 1;
        bottom_run += 1;
    }
    if bottom_run < UNIFORM_PADDING_MIN_RUN {
        // Too short a run to be confident — restore.
        bottom += bottom_run;
    }

    // Trim from top.
    let mut top_run = 0u32;
    while top + 1 < bottom {
        if !is_uniform_row(pixels, stride, top, left, right) {
            break;
        }
        top += 1;
        top_run += 1;
    }
    if top_run < UNIFORM_PADDING_MIN_RUN {
        top -= top_run;
    }

    // Trim from right.
    let mut right_run = 0u32;
    while right > left + 1 {
        if !is_uniform_col(pixels, stride, right - 1, top, bottom) {
            break;
        }
        right -= 1;
        right_run += 1;
    }
    if right_run < UNIFORM_PADDING_MIN_RUN {
        right += right_run;
    }

    // Trim from left.
    let mut left_run = 0u32;
    while left + 1 < right {
        if !is_uniform_col(pixels, stride, left, top, bottom) {
            break;
        }
        left += 1;
        left_run += 1;
    }
    if left_run < UNIFORM_PADDING_MIN_RUN {
        left -= left_run;
    }

    FrameRect::new(left, top, right - left, bottom - top)
}

/// Max Manhattan distance (sum of |Δr|+|Δg|+|Δb|) below which we treat two
/// pixels as "the same" for padding detection. Tolerates GTK's anti-aliased
/// shadow edges, sub-pixel rendering, and faint grid lines so the bottom of
/// gnome-system-monitor / Firefox new-tab / etc. is still recognised as
/// padding even when a few stray pixels disturb strict uniformity.
const PADDING_COLOR_TOLERANCE: u32 = 12;

/// Max fraction of "outlier" pixels (those farther than
/// `PADDING_COLOR_TOLERANCE` from the row/col's reference colour) tolerated
/// before we give up and declare the row/col real content. 1/20 ≈ 5% — enough
/// for a thin label or shadow stripe to cross the edge without surviving the
/// trim, but stingy enough to keep real UI lines intact.
const PADDING_OUTLIER_RATIO: usize = 20;

fn pixel_distance(a: u32, b: u32) -> u32 {
    let (ar, ag, ab) = ((a >> 16) & 0xff, (a >> 8) & 0xff, a & 0xff);
    let (br, bg, bb) = ((b >> 16) & 0xff, (b >> 8) & 0xff, b & 0xff);
    ar.abs_diff(br) + ag.abs_diff(bg) + ab.abs_diff(bb)
}

fn is_uniform_row(pixels: &[u32], stride: u32, y: u32, x_start: u32, x_end: u32) -> bool {
    if x_start >= x_end {
        return true;
    }
    let row = (y * stride) as usize;
    let first = pixels[row + x_start as usize];
    let width = (x_end - x_start) as usize;
    let allow = (width / PADDING_OUTLIER_RATIO).max(1);
    let mut outliers = 0usize;
    for x in (x_start + 1)..x_end {
        let p = pixels[row + x as usize];
        if pixel_distance(p, first) > PADDING_COLOR_TOLERANCE {
            outliers += 1;
            if outliers > allow {
                return false;
            }
        }
    }
    true
}

fn is_uniform_col(pixels: &[u32], stride: u32, x: u32, y_start: u32, y_end: u32) -> bool {
    if y_start >= y_end {
        return true;
    }
    let first = pixels[(y_start * stride) as usize + x as usize];
    let height = (y_end - y_start) as usize;
    let allow = (height / PADDING_OUTLIER_RATIO).max(1);
    let mut outliers = 0usize;
    for y in (y_start + 1)..y_end {
        let p = pixels[(y * stride) as usize + x as usize];
        if pixel_distance(p, first) > PADDING_COLOR_TOLERANCE {
            outliers += 1;
            if outliers > allow {
                return false;
            }
        }
    }
    true
}

impl FrameRect {
    pub(crate) fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self {
            x,
            y,
            w: w.max(1),
            h: h.max(1),
        }
    }
}

pub(crate) fn trim_content_rect(rect: FrameRect) -> FrameRect {
    let trim = CONTENT_EDGE_TRIM_PX;
    if rect.w <= trim * 2 || rect.h <= trim * 2 {
        return rect;
    }
    FrameRect::new(
        rect.x + trim,
        rect.y + trim,
        rect.w - trim * 2,
        rect.h - trim * 2,
    )
}

#[cfg(test)]
pub(crate) fn copy_scaled_frame_region(
    src: &[u32],
    src_width: usize,
    region: FrameRect,
    matte: u32,
    dst: &mut [u32],
    dst_width: usize,
    dst_height: usize,
) {
    let src_x0 = region.x as usize;
    let src_y0 = region.y as usize;
    let src_region_width = region.w as usize;
    let src_region_height = region.h as usize;

    if src_region_width == dst_width && src_region_height == dst_height {
        for y in 0..dst_height {
            let src_row = (src_y0 + y) * src_width + src_x0;
            let dst_row = y * dst_width;
            for x in 0..dst_width {
                dst[dst_row + x] = pixel_for_softbuffer(src[src_row + x], matte);
            }
        }
        return;
    }

    for y in 0..dst_height {
        let src_y = src_y0 + y * src_region_height / dst_height;
        let src_row = src_y * src_width;
        let dst_row = y * dst_width;
        for x in 0..dst_width {
            let src_x = src_x0 + x * src_region_width / dst_width;
            dst[dst_row + x] = pixel_for_softbuffer(src[src_row + src_x], matte);
        }
    }
}

pub(crate) fn copy_frame_region_preserve_aspect(
    src: &[u32],
    src_width: usize,
    region: FrameRect,
    matte: u32,
    dst: &mut [u32],
    dst_width: usize,
    dst_height: usize,
) {
    dst.fill(opaque_softbuffer_pixel(matte));
    let render = aspect_fit_rect(region.w, region.h, dst_width as u32, dst_height as u32);
    copy_scaled_frame_region_into_rect(src, src_width, region, matte, dst, dst_width, render);
}

fn copy_scaled_frame_region_into_rect(
    src: &[u32],
    src_width: usize,
    region: FrameRect,
    matte: u32,
    dst: &mut [u32],
    dst_width: usize,
    render: RenderRect,
) {
    let src_x0 = region.x as usize;
    let src_y0 = region.y as usize;
    let src_region_width = region.w as usize;
    let src_region_height = region.h as usize;
    let dst_x0 = render.x as usize;
    let dst_y0 = render.y as usize;
    let render_width = render.w as usize;
    let render_height = render.h as usize;

    for y in 0..render_height {
        let src_y = src_y0 + y * src_region_height / render_height;
        let src_row = src_y * src_width;
        let dst_row = (dst_y0 + y) * dst_width + dst_x0;
        for x in 0..render_width {
            let src_x = src_x0 + x * src_region_width / render_width;
            dst[dst_row + x] = pixel_for_softbuffer(src[src_row + src_x], matte);
        }
    }
}

pub(crate) fn aspect_fit_rect(
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> RenderRect {
    let src_width = u64::from(src_width.max(1));
    let src_height = u64::from(src_height.max(1));
    let dst_width_u64 = u64::from(dst_width.max(1));
    let dst_height_u64 = u64::from(dst_height.max(1));

    let (width, height) = if src_width * dst_height_u64 > dst_width_u64 * src_height {
        let width = dst_width_u64;
        let height = ((dst_width_u64 * src_height + src_width / 2) / src_width).max(1);
        (width, height.min(dst_height_u64))
    } else {
        let height = dst_height_u64;
        let width = ((dst_height_u64 * src_width + src_height / 2) / src_height).max(1);
        (width.min(dst_width_u64), height)
    };

    RenderRect {
        x: ((dst_width_u64 - width) / 2) as u32,
        y: ((dst_height_u64 - height) / 2) as u32,
        w: width as u32,
        h: height as u32,
    }
}

pub(crate) fn rgba_to_pixel(r: u32, g: u32, b: u32, a: u32) -> u32 {
    ((a & 0xff) << 24) | ((r & 0xff) << 16) | ((g & 0xff) << 8) | (b & 0xff)
}

pub(crate) fn rgb_to_pixel(r: u32, g: u32, b: u32) -> u32 {
    ((r & 0xff) << 16) | ((g & 0xff) << 8) | (b & 0xff)
}

pub(crate) fn pixel_alpha(pixel: u32) -> u8 {
    ((pixel >> 24) & 0xff) as u8
}

fn pixel_r(pixel: u32) -> u8 {
    ((pixel >> 16) & 0xff) as u8
}

fn pixel_g(pixel: u32) -> u8 {
    ((pixel >> 8) & 0xff) as u8
}

fn pixel_b(pixel: u32) -> u8 {
    (pixel & 0xff) as u8
}

pub(crate) fn pixel_for_softbuffer(pixel: u32, matte: u32) -> u32 {
    let a = u32::from(pixel_alpha(pixel));
    if a >= 255 {
        return opaque_softbuffer_pixel(pixel);
    }
    if a < VISIBLE_ALPHA_THRESHOLD as u32 {
        return opaque_softbuffer_pixel(matte);
    }

    let matte_r = (matte >> 16) & 0xff;
    let matte_g = (matte >> 8) & 0xff;
    let matte_b = matte & 0xff;
    let r = (u32::from(pixel_r(pixel)) * a + matte_r * (255 - a)) / 255;
    let g = (u32::from(pixel_g(pixel)) * a + matte_g * (255 - a)) / 255;
    let b = (u32::from(pixel_b(pixel)) * a + matte_b * (255 - a)) / 255;
    opaque_softbuffer_pixel(rgb_to_pixel(r, g, b))
}

pub(crate) fn opaque_softbuffer_pixel(pixel: u32) -> u32 {
    0xff00_0000 | (pixel & 0x00ff_ffff)
}

pub(crate) fn scaled_len(len: u32, scale: f32) -> u32 {
    ((len.max(1) as f32) * scale)
        .round()
        .clamp(1.0, u32::MAX as f32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use vbox_proto::{FrameTile, PixelEncoding};

    #[test]
    fn frame_buffer_applies_tile_and_grows_to_extent() {
        let mut frame = FrameBuffer::new(1, 1);
        let tile = FrameTile {
            id: 2,
            x: 1,
            y: 1,
            w: 1,
            h: 1,
            stride: 4,
            encoding: PixelEncoding::RawRgba,
            bytes: vec![255, 0, 0, 255],
        };

        frame.apply_tile(&tile).unwrap();

        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(frame.pixels[3], 0xffff0000);
        assert_eq!(frame.content, FrameRect::new(1, 1, 1, 1));
    }

    #[test]
    fn frame_buffer_growth_preserves_existing_pixels() {
        let mut frame = FrameBuffer::new(1, 1);
        frame
            .apply_tile(&FrameTile {
                id: 2,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                stride: 4,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![0, 255, 0, 255],
            })
            .unwrap();
        frame
            .apply_tile(&FrameTile {
                id: 2,
                x: 1,
                y: 0,
                w: 1,
                h: 1,
                stride: 4,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![255, 0, 0, 255],
            })
            .unwrap();

        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.pixels[0], 0xff00ff00);
        assert_eq!(frame.pixels[1], 0xffff0000);
    }

    #[test]
    fn frame_buffer_crops_transparent_border() {
        let mut frame = FrameBuffer::new(3, 3);
        let tile = FrameTile {
            id: 2,
            x: 0,
            y: 0,
            w: 3,
            h: 3,
            stride: 12,
            encoding: PixelEncoding::RawRgba,
            bytes: vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64, 128, 192, 255, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };

        let display_changed = frame.apply_tile(&tile).unwrap();

        assert!(display_changed);
        assert_eq!(frame.content, FrameRect::new(1, 1, 1, 1));
        assert_eq!(frame.display_size(), (1, 1));
        assert_eq!(frame.matte, 0x004080c0);
    }

    #[test]
    fn content_rect_passes_through_when_trim_is_zero() {
        // CONTENT_EDGE_TRIM_PX was lowered to 0 so we no longer clip the
        // toplevel's rightmost column (where GNOME's close button sits).
        assert_eq!(
            trim_content_rect(FrameRect::new(24, 24, 1090, 1816)),
            FrameRect::new(24, 24, 1090, 1816)
        );
        assert_eq!(
            trim_content_rect(FrameRect::new(1, 1, 1, 1)),
            FrameRect::new(1, 1, 1, 1)
        );
    }

    #[test]
    fn uniform_padding_trim_strips_long_solid_bottom() {
        // Simulate a GTK toplevel: a handful of varied content rows over a
        // long uniformly-white tail (the empty-area bg fill that the alpha
        // bbox cannot detect because the pixels are opaque). The trim peels
        // off the white tail and stops at the first non-uniform row.
        const W: u32 = 8;
        const H: u32 = 24;
        let mut pixels = vec![0xff_ff_ff_ff_u32; (W * H) as usize];
        // Make every "content" row non-uniform so the trim has a clear
        // boundary — row 3 is the last varied row, everything ≥ row 4 is
        // uniform white (20 rows, well above UNIFORM_PADDING_MIN_RUN).
        for x in 0..W {
            pixels[x as usize] = if x % 2 == 0 {
                0xff_11_22_33
            } else {
                0xff_44_55_66
            };
            pixels[(W + x) as usize] = if x < 4 { 0xff_aa_bb_cc } else { 0xff_de_ad_be };
            pixels[(2 * W + x) as usize] = if x < 4 { 0xff_00_11_22 } else { 0xff_33_44_55 };
            pixels[(3 * W + x) as usize] = if x < 6 { 0xff_77_88_99 } else { 0xff_ca_fe_00 };
        }
        let rect = FrameRect::new(0, 0, W, H);
        let trimmed = trim_uniform_padding(&pixels, W, rect);
        assert_eq!(trimmed, FrameRect::new(0, 0, W, 4));
    }

    #[test]
    fn uniform_padding_trim_preserves_short_uniform_runs() {
        // A run shorter than UNIFORM_PADDING_MIN_RUN (e.g. a 4-row solid
        // border or a single-color title strip) is intentional content; the
        // trim must keep it intact so we don't clip legitimate UI elements.
        const W: u32 = 8;
        const H: u32 = 8;
        let mut pixels = vec![0xff_22_22_22_u32; (W * H) as usize];
        // Make rows 4..8 a varied stripe so the bottom isn't uniform.
        for x in 0..W {
            pixels[(4 * W + x) as usize] = if x % 2 == 0 {
                0xff_11_22_33
            } else {
                0xff_99_88_77
            };
            pixels[(5 * W + x) as usize] = 0xff_55_55_55;
            pixels[(6 * W + x) as usize] = 0xff_aa_aa_aa;
            pixels[(7 * W + x) as usize] = 0xff_dd_dd_dd;
        }
        let rect = FrameRect::new(0, 0, W, H);
        let trimmed = trim_uniform_padding(&pixels, W, rect);
        // Top 4 rows are uniform (a "border") but only 4 rows — below the
        // 16-row threshold, so the trim leaves them alone.
        assert_eq!(trimmed, rect);
    }

    #[test]
    fn frame_buffer_trims_uniform_white_tail() {
        // End-to-end: a tile whose top rows have varied (non-uniform) content
        // and whose bottom is solid white. After apply_tile +
        // update_content_rect, display_size shrinks to the varied portion —
        // this is the missing piece that makes the host window adopt the
        // *visible* content size instead of the GTK CSD-sized toplevel.
        const W: u32 = 8;
        const H: u32 = 24;
        let row_stride = (W * 4) as usize;
        let mut bytes = vec![0xff_u8; (W * H) as usize * 4];
        // Rows 0..4 carry a per-pixel alternating pattern so each one is
        // non-uniform. Rows 4..24 stay solid white (the default fill).
        for y in 0..4 {
            for x in 0..W as usize {
                let off = y * row_stride + x * 4;
                let shade = (x as u8).wrapping_mul(37).wrapping_add(y as u8 * 11);
                bytes[off] = shade;
                bytes[off + 1] = shade ^ 0x5a;
                bytes[off + 2] = 0x10 + (y as u8) * 30;
                bytes[off + 3] = 0xff;
            }
        }
        let mut frame = FrameBuffer::new(W, H);
        frame
            .apply_tile(&FrameTile {
                id: 1,
                x: 0,
                y: 0,
                w: W,
                h: H,
                stride: W * 4,
                encoding: PixelEncoding::RawRgba,
                bytes,
            })
            .unwrap();
        let (_, display_h) = frame.display_size();
        assert!(
            display_h < H,
            "expected display height < {H} after trimming uniform white tail, got {display_h}"
        );
        assert!(
            display_h >= 4,
            "trim was too aggressive: kept only {display_h} rows of varied content"
        );
    }

    #[test]
    fn frame_buffer_preserves_existing_pixels_on_transparent_overlay() {
        // Simulate the GTK4 popover fade-in: the parent surface paints
        // opaque content at (0..2, 0..1), then a popup commits over the
        // same region with alpha=0 (animation start). Without the
        // alpha-test in apply_tile, the popup's transparent pixels would
        // clobber the parent's opaque ones and `pixel_for_softbuffer`
        // would render them as matte at present time — exactly the
        // "팝업 상단이 흰색으로 잘려보임" symptom from the user report.
        let mut frame = FrameBuffer::new(2, 1);
        frame
            .apply_tile(&FrameTile {
                id: 1,
                x: 0,
                y: 0,
                w: 2,
                h: 1,
                stride: 8,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![10, 20, 30, 255, 40, 50, 60, 255],
            })
            .unwrap();
        let parent_pixel_0 = frame.pixels[0];
        let parent_pixel_1 = frame.pixels[1];

        // Popup commit with alpha=0 over both pixels.
        frame
            .apply_tile(&FrameTile {
                id: 1,
                x: 0,
                y: 0,
                w: 2,
                h: 1,
                stride: 8,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![200, 200, 200, 0, 200, 200, 200, 0],
            })
            .unwrap();

        assert_eq!(
            frame.pixels[0], parent_pixel_0,
            "transparent overlay must not clobber opaque parent pixel"
        );
        assert_eq!(frame.pixels[1], parent_pixel_1);
    }

    #[test]
    fn frame_buffer_overwrites_when_alpha_above_threshold() {
        // Sanity-check the inverse of the popover preservation rule —
        // mostly-opaque source pixels MUST replace the dst so a real popup
        // body / a window's content lands on top of whatever was there.
        let mut frame = FrameBuffer::new(1, 1);
        frame
            .apply_tile(&FrameTile {
                id: 1,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                stride: 4,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![10, 20, 30, 255],
            })
            .unwrap();
        frame
            .apply_tile(&FrameTile {
                id: 1,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                stride: 4,
                encoding: PixelEncoding::RawRgba,
                bytes: vec![240, 250, 200, 200],
            })
            .unwrap();
        assert_eq!(frame.pixels[0], rgba_to_pixel(240, 250, 200, 200));
    }

    #[test]
    fn frame_buffer_rejects_short_stride() {
        let mut frame = FrameBuffer::new(1, 1);
        let tile = FrameTile {
            id: 2,
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            stride: 4,
            encoding: PixelEncoding::RawRgba,
            bytes: vec![0; 8],
        };

        assert!(frame.apply_tile(&tile).is_err());
    }

    #[test]
    fn scaled_frame_copy_downscales_to_window_size() {
        let src = [
            rgba_to_pixel(1, 0, 0, 255),
            rgba_to_pixel(2, 0, 0, 255),
            rgba_to_pixel(3, 0, 0, 255),
            rgba_to_pixel(4, 0, 0, 255),
        ];
        let mut dst = [0];

        copy_scaled_frame_region(
            &src,
            2,
            FrameRect::new(0, 0, 2, 2),
            DEFAULT_MATTE_PIXEL,
            &mut dst,
            1,
            1,
        );

        assert_eq!(dst, [0xff010000]);
    }

    #[test]
    fn scaled_frame_copy_upscales_to_window_size() {
        let src = [rgba_to_pixel(7, 0, 0, 255)];
        let mut dst = [0; 4];

        copy_scaled_frame_region(
            &src,
            1,
            FrameRect::new(0, 0, 1, 1),
            DEFAULT_MATTE_PIXEL,
            &mut dst,
            2,
            2,
        );

        assert_eq!(dst, [0xff070000, 0xff070000, 0xff070000, 0xff070000]);
    }

    #[test]
    fn aspect_fit_rect_preserves_source_ratio() {
        assert_eq!(
            aspect_fit_rect(100, 50, 300, 300),
            RenderRect {
                x: 0,
                y: 75,
                w: 300,
                h: 150,
            }
        );
        assert_eq!(
            aspect_fit_rect(50, 100, 300, 300),
            RenderRect {
                x: 75,
                y: 0,
                w: 150,
                h: 300,
            }
        );
    }

    #[test]
    fn frame_resize_preserves_old_pixels_on_grow() {
        // Regression: FrameBuffer::resize used to clear pixels to TRANSPARENT
        // on every size change, which made macOS Split View / drag-resize
        // flash a black background until the next server frame arrived.
        let mut frame = FrameBuffer::new(2, 2);
        frame.pixels = vec![0xff_00_00_00, 0x00_ff_00_00, 0x00_00_ff_00, 0xff_ff_ff_00];
        frame.resize(4, 4);
        assert_eq!(frame.pixels[0], 0xff_00_00_00);
        assert_eq!(frame.pixels[1], 0x00_ff_00_00);
        assert_eq!(frame.pixels[4], 0x00_00_ff_00);
        assert_eq!(frame.pixels[5], 0xff_ff_ff_00);
        assert_eq!(frame.pixels[3], TRANSPARENT_PIXEL);
        assert_eq!(frame.pixels[12], TRANSPARENT_PIXEL);
    }

    #[test]
    fn frame_resize_preserves_old_pixels_on_shrink() {
        let mut frame = FrameBuffer::new(4, 4);
        frame.pixels = (0..16).map(|n| 0xff_00_00_00 | (n as u32)).collect();
        frame.resize(2, 2);
        assert_eq!(frame.pixels[0], 0xff_00_00_00);
        assert_eq!(frame.pixels[1], 0xff_00_00_01);
        assert_eq!(frame.pixels[2], 0xff_00_00_04);
        assert_eq!(frame.pixels[3], 0xff_00_00_05);
        assert_eq!(frame.pixels.len(), 4);
    }

    #[test]
    fn frame_resize_preserves_matte() {
        // matte (the letterbox fill colour) used to be force-reset to the
        // dark DEFAULT_MATTE_PIXEL on every resize, contributing to the
        // black-flash during fast resize. The fix keeps whatever the last
        // good frame's average colour was.
        let mut frame = FrameBuffer::new(2, 2);
        frame.matte = 0x00_ff_ff_ff; // white
        frame.resize(3, 3);
        assert_eq!(frame.matte, 0x00_ff_ff_ff);
    }

    #[test]
    fn preserve_aspect_copy_fills_letterbox_with_matte() {
        let src = [rgba_to_pixel(7, 0, 0, 255)];
        let mut dst = [0; 8];

        copy_frame_region_preserve_aspect(
            &src,
            1,
            FrameRect::new(0, 0, 1, 1),
            0x00123456,
            &mut dst,
            4,
            2,
        );

        assert_eq!(
            dst,
            [
                0xff123456, 0xff070000, 0xff070000, 0xff123456, 0xff123456, 0xff070000, 0xff070000,
                0xff123456,
            ]
        );
    }

    #[test]
    fn softbuffer_pixel_uses_matte_for_transparent_pixels() {
        assert_eq!(
            pixel_for_softbuffer(rgba_to_pixel(255, 255, 255, 0), 0x00123456),
            0xff123456
        );
    }
}
