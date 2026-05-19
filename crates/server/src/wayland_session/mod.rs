mod clipboard;
mod host_clipboard;
mod input_registration;
mod io;
mod output;
mod signal;
mod socket;
mod surface_capture;
mod window_debug;

pub use clipboard::send_clipboard;
pub use input_registration::send_input;

use crate::debug_enabled;
use anyhow::{Context, Result};
use clipboard::{ClipboardRegistration, read_guest_text_selection};
use host_clipboard::HostClipboard;
use input_registration::InputRegistration;
pub(crate) use io::WaylandIo;
use output::{create_output, nonzero_size, restore_size_for_mode_entry};
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::{
    backend::input::{ButtonState, KeyState, Keycode},
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, KeyboardHandle},
        pointer::{ButtonEvent, MotionEvent, PointerHandle},
    },
    output::{Mode, Output, Scale},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
        backend::{ClientData, ClientId, DisconnectReason, GlobalId, ObjectId},
        protocol::{wl_buffer, wl_output, wl_seat, wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            self, CompositorClientState, CompositorHandler, CompositorState, SubsurfaceCachedState,
        },
        output::OutputHandler,
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
    },
};
use socket::{SocketCleanup, bind_wayland_socket};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use surface_capture::{
    CommitBuffer, SurfaceBounds, SurfaceCommit, copy_shm_buffer, copy_shm_buffer_clipped,
    discard_surface_commit, send_frame_callbacks, take_surface_commit,
};
use vbox_proto::{
    FrameTile, InputButtonState, InputEvent, InputKeyState, Message, ViewRequest, WindowEvent,
    WindowGeometry, is_text_mime,
};

const MOVE_UNMAXIMIZE_SUPPRESS_GRACE: Duration = Duration::from_millis(600);

/// Cap on pending text-input v3 events buffered between focus changes. The
/// queue drains when a text-input gets focus; this is the safety bound for
/// pathological cases (rapid focus toggle, client never picking up).
const PENDING_TEXT_INPUT_LIMIT: usize = 32;

