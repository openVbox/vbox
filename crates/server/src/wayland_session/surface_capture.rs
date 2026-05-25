//! Wayland surface commit + SHM buffer → `FrameTile` capture.
//!
//! Smithay surfaces carry a per-commit "current" attribute set that the
//! compositor must drain on each commit: the buffer assignment, the
//! damage rects, and the frame-callback list. [`take_surface_commit`] does
//! that drain and returns the snapshot the App needs to decide whether to
//! emit a new `FrameTile`. [`discard_surface_commit`] is the
//! drain-and-release path for commits we don't render (e.g. background
//! surfaces or surfaces that fell out of the active toplevel).
//!
//! [`copy_shm_buffer`] is the actual SHM → `FrameTile` conversion: clip
//! against the toplevel's content rect via [`surface_copy_rect`], walk the
//! source rows with [`convert_shm_pixel`] for format normalisation, and
//! emit a tightly packed `RawRgba` buffer ready for wire transmission.
use anyhow::{Context, Result, bail};
use smithay::reexports::wayland_server::protocol::{
    wl_buffer, wl_shm,
    wl_surface::{self, WlSurface},
};
use smithay::wayland::compositor::{
    self, BufferAssignment, Damage, SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::shm::with_buffer_contents;
use vbox_proto::{FrameTile, PixelEncoding};

pub(crate) struct SurfaceCommit {
    pub(crate) buffer: CommitBuffer,
    pub(crate) damage_count: usize,
    pub(crate) damage_summary: Option<String>,
}

impl SurfaceCommit {
    pub(crate) fn buffer_label(&self) -> &'static str {
        match self.buffer {
            CommitBuffer::New(_) => "new",
            CommitBuffer::Removed => "removed",
            CommitBuffer::None if self.damage_count > 0 => "reused",
            CommitBuffer::None => "none",
        }
    }
}

#[derive(Debug)]
pub(crate) enum CommitBuffer {
    New(wl_buffer::WlBuffer),
    Removed,
    None,
}

pub(crate) fn take_surface_commit(
    surface: &WlSurface,
    capture_damage_summary: bool,
) -> SurfaceCommit {
    compositor::with_states(surface, |states| {
        let mut attributes = states.cached_state.get::<SurfaceAttributes>();
        let current = attributes.current();
        let damage_count = current.damage.len();
        let damage_summary = capture_damage_summary.then(|| summarize_damage(&current.damage));
        current.damage.clear();
        let buffer = match current.buffer.take() {
            Some(BufferAssignment::NewBuffer(buffer)) => CommitBuffer::New(buffer),
            Some(BufferAssignment::Removed) => CommitBuffer::Removed,
            None => CommitBuffer::None,
        };
        SurfaceCommit {
            buffer,
            damage_count,
            damage_summary,
        }
    })
}

pub(crate) fn discard_surface_commit(surface: &WlSurface) {
    if let CommitBuffer::New(buffer) = take_surface_commit(surface, false).buffer {
        buffer.release();
    }
}

pub(crate) fn copy_shm_buffer(
    buffer: &wl_buffer::WlBuffer,
    id: u64,
    offset_x: i32,
    offset_y: i32,
) -> Result<Option<FrameTile>> {
    copy_shm_buffer_with_bounds(buffer, id, offset_x, offset_y, None)
}

pub(crate) fn copy_shm_buffer_clipped(
    buffer: &wl_buffer::WlBuffer,
    id: u64,
    offset_x: i32,
    offset_y: i32,
    bounds: SurfaceBounds,
) -> Result<Option<FrameTile>> {
    copy_shm_buffer_with_bounds(buffer, id, offset_x, offset_y, Some(bounds))
}

