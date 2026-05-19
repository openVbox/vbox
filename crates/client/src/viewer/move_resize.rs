//! Manual window move + resize geometry.
//!
//! `winit` on macOS doesn't expose a way to ask AppKit to drive an NSWindow
//! drag from a remote `xdg_toplevel.move`, and X11/Wayland under winit have
//! the same gap for resize-from-content-area. So the viewer drives both
//! gestures itself: it tracks the press-time outer/cursor, recomputes the
//! target outer on every CursorMoved, and writes `set_outer_position` /
//! `request_inner_size` directly.
//!
//! Edge-snap math (cursor-near-edge, left/right half tiling) also lives
//! here. The functions are pure: positions in, target rect out.
//!
//! The state machine for a press → release lifecycle:
//!
//! ```text
//!   Idle → [Pressed] → Dragging → [snap?] → Idle
//!                         ↑
//!         Released ←──────┘   (non-macOS only, kept for grace-period upgrade)
//! ```
#[cfg(not(target_os = "macos"))]
use std::time::Instant;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::window::ResizeDirection;

pub(crate) const RESIZE_BORDER_PX: f64 = 8.0;
pub(crate) const MIN_VIEWER_WIDTH: u32 = 240;
pub(crate) const MIN_VIEWER_HEIGHT: u32 = 160;
#[cfg(test)]
pub(crate) const EDGE_SNAP_DISTANCE_PX: i32 = 18;
pub(crate) const CURSOR_EDGE_SNAP_DISTANCE_PX: f64 = 96.0;
pub(crate) const LEFT_RIGHT_EDGE_SNAP_DISTANCE_PX: f64 = 8.0;
pub(crate) const TITLEBAR_BUTTON_HEIGHT_PX: i32 = 64;
pub(crate) const TITLEBAR_DOUBLE_CLICK_HEIGHT_PX: i32 = 96;
pub(crate) const TITLEBAR_RIGHT_CONTROL_WIDTH_PX: i32 = 120;
// Maximize button hit zone in frame-physical px, measured from the right edge:
// x ∈ [frame_w - LEFT, frame_w - RIGHT]. Width stays narrower than GNOME's
// close-button slot (~30px right) so close clicks aren't swallowed.
pub(crate) const TITLEBAR_MAXIMIZE_LEFT_OFFSET_PX: i32 = 100;
pub(crate) const TITLEBAR_MAXIMIZE_RIGHT_OFFSET_PX: i32 = 60;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResizeDrag {
    pub(crate) direction: ResizeDirection,
    pub(crate) start_outer: PhysicalPosition<i32>,
    pub(crate) start_size: PhysicalSize<u32>,
}