pub fn run(req: ViewRequest, io: WaylandIo) -> Result<()> {
    let width = req.width.max(1);
    let height = req.height.max(1);
    let debug = debug_enabled();

    let mut display: Display<App> = Display::new().context("creating Wayland display")?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<App>(&dh);
    let xdg_shell_state = XdgShellState::new::<App>(&dh);
    let shm_state = ShmState::new::<App>(&dh, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "vbox");
    let mut seat_for_caps = seat.clone();
    let pointer = seat_for_caps.add_pointer();
    let keyboard = seat_for_caps
        .add_keyboard(Default::default(), 200, 25)
        .context("adding Wayland keyboard capability")?;
    let output = create_output::<App>(&dh, width, height);
    let data_device_state = DataDeviceState::new::<App>(&dh);
    let text_input_global =
        dh.create_global::<App, ZwpTextInputManagerV3, _>(1, TextInputManagerData);

    let listener = bind_wayland_socket(&req.socket_name)?;
    let _socket_cleanup = SocketCleanup::new(&req.socket_name);
    eprintln!(
        "Wayland compositor ready: WAYLAND_DISPLAY={} (run apps inside the guest)",
        req.socket_name
    );
    if debug {
        eprintln!("debug: compositor size={}x{}", width, height);
    }

    let (tx, rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel();
    let (clipboard_tx, clipboard_rx) = mpsc::channel();
    let _input_registration = InputRegistration::new(input_tx);
    let _clipboard_registration = ClipboardRegistration::new(clipboard_tx);
    let dh_for_state = dh.clone();
    let seat_for_state = seat.clone();
    let mut state = App {
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat: seat_for_state,
        display_handle: dh_for_state,
        pointer,
        keyboard,
        output,
        max_output_size: (width, height),
        output_size: (width, height),
        _text_input_global: text_input_global,
        tx,
        input_rx,
        clipboard_rx,
        host_clipboard: HostClipboard::default(),
        guest_clipboard_serial: 0,
        windows: HashMap::new(),
        surface_to_window: HashMap::new(),
        popup_routes: HashMap::new(),
        popup_order: Vec::new(),
        child_surface_window: HashMap::new(),
        child_surfaces: HashMap::new(),
        child_surface_last_rect: HashMap::new(),
        next_window_id: 1,
        active_window_id: None,
        text_inputs: Vec::new(),
        pending_text_input: VecDeque::new(),
        start: Instant::now(),
        debug,
        commit_count: 0,
        frame_count: 0,
        current_buffers: HashMap::new(),
    };
    let mut clients = Vec::new();

    // SIGUSR1 → "operator wants a window dump" for `./vbox windows`.
    // Idempotent across reconnects; lives on stderr so the operator can
    // capture it from server.log without adding any extra channel.
    signal::install_handler();

    loop {
        if io.disconnected() {
            eprintln!("viewer disconnected; stopping Wayland compositor");
            return Ok(());
        }

        // Drain any pending dump request before touching wayland state so
        // the snapshot reflects the moment SIGUSR1 was observed, not the
        // result of whatever clients we are about to dispatch.
        if signal::take_window_dump_request() {
            let mut stderr = std::io::stderr().lock();
            if let Err(e) = window_debug::dump_windows(&state, &mut stderr) {
                eprintln!("window-dump: error writing dump: {e}");
            }
        }

        while let Some(stream) = listener.accept().context("accepting Wayland client")? {
            eprintln!("Wayland client connected: {stream:?}");
            let client = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
                .context("inserting Wayland client")?;
            clients.push(client);
        }

        display
            .dispatch_clients(&mut state)
            .context("dispatching Wayland clients")?;
        display
            .flush_clients()
            .context("flushing Wayland clients")?;

        while let Ok(event) = state.input_rx.try_recv() {
            state.handle_input_event(event);
        }

        // Drain host→guest clipboard frames before flushing outbound, so
        // a paste request that lands in the same tick as the install is
        // immediately satisfiable.
        while let Ok(payload) = state.clipboard_rx.try_recv() {
            state.apply_host_clipboard(payload);
        }

        while let Some(msg) = io.try_recv() {
            match msg {
                Message::InputEvent(event) => state.handle_input_event(event),
                Message::Clipboard(payload) => state.apply_host_clipboard(payload),
                Message::VolumeChange(change) => super::forward_volume_event(change),
                Message::Goodbye(gb) => {
                    eprintln!("viewer disconnected: {}", gb.reason);
                    return Ok(());
                }
                other if debug => eprintln!("debug: WaylandIo ignored inbound {:?}", other.kind()),
                _ => {}
            }
        }

        for msg in rx.try_iter() {
            io.send(msg).context("writing remote frame")?;
        }

        std::thread::sleep(Duration::from_millis(8));
    }
}

struct App {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
    display_handle: DisplayHandle,
    pointer: PointerHandle<Self>,
    keyboard: KeyboardHandle<Self>,
    output: Output,
    max_output_size: (u32, u32),
    output_size: (u32, u32),
    _text_input_global: GlobalId,
    tx: mpsc::Sender<Message>,
    input_rx: mpsc::Receiver<InputEvent>,
    clipboard_rx: mpsc::Receiver<vbox_proto::Clipboard>,
    host_clipboard: HostClipboard,
    /// Last serial we minted for a guest→host clipboard relay. Used purely
    /// for diagnostics on the wire and never trusted from a peer.
    guest_clipboard_serial: u64,
    windows: HashMap<u64, RemoteWindow>,
    surface_to_window: HashMap<ObjectId, u64>,
    popup_routes: HashMap<ObjectId, PopupRoute>,
    popup_order: Vec<ObjectId>,
    /// Child surfaces (wl_subsurface and xdg_popup descendants) that have
    /// been observed via `commit`, mapped to their root toplevel window id.
    /// Used by [`CompositorHandler::destroyed`] to re-emit the parent
    /// toplevel when a child surface is destroyed without first dropping
    /// its buffer — otherwise the client framebuffer keeps the child's
    /// last opaque pixels (e.g., GTK4 popover backdrops) as a stale ghost
    /// in the top-left of the window. Mirror cleanup is wired into
    /// `toplevel_destroyed` / `popup_destroyed` so this never accumulates
    /// dead entries across reconnects.
    child_surface_window: HashMap<ObjectId, u64>,
    /// Strong handles for child surfaces we may need to repaint from a parent
    /// commit. Subsurface position changes can be applied by committing the
    /// parent, without the child attaching a fresh buffer; keeping the surface
    /// handle lets us resolve its new offset and re-emit the cached child
    /// buffer at that position.
    child_surfaces: HashMap<ObjectId, WlSurface>,
    /// Last observed (offset_x, offset_y, width, height) for each child
    /// surface, so the commit handler can detect when a sub-surface shrinks
    /// or slides (e.g. GTK4 `AdwOverlaySplitView` sliding its sidebar
    /// off-screen). Without this, the parent framebuffer keeps the
    /// previous-position child pixels and only the new child rect gets
    /// overwritten — producing the visible "사이드바 잔상" / popup-edge
    /// torn-pixel flicker during the close animation.
    child_surface_last_rect: HashMap<ObjectId, (i32, i32, u32, u32)>,
    next_window_id: u64,
    active_window_id: Option<u64>,
    text_inputs: Vec<VBoxTextInput>,
    pending_text_input: VecDeque<InputEvent>,
    start: Instant,
    debug: bool,
    commit_count: u64,
    frame_count: u64,
    current_buffers: HashMap<ObjectId, wl_buffer::WlBuffer>,
}

#[derive(Debug)]
struct RemoteWindow {
    toplevel: ToplevelSurface,
    surface: WlSurface,
    size: (u32, u32),
    mode: WindowMode,
    last_move_request_at: Option<Instant>,
}

#[derive(Clone, Debug)]
struct PopupRoute {
    window_id: u64,
    surface: WlSurface,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn popup_route_is_pointer_candidate(
    has_live_buffer: bool,
    route_window_id: u64,
    target_window_id: u64,
) -> bool {
    has_live_buffer && route_window_id == target_window_id
}

fn u32_to_i32_saturating(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

fn popup_constraint_target(
    window_size: (u32, u32),
    parent_x: i32,
    parent_y: i32,
) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (parent_x.saturating_neg(), parent_y.saturating_neg()).into(),
        (
            u32_to_i32_saturating(window_size.0),
            u32_to_i32_saturating(window_size.1),
        )
            .into(),
    )
}

fn popup_geometry_for_window(
    positioner: PositionerState,
    window_size: Option<(u32, u32)>,
    parent_x: i32,
    parent_y: i32,
) -> Rectangle<i32, Logical> {
    match window_size {
        Some(size) => {
            positioner.get_unconstrained_geometry(popup_constraint_target(size, parent_x, parent_y))
        }
        None => positioner.get_geometry(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowMode {
    Normal,
    Maximized { restore_size: (u32, u32) },
    Fullscreen { restore_size: (u32, u32) },
}

impl WindowMode {
    fn is_maximized(self) -> bool {
        matches!(self, Self::Maximized { .. })
    }

    fn is_fullscreen(self) -> bool {
        matches!(self, Self::Fullscreen { .. })
    }

    fn enter_maximized(&mut self, restore_size: (u32, u32)) {
        if self.is_maximized() {
            return;
        }
        *self = Self::Maximized {
            restore_size: nonzero_size(restore_size),
        };
    }

    fn enter_fullscreen(&mut self, restore_size: (u32, u32)) {
        if self.is_fullscreen() {
            return;
        }
        *self = Self::Fullscreen {
            restore_size: nonzero_size(restore_size),
        };
    }

    fn leave_maximized(&mut self) -> Option<(u32, u32)> {
        let Self::Maximized { restore_size } = *self else {
            return None;
        };
        *self = Self::Normal;
        Some(nonzero_size(restore_size))
    }

    fn leave_fullscreen(&mut self) -> Option<(u32, u32)> {
        let Self::Fullscreen { restore_size } = *self else {
            return None;
        };
        *self = Self::Normal;
        Some(nonzero_size(restore_size))
    }
}

impl App {
    fn enqueue_message(&self, msg: Message) {
        let _ = self.tx.send(msg);
    }

    fn window_geom(window: &RemoteWindow) -> WindowGeometry {
        WindowGeometry {
            x: 0,
            y: 0,
            w: window.size.0,
            h: window.size.1,
        }
    }

    fn active_surface(&self) -> Option<WlSurface> {
        let id = self.active_window_id?;
        self.windows.get(&id).map(|window| window.surface.clone())
    }

    fn window_surface(&self, id: u64) -> Option<WlSurface> {
        self.windows.get(&id).map(|window| window.surface.clone())
    }

    fn surface_window_offset(&self, surface: &WlSurface) -> Option<(u64, i32, i32)> {
        let mut current = surface.clone();
        let mut x = 0_i32;
        let mut y = 0_i32;

        loop {
            if let Some(id) = self.surface_to_window.get(&current.id()) {
                return Some((*id, x, y));
            }
            if let Some(route) = self.popup_routes.get(&current.id()) {
                return Some((
                    route.window_id,
                    x.saturating_add(route.x),
                    y.saturating_add(route.y),
                ));
            }

            let location = compositor::with_states(&current, |states| {
                states
                    .cached_state
                    .get::<SubsurfaceCachedState>()
                    .current()
                    .location
            });
            x = x.saturating_add(location.x);
            y = y.saturating_add(location.y);
            current = compositor::get_parent(&current)?;
        }
    }

    fn route_popup(&mut self, surface: &PopupSurface, positioner: PositionerState) -> bool {
        let Some(parent) = surface.get_parent_surface() else {
            return false;
        };
        let Some((window_id, parent_x, parent_y)) = self.surface_window_offset(&parent) else {
            return false;
        };
        let window_size = self.windows.get(&window_id).map(|window| window.size);
        let geometry = popup_geometry_for_window(positioner, window_size, parent_x, parent_y);
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        let route = PopupRoute {
            window_id,
            surface: surface.wl_surface().clone(),
            x: parent_x.saturating_add(geometry.loc.x),
            y: parent_y.saturating_add(geometry.loc.y),
            width: geometry.size.w.max(1) as u32,
            height: geometry.size.h.max(1) as u32,
        };
        let key = route.surface.id();
        self.popup_routes.insert(key.clone(), route);
        self.popup_order.retain(|known| *known != key);
        self.popup_order.push(key);
        true
    }

    fn update_popup_size(&mut self, surface: &WlSurface, width: u32, height: u32) {
        if let Some(route) = self.popup_routes.get_mut(&surface.id()) {
            route.width = width.max(1);
            route.height = height.max(1);
        }
    }

    fn remove_popup_route_for_surface(&mut self, key: &ObjectId) -> Option<u64> {
        let parent_window_id = self.popup_routes.remove(key).map(|route| route.window_id);
        if parent_window_id.is_some() {
            self.popup_order.retain(|known| known != key);
        }
        parent_window_id
    }

    fn clear_child_surface_tracking(&mut self, key: &ObjectId) {
        self.current_buffers.remove(key);
        self.child_surface_window.remove(key);
        self.child_surfaces.remove(key);
        self.child_surface_last_rect.remove(key);
    }

    fn pointer_target_at(
        &self,
        id: u64,
        x: i32,
        y: i32,
    ) -> Option<(WlSurface, i32, i32, i32, i32)> {
        for key in self.popup_order.iter().rev() {
            let Some(route) = self.popup_routes.get(key) else {
                continue;
            };
            if !popup_route_is_pointer_candidate(
                self.current_buffers.contains_key(key),
                route.window_id,
                id,
            ) {
                continue;
            }
            let right = route.x.saturating_add(route.width as i32);
            let bottom = route.y.saturating_add(route.height as i32);
            if x >= route.x && x < right && y >= route.y && y < bottom {
                return Some((route.surface.clone(), route.x, route.y, x, y));
            }
        }

        let window = self.windows.get(&id)?;
        let root_x = x.clamp(0, window.size.0.saturating_sub(1) as i32);
        let root_y = y.clamp(0, window.size.1.saturating_sub(1) as i32);
        Some((window.surface.clone(), 0, 0, root_x, root_y))
    }

    fn handle_input_event(&mut self, event: InputEvent) {
        match event {
            InputEvent::PointerMotion { id, x, y } => self.pointer_motion(id, x, y),
            InputEvent::PointerButton { id, button, state } => {
                self.pointer_button(id, button, state)
            }
            InputEvent::PointerScroll {
                id,
                delta_x_millis,
                delta_y_millis,
            } => self.pointer_scroll(id, delta_x_millis, delta_y_millis),
            InputEvent::Key { id, keycode, state } => self.keyboard_key(id, keycode, state),
            InputEvent::Text { id, text } => self.inject_text(id, &text),
            InputEvent::Focus { id, focused } => {
                if focused {
                    self.focus_window(id);
                } else {
                    self.clear_window_focus(id);
                }
            }
            InputEvent::Resize { id, width, height } => {
                self.request_window_resize(id, width, height)
            }
            InputEvent::ToggleMaximize { id } => self.toggle_window_maximize(id),
            InputEvent::SetFullscreen { id, fullscreen } => {
                self.set_window_fullscreen(id, fullscreen)
            }
            InputEvent::Close { id } => self.request_window_close(id),
            InputEvent::Preedit {
                id,
                text,
                cursor_begin,
                cursor_end,
            } => self.preedit_text_input(id, &text, cursor_begin, cursor_end),
        }
    }

    fn focus_window(&mut self, id: u64) {
        let Some(surface) = self.window_surface(id) else {
            return;
        };
        let changed = self.active_window_id != Some(id);
        self.active_window_id = Some(id);
        if changed {
            self.set_toplevel_activation(Some(id));
        }
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, Some(surface.clone()), serial);
        // smithay does not piggyback clipboard focus on keyboard focus —
        // see host_clipboard.rs for the rationale. Without this, the host
        // can install selections forever and no guest app ever sees them.
        self.sync_clipboard_focus_to_surface(Some(&surface));
        self.set_text_input_focus();
    }

    fn clear_window_focus(&mut self, id: u64) {
        if self.active_window_id == Some(id) {
            self.clear_focus();
        }
    }

    fn clear_focus(&mut self) {
        if self.active_window_id.take().is_some() {
            self.set_toplevel_activation(None);
        }
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, None, serial);
        // Drop the data-device clipboard focus alongside the keyboard;
        // see host_clipboard.rs for why this isn't automatic.
        self.sync_clipboard_focus_to_surface(None);
        self.clear_text_input_focus();
    }

    fn request_window_close(&mut self, id: u64) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        if self.debug {
            eprintln!("debug: request close id={id}");
        }
        window.toplevel.send_close();
    }

    fn request_window_resize(&mut self, id: u64, width: u32, height: u32) {
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        let requested_size = nonzero_size((width, height));
        let was_maximized = window.mode.is_maximized();
        let was_fullscreen = window.mode.is_fullscreen();
        if window.size == requested_size && !was_maximized && !was_fullscreen {
            return;
        }
        if self.debug {
            eprintln!(
                "debug: request resize id={id} size={}x{}",
                requested_size.0, requested_size.1
            );
        }
        self.set_output_size(requested_size);
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        window.mode = WindowMode::Normal;
        window.toplevel.with_pending_state(|state| {
            state.size = Some((requested_size.0 as i32, requested_size.1 as i32).into());
            state.states.unset(xdg_toplevel::State::Maximized);
            state.states.unset(xdg_toplevel::State::Fullscreen);
        });
        window.toplevel.send_configure();
        if was_fullscreen {
            self.enqueue_message(Message::WindowEvent(WindowEvent::FullscreenChanged {
                id,
                fullscreen: false,
            }));
        }
    }

    fn toggle_window_maximize(&mut self, id: u64) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        if window.mode.is_maximized() {
            self.request_window_unmaximize_by_id(id, false);
        } else {
            self.request_window_maximize_by_id(id);
        }
    }

    fn request_window_maximize(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        self.request_window_maximize_by_id(id);
    }

    fn request_window_maximize_by_id(&mut self, id: u64) {
        let target = nonzero_size(self.max_output_size);
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        if window.mode.is_maximized() && window.size == target {
            return;
        }
        let restore_size = match window.mode {
            WindowMode::Maximized { restore_size } => restore_size,
            _ => restore_size_for_mode_entry(window.size, self.max_output_size),
        };
        window.mode.enter_maximized(restore_size);
        if self.debug {
            eprintln!(
                "debug: request maximize id={id} size={}x{}",
                target.0, target.1
            );
        }
        window.toplevel.with_pending_state(|state| {
            state.size = Some((target.0 as i32, target.1 as i32).into());
            state.states.set(xdg_toplevel::State::Maximized);
            state.states.unset(xdg_toplevel::State::Fullscreen);
        });
        window.toplevel.send_configure();
    }

    fn request_window_unmaximize(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        self.request_window_unmaximize_by_id(id, true);
    }

    fn request_window_unmaximize_by_id(&mut self, id: u64, suppress_recent_move: bool) {
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        if suppress_recent_move
            && window
                .last_move_request_at
                .is_some_and(|at| at.elapsed() <= MOVE_UNMAXIMIZE_SUPPRESS_GRACE)
        {
            window.last_move_request_at = None;
            if self.debug {
                eprintln!("debug: ignore drag unmaximize id={id}");
            }
            return;
        }
        let Some(target) = window.mode.leave_maximized() else {
            return;
        };
        if self.debug {
            eprintln!(
                "debug: request unmaximize id={id} restore={}x{}",
                target.0, target.1
            );
        }
        window.toplevel.with_pending_state(|state| {
            state.size = Some((target.0 as i32, target.1 as i32).into());
            state.states.unset(xdg_toplevel::State::Maximized);
        });
        window.toplevel.send_configure();
    }

    fn set_window_fullscreen(&mut self, id: u64, fullscreen: bool) {
        if fullscreen {
            self.request_window_fullscreen_by_id(id);
        } else {
            self.request_window_unfullscreen_by_id(id, false);
        }
    }

    fn request_window_fullscreen(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        self.request_window_fullscreen_by_id(id);
    }

    fn request_window_fullscreen_by_id(&mut self, id: u64) {
        let target = nonzero_size(self.max_output_size);
        // `if let { ... } else { return; }` instead of `let-else` on
        // purpose: when the window is present we may fall through this
        // block as a no-op (already at target) and still want the
        // always-echo below to fire so the host converges. `let-else`
        // would short-circuit before that echo.
        if let Some(window) = self.windows.get_mut(&id) {
            // Restore size: keep the one we captured on the original entry if
            // we're already fullscreen (mirrors the maximize path). For a
            // fresh entry, capture the current size as the restore target.
            let restore_size = match window.mode {
                WindowMode::Fullscreen { restore_size } => restore_size,
                _ => restore_size_for_mode_entry(window.size, self.max_output_size),
            };
            let already_aligned = window.mode.is_fullscreen() && window.size == target;
            if !already_aligned {
                window.mode.enter_fullscreen(restore_size);
                if self.debug {
                    eprintln!(
                        "debug: request fullscreen id={id} size={}x{}",
                        target.0, target.1
                    );
                }
                window.toplevel.with_pending_state(|state| {
                    state.size = Some((target.0 as i32, target.1 as i32).into());
                    state.states.set(xdg_toplevel::State::Fullscreen);
                    state.states.unset(xdg_toplevel::State::Maximized);
                });
                window.toplevel.send_configure();
            }
        } else {
            return;
        }
        // Always echo the canonical state to the host so a client that called
        // SetFullscreen optimistically can converge even when the server side
        // is already aligned (no race).
        self.enqueue_message(Message::WindowEvent(WindowEvent::FullscreenChanged {
            id,
            fullscreen: true,
        }));
    }

    fn request_window_unfullscreen(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        if self.debug {
            let surface_label = format!("{:?}", surface.wl_surface().id());
            eprintln!(
                "trace fs-exit: server.request_window_unfullscreen enter id={id} surface={surface_label}",
            );
        }
        self.request_window_unfullscreen_by_id(id, true);
    }

    fn request_window_unfullscreen_by_id(&mut self, id: u64, suppress_recent_move: bool) {
        // Same `if let { ... } else { return; }` rationale as in
        // request_window_fullscreen_by_id: the body may no-op (mode was
        // already Normal) while the always-echo below still has to
        // re-state the canonical truth back to the host.
        if let Some(window) = self.windows.get_mut(&id) {
            let last_move_ms = window
                .last_move_request_at
                .map(|at| at.elapsed().as_millis());
            let in_grace = window
                .last_move_request_at
                .is_some_and(|at| at.elapsed() <= MOVE_UNMAXIMIZE_SUPPRESS_GRACE);
            if self.debug {
                eprintln!(
                    "trace fs-exit: server.request_window_unfullscreen_by_id id={id} suppress_recent_move={suppress_recent_move} last_move_ms={last_move_ms:?} in_grace={in_grace} mode={:?} size={}x{}",
                    window.mode, window.size.0, window.size.1,
                );
            }
            if suppress_recent_move && in_grace {
                window.last_move_request_at = None;
                if self.debug {
                    eprintln!(
                        "trace fs-exit: server.request_window_unfullscreen_by_id suppressed id={id} (recent move)",
                    );
                    eprintln!("debug: ignore drag unfullscreen id={id}");
                }
                return;
            }
            if let Some(target) = window.mode.leave_fullscreen() {
                if self.debug {
                    eprintln!(
                        "trace fs-exit: server.leave_fullscreen id={id} restore_size={}x{}",
                        target.0, target.1,
                    );
                    eprintln!(
                        "debug: request unfullscreen id={id} restore={}x{}",
                        target.0, target.1
                    );
                }
                window.toplevel.with_pending_state(|state| {
                    state.size = Some((target.0 as i32, target.1 as i32).into());
                    state.states.unset(xdg_toplevel::State::Fullscreen);
                });
                window.toplevel.send_configure();
                if self.debug {
                    eprintln!(
                        "trace fs-exit: server.send_configure id={id} new_size={}x{}",
                        target.0, target.1,
                    );
                }
            }
        } else {
            if self.debug {
                eprintln!(
                    "trace fs-exit: server.request_window_unfullscreen_by_id unknown id={id}",
                );
            }
            return;
        }
        // Always echo so the host converges even when the guest mode was
        // already Normal (e.g. user toggled the macOS shortcut while the
        // guest had already left fullscreen on its own).
        if self.debug {
            eprintln!("trace fs-exit: server.enqueue_fullscreen_changed id={id} fullscreen=false",);
        }
        self.enqueue_message(Message::WindowEvent(WindowEvent::FullscreenChanged {
            id,
            fullscreen: false,
        }));
    }

    fn request_window_minimize(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        if self.debug {
            eprintln!("debug: request minimize id={id}");
        }
        self.enqueue_message(Message::WindowEvent(WindowEvent::Minimized { id }));
    }

    fn request_window_move(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        if self.debug {
            eprintln!("debug: request move id={id}");
        }
        if let Some(window) = self.windows.get_mut(&id) {
            window.last_move_request_at = Some(Instant::now());
        }
        self.enqueue_message(Message::WindowEvent(WindowEvent::MoveRequested { id }));
    }

    fn set_toplevel_activation(&mut self, active_id: Option<u64>) {
        for (id, window) in &self.windows {
            window.toplevel.with_pending_state(|state| {
                if Some(*id) == active_id {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
            });
            window.toplevel.send_configure();
        }
    }

    fn pointer_motion(&mut self, id: u64, x: i32, y: i32) {
        let Some((surface, origin_x, origin_y, x, y)) = self.pointer_target_at(id, x, y) else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.time_msec();
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            Some((
                surface,
                Point::<f64, Logical>::from((f64::from(origin_x), f64::from(origin_y))),
            )),
            &MotionEvent {
                location: Point::<f64, Logical>::from((f64::from(x), f64::from(y))),
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    fn pointer_button(&mut self, id: u64, button: u32, state: InputButtonState) {
        self.focus_window(id);
        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: self.time_msec(),
                button,
                state: match state {
                    InputButtonState::Pressed => ButtonState::Pressed,
                    InputButtonState::Released => ButtonState::Released,
                },
            },
        );
        pointer.frame(self);
    }

    fn pointer_scroll(&mut self, id: u64, delta_x_millis: i32, delta_y_millis: i32) {
        self.focus_window(id);
        let dx = f64::from(delta_x_millis) / 1000.0;
        let dy = f64::from(delta_y_millis) / 1000.0;
        let mut frame = smithay::input::pointer::AxisFrame::new(self.time_msec())
            .source(smithay::backend::input::AxisSource::Wheel);
        if dy != 0.0 {
            frame = frame.value(smithay::backend::input::Axis::Vertical, dy);
            let steps_v120 = (dy / 15.0 * 120.0).round() as i32;
            if steps_v120 != 0 {
                frame = frame.v120(smithay::backend::input::Axis::Vertical, steps_v120);
            }
        }
        if dx != 0.0 {
            frame = frame.value(smithay::backend::input::Axis::Horizontal, dx);
            let steps_v120 = (dx / 15.0 * 120.0).round() as i32;
            if steps_v120 != 0 {
                frame = frame.v120(smithay::backend::input::Axis::Horizontal, steps_v120);
            }
        }
        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn keyboard_key(&mut self, id: u64, keycode: u32, state: InputKeyState) {
        self.focus_window(id);
        let keycode = Keycode::new(keycode + 8);
        let keyboard = self.keyboard.clone();
        keyboard.input(
            self,
            keycode,
            match state {
                InputKeyState::Pressed => KeyState::Pressed,
                InputKeyState::Released => KeyState::Released,
            },
            SERIAL_COUNTER.next_serial(),
            self.time_msec(),
            |_, _, _| FilterResult::<()>::Forward,
        );
    }

    fn inject_text(&mut self, id: u64, text: &str) {
        self.focus_window(id);
        match self.commit_text_input(text) {
            TextInputDelivery::Sent => return,
            TextInputDelivery::WaitingForEnable => {
                if self.synthesize_ascii_text(id, text) {
                    if self.debug {
                        eprintln!("debug: ascii key synthesis fallback while text-input disabled");
                    }
                    return;
                }
                self.queue_pending_text_input(InputEvent::Text {
                    id,
                    text: text.to_string(),
                });
                return;
            }
            TextInputDelivery::Unavailable => {}
        }
        self.synthesize_ascii_text(id, text);
    }

    fn synthesize_ascii_text(&mut self, id: u64, text: &str) -> bool {
        let mut sent_any = false;
        for ch in text.chars() {
            match ascii_keycode(ch) {
                Some((keycode, shifted)) => {
                    sent_any = true;
                    if shifted {
                        self.keyboard_key(id, 42, InputKeyState::Pressed);
                    }
                    self.keyboard_key(id, keycode, InputKeyState::Pressed);
                    self.keyboard_key(id, keycode, InputKeyState::Released);
                    if shifted {
                        self.keyboard_key(id, 42, InputKeyState::Released);
                    }
                }
                None if self.debug => {
                    eprintln!("debug: text input char not mapped yet: U+{:04X}", ch as u32);
                }
                None => {}
            }
        }
        sent_any
    }

    fn time_msec(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn set_text_input_focus(&mut self) {
        let Some(surface) = self.active_surface() else {
            return;
        };
        for input in &mut self.text_inputs {
            if !input.resource.is_alive() {
                continue;
            }
            if !input.resource.id().same_client_as(&surface.id()) {
                input.leave();
                continue;
            }

            let already_focused = input
                .focused_surface
                .as_ref()
                .is_some_and(|focused| focused.id() == surface.id());
            if already_focused {
                continue;
            }

            input.leave();
            input.resource.enter(&surface);
            input.focused_surface = Some(surface.clone());
            if self.debug {
                eprintln!("debug: text-input-v3 enter");
            }
        }
    }

    fn clear_text_input_focus(&mut self) {
        for input in &mut self.text_inputs {
            input.leave();
        }
    }

    fn commit_text_input(&mut self, text: &str) -> TextInputDelivery {
        if text.is_empty() {
            return TextInputDelivery::Sent;
        }
        let Some(surface) = self.active_surface() else {
            return TextInputDelivery::Unavailable;
        };
        let mut has_same_client_text_input = false;
        let mut sent = false;
        for input in &mut self.text_inputs {
            if !input.resource.id().same_client_as(&surface.id()) {
                continue;
            }
            has_same_client_text_input = true;
            if !input.enabled {
                continue;
            }
            input.resource.commit_string(Some(text.to_string()));
            input.resource.done(input.serial);
            sent = true;
            if self.debug {
                eprintln!(
                    "debug: text-input-v3 commit chars={} serial={}",
                    text.chars().count(),
                    input.serial
                );
            }
        }
        if sent {
            TextInputDelivery::Sent
        } else if has_same_client_text_input {
            TextInputDelivery::WaitingForEnable
        } else {
            TextInputDelivery::Unavailable
        }
    }

    fn preedit_text_input(&mut self, id: u64, text: &str, cursor_begin: i32, cursor_end: i32) {
        self.focus_window(id);
        let Some(surface) = self.active_surface() else {
            return;
        };
        let mut has_same_client_text_input = false;
        let mut sent = false;
        for input in &mut self.text_inputs {
            if !input.resource.id().same_client_as(&surface.id()) {
                continue;
            }
            has_same_client_text_input = true;
            if !input.enabled {
                continue;
            }
            let preedit_chars = text.chars().count();
            let preedit = if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            };
            input
                .resource
                .preedit_string(preedit, cursor_begin, cursor_end);
            input.resource.done(input.serial);
            if self.debug {
                eprintln!(
                    "debug: text-input-v3 preedit chars={} cursor=({}, {}) serial={}",
                    preedit_chars, cursor_begin, cursor_end, input.serial
                );
            }
            sent = true;
        }
        if !sent && has_same_client_text_input {
            self.queue_pending_text_input(InputEvent::Preedit {
                id,
                text: text.to_string(),
                cursor_begin,
                cursor_end,
            });
        }
    }

    fn queue_pending_text_input(&mut self, event: InputEvent) {
        if self.pending_text_input.len() >= PENDING_TEXT_INPUT_LIMIT {
            if self.debug {
                eprintln!("debug: dropping pending text-input event: queue full");
            }
            return;
        }
        if self.debug {
            eprintln!(
                "debug: queue pending text-input event count={}",
                self.pending_text_input.len() + 1
            );
        }
        self.pending_text_input.push_back(event);
    }

    fn flush_pending_text_input(&mut self) {
        if self.pending_text_input.is_empty() {
            return;
        }
        if self.debug {
            eprintln!(
                "debug: flush pending text-input events count={}",
                self.pending_text_input.len()
            );
        }
        let pending = std::mem::take(&mut self.pending_text_input);
        for event in pending {
            self.handle_input_event(event);
        }
    }

    fn text_input_mut(&mut self, resource: &ZwpTextInputV3) -> Option<&mut VBoxTextInput> {
        self.text_inputs
            .iter_mut()
            .find(|input| input.resource == *resource)
    }

    fn register_text_input(&mut self, resource: ZwpTextInputV3) {
        let mut input = VBoxTextInput::new(resource);
        if let Some(surface) = self
            .active_surface()
            .filter(|surface| input.resource.id().same_client_as(&surface.id()))
        {
            input.resource.enter(&surface);
            input.focused_surface = Some(surface.clone());
        }
        self.text_inputs.push(input);
        if self.debug {
            eprintln!("debug: text-input-v3 instance registered");
        }
    }

    fn is_root_surface_for_window(&self, surface: &WlSurface, id: u64) -> bool {
        self.surface_to_window
            .get(&surface.id())
            .is_some_and(|window_id| *window_id == id)
    }

    fn resize_root_window(&mut self, id: u64, width: u32, height: u32) -> Option<WindowGeometry> {
        let window = self.windows.get_mut(&id)?;
        let new_size = nonzero_size((width, height));
        if window.size == new_size {
            return None;
        }
        window.size = new_size;
        Some(Self::window_geom(window))
    }

    fn set_output_size(&mut self, size: (u32, u32)) {
        let size = nonzero_size(size);
        if self.output_size == size {
            return;
        }
        self.output_size = size;
        let mode = Mode {
            size: (size.0 as i32, size.1 as i32).into(),
            refresh: 60_000,
        };
        self.output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        self.output.set_preferred(mode);
        if self.debug {
            eprintln!("debug: output resized {}x{}", size.0, size.1);
        }
    }
}

impl BufferHandler for App {
    fn buffer_destroyed(&mut self, buffer: &wl_buffer::WlBuffer) {
        self.current_buffers.retain(|_, cached| cached != buffer);
    }
}

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.commit_count = self.commit_count.saturating_add(1);
        let Some((window_id, offset_x, offset_y)) = self.surface_window_offset(surface) else {
            if self.debug && should_log_count(self.commit_count) {
                eprintln!(
                    "debug: ignored commit count={} surface={} no-window-tree-match",
                    self.commit_count,
                    surface.id().protocol_id()
                );
            }
            discard_surface_commit(surface);
            send_frame_callbacks(surface, self.start.elapsed().as_millis() as u32);
            return;
        };

        let log_commit = self.debug && should_log_count(self.commit_count);
        let commit = take_surface_commit(surface, log_commit);
        if log_commit {
            eprintln!(
                "debug: surface commit count={} surface={} offset=({}, {}) damage={} buffer={}",
                self.commit_count,
                surface.id().protocol_id(),
                offset_x,
                offset_y,
                commit.damage_summary.as_deref().unwrap_or("not-captured"),
                commit.buffer_label()
            );
        }

        // Buffer detach on a child surface (subsurface or popup) means the
        // child's pixels are gone — but our client framebuffer still has
        // them painted at the child's offset. Re-emit the parent toplevel
        // so it overwrites that stale region. This covers Firefox's
        // autocomplete / right-click menus, which hide themselves by
        // `wl_surface.attach(null)` rather than destroying the xdg_popup
        // (so `popup_destroyed` never fires).
        let buffer_detached = matches!(commit.buffer, CommitBuffer::Removed);
        let is_child_surface = !(offset_x == 0
            && offset_y == 0
            && self.is_root_surface_for_window(surface, window_id));
        if is_child_surface {
            // Remember the root toplevel for this child so [`Self::destroyed`]
            // can repaint the parent if the child is dropped silently
            // (GTK4 popovers / Firefox autocomplete tear down their
            // sub-surfaces without re-committing the toplevel).
            self.child_surface_window.insert(surface.id(), window_id);
            self.child_surfaces.insert(surface.id(), surface.clone());
        }

        let prev_child_rect = if is_child_surface {
            self.child_surface_last_rect.get(&surface.id()).copied()
        } else {
            None
        };

        match self.copy_committed_shm_buffer(surface, commit, window_id, offset_x, offset_y) {
            Ok(Some(tile)) => {
                self.update_popup_size(surface, tile.w, tile.h);
                if is_child_surface {
                    let new_rect = (offset_x, offset_y, tile.w, tile.h);
                    let geom_changed = match prev_child_rect {
                        Some(prev) => prev != new_rect,
                        None => false,
                    };
                    self.child_surface_last_rect.insert(surface.id(), new_rect);
                    if geom_changed {
                        // Sub-surface moved or shrank between commits — e.g.
                        // GTK4 `AdwOverlaySplitView` sliding its info-sidebar
                        // off-screen. The previous-position pixels are still
                        // painted in the parent framebuffer; repaint the
                        // parent first so the new child tile (enqueued
                        // below) lands on top of fresh parent pixels rather
                        // than the sidebar's previous-frame left edge.
                        if self.debug {
                            eprintln!(
                                "debug: child-surface geom changed prev={:?} new={:?} → parent repaint",
                                prev_child_rect, new_rect
                            );
                        }
                        self.repaint_window_from_cached_buffer(window_id);
                    }
                }
                let configured_geom = if offset_x == 0
                    && offset_y == 0
                    && self.is_root_surface_for_window(surface, window_id)
                {
                    self.resize_root_window(window_id, tile.w, tile.h)
                } else {
                    None
                };
                self.frame_count = self.frame_count.saturating_add(1);
                if self.debug && should_log_count(self.frame_count) {
                    eprintln!(
                        "debug: frame-tile queued seq={} id={} damage={}x{}+{}+{} stride={} bytes={}",
                        self.frame_count,
                        tile.id,
                        tile.w,
                        tile.h,
                        tile.x,
                        tile.y,
                        tile.stride,
                        tile.bytes.len()
                    );
                }
                if let Some(geom) = configured_geom {
                    if self.debug {
                        eprintln!("debug: remote geometry changed {}x{}", geom.w, geom.h);
                    }
                    self.enqueue_message(Message::WindowEvent(WindowEvent::Configured {
                        id: window_id,
                        geom,
                    }));
                }
                self.enqueue_message(Message::FrameTile(tile));
                if offset_x == 0
                    && offset_y == 0
                    && self.is_root_surface_for_window(surface, window_id)
                {
                    self.repaint_moved_child_surfaces(window_id, false);
                }
            }
            Ok(None) => {
                if self.debug && should_log_count(self.commit_count) {
                    eprintln!(
                        "debug: commit produced no frame count={} surface={}",
                        self.commit_count,
                        surface.id().protocol_id()
                    );
                }
                if offset_x == 0
                    && offset_y == 0
                    && self.is_root_surface_for_window(surface, window_id)
                {
                    self.repaint_moved_child_surfaces(window_id, true);
                }
            }
            Err(e) => eprintln!("failed to copy Wayland surface buffer: {e:#}"),
        }
        if buffer_detached && is_child_surface {
            // Same reasoning as `popup_destroyed`: the client framebuffer
            // is left with the child's last opaque pixels, which would
            // otherwise linger until GTK / the toolkit decides to redraw
            // the parent. Re-emit immediately to close the visual gap.
            let key = surface.id();
            self.remove_popup_route_for_surface(&key);
            self.clear_child_surface_tracking(&key);
            self.repaint_window_from_cached_buffer(window_id);
        }
        send_frame_callbacks(surface, self.start.elapsed().as_millis() as u32);
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        // Track subsurface destruction for cache cleanup, but repaint only
        // when the destroyed surface is a routed xdg_popup. GTK4 popovers
        // cycle internal subsurfaces *while the popup is still open* — every
        // hover effect / widget reflow destroys and recreates a child surface.
        // Repainting for those internal children overwrites the live popup
        // region for one frame before the popup's next commit lands. Real
        // popup dismissal still produces a clean teardown:
        //   - `xdg_popup.destroy` → `popup_destroyed` → parent repaint
        //   - `wl_surface.attach(null) + commit` → commit handler's
        //     `buffer_detached && is_child_surface` → parent repaint
        //   - routed popup wl_surface destruction → parent repaint below
        let key = surface.id();
        let popup_parent_window_id = self.remove_popup_route_for_surface(&key);
        self.current_buffers.remove(&key);
        self.child_surfaces.remove(&key);
        self.child_surface_last_rect.remove(&key);
        if let Some(window_id) = self.child_surface_window.remove(&key) {
            if self.debug {
                eprintln!(
                    "debug: child surface destroyed id={} window={} (no immediate repaint)",
                    key.protocol_id(),
                    window_id
                );
            }
        }
        if let Some(window_id) = popup_parent_window_id {
            if self.debug {
                eprintln!(
                    "debug: popup surface destroyed id={} window={} -> parent repaint",
                    key.protocol_id(),
                    window_id
                );
            }
            self.repaint_window_from_cached_buffer(window_id);
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for App {}

impl SelectionHandler for App {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        source: Option<smithay::wayland::selection::SelectionSource>,
        _seat: Seat<Self>,
    ) {
        // Primary-selection (middle-click paste on Linux) is intentionally
        // out of scope; macOS doesn't expose a separate primary buffer.
        if ty != smithay::wayland::selection::SelectionTarget::Clipboard {
            return;
        }
        let Some(source) = source else {
            // Selection cleared. We *don't* propagate that to the host —
            // pasting host text into the guest after the guest clears its
            // own clipboard is a common workflow and round-tripping the
            // clear would surprise the user.
            return;
        };
        // Pick the first text-like mime the guest source offers. If none
        // match we silently skip — no point shipping a PNG over the wire.
        let mime_types = source.mime_types();
        let Some(mime) = mime_types.iter().find(|m| is_text_mime(m)).cloned() else {
            host_clipboard::clip_trace!(
                self.debug,
                "server.new_selection drop=no_text_mime offered={:?}",
                mime_types
            );
            return;
        };
        // Ask the source to write into our pipe; the reader thread drains
        // it and posts the result through the same outbound channel that
        // carries window events, preserving wire order. CLOEXEC is set so
        // the fds don't leak into anything the guest might exec, and the
        // pipe stays blocking — the source.send side writes synchronously.
        let Some((read_fd, write_fd)) = make_clipboard_pipe() else {
            return;
        };
        if let Err(e) =
            smithay::wayland::selection::data_device::request_data_device_client_selection::<App>(
                &self.seat, mime, write_fd,
            )
        {
            host_clipboard::clip_trace!(
                self.debug,
                "server.new_selection drop=request_failed err={e}"
            );
            // read_fd drops here, closing the read end and unblocking any
            // reader thread we'd otherwise have spawned.
            drop(read_fd);
            return;
        }
        self.guest_clipboard_serial = self.guest_clipboard_serial.saturating_add(1);
        read_guest_text_selection(
            read_fd,
            self.guest_clipboard_serial,
            self.tx.clone(),
            self.debug,
        );
    }

    fn send_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        mime_type: String,
        fd: std::os::unix::io::OwnedFd,
        _seat: Seat<Self>,
        _user_data: &Self::SelectionUserData,
    ) {
        // Entry trace: this is the single most useful diagnostic in the
        // whole host→guest path. If it's missing from the log, smithay
        // never routed the guest paste to us (focus / selection install
        // problem upstream). If it fires but a later guard rejects, the
        // reason will appear on the *next* line.
        host_clipboard::clip_trace!(
            self.debug,
            "server.send_selection enter ty={:?} mime={:?} has_text={}",
            ty,
            mime_type,
            self.host_clipboard.text.is_some()
        );
        if ty != smithay::wayland::selection::SelectionTarget::Clipboard {
            return;
        }
        if !is_text_mime(&mime_type) {
            host_clipboard::clip_trace!(
                self.debug,
                "server.send_selection drop=non_text_mime mime={:?}",
                mime_type
            );
            return;
        }
        let Some(text) = self.host_clipboard.text.as_deref() else {
            host_clipboard::clip_trace!(
                self.debug,
                "server.send_selection drop=no_host_text mime={:?}",
                mime_type
            );
            return;
        };
        // The guest opened the read end of a pipe and is blocking on it;
        // a partial write is fine, but a failure to write anything means
        // the guest disconnected. Either way we close the fd by dropping.
        use std::io::Write;
        let mut file = std::fs::File::from(fd);
        let bytes = text.len();
        if let Err(e) = file.write_all(text.as_bytes()) {
            host_clipboard::clip_trace!(
                self.debug,
                "server.send_selection write_err mime={:?} bytes={} err={e:#}",
                mime_type,
                bytes
            );
        } else {
            host_clipboard::clip_trace!(
                self.debug,
                "server.send_selection wrote mime={:?} bytes={}",
                mime_type,
                bytes
            );
        }
        // Dropping `file` closes the write fd, which is the signal the
        // guest is waiting for: EOF means "that's all the bytes".
    }
}

impl ClientDndGrabHandler for App {}

impl ServerDndGrabHandler for App {}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = self.next_window_id;
        self.next_window_id = self.next_window_id.saturating_add(1);
        let geom = WindowGeometry {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        surface.with_pending_state(|state| {
            state.size = Some((0, 0).into());
        });

        let wl_surface = surface.wl_surface().clone();
        self.surface_to_window.insert(wl_surface.id(), id);
        self.windows.insert(
            id,
            RemoteWindow {
                toplevel: surface.clone(),
                surface: wl_surface,
                size: (geom.w, geom.h),
                mode: WindowMode::Normal,
                last_move_request_at: None,
            },
        );
        self.focus_window(id);

        if self.debug {
            eprintln!(
                "debug: toplevel created id={} title='{}' app_id='{}'",
                id,
                title_for(&surface),
                app_id_for(&surface)
            );
        }
        self.enqueue_message(Message::WindowEvent(WindowEvent::Created {
            id,
            geom,
            title: title_for(&surface),
            app_id: app_id_for(&surface),
        }));
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        if self.route_popup(&surface, positioner) {
            if self.debug {
                if let Some(route) = self.popup_routes.get(&surface.wl_surface().id()) {
                    eprintln!(
                        "debug: popup created id={} offset={}x{} size={}x{}",
                        route.window_id, route.x, route.y, route.width, route.height
                    );
                }
            }
        } else if self.debug {
            eprintln!("debug: popup created without parent window route");
        }
        if let Err(e) = surface.send_configure() {
            eprintln!("failed to configure popup: {e:?}");
        }
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.request_window_move(surface);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if !gtk_wm_requests_enabled() {
            if self.debug {
                eprintln!("debug: ignore GTK maximize_request (VBOX_GTK_WM_REQUESTS off)");
            }
            return;
        }
        self.request_window_maximize(surface);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if !gtk_wm_requests_enabled() {
            if self.debug {
                eprintln!("debug: ignore GTK unmaximize_request (VBOX_GTK_WM_REQUESTS off)");
            }
            return;
        }
        self.request_window_unmaximize(surface);
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        // No VBOX_GTK_WM_REQUESTS gate here on purpose: video players and
        // similar apps drive fullscreen via xdg_toplevel.set_fullscreen and
        // we always want to forward that to the host viewer. The gate on
        // maximize_request is about GTK's own titlebar buttons fighting
        // host-driven maximize state, which doesn't apply to fullscreen.
        self.request_window_fullscreen(surface);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        // See fullscreen_request for why this is intentionally ungated.
        self.request_window_unfullscreen(surface);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.request_window_minimize(surface);
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.route_popup(&surface, positioner);
        surface.send_repositioned(token);
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        let Some(id) = self
            .surface_to_window
            .get(&surface.wl_surface().id())
            .copied()
        else {
            return;
        };
        if self.debug {
            eprintln!(
                "debug: title changed id={} title='{}'",
                id,
                title_for(&surface)
            );
        }
        self.enqueue_message(Message::WindowEvent(WindowEvent::TitleChanged {
            id,
            title: title_for(&surface),
        }));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.surface_to_window.remove(&surface.wl_surface().id()) else {
            return;
        };
        self.windows.remove(&id);
        self.current_buffers.remove(&surface.wl_surface().id());
        self.popup_routes.retain(|_, route| route.window_id != id);
        self.popup_order
            .retain(|key| self.popup_routes.contains_key(key));
        self.child_surface_window
            .retain(|_, window_id| *window_id != id);
        self.child_surfaces
            .retain(|key, _| self.child_surface_window.contains_key(key));
        // The rect cache uses the same object-id keyspace; without
        // explicit cleanup, stale entries from a closed window can
        // collide with a future surface that reuses the object id.
        self.child_surface_last_rect
            .retain(|key, _| self.child_surface_window.contains_key(key));

        if self.active_window_id == Some(id) {
            if let Some(next_id) = self.windows.keys().next().copied() {
                self.focus_window(next_id);
            } else {
                self.clear_focus();
            }
        }
        if self.debug {
            eprintln!("debug: toplevel destroyed id={id}");
        }
        self.enqueue_message(Message::WindowEvent(WindowEvent::Destroyed { id }));
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        let key = surface.wl_surface().id();
        let parent_window_id = self.remove_popup_route_for_surface(&key);
        // The popup's surface is about to be destroyed; drop our child
        // mapping here so the upcoming [`Self::destroyed`] callback doesn't
        // trigger a redundant repaint for the same surface (we're already
        // doing one below) and so the entry doesn't outlive the popup if
        // [`Self::destroyed`] races.
        self.clear_child_surface_tracking(&key);
        if self.debug {
            eprintln!("debug: popup destroyed");
        }

        // GTK4 popovers dismiss with a short fade animation that streams
        // partially-transparent buffers into the popup region, and then the
        // popup surface is destroyed before GTK gets around to re-committing
        // the parent toplevel. The client framebuffer is left with alpha=0
        // pixels where the popup used to be — `pixel_for_softbuffer` paints
        // those as the matte colour, so the parent's header-bar buttons
        // (≡, _, □, ✕) blink to white for one or two host frames before
        // GTK's eventual parent redraw lands. Re-emitting the parent's
        // current cached buffer here closes that gap atomically.
        if let Some(window_id) = parent_window_id {
            self.repaint_window_from_cached_buffer(window_id);
        }
    }
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        eprintln!("Wayland client initialized");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        eprintln!("Wayland client disconnected");
    }
}

