//! Pure viewer geometry helpers.
//!
//! `app.rs` owns the winit event loop and mutable window state. This module
//! keeps the small coordinate/resize calculations as plain value transforms so
//! they stay testable without a live host window.

use winit::dpi::PhysicalSize;
use winit::window::ResizeDirection;

use crate::viewer::frame::{FrameRect, aspect_fit_rect};
use crate::viewer::move_resize::{ResizeDrag, clamp_f64_to_i32, physical_size_close};

/// Pure compute of the guest-side remote size that matches a given host
/// inner size. The math: divide by the view-scale to undo any DPI scaling the
/// host applied, round to whole pixels, and add back the "hidden" frame
/// (server-side chrome that lives outside the visible content rect).
pub(crate) fn compute_remote_size(
    inner: PhysicalSize<u32>,
    view_scale: f32,
    content_w: u32,
    content_h: u32,
    frame_w: u32,
    frame_h: u32,
) -> (u32, u32) {
    let scale = f64::from(view_scale.max(0.01));
    let hidden_width = frame_w.saturating_sub(content_w);
    let hidden_height = frame_h.saturating_sub(content_h);
    let width = ((f64::from(inner.width.max(1)) / scale).round().max(1.0) as u32)
        .saturating_add(hidden_width);
    let height = ((f64::from(inner.height.max(1)) / scale).round().max(1.0) as u32)
        .saturating_add(hidden_height);
    (width, height)
}

/// Pure conversion from physical pixels (what winit's resize-target payload
/// carries) to logical points (what `request_inner_size` expects on macOS).
pub(crate) fn physical_to_logical_size(physical_w: u32, physical_h: u32, scale: f64) -> (f64, f64) {
    let safe_scale = scale.max(1.0);
    let logical_w = (f64::from(physical_w) / safe_scale).round().max(1.0);
    let logical_h = (f64::from(physical_h) / safe_scale).round().max(1.0);
    (logical_w, logical_h)
}

#[cfg(test)]
pub(crate) fn matches_pending_programmatic_resize(
    pending: Option<PhysicalSize<u32>>,
    actual: PhysicalSize<u32>,
) -> bool {
    pending.is_some_and(|expected| programmatic_resize_matches(expected, actual))
}

fn programmatic_resize_matches(expected: PhysicalSize<u32>, actual: PhysicalSize<u32>) -> bool {
    if physical_size_close(expected, actual) {
        return true;
    }
    let width_applied = physical_size_close(
        PhysicalSize::new(expected.width, 1),
        PhysicalSize::new(actual.width, 1),
    );
    let height_applied = physical_size_close(
        PhysicalSize::new(1, expected.height),
        PhysicalSize::new(1, actual.height),
    );
    (width_applied && actual.height <= expected.height)
        || (height_applied && actual.width <= expected.width)
}

/// Pure cursor mapping from host window coordinates to guest frame
/// coordinates. The guest frame is letterboxed inside the host window, so a
/// host pixel must be shifted into the letterbox rect, scaled by the
/// content-vs-mapping ratio, and clamped to a valid frame index.
pub(crate) fn cursor_to_frame_coords(
    host_x: f64,
    host_y: f64,
    inner: (u32, u32),
    content: FrameRect,
    frame_size: (u32, u32),
) -> (i32, i32) {
    let mapping = aspect_fit_rect(content.w, content.h, inner.0, inner.1);
    let local_x =
        (host_x - f64::from(mapping.x)).clamp(0.0, f64::from(mapping.w.saturating_sub(1)));
    let local_y =
        (host_y - f64::from(mapping.y)).clamp(0.0, f64::from(mapping.h.saturating_sub(1)));
    let frame_x =
        f64::from(content.x) + local_x * f64::from(content.w.max(1)) / f64::from(mapping.w.max(1));
    let frame_y =
        f64::from(content.y) + local_y * f64::from(content.h.max(1)) / f64::from(mapping.h.max(1));
    let max_x = frame_size.0.saturating_sub(1) as i32;
    let max_y = frame_size.1.saturating_sub(1) as i32;
    (
        clamp_f64_to_i32(frame_x, 0, max_x),
        clamp_f64_to_i32(frame_y, 0, max_y),
    )
}

/// Pure rectangle update for the eight resize directions.
pub(crate) fn apply_resize_direction(
    drag: &ResizeDrag,
    cursor_x: f64,
    cursor_y: f64,
) -> (f64, f64, f64, f64) {
    let mut left = f64::from(drag.start_outer.x);
    let mut top = f64::from(drag.start_outer.y);
    let mut right = left + f64::from(drag.start_size.width);
    let mut bottom = top + f64::from(drag.start_size.height);

    match drag.direction {
        ResizeDirection::East => right = cursor_x,
        ResizeDirection::North => top = cursor_y,
        ResizeDirection::NorthEast => {
            top = cursor_y;
            right = cursor_x;
        }
        ResizeDirection::NorthWest => {
            top = cursor_y;
            left = cursor_x;
        }
        ResizeDirection::South => bottom = cursor_y,
        ResizeDirection::SouthEast => {
            right = cursor_x;
            bottom = cursor_y;
        }
        ResizeDirection::SouthWest => {
            left = cursor_x;
            bottom = cursor_y;
        }
        ResizeDirection::West => left = cursor_x,
    }
    (left, top, right, bottom)
}

/// Pure picker for the rect the viewer treats as the guest's visible content.
pub(crate) fn compute_effective_content_rect(
    uses_own_chrome: bool,
    frame_w: u32,
    frame_h: u32,
    content: FrameRect,
) -> FrameRect {
    if uses_own_chrome {
        FrameRect::new(0, 0, frame_w.max(1), frame_h.max(1))
    } else {
        content
    }
}

/// Clamp a frame `(w, h)` pair so neither dimension is zero.
pub(crate) fn clamp_display_size(w: u32, h: u32) -> (u32, u32) {
    (w.max(1), h.max(1))
}

pub(crate) fn host_resize_resync_needed(
    current: PhysicalSize<u32>,
    desired: PhysicalSize<u32>,
) -> bool {
    if physical_size_close(current, desired) {
        return false;
    }
    desired.width > current.width || desired.height > current.height
}

pub(crate) fn consume_programmatic_resize_gate(
    pending: &mut Option<PhysicalSize<u32>>,
    actual: PhysicalSize<u32>,
) -> bool {
    let Some(expected) = pending.take() else {
        return false;
    };
    programmatic_resize_matches(expected, actual)
}
