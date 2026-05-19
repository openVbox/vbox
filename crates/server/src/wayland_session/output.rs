//! Wayland output (display) plumbing.
//!
//! [`create_output`] builds Smithay's `Output` global with the placeholder
//! geometry our nested compositor advertises. The remaining helpers compute
//! restore sizes when a toplevel exits maximized/fullscreen: the geometry
//! the host viewer should snap back to, capped at the current output's
//! bounds so an oversized cached size from an earlier output config can't
//! over-flow the viewer.
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::{
    DisplayHandle, GlobalDispatch, protocol::wl_output::WlOutput,
};
use smithay::utils::Transform;
use smithay::wayland::output::WlOutputData;

pub(crate) fn create_output<D>(dh: &DisplayHandle, width: u32, height: u32) -> Output
where
    D: GlobalDispatch<WlOutput, WlOutputData> + 'static,
{
    let output = Output::new(
        "vbox-0".into(),
        PhysicalProperties {
            size: (280, 180).into(),
            subpixel: Subpixel::Unknown,
            make: "vbox".into(),
            model: "nested-wayland".into(),
        },
    );
    output.create_global::<D>(dh);
    let mode = Mode {
        size: (width as i32, height as i32).into(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    output
}

pub(crate) fn nonzero_size(size: (u32, u32)) -> (u32, u32) {
    (size.0.max(1), size.1.max(1))
}

pub(crate) fn restore_size_for_mode_entry(
    current_size: (u32, u32),
    output_size: (u32, u32),
) -> (u32, u32) {
    let current_size = nonzero_size(current_size);
    let output_size = nonzero_size(output_size);
    if current_size.0 <= 32 || current_size.1 <= 32 {
        return default_restore_size(output_size);
    }
    (
        current_size.0.min(output_size.0),
        current_size.1.min(output_size.1),
    )
}

pub(crate) fn default_restore_size(output_size: (u32, u32)) -> (u32, u32) {
    let output_size = nonzero_size(output_size);
    let width = ((u64::from(output_size.0) * 7) / 10)
        .max(1)
        .min(u64::from(output_size.0)) as u32;
    let height = ((u64::from(output_size.1) * 7) / 10)
        .max(1)
        .min(u64::from(output_size.1)) as u32;
    nonzero_size((width, height))
}