#[derive(Clone, Copy, Debug)]
struct TextInputManagerData;

#[derive(Debug)]
struct VBoxTextInput {
    resource: ZwpTextInputV3,
    serial: u32,
    enabled: bool,
    pending_enabled: Option<bool>,
    focused_surface: Option<WlSurface>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextInputDelivery {
    Sent,
    WaitingForEnable,
    Unavailable,
}

impl VBoxTextInput {
    fn new(resource: ZwpTextInputV3) -> Self {
        Self {
            resource,
            serial: 0,
            enabled: false,
            pending_enabled: None,
            focused_surface: None,
        }
    }

    fn leave(&mut self) {
        if let Some(surface) = self.focused_surface.take() {
            self.resource.leave(&surface);
        }
    }
}

impl GlobalDispatch<ZwpTextInputManagerV3, TextInputManagerData, App> for App {
    fn bind(
        _state: &mut App,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _global_data: &TextInputManagerData,
        data_init: &mut DataInit<'_, App>,
    ) {
        data_init.init(resource, TextInputManagerData);
    }
}

impl Dispatch<ZwpTextInputManagerV3, TextInputManagerData, App> for App {
    fn request(
        state: &mut App,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &TextInputManagerData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, App>,
    ) {
        match request {
            zwp_text_input_manager_v3::Request::GetTextInput { id, seat: _ } => {
                let text_input = data_init.init(id, ());
                state.register_text_input(text_input);
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpTextInputV3, (), App> for App {
    fn request(
        state: &mut App,
        _client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, App>,
    ) {
        let mut flush_pending = false;
        {
            let debug = state.debug;
            let Some(input) = state.text_input_mut(resource) else {
                return;
            };

            match request {
                zwp_text_input_v3::Request::Enable => {
                    input.pending_enabled = Some(true);
                }
                zwp_text_input_v3::Request::Disable => {
                    input.pending_enabled = Some(false);
                }
                zwp_text_input_v3::Request::SetSurroundingText { .. }
                | zwp_text_input_v3::Request::SetTextChangeCause { .. }
                | zwp_text_input_v3::Request::SetContentType { .. }
                | zwp_text_input_v3::Request::SetCursorRectangle { .. } => {}
                zwp_text_input_v3::Request::Commit => {
                    input.serial = input.serial.saturating_add(1);
                    if let Some(enabled) = input.pending_enabled.take() {
                        input.enabled = enabled;
                        flush_pending = enabled;
                        if debug {
                            eprintln!(
                                "debug: text-input-v3 {} serial={}",
                                if enabled { "enabled" } else { "disabled" },
                                input.serial
                            );
                        }
                    }
                }
                zwp_text_input_v3::Request::Destroy => {}
                _ => unreachable!(),
            }
        }
        if flush_pending {
            state.flush_pending_text_input();
        }
    }

    fn destroyed(state: &mut App, _client: ClientId, resource: &ZwpTextInputV3, _data: &()) {
        state
            .text_inputs
            .retain(|input| input.resource.id() != resource.id());
        if state.debug {
            eprintln!("debug: text-input-v3 instance destroyed");
        }
    }
}

fn title_for(surface: &ToplevelSurface) -> String {
    compositor::with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().and_then(|role| role.title.clone()))
            .unwrap_or_default()
    })
}

fn app_id_for(surface: &ToplevelSurface) -> String {
    compositor::with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().and_then(|role| role.app_id.clone()))
            .unwrap_or_default()
    })
}