fn copy_shm_buffer_with_bounds(
    buffer: &wl_buffer::WlBuffer,
    id: u64,
    offset_x: i32,
    offset_y: i32,
    bounds: Option<SurfaceBounds>,
) -> Result<Option<FrameTile>> {
    with_buffer_contents(buffer, |ptr, len, data| -> Result<Option<FrameTile>> {
        if data.offset < 0 || data.width <= 0 || data.height <= 0 || data.stride <= 0 {
            bail!("invalid shm buffer data: {data:?}");
        }
        let width = data.width as u32;
        let height = data.height as u32;
        let Some(copy_rect) =
            surface_copy_rect_with_bounds(width, height, offset_x, offset_y, bounds)
        else {
            return Ok(None);
        };
        let src_stride = data.stride as usize;
        let dst_stride = copy_rect
            .width
            .checked_mul(4)
            .context("destination stride overflow")?;
        let offset = data.offset as usize;
        let min_len = offset
            .checked_add(
                src_stride
                    .checked_mul(height as usize)
                    .context("source stride overflow")?,
            )
            .context("source length overflow")?;
        if len < min_len {
            bail!("shm buffer too short: {len} < {min_len}");
        }

        // Smithay gives a raw shm pointer that is valid only inside this callback.
        let src = unsafe { std::slice::from_raw_parts(ptr, len) };
        let mut bytes = vec![0u8; dst_stride as usize * copy_rect.height as usize];

        for y in 0..copy_rect.height as usize {
            let src_y = copy_rect.skip_y as usize + y;
            let src_row = offset + src_y * src_stride;
            let dst_row = y * dst_stride as usize;
            for x in 0..copy_rect.width as usize {
                let src_x = copy_rect.skip_x as usize + x;
                let src_px = src_row + src_x * 4;
                let dst_px = dst_row + x * 4;
                let rgba = convert_shm_pixel(data.format, &src[src_px..src_px + 4])
                    .with_context(|| format!("unsupported wl_shm format {:?}", data.format))?;
                bytes[dst_px..dst_px + 4].copy_from_slice(&rgba);
            }
        }

        Ok(Some(FrameTile {
            id,
            x: copy_rect.dst_x,
            y: copy_rect.dst_y,
            w: copy_rect.width,
            h: copy_rect.height,
            stride: dst_stride,
            encoding: PixelEncoding::RawRgba,
            bytes,
        }))
    })
    .context("accessing shm buffer")?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceBounds {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl SurfaceBounds {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyRect {
    pub(crate) skip_x: u32,
    pub(crate) skip_y: u32,
    pub(crate) dst_x: u32,
    pub(crate) dst_y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[cfg(test)]
pub(crate) fn surface_copy_rect(
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
) -> Option<CopyRect> {
    surface_copy_rect_with_bounds(width, height, offset_x, offset_y, None)
}

#[cfg(test)]
pub(crate) fn surface_copy_rect_clipped(
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
    bounds: SurfaceBounds,
) -> Option<CopyRect> {
    surface_copy_rect_with_bounds(width, height, offset_x, offset_y, Some(bounds))
}

fn surface_copy_rect_with_bounds(
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
    bounds: Option<SurfaceBounds>,
) -> Option<CopyRect> {
    let skip_x = if offset_x < 0 {
        offset_x.unsigned_abs()
    } else {
        0
    };
    let skip_y = if offset_y < 0 {
        offset_y.unsigned_abs()
    } else {
        0
    };
    if skip_x >= width || skip_y >= height {
        return None;
    }

    let dst_x = offset_x.max(0) as u32;
    let dst_y = offset_y.max(0) as u32;
    let mut copy_width = width - skip_x;
    let mut copy_height = height - skip_y;
    if let Some(bounds) = bounds {
        if bounds.width == 0 || bounds.height == 0 {
            return None;
        }
        if dst_x >= bounds.width || dst_y >= bounds.height {
            return None;
        }
        copy_width = copy_width.min(bounds.width - dst_x);
        copy_height = copy_height.min(bounds.height - dst_y);
        if copy_width == 0 || copy_height == 0 {
            return None;
        }
    }

    Some(CopyRect {
        skip_x,
        skip_y,
        dst_x,
        dst_y,
        width: copy_width,
        height: copy_height,
    })
}

pub(crate) fn summarize_damage(damage: &[Damage]) -> String {
    match damage {
        [] => "none".to_string(),
        [one] => format!("1 {one:?}"),
        [first, ..] => format!("{} first={first:?}", damage.len()),
    }
}

pub(crate) fn convert_shm_pixel(format: wl_shm::Format, px: &[u8]) -> Option<[u8; 4]> {
    match format {
        wl_shm::Format::Argb8888 => Some([px[2], px[1], px[0], px[3]]),
        wl_shm::Format::Xrgb8888 => Some([px[2], px[1], px[0], 255]),
        wl_shm::Format::Abgr8888 => Some([px[0], px[1], px[2], px[3]]),
        wl_shm::Format::Xbgr8888 => Some([px[0], px[1], px[2], 255]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_rect_keeps_surface_larger_than_output() {
        assert_eq!(
            surface_copy_rect(1074, 818, 0, 0),
            Some(CopyRect {
                skip_x: 0,
                skip_y: 0,
                dst_x: 0,
                dst_y: 0,
                width: 1074,
                height: 818,
            })
        );
    }

    #[test]
    fn copy_rect_handles_negative_surface_offsets() {
        assert_eq!(
            surface_copy_rect(100, 80, -10, -20),
            Some(CopyRect {
                skip_x: 10,
                skip_y: 20,
                dst_x: 0,
                dst_y: 0,
                width: 90,
                height: 60,
            })
        );
        assert_eq!(surface_copy_rect(100, 80, -100, 0), None);
    }

    #[test]
    fn copy_rect_keeps_positive_surface_offsets() {
        assert_eq!(
            surface_copy_rect(100, 80, 50, 20),
            Some(CopyRect {
                skip_x: 0,
                skip_y: 0,
                dst_x: 50,
                dst_y: 20,
                width: 100,
                height: 80,
            })
        );
    }

    #[test]
    fn copy_rect_clips_positive_overflow_to_bounds() {
        assert_eq!(
            surface_copy_rect_clipped(100, 80, 760, 580, SurfaceBounds::new(800, 600)),
            Some(CopyRect {
                skip_x: 0,
                skip_y: 0,
                dst_x: 760,
                dst_y: 580,
                width: 40,
                height: 20,
            })
        );
    }

    #[test]
    fn copy_rect_clips_negative_offset_then_positive_overflow() {
        assert_eq!(
            surface_copy_rect_clipped(100, 80, -10, 580, SurfaceBounds::new(800, 600)),
            Some(CopyRect {
                skip_x: 10,
                skip_y: 0,
                dst_x: 0,
                dst_y: 580,
                width: 90,
                height: 20,
            })
        );
    }

    #[test]
    fn copy_rect_drops_surfaces_outside_bounds() {
        assert_eq!(
            surface_copy_rect_clipped(100, 80, 800, 10, SurfaceBounds::new(800, 600)),
            None
        );
        assert_eq!(
            surface_copy_rect_clipped(100, 80, 10, 600, SurfaceBounds::new(800, 600)),
            None
        );
    }
}

// Smithay surface-tree traversal — covered by integration paths, not unit tests.
#[allow(clippy::items_after_test_module)]
pub(crate) fn send_frame_callbacks(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}