/// Press-time anchors for a manual window drag. See
/// [`compute_manual_move_outer`] for why these are anchored on press time
/// rather than the last committed outer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MoveDrag {
    /// Window-local cursor at mouse-down; the X-component of the absolute
    /// `new_outer = press_outer + (cursor - start_cursor)` mapping.
    pub(crate) start_cursor_window: PhysicalPosition<f64>,
    /// Outer at the press instant. Never changes during a drag.
    pub(crate) press_outer: PhysicalPosition<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EdgeSnapRect {
    pub(crate) position: PhysicalPosition<i32>,
    pub(crate) size: PhysicalSize<u32>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum MoveState {
    #[default]
    Idle,
    // macOS reads back the press-time drag only via `begin_requested_move`,
    // which is gated out — but the variant itself still has to carry the drag
    // so a non-macOS `MoveRequested` can resume it.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Pressed(MoveDrag),
    /// Left button released while still in Pressed state. Only the non-macOS
    /// path uses this — `begin_requested_move` upgrades a recent Released back
    /// to Dragging within `REMOTE_MOVE_RELEASE_GRACE`. macOS lets AppKit drive
    /// the drag, so a Pressed-then-released window just falls back to Idle.
    #[cfg(not(target_os = "macos"))]
    Released {
        drag: MoveDrag,
        at: Instant,
    },
    Dragging(MoveDrag),
}

impl MoveState {
    pub(crate) fn is_dragging(self) -> bool {
        matches!(self, Self::Dragging(_))
    }
}

/// Compute the new host-window outer during a manual move, anchored on the
/// *press-time* outer and cursor. Pinning to `press_outer` (rather than the
/// last committed outer) is what keeps macOS drag stable: winit's CursorMoved
/// positions stay relative to the pre-move outer during a drag, so deltas
/// from the last committed outer accumulate and run the window past the
/// cursor (the "viewer flies off" jitter).
pub(crate) fn compute_manual_move_outer(
    press_outer: PhysicalPosition<i32>,
    start_cursor_window: PhysicalPosition<f64>,
    cursor_window: PhysicalPosition<f64>,
) -> PhysicalPosition<i32> {
    let dx = cursor_window.x - start_cursor_window.x;
    let dy = cursor_window.y - start_cursor_window.y;
    PhysicalPosition::new(
        press_outer.x + dx.round() as i32,
        press_outer.y + dy.round() as i32,
    )
}

pub(crate) fn resize_direction_at(
    position: PhysicalPosition<f64>,
    size: PhysicalSize<u32>,
) -> Option<ResizeDirection> {
    let width = f64::from(size.width.max(1));
    let height = f64::from(size.height.max(1));
    let border = RESIZE_BORDER_PX.min(width / 2.0).min(height / 2.0);

    let west = position.x <= border;
    let east = position.x >= width - border;
    let north = position.y <= border;
    let south = position.y >= height - border;

    match (west, east, north, south) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

pub(crate) fn clamp_manual_resize(
    direction: ResizeDirection,
    left: &mut f64,
    top: &mut f64,
    right: &mut f64,
    bottom: &mut f64,
) {
    let min_width = f64::from(MIN_VIEWER_WIDTH);
    let min_height = f64::from(MIN_VIEWER_HEIGHT);

    if *right - *left < min_width {
        if uses_west_edge(direction) {
            *left = *right - min_width;
        } else {
            *right = *left + min_width;
        }
    }

    if *bottom - *top < min_height {
        if uses_north_edge(direction) {
            *top = *bottom - min_height;
        } else {
            *bottom = *top + min_height;
        }
    }
}

pub(crate) fn uses_west_edge(direction: ResizeDirection) -> bool {
    matches!(
        direction,
        ResizeDirection::West | ResizeDirection::NorthWest | ResizeDirection::SouthWest
    )
}

pub(crate) fn uses_north_edge(direction: ResizeDirection) -> bool {
    matches!(
        direction,
        ResizeDirection::North | ResizeDirection::NorthEast | ResizeDirection::NorthWest
    )
}

pub(crate) fn physical_size_close(expected: PhysicalSize<u32>, actual: PhysicalSize<u32>) -> bool {
    u32_abs_diff(expected.width, actual.width) <= 32
        && u32_abs_diff(expected.height, actual.height) <= 32
}

fn u32_abs_diff(a: u32, b: u32) -> u32 {
    a.max(b) - a.min(b)
}

pub(crate) fn clamp_f64_to_i32(value: f64, min: i32, max: i32) -> i32 {
    value.clamp(f64::from(min), f64::from(max)) as i32
}

#[cfg(test)]
pub(crate) fn edge_tile_rect(
    outer_position: PhysicalPosition<i32>,
    outer_size: PhysicalSize<u32>,
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
) -> Option<EdgeSnapRect> {
    let monitor_left = monitor_position.x;
    let monitor_top = monitor_position.y;
    let monitor_right = monitor_left.saturating_add(monitor_size.width as i32);
    let monitor_bottom = monitor_top.saturating_add(monitor_size.height as i32);

    let outer_left = outer_position.x;
    let outer_top = outer_position.y;
    let outer_right = outer_left.saturating_add(outer_size.width as i32);
    let outer_bottom = outer_top.saturating_add(outer_size.height as i32);

    let snap_west =
        outer_left <= monitor_left + EDGE_SNAP_DISTANCE_PX && outer_right > monitor_left;
    let snap_east =
        outer_right >= monitor_right - EDGE_SNAP_DISTANCE_PX && outer_left < monitor_right;
    let snap_north = outer_top <= monitor_top + EDGE_SNAP_DISTANCE_PX && outer_bottom > monitor_top;
    let snap_south =
        outer_bottom >= monitor_bottom - EDGE_SNAP_DISTANCE_PX && outer_top < monitor_bottom;
    if !(snap_west || snap_east || snap_north || snap_south) {
        return None;
    }

    let rect = tile_rect_for_edges(
        monitor_position,
        monitor_size,
        SnappedEdges {
            west: snap_west,
            east: snap_east,
            north: snap_north,
            south: snap_south,
        },
    );
    reject_unchanged_tile(rect, outer_position, outer_size)
}

pub(crate) fn cursor_edge_tile_rect(
    cursor_screen: PhysicalPosition<f64>,
    outer_position: PhysicalPosition<i32>,
    outer_size: PhysicalSize<u32>,
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
) -> Option<EdgeSnapRect> {
    let edges = cursor_snap_edges(cursor_screen, monitor_position, monitor_size)?;
    let rect = tile_rect_for_edges(monitor_position, monitor_size, edges);
    reject_unchanged_tile(rect, outer_position, outer_size)
}

pub(crate) fn cursor_left_right_tile_rect(
    cursor_screen: PhysicalPosition<f64>,
    outer_position: PhysicalPosition<i32>,
    outer_size: PhysicalSize<u32>,
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    top_inset_px: u32,
) -> Option<EdgeSnapRect> {
    let monitor_left = f64::from(monitor_position.x);
    let monitor_right = monitor_left + f64::from(monitor_size.width.max(1));
    let on_left = cursor_screen.x <= monitor_left + LEFT_RIGHT_EDGE_SNAP_DISTANCE_PX;
    let on_right = cursor_screen.x >= monitor_right - LEFT_RIGHT_EDGE_SNAP_DISTANCE_PX;
    if on_left == on_right {
        return None;
    }
    let monitor_width = monitor_size.width.max(1);
    let visible_height = monitor_size.height.saturating_sub(top_inset_px).max(1);
    let visible_top = monitor_position.y.saturating_add(top_inset_px as i32);
    let half_width = (monitor_width / 2).max(MIN_VIEWER_WIDTH.min(monitor_width));
    let rect = if on_left {
        EdgeSnapRect {
            position: PhysicalPosition::new(monitor_position.x, visible_top),
            size: PhysicalSize::new(half_width, visible_height),
        }
    } else {
        let right_width = monitor_width.saturating_sub(half_width).max(1);
        EdgeSnapRect {
            position: PhysicalPosition::new(
                monitor_position.x.saturating_add(half_width as i32),
                visible_top,
            ),
            size: PhysicalSize::new(right_width, visible_height),
        }
    };
    reject_unchanged_tile(rect, outer_position, outer_size)
}

fn cursor_snap_edges(
    cursor_screen: PhysicalPosition<f64>,
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
) -> Option<SnappedEdges> {
    let monitor_left = f64::from(monitor_position.x);
    let monitor_top = f64::from(monitor_position.y);
    let monitor_right = monitor_left + f64::from(monitor_size.width.max(1));
    let monitor_bottom = monitor_top + f64::from(monitor_size.height.max(1));
    let snap_distance = CURSOR_EDGE_SNAP_DISTANCE_PX;

    let west = cursor_screen.x <= monitor_left + snap_distance;
    let east = cursor_screen.x >= monitor_right - snap_distance;
    let north = cursor_screen.y <= monitor_top + snap_distance;
    let south = cursor_screen.y >= monitor_bottom - snap_distance;

    if west || east || north || south {
        Some(SnappedEdges {
            west,
            east,
            north,
            south,
        })
    } else {
        None
    }
}

/// True for frame coordinates that fall inside the GNOME maximize button hit
/// zone. Used to mirror a guest maximize click onto the host NSWindow zoom
/// toggle — otherwise the wrapper geometry desyncs from the guest's frame.
/// See `TITLEBAR_MAXIMIZE_*_OFFSET_PX` for the geometry.
pub(crate) fn is_titlebar_maximize_button(x: i32, y: i32, frame_width: u32) -> bool {
    let frame_width = frame_width.max(1) as i32;
    (0..=TITLEBAR_BUTTON_HEIGHT_PX).contains(&y)
        && x >= frame_width.saturating_sub(TITLEBAR_MAXIMIZE_LEFT_OFFSET_PX)
        && x <= frame_width.saturating_sub(TITLEBAR_MAXIMIZE_RIGHT_OFFSET_PX)
}

pub(crate) fn is_titlebar_double_click_area(x: i32, y: i32, frame_width: u32) -> bool {
    let frame_width = frame_width.max(1) as i32;
    (0..=TITLEBAR_DOUBLE_CLICK_HEIGHT_PX).contains(&y)
        && x >= 0
        && x < frame_width.saturating_sub(TITLEBAR_RIGHT_CONTROL_WIDTH_PX)
}

#[derive(Debug, Clone, Copy)]
struct SnappedEdges {
    west: bool,
    east: bool,
    north: bool,
    south: bool,
}

fn tile_rect_for_edges(
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    edges: SnappedEdges,
) -> EdgeSnapRect {
    let monitor_left = monitor_position.x;
    let monitor_top = monitor_position.y;
    let monitor_width = monitor_size.width.max(1);
    let monitor_height = monitor_size.height.max(1);
    let half_width = (monitor_width / 2).max(MIN_VIEWER_WIDTH.min(monitor_width));
    let right_width = monitor_width.saturating_sub(half_width).max(1);
    let half_height = (monitor_height / 2).max(MIN_VIEWER_HEIGHT.min(monitor_height));
    let bottom_height = monitor_height.saturating_sub(half_height).max(1);

    let (x, width) = match (edges.west, edges.east) {
        (true, false) => (monitor_left, half_width),
        (false, true) => (monitor_left.saturating_add(half_width as i32), right_width),
        _ => (monitor_left, monitor_width),
    };

    let (y, height) = match (edges.north, edges.south) {
        (true, false) if edges.west || edges.east => (monitor_top, half_height),
        (false, true) if edges.west || edges.east => (
            monitor_top.saturating_add(half_height as i32),
            bottom_height,
        ),
        (false, true) => (
            monitor_top.saturating_add(half_height as i32),
            bottom_height,
        ),
        _ => (monitor_top, monitor_height),
    };

    EdgeSnapRect {
        position: PhysicalPosition::new(x, y),
        size: PhysicalSize::new(width, height),
    }
}

fn reject_unchanged_tile(
    rect: EdgeSnapRect,
    outer_position: PhysicalPosition<i32>,
    outer_size: PhysicalSize<u32>,
) -> Option<EdgeSnapRect> {
    if physical_size_close(rect.size, outer_size)
        && (rect.position.x - outer_position.x).abs() <= 1
        && (rect.position.y - outer_position.y).abs() <= 1
    {
        None
    } else {
        Some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_tile_rect_snaps_left_edge_to_split_view() {
        let rect = edge_tile_rect(
            PhysicalPosition::new(6, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(0, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 900));
    }

    #[test]
    fn edge_tile_rect_accepts_left_edge_overshoot() {
        let rect = edge_tile_rect(
            PhysicalPosition::new(-90, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(0, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 900));
    }

    #[test]
    fn edge_tile_rect_maximizes_at_top_edge() {
        let rect = edge_tile_rect(
            PhysicalPosition::new(200, 8),
            PhysicalSize::new(500, 500),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(0, 0));
        assert_eq!(rect.size, PhysicalSize::new(1440, 900));
    }

    #[test]
    fn edge_tile_rect_snaps_right_edge_to_split_view() {
        let rect = edge_tile_rect(
            PhysicalPosition::new(1034, 100),
            PhysicalSize::new(400, 500),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(720, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 900));
    }

    #[test]
    fn edge_tile_rect_ignores_windows_away_from_edges() {
        assert!(
            edge_tile_rect(
                PhysicalPosition::new(100, 120),
                PhysicalSize::new(640, 360),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1440, 900),
            )
            .is_none()
        );
    }

    #[test]
    fn cursor_edge_tile_rect_uses_cursor_not_window_edge() {
        let rect = cursor_edge_tile_rect(
            PhysicalPosition::new(84.0, 500.0),
            PhysicalPosition::new(420, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(0, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 900));
    }

    #[test]
    fn cursor_edge_tile_rect_supports_corner_quarters() {
        let rect = cursor_edge_tile_rect(
            PhysicalPosition::new(30.0, 30.0),
            PhysicalPosition::new(420, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(0, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 450));
    }

    #[test]
    fn cursor_edge_tile_rect_supports_bottom_half() {
        let rect = cursor_edge_tile_rect(
            PhysicalPosition::new(700.0, 880.0),
            PhysicalPosition::new(420, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
        )
        .unwrap();

        assert_eq!(rect.position, PhysicalPosition::new(0, 450));
        assert_eq!(rect.size, PhysicalSize::new(1440, 450));
    }

    #[test]
    fn cursor_edge_tile_rect_ignores_center_cursor() {
        assert!(
            cursor_edge_tile_rect(
                PhysicalPosition::new(700.0, 500.0),
                PhysicalPosition::new(0, 120),
                PhysicalSize::new(640, 360),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1440, 900),
            )
            .is_none()
        );
    }

    #[test]
    fn cursor_left_right_tile_rect_snaps_to_left_half() {
        let rect = cursor_left_right_tile_rect(
            PhysicalPosition::new(2.0, 450.0),
            PhysicalPosition::new(420, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
            0,
        )
        .unwrap();
        assert_eq!(rect.position, PhysicalPosition::new(0, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 900));
    }

    #[test]
    fn cursor_left_right_tile_rect_snaps_to_right_half() {
        let rect = cursor_left_right_tile_rect(
            PhysicalPosition::new(1438.0, 450.0),
            PhysicalPosition::new(420, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
            0,
        )
        .unwrap();
        assert_eq!(rect.position, PhysicalPosition::new(720, 0));
        assert_eq!(rect.size, PhysicalSize::new(720, 900));
    }

    #[test]
    fn cursor_left_right_tile_rect_ignores_top_and_bottom() {
        assert!(
            cursor_left_right_tile_rect(
                PhysicalPosition::new(700.0, 2.0),
                PhysicalPosition::new(420, 120),
                PhysicalSize::new(640, 360),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1440, 900),
                0,
            )
            .is_none()
        );
        assert!(
            cursor_left_right_tile_rect(
                PhysicalPosition::new(700.0, 898.0),
                PhysicalPosition::new(420, 120),
                PhysicalSize::new(640, 360),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1440, 900),
                0,
            )
            .is_none()
        );
    }

    #[test]
    fn cursor_left_right_tile_rect_ignores_center_cursor() {
        assert!(
            cursor_left_right_tile_rect(
                PhysicalPosition::new(700.0, 450.0),
                PhysicalPosition::new(420, 120),
                PhysicalSize::new(640, 360),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1440, 900),
                0,
            )
            .is_none()
        );
    }

    #[test]
    fn cursor_left_right_tile_rect_applies_top_inset() {
        let rect = cursor_left_right_tile_rect(
            PhysicalPosition::new(2.0, 450.0),
            PhysicalPosition::new(420, 120),
            PhysicalSize::new(640, 360),
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1440, 900),
            50,
        )
        .unwrap();
        assert_eq!(rect.position, PhysicalPosition::new(0, 50));
        assert_eq!(rect.size, PhysicalSize::new(720, 850));
    }

    #[test]
    fn titlebar_maximize_button_hits_narrow_strip_clear_of_close() {
        // Maximize hit zone was re-enabled to drive host NSWindow.set_maximized
        // when the user clicks the GNOME maximize control.
        let center_x =
            1800 - (TITLEBAR_MAXIMIZE_LEFT_OFFSET_PX + TITLEBAR_MAXIMIZE_RIGHT_OFFSET_PX) / 2;
        assert!(is_titlebar_maximize_button(center_x, 18, 1800));
        // Close button range (last ~40px). MUST NOT hit.
        assert!(!is_titlebar_maximize_button(1760, 18, 1800));
        assert!(!is_titlebar_maximize_button(1790, 18, 1800));
        // Outside titlebar vertically.
        assert!(!is_titlebar_maximize_button(center_x, 70, 1800));
        // Empty-frame guard.
        assert!(!is_titlebar_maximize_button(0, 0, 0));
    }

    #[test]
    fn manual_move_does_not_accumulate_outer() {
        // Regression: `update_manual_move` used to base each new outer on
        // `last_committed_outer + (position - start_cursor)`, but `position`
        // keeps coming back in pre-move coordinates on macOS — so the same
        // (dx, dy) was added on every CursorMoved event and the window raced
        // past the cursor. With press_outer-anchored computation a stationary
        // cursor never moves the outer, no matter how many events fire.
        let press_outer = PhysicalPosition::new(442, 80);
        let start = PhysicalPosition::new(360.0, 96.0);
        let pos = PhysicalPosition::new(470.0, 184.0);
        let new1 = compute_manual_move_outer(press_outer, start, pos);
        assert_eq!(new1, PhysicalPosition::new(552, 168));
        let new2 = compute_manual_move_outer(press_outer, start, pos);
        let new3 = compute_manual_move_outer(press_outer, start, pos);
        assert_eq!(new2, new1);
        assert_eq!(new3, new1);
    }

    #[test]
    fn manual_move_outer_follows_cursor_delta() {
        // Sanity: when the cursor does move, the outer follows by exactly
        // the cursor delta (not delta + accumulated drift).
        let press_outer = PhysicalPosition::new(100, 100);
        let start = PhysicalPosition::new(50.0, 30.0);
        let new = compute_manual_move_outer(press_outer, start, PhysicalPosition::new(70.0, 40.0));
        assert_eq!(new, PhysicalPosition::new(120, 110));
    }

    #[test]
    fn titlebar_double_click_area_excludes_right_controls() {
        assert!(is_titlebar_double_click_area(900, 20, 1800));
        assert!(is_titlebar_double_click_area(900, 70, 1800));
        assert!(!is_titlebar_double_click_area(1710, 20, 1800));
        assert!(!is_titlebar_double_click_area(900, 110, 1800));
    }

    #[test]
    fn resize_hit_test_edges_and_corners() {
        let size = PhysicalSize::new(100, 80);

        assert_eq!(
            resize_direction_at(PhysicalPosition::new(1.0, 1.0), size),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_direction_at(PhysicalPosition::new(99.0, 1.0), size),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            resize_direction_at(PhysicalPosition::new(1.0, 79.0), size),
            Some(ResizeDirection::SouthWest)
        );
        assert_eq!(
            resize_direction_at(PhysicalPosition::new(99.0, 79.0), size),
            Some(ResizeDirection::SouthEast)
        );
        assert_eq!(
            resize_direction_at(PhysicalPosition::new(50.0, 1.0), size),
            Some(ResizeDirection::North)
        );
        assert_eq!(
            resize_direction_at(PhysicalPosition::new(50.0, 40.0), size),
            None
        );
    }

    #[test]
    fn programmatic_resize_match_allows_small_rounding() {
        assert!(physical_size_close(
            PhysicalSize::new(800, 600),
            PhysicalSize::new(801, 599)
        ));
        assert!(physical_size_close(
            PhysicalSize::new(3600, 2262),
            PhysicalSize::new(3592, 2250)
        ));
        assert!(!physical_size_close(
            PhysicalSize::new(800, 600),
            PhysicalSize::new(850, 600)
        ));
    }
}