impl App {
    /// Re-emit cached child buffers whose effective window-relative rect
    /// changed during a parent commit. This catches synchronized
    /// `wl_subsurface.set_position` updates: the parent commits the new child
    /// placement, but the child may not attach or damage a buffer of its own.
    /// Without this pass the old child pixels remain in the host framebuffer
    /// until another parent redraw happens.
    fn repaint_moved_child_surfaces(&mut self, window_id: u64, repaint_parent_first: bool) {
        let Some(bounds) = self
            .windows
            .get(&window_id)
            .map(|window| SurfaceBounds::new(window.size.0, window.size.1))
        else {
            return;
        };
        let candidates = self
            .child_surface_window
            .iter()
            .filter_map(|(key, child_window_id)| {
                if *child_window_id != window_id {
                    return None;
                }
                self.child_surfaces
                    .get(key)
                    .cloned()
                    .map(|surface| (key.clone(), surface))
            })
            .collect::<Vec<_>>();

        let mut changed_tiles = Vec::new();
        let mut parent_repaint_needed = false;

        for (key, surface) in candidates {
            let Some((resolved_window_id, offset_x, offset_y)) =
                self.surface_window_offset(&surface)
            else {
                continue;
            };
            if resolved_window_id != window_id {
                continue;
            }

            let Some(buffer) = self.current_buffers.get(&key).cloned() else {
                self.child_surface_last_rect.remove(&key);
                continue;
            };

            match copy_shm_buffer_clipped(&buffer, window_id, offset_x, offset_y, bounds) {
                Ok(Some(tile)) => {
                    let new_rect = (offset_x, offset_y, tile.w, tile.h);
                    if self
                        .child_surface_last_rect
                        .get(&key)
                        .copied()
                        .is_some_and(|prev| prev != new_rect)
                    {
                        if self.debug {
                            eprintln!(
                                "debug: child parent-commit geom changed surface={} new={:?}",
                                key.protocol_id(),
                                new_rect
                            );
                        }
                        self.child_surface_last_rect.insert(key, new_rect);
                        parent_repaint_needed = true;
                        changed_tiles.push(tile);
                    }
                }
                Ok(None) => {
                    let new_rect = (offset_x, offset_y, 0, 0);
                    if self
                        .child_surface_last_rect
                        .get(&key)
                        .copied()
                        .is_some_and(|prev| prev != new_rect)
                    {
                        if self.debug {
                            eprintln!(
                                "debug: child parent-commit moved offscreen surface={} new={:?}",
                                key.protocol_id(),
                                new_rect
                            );
                        }
                        self.child_surface_last_rect.insert(key, new_rect);
                        parent_repaint_needed = true;
                    }
                }
                Err(e) => eprintln!("child parent-commit repaint failed: {e:#}"),
            }
        }

        if parent_repaint_needed && repaint_parent_first {
            self.repaint_window_from_cached_buffer(window_id);
        }
        for tile in changed_tiles {
            self.enqueue_message(Message::FrameTile(tile));
        }
    }

    /// Re-emit the parent toplevel's cached shm buffer as a `FrameTile`
    /// covering the whole window. Used by `popup_destroyed` to overwrite
    /// the popup-region pixels that GTK leaves behind during its fade-out
    /// animation — see the comment in `popup_destroyed` for the failure
    /// mode this prevents. Silent no-op if the parent has no cached buffer
    /// yet (e.g., popup raced its parent's first commit).
    fn repaint_window_from_cached_buffer(&self, window_id: u64) {
        let Some(parent_surface) = self.windows.get(&window_id).map(|w| w.surface.clone()) else {
            return;
        };
        let Some(buffer) = self.current_buffers.get(&parent_surface.id()).cloned() else {
            return;
        };
        match copy_shm_buffer(&buffer, window_id, 0, 0) {
            Ok(Some(tile)) => {
                if self.debug {
                    eprintln!(
                        "debug: popup-dismiss repaint id={} {}x{}+{}+{}",
                        tile.id, tile.w, tile.h, tile.x, tile.y
                    );
                }
                self.enqueue_message(Message::FrameTile(tile));
            }
            Ok(None) => {
                if self.debug {
                    eprintln!("debug: popup-dismiss repaint produced no tile id={window_id}");
                }
            }
            Err(e) => eprintln!("popup-dismiss repaint failed: {e:#}"),
        }
    }

    fn copy_committed_shm_buffer(
        &mut self,
        surface: &WlSurface,
        commit: SurfaceCommit,
        id: u64,
        offset_x: i32,
        offset_y: i32,
    ) -> Result<Option<FrameTile>> {
        let key = surface.id();
        let (buffer, reused) = match commit.buffer {
            CommitBuffer::New(buffer) => {
                self.current_buffers.insert(key, buffer.clone());
                (Some(buffer), false)
            }
            CommitBuffer::Removed => {
                self.current_buffers.remove(&key);
                return Ok(None);
            }
            CommitBuffer::None if commit.damage_count > 0 => {
                (self.current_buffers.get(&key).cloned(), true)
            }
            CommitBuffer::None => (None, true),
        };

        let Some(buffer) = buffer else {
            return Ok(None);
        };

        let is_root_surface = self.is_root_surface_for_window(surface, id);
        let bounds = if is_root_surface {
            None
        } else {
            self.windows
                .get(&id)
                .map(|window| SurfaceBounds::new(window.size.0, window.size.1))
        };
        let tile = match bounds {
            Some(bounds) => copy_shm_buffer_clipped(&buffer, id, offset_x, offset_y, bounds)?,
            None => copy_shm_buffer(&buffer, id, offset_x, offset_y)?,
        };
        if !reused {
            buffer.release();
        }
        Ok(tile)
    }
}

/// Create a `(read, write)` pipe pair with `O_CLOEXEC` for clipboard
/// transfer. Returns `None` on failure with a stderr log; callers treat
/// that as "drop this clipboard event" rather than a session-fatal error.
fn make_clipboard_pipe() -> Option<(std::os::unix::io::OwnedFd, std::os::unix::io::OwnedFd)> {
    use std::os::unix::io::FromRawFd;
    let mut fds: [libc::c_int; 2] = [-1, -1];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("trace clip: server.make_clipboard_pipe pipe2_err err={err}");
        return None;
    }
    // SAFETY: pipe2 succeeded with rc == 0, so both fds are valid and we
    // own them. Wrap each in OwnedFd so Drop closes them on early return.
    let read_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fds[1]) };
    Some((read_fd, write_fd))
}

fn gtk_wm_requests_enabled() -> bool {
    matches!(
        crate::brand::env_var("VBOX_GTK_WM_REQUESTS").as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "on")
    )
}

fn should_log_count(count: u64) -> bool {
    count <= 5 || count % 60 == 0
}

fn ascii_keycode(ch: char) -> Option<(u32, bool)> {
    Some(match ch {
        '1' => (2, false),
        '2' => (3, false),
        '3' => (4, false),
        '4' => (5, false),
        '5' => (6, false),
        '6' => (7, false),
        '7' => (8, false),
        '8' => (9, false),
        '9' => (10, false),
        '0' => (11, false),
        '!' => (2, true),
        '@' => (3, true),
        '#' => (4, true),
        '$' => (5, true),
        '%' => (6, true),
        '^' => (7, true),
        '&' => (8, true),
        '*' => (9, true),
        '(' => (10, true),
        ')' => (11, true),
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '\t' => (15, false),
        'q' => (16, false),
        'Q' => (16, true),
        'w' => (17, false),
        'W' => (17, true),
        'e' => (18, false),
        'E' => (18, true),
        'r' => (19, false),
        'R' => (19, true),
        't' => (20, false),
        'T' => (20, true),
        'y' => (21, false),
        'Y' => (21, true),
        'u' => (22, false),
        'U' => (22, true),
        'i' => (23, false),
        'I' => (23, true),
        'o' => (24, false),
        'O' => (24, true),
        'p' => (25, false),
        'P' => (25, true),
        '[' => (26, false),
        '{' => (26, true),
        ']' => (27, false),
        '}' => (27, true),
        '\n' | '\r' => (28, false),
        'a' => (30, false),
        'A' => (30, true),
        's' => (31, false),
        'S' => (31, true),
        'd' => (32, false),
        'D' => (32, true),
        'f' => (33, false),
        'F' => (33, true),
        'g' => (34, false),
        'G' => (34, true),
        'h' => (35, false),
        'H' => (35, true),
        'j' => (36, false),
        'J' => (36, true),
        'k' => (37, false),
        'K' => (37, true),
        'l' => (38, false),
        'L' => (38, true),
        ';' => (39, false),
        ':' => (39, true),
        '\'' => (40, false),
        '"' => (40, true),
        '`' => (41, false),
        '~' => (41, true),
        '\\' => (43, false),
        '|' => (43, true),
        'z' => (44, false),
        'Z' => (44, true),
        'x' => (45, false),
        'X' => (45, true),
        'c' => (46, false),
        'C' => (46, true),
        'v' => (47, false),
        'V' => (47, true),
        'b' => (48, false),
        'B' => (48, true),
        'n' => (49, false),
        'N' => (49, true),
        'm' => (50, false),
        'M' => (50, true),
        ',' => (51, false),
        '<' => (51, true),
        '.' => (52, false),
        '>' => (52, true),
        '/' => (53, false),
        '?' => (53, true),
        ' ' => (57, false),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
        Anchor, ConstraintAdjustment, Gravity,
    };

    #[test]
    fn nonzero_size_clamps_empty_dimensions() {
        assert_eq!(nonzero_size((1024, 768)), (1024, 768));
        assert_eq!(nonzero_size((0, 0)), (1, 1));
    }

    #[test]
    fn window_mode_keeps_restore_size_only_while_maximized() {
        let mut mode = WindowMode::Normal;

        mode.enter_maximized((640, 480));
        assert_eq!(
            mode,
            WindowMode::Maximized {
                restore_size: (640, 480)
            }
        );

        mode.enter_maximized((800, 600));
        assert_eq!(mode.leave_maximized(), Some((640, 480)));
        assert_eq!(mode, WindowMode::Normal);
        assert_eq!(mode.leave_maximized(), None);
    }

    #[test]
    fn restore_size_for_mode_entry_replaces_tiny_initial_size() {
        assert_eq!(
            restore_size_for_mode_entry((1, 1), (1800, 1129)),
            (1260, 790)
        );
    }

    #[test]
    fn restore_size_for_mode_entry_clamps_to_output() {
        assert_eq!(
            restore_size_for_mode_entry((2400, 1600), (1800, 1129)),
            (1800, 1129)
        );
    }

    #[test]
    fn window_mode_tracks_fullscreen_restore_size() {
        let mut mode = WindowMode::Normal;

        mode.enter_fullscreen((800, 600));
        assert_eq!(
            mode,
            WindowMode::Fullscreen {
                restore_size: (800, 600)
            }
        );

        mode.enter_fullscreen((1200, 900));
        assert_eq!(mode.leave_fullscreen(), Some((800, 600)));
        assert_eq!(mode, WindowMode::Normal);
        assert_eq!(mode.leave_fullscreen(), None);
    }

    #[test]
    fn enter_fullscreen_from_maximized_drops_maximized_restore_size() {
        // Documents the current behaviour: going Maximized → Fullscreen
        // captures the *maximized* size as the new fullscreen restore
        // size (not the pre-maximize size). Leaving fullscreen therefore
        // lands at the maximized size, not the original normal size.
        // Picked up in review — if this ever feels wrong, this test is
        // the place to flip the contract.
        let mut mode = WindowMode::Normal;
        mode.enter_maximized((800, 600));
        assert_eq!(
            mode,
            WindowMode::Maximized {
                restore_size: (800, 600)
            }
        );

        mode.enter_fullscreen((1800, 1129));
        assert_eq!(
            mode,
            WindowMode::Fullscreen {
                restore_size: (1800, 1129)
            }
        );
        assert_eq!(mode.leave_fullscreen(), Some((1800, 1129)));
        assert_eq!(mode, WindowMode::Normal);
    }

    #[test]
    fn leave_fullscreen_is_idempotent_from_normal() {
        // request_window_unfullscreen_by_id relies on this to no-op on
        // `leave_fullscreen()` and still fall through to the always-echo
        // path so the host can converge from a state mismatch.
        let mut mode = WindowMode::Normal;
        assert_eq!(mode.leave_fullscreen(), None);
        assert_eq!(mode, WindowMode::Normal);
    }

    #[test]
    fn popup_route_pointer_candidate_requires_live_buffer_and_target_window() {
        assert!(popup_route_is_pointer_candidate(true, 7, 7));
        assert!(!popup_route_is_pointer_candidate(false, 7, 7));
        assert!(!popup_route_is_pointer_candidate(true, 7, 8));
    }

    #[test]
    fn popup_constraint_target_is_parent_relative() {
        let target = popup_constraint_target((800, 600), 120, 40);

        assert_eq!(target.loc.x, -120);
        assert_eq!(target.loc.y, -40);
        assert_eq!(target.size.w, 800);
        assert_eq!(target.size.h, 600);
    }

    #[test]
    fn popup_geometry_slides_into_window_bounds() {
        let positioner = PositionerState {
            rect_size: (100, 80).into(),
            anchor_rect: Rectangle::new((790, 590).into(), (1, 1).into()),
            anchor_edges: Anchor::BottomRight,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY,
            ..Default::default()
        };

        let geometry = popup_geometry_for_window(positioner, Some((800, 600)), 0, 0);

        assert_eq!(geometry.loc.x, 700);
        assert_eq!(geometry.loc.y, 520);
        assert_eq!(geometry.size.w, 100);
        assert_eq!(geometry.size.h, 80);
    }
}

delegate_xdg_shell!(App);
delegate_compositor!(App);
delegate_shm!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_data_device!(App);
