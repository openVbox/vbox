//! `ViewerApp` (the `ApplicationHandler`) + `ViewerWindow` (one host NSWindow
//! per guest toplevel) + the GUI-side helpers tied to them.
//!
//! Network frames arrive on the winit event-loop as
//! [`ViewerEvent::Message`]; this module reacts to them, updates the
//! per-window [`crate::viewer::frame::FrameBuffer`], and dispatches host
//! keyboard / pointer events back through the input `mpsc::Sender`.
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use vbox_proto::{
    InputButtonState, InputEvent, InputKeyState, Message, WindowEvent as RemoteWindowEvent,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{CursorIcon, ResizeDirection, Theme, Window, WindowAttributes, WindowId};

use crate::app_icon::AppIconCache;
use crate::viewer::adopt::{
    initial_adopt_cap, is_content_rect_adoptable, should_adopt_host_window,
};
#[cfg(not(target_os = "macos"))]
use crate::viewer::env::REMOTE_MOVE_RELEASE_GRACE;
use crate::viewer::env::{
    FULLSCREEN_EXIT_RESYNC_DELAY, HOST_CHROME_ENV, HOST_RESIZE_RESYNC_DELAY,
    INITIAL_HIDDEN_WINDOW_SIZE, INITIAL_WINDOW_CASCADE_PX, INITIAL_WINDOW_X, INITIAL_WINDOW_Y,
    MAX_VIEW_SCALE, MIN_VIEW_SCALE, REMOTE_RESIZE_DEBOUNCE, SCROLL_LINE_TO_AXIS_UNITS,
    TITLEBAR_DOUBLE_CLICK_DISTANCE_PX, TITLEBAR_DOUBLE_CLICK_INTERVAL, VIEW_SCALE_STEP,
    WINDOW_REPLACEMENT_GRACE, edge_snap_enabled, guest_app_uses_own_chrome, host_chrome_enabled,
    macos_menubar_inset_logical_pt, should_log_count,
};
use crate::viewer::frame::{
    DARK_MATTE_PIXEL, DEFAULT_MATTE_PIXEL, FrameBuffer, LIGHT_MATTE_PIXEL, aspect_fit_rect,
    copy_frame_region_preserve_aspect, scaled_len,
};
use crate::viewer::fullscreen::{
    apply_window_fullscreen, is_fullscreen_shortcut, is_window_fullscreen,
};
#[cfg(test)]
use crate::viewer::geometry::matches_pending_programmatic_resize;
use crate::viewer::geometry::{
    apply_resize_direction, clamp_display_size, compute_effective_content_rect,
    compute_remote_size, consume_programmatic_resize_gate, cursor_to_frame_coords,
    host_resize_resync_needed, physical_to_logical_size,
};
use crate::viewer::ime::{self, ComposerAction, InputComposer, emoji};
use crate::viewer::input::{
    KEY_BACKSPACE, KEY_LEFTCTRL, KEY_LEFTSHIFT, guest_modifier_keycode, host_move_modifier_active,
    ime_cursor_range, is_macos_emoji_picker_shortcut, keyboard_command_modifiers_active,
    mouse_button_code, named_keycode, printable_keyboard_text, shortcut_keycode,
    shortcut_modifiers_active, usize_to_i32,
};
use crate::viewer::keyboard_shortcuts::is_window_dump_shortcut;
use crate::viewer::move_resize::{
    EdgeSnapRect, MIN_VIEWER_HEIGHT, MIN_VIEWER_WIDTH, MoveDrag, MoveState, ResizeDrag,
    TITLEBAR_DOUBLE_CLICK_HEIGHT_PX, clamp_manual_resize, compute_manual_move_outer,
    cursor_edge_tile_rect, cursor_left_right_tile_rect, is_titlebar_double_click_area,
    is_titlebar_maximize_button, resize_direction_at, uses_north_edge, uses_west_edge,
};
use crate::viewer::window_debug::{ViewerWindowSnapshot, dump_viewer_windows, snapshot_from_parts};

#[derive(Debug)]
pub(crate) enum ViewerEvent {
    Message(Message),
    Disconnected(String),
    /// Operator asked for a one-shot dump of every viewer window's
    /// state. Carried only on POSIX targets where SIGUSR1
    /// (`crate::viewer::dump_signal`) can land outside the winit
    /// thread and forward the request via `EventLoopProxy`. The
    /// keyboard shortcut path runs `dump_windows_to_stderr` directly
    /// without going through this variant, so non-unix builds need
    /// no equivalent.
    #[cfg(unix)]
    DumpWindows,
}

pub(crate) struct ViewerApp {
    keepalive_window: Option<Arc<Window>>,
    windows: HashMap<u64, ViewerWindow>,
    window_ids: HashMap<WindowId, u64>,
    pending_resizes: HashMap<u64, PendingRemoteResize>,
    pending_fullscreen_exit_resyncs: HashMap<u64, Instant>,
    pending_host_resize_resyncs: HashMap<u64, Instant>,
    /// Unified outbound channel that ferries input events (and clipboard
    /// frames, when the bridge is wired up) to the input-network thread.
    /// Carrying `Message` here lets multiple subsystems share the single
    /// client→server connection without each opening its own.
    outbound_tx: mpsc::Sender<vbox_proto::Message>,
    /// Inbound clipboard relay. `Some` only when [`crate::clipboard::start`]
    /// produced a working bridge (currently: macOS hosts). On `None`
    /// platforms incoming `Message::Clipboard` frames are dropped on the
    /// floor — input/window functionality is unaffected.
    clipboard_inbound: Option<mpsc::Sender<vbox_proto::Clipboard>>,
    modifiers: ModifiersState,
    pressed_guest_keys: HashMap<u32, u64>,
    pressed_guest_modifier_counts: HashMap<u32, usize>,
    retired_window_ids: HashSet<u64>,
    warned_chrome_apps: HashSet<String>,
    // Frozen once at process start so every viewer toplevel in this
    // session shares the same chrome policy — flipping VBOX_HOST_CHROME
    // mid-session would otherwise yield one window with a titlebar and
    // the next without.
    host_chrome_enabled: bool,
    debug: bool,
    tile_screen_size: PhysicalSize<u32>,
    frame_count: u64,
    empty_windows_since: Option<Instant>,
    app_icons: AppIconCache,
}

struct ViewerWindow {
    window: Arc<Window>,
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    title: String,
    app_id: String,
    frame: FrameBuffer,
    composer: InputComposer,
    last_cursor: (i32, i32),
    last_window_cursor: Option<PhysicalPosition<f64>>,
    last_screen_cursor: Option<PhysicalPosition<f64>>,
    redraw_request_count: u64,
    render_count: u64,
    redraw_pending: bool,
    visible: bool,
    view_scale: f32,
    theme_matte: u32,
    tile_screen_size: PhysicalSize<u32>,
    hover_resize_direction: Option<ResizeDirection>,
    active_resize: Option<ResizeDrag>,
    move_state: MoveState,
    pending_programmatic_resize: Option<PhysicalSize<u32>>,
    pending_tile_remote_resize: Option<(u32, u32)>,
    remote_resize_in_flight: Option<(u32, u32)>,
    last_titlebar_click: Option<TitlebarClick>,
    suppress_next_left_release: bool,
    /// Whether the host window has already taken its size from a server
    /// `Configured` event. The first `Configured` adopts the guest's geometry
    /// so the viewer comes up at a sensible size; every subsequent one only
    /// updates the frame buffer. Without this gate the guest compositor
    /// repeatedly overrides the user's manual resize — the toplevel re-issues
    /// a `Configured` with its preferred geometry, we call
    /// `request_scaled_inner_size`, and the host window snaps back to the
    /// guest's size on every iteration ("창이 늘어남").
    adopted_initial_size: bool,
    /// One-shot gate for the FrameTile-driven adoption. Held *separately* from
    /// `adopted_initial_size` so the first real content-rect (after pixels
    /// arrive) can override an oversized initial Configured/Created geometry.
    /// Without this, an app whose toplevel reports a larger geom than its
    /// actual painted content (GTK CSD margins, GNOME 시스템 정보 등) is
    /// locked into the bigger size by the Created/Configured path, and the
    /// short content shows up as a fat white letterbox at the bottom of the
    /// host window ("앱 실행시 여백이 보이는" 버그).
    adopted_from_frame_tile: bool,
    /// Whether the guest app must use the raw frame rect (Firefox, Chrome,
    /// Electron, terminal emulators). For these the alpha-bbox +
    /// uniform-padding trim that builds `frame.content` is harmful: browser
    /// chrome covers the entire surface, and terminal viewports often contain
    /// large uniform empty regions that are still real content. A tight
    /// content rect shrinks the visible region, causing letterboxed fullscreen
    /// and bad click-coordinate mapping. When this flag is true the viewer
    /// uses the raw `(0, 0, frame.width, frame.height)` rect for both.
    uses_own_chrome: bool,
    /// Last fullscreen state we forwarded to the guest. Used by the
    /// `WindowEvent::Resized` handler to notice when the user enters or
    /// leaves macOS native fullscreen (green window button /
    /// `toggleFullScreen:`): winit doesn't emit a dedicated event for
    /// that path, only Resized to the screen size. Without this cache
    /// the host would be in native fullscreen while the guest stayed in
    /// Normal mode, leaving the guest's small toplevel floating inside a
    /// huge grey backdrop — the visual bug the user reported as
    /// "동영상 전체화면과 맥 전체화면후 창버그".
    last_host_fullscreen: bool,
}

#[derive(Debug, Clone, Copy)]
struct TitlebarClick {
    at: Instant,
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Copy)]
struct PendingRemoteResize {
    width: u32,
    height: u32,
    due: Instant,
}

impl ViewerApp {
    pub(crate) fn new(
        outbound_tx: mpsc::Sender<vbox_proto::Message>,
        clipboard_inbound: Option<mpsc::Sender<vbox_proto::Clipboard>>,
        debug: bool,
        tile_screen_size: PhysicalSize<u32>,
    ) -> Self {
        Self {
            keepalive_window: None,
            windows: HashMap::new(),
            window_ids: HashMap::new(),
            pending_resizes: HashMap::new(),
            pending_fullscreen_exit_resyncs: HashMap::new(),
            pending_host_resize_resyncs: HashMap::new(),
            outbound_tx,
            clipboard_inbound,
            modifiers: ModifiersState::empty(),
            pressed_guest_keys: HashMap::new(),
            pressed_guest_modifier_counts: HashMap::new(),
            retired_window_ids: HashSet::new(),
            warned_chrome_apps: HashSet::new(),
            host_chrome_enabled: host_chrome_enabled(),
            debug,
            tile_screen_size,
            frame_count: 0,
            empty_windows_since: None,
            app_icons: AppIconCache::default(),
        }
    }

    fn ensure_keepalive_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.keepalive_window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("vbox")
            .with_decorations(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(
                INITIAL_HIDDEN_WINDOW_SIZE,
                INITIAL_HIDDEN_WINDOW_SIZE,
            ));
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                eprintln!("failed to create keepalive window: {e}");
                event_loop.exit();
                return;
            }
        };
        if self.debug {
            eprintln!("debug: keepalive window created");
        }
        self.keepalive_window = Some(window);
    }

    fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: u64,
        width: u32,
        height: u32,
        title: &str,
        app_id: &str,
    ) {
        let width = width.max(1);
        let height = height.max(1);
        let title = window_title(title).to_string();
        let icon = self.app_icons.apply_for(app_id);
        if let Some(view) = self.windows.get_mut(&id) {
            self.retired_window_ids.remove(&id);
            view.window.set_visible(true);
            view.set_window_title(title);
            view.app_id = app_id.to_string();
            view.uses_own_chrome |= guest_app_uses_own_chrome(app_id, &view.title).is_some();
            if let Some(icon) = icon.as_ref() {
                view.window.set_window_icon(Some((**icon).clone()));
            }
            view.resize_to_remote_size(width, height);
            return;
        }
        self.maybe_warn_about_guest_chrome(app_id, &title);
        let uses_own_chrome = guest_app_uses_own_chrome(app_id, &title).is_some();
        let attrs =
            viewer_window_attributes(&title, icon.as_deref().cloned(), self.host_chrome_enabled);
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let window_id = window.id();
        let theme_matte = matte_for_theme(event_loop.system_theme().or_else(|| window.theme()));
        set_initial_window_position(&window, id);
        configure_host_window(&window);
        window.set_ime_allowed(true);
        window.set_ime_cursor_area(
            LogicalPosition::new(12.0, 24.0),
            LogicalSize::new(1.0, 20.0),
        );
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(context) => context,
            Err(e) => {
                eprintln!("failed to create softbuffer context: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(surface) => surface,
            Err(e) => {
                eprintln!("failed to create softbuffer surface: {e}");
                event_loop.exit();
                return;
            }
        };
        self.window_ids.insert(window_id, id);
        self.windows.insert(
            id,
            ViewerWindow {
                window,
                _context: context,
                surface,
                title,
                app_id: app_id.to_string(),
                frame: FrameBuffer::new(INITIAL_HIDDEN_WINDOW_SIZE, INITIAL_HIDDEN_WINDOW_SIZE),
                composer: InputComposer::default(),
                last_cursor: (0, 0),
                last_window_cursor: None,
                last_screen_cursor: None,
                redraw_request_count: 0,
                render_count: 0,
                redraw_pending: false,
                visible: false,
                view_scale: 1.0,
                theme_matte,
                tile_screen_size: self.tile_screen_size,
                hover_resize_direction: None,
                active_resize: None,
                move_state: MoveState::Idle,
                pending_programmatic_resize: None,
                pending_tile_remote_resize: None,
                remote_resize_in_flight: None,
                last_titlebar_click: None,
                suppress_next_left_release: false,
                adopted_initial_size: false,
                adopted_from_frame_tile: false,
                uses_own_chrome,
                last_host_fullscreen: false,
            },
        );
        if let Some(view) = self.windows.get_mut(&id) {
            view.resize_to_remote_size(width, height);
        }
    }

    fn active_window_count(&self) -> usize {
        self.windows
            .keys()
            .filter(|id| !self.retired_window_ids.contains(id))
            .count()
    }

    fn remove_window(&mut self, id: u64) {
        self.retired_window_ids.remove(&id);
        self.pending_resizes.remove(&id);
        self.pending_fullscreen_exit_resyncs.remove(&id);
        self.pending_host_resize_resyncs.remove(&id);
        if let Some(view) = self.windows.remove(&id) {
            self.window_ids.remove(&view.window.id());
        }
        self.release_window_guest_keys(id);
    }

    fn retire_last_window(&mut self, id: u64) {
        if let Some(view) = self.windows.get_mut(&id) {
            view.hide();
        }
        self.pending_resizes.remove(&id);
        self.pending_fullscreen_exit_resyncs.remove(&id);
        self.pending_host_resize_resyncs.remove(&id);
        self.release_window_guest_keys(id);
        self.retired_window_ids.insert(id);
    }

    fn remove_retired_windows(&mut self) {
        let retired: Vec<u64> = self.retired_window_ids.drain().collect();
        for id in retired {
            self.remove_window(id);
        }
    }

    fn handle_message(&mut self, event_loop: &ActiveEventLoop, msg: Message) {
        match msg {
            Message::WindowEvent(event) => self.handle_remote_window_event(event, event_loop),
            Message::FrameTile(tile) => {
                let Some(view) = self.windows.get_mut(&tile.id) else {
                    eprintln!("dropping frame tile for unknown window id={}", tile.id);
                    return;
                };
                let display_size_changed = match view.frame.apply_tile(&tile) {
                    Ok(display_size_changed) => display_size_changed,
                    Err(e) => {
                        eprintln!("dropping invalid frame tile: {e:#}");
                        return;
                    }
                };
                let queue_host_resize_resync = view.remote_resize_in_flight.take().is_some()
                    && view.needs_host_resize_resync_after_remote();
                // Only the *first* content-rect change adopts the host
                // window; later tile-driven content shifts (e.g. Firefox
                // relayouts during launch) must NOT keep resizing the host
                // window or the wrapper flickers visibly. Same rationale as
                // resize_to_remote_size.
                //
                // The FrameTile path uses its OWN one-shot gate
                // (`adopted_from_frame_tile`) so the first real content-rect
                // can correct an oversized geometry adopted by Created /
                // Configured (the toplevel may report a larger geom than the
                // actual painted content; without this override the user sees
                // a fat letterbox margin under the app).
                if should_adopt_host_window(view.adopted_from_frame_tile, display_size_changed) {
                    // Extra gate on this path only: refuse to adopt a
                    // partial-paint content rect (Firefox at launch reports
                    // ~2000×80 — just the tab bar — and we'd lock the host
                    // window into a wide horizontal strip). Stay in the
                    // "not yet adopted" state and let a fuller frame win.
                    let (cw, ch) = view.frame.display_size();
                    if is_content_rect_adoptable(cw, ch) {
                        view.adopted_from_frame_tile = true;
                        view.mark_adopted_initial_size();
                        view.request_scaled_inner_size();
                    } else if self.debug {
                        eprintln!(
                            "debug: deferring frame-tile adoption id={} content={}x{} (not adoptable)",
                            tile.id, cw, ch
                        );
                    }
                }
                // Post-adoption host sync is intentionally one-shot and tied
                // to a resize we actually sent to the guest. That catches the
                // important case where the user shrinks below a GTK min-size:
                // the guest returns a larger frame, so we restore the host to
                // that frame after the drag settles. We still avoid the old
                // "follow every content-rect change" loop that made GTK graph
                // apps jitter as live content shifted by a few pixels.
                self.frame_count = self.frame_count.saturating_add(1);
                if self.debug && should_log_count(self.frame_count) {
                    eprintln!(
                        "debug: frame-tile recv seq={} id={} damage={}x{}+{}+{} stride={} bytes={} framebuffer={}x{} display={}x{}+{}+{}",
                        self.frame_count,
                        tile.id,
                        tile.w,
                        tile.h,
                        tile.x,
                        tile.y,
                        tile.stride,
                        tile.bytes.len(),
                        view.frame.width,
                        view.frame.height,
                        view.frame.content.w,
                        view.frame.content.h,
                        view.frame.content.x,
                        view.frame.content.y
                    );
                }
                view.request_redraw(self.debug, "frame-tile", self.frame_count);
                view.render(self.debug, "frame-tile", self.frame_count);
                if queue_host_resize_resync {
                    self.queue_host_resize_resync(tile.id);
                }
            }
            Message::Goodbye(gb) => {
                eprintln!("server closed viewer: {}", gb.reason);
                event_loop.exit();
            }
            Message::Error(e) => {
                eprintln!("server error {}: {}", e.code, e.message);
                event_loop.exit();
            }
            Message::Clipboard(payload) => {
                if let Some(inbound) = &self.clipboard_inbound {
                    if inbound.send(payload).is_err() && self.debug {
                        eprintln!("debug: clipboard inbound channel closed");
                    }
                } else if self.debug {
                    eprintln!("debug: dropping inbound clipboard (no bridge on this platform)");
                }
            }
            other => eprintln!("viewer ignoring {:?}", other.kind()),
        }
    }

    fn handle_remote_window_event(
        &mut self,
        event: RemoteWindowEvent,
        event_loop: &ActiveEventLoop,
    ) {
        match event {
            RemoteWindowEvent::Created {
                id,
                geom,
                title,
                app_id,
            } => {
                self.empty_windows_since = None;
                if self.debug {
                    eprintln!(
                        "debug: remote window created id={} title='{}' app_id='{}' geom={}x{}",
                        id, title, app_id, geom.w, geom.h
                    );
                }
                let width = geom.w.max(1);
                let height = geom.h.max(1);
                self.create_window(event_loop, id, width, height, &title, &app_id);
            }
            RemoteWindowEvent::Configured { id, geom } => {
                if self.debug {
                    eprintln!(
                        "debug: remote window configured id={} geom={}x{}",
                        id, geom.w, geom.h
                    );
                }
                let width = geom.w.max(1);
                let height = geom.h.max(1);
                if let Some(view) = self.windows.get_mut(&id) {
                    view.resize_to_remote_size(width, height);
                    view.request_redraw(self.debug, "remote-configured", self.frame_count);
                }
            }
            RemoteWindowEvent::TitleChanged { id, title } => {
                if self.debug {
                    eprintln!("debug: remote title changed id={id} title='{title}'");
                }
                let mut warn_about_chrome = None;
                if let Some(view) = self.windows.get_mut(&id) {
                    let title = window_title(&title).to_string();
                    let uses_own_chrome = guest_app_uses_own_chrome(&view.app_id, &title).is_some();
                    let newly_detected_own_chrome = uses_own_chrome && !view.uses_own_chrome;
                    if newly_detected_own_chrome {
                        view.uses_own_chrome = true;
                        view.request_scaled_inner_size();
                        view.request_redraw(
                            self.debug,
                            "own-chrome-title-detected",
                            self.frame_count,
                        );
                        if self.debug {
                            eprintln!(
                                "debug: own-chrome detected after title change id={id} app_id='{}'",
                                view.app_id
                            );
                        }
                    }
                    if newly_detected_own_chrome {
                        warn_about_chrome = Some((view.app_id.clone(), title.clone()));
                    }
                    view.set_window_title(title);
                }
                if let Some((app_id, title)) = warn_about_chrome {
                    self.maybe_warn_about_guest_chrome(&app_id, &title);
                }
            }
            RemoteWindowEvent::Minimized { id } => {
                if self.debug {
                    eprintln!("debug: remote window minimized id={id}");
                }
                if let Some(view) = self.windows.get(&id) {
                    view.window.set_minimized(true);
                }
            }
            RemoteWindowEvent::MoveRequested { id } => {
                if self.debug {
                    eprintln!("debug: remote window move requested id={id}");
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let resize = self.windows.get_mut(&id).and_then(|view| {
                        view.begin_requested_move(self.debug, id);
                        view.take_pending_tile_remote_resize()
                    });
                    if let Some((width, height)) = resize {
                        self.queue_remote_resize(id, width, height);
                    }
                }
            }
            RemoteWindowEvent::FullscreenChanged { id, fullscreen } => {
                if self.debug {
                    eprintln!("debug: remote fullscreen changed id={id} fullscreen={fullscreen}");
                }
                let mut schedule_post_exit_resync = false;
                if let Some(view) = self.windows.get_mut(&id) {
                    if self.debug {
                        let inner = view.window.inner_size();
                        let was_fs = is_window_fullscreen(&view.window);
                        eprintln!(
                            "trace fs-exit: client.FullscreenChanged enter id={id} target={fullscreen} was_fs={was_fs} inner={}x{} last_host_fs={} frame={}x{}",
                            inner.width,
                            inner.height,
                            view.last_host_fullscreen,
                            view.frame.width,
                            view.frame.height,
                        );
                    }
                    view.pending_programmatic_resize = None;
                    apply_window_fullscreen(&view.window, fullscreen);
                    // Keep the cache in lockstep with the guest-side state
                    // we just applied. Otherwise the next Resized would see
                    // `last_host_fullscreen=false` while is_window_fullscreen
                    // is true, classify it as a user-entered fullscreen, and
                    // bounce another SetFullscreen back to the guest.
                    view.last_host_fullscreen = fullscreen;
                    if self.debug {
                        let inner = view.window.inner_size();
                        let now_fs = is_window_fullscreen(&view.window);
                        eprintln!(
                            "trace fs-exit: client.apply_window_fullscreen applied id={id} now_fs={now_fs} inner={}x{}",
                            inner.width, inner.height,
                        );
                    }
                    if !fullscreen {
                        // While the host was fullscreen we deliberately
                        // stopped forwarding winit Resized events to the
                        // guest (otherwise the fullscreen mode would flip
                        // back to Normal in a loop). On exit, the guest
                        // and host can be out of sync — the guest still
                        // holds the pre-fullscreen restore size, while
                        // the host inner_size now reflects content area
                        // minus the macOS titlebar. Send one resize so
                        // the guest catches up to whatever the host
                        // window actually shows.
                        schedule_post_exit_resync = true;
                        if self.debug {
                            eprintln!("trace fs-exit: client.schedule_post_exit_resync id={id}",);
                        }
                    }
                }
                if fullscreen {
                    self.pending_fullscreen_exit_resyncs.remove(&id);
                    self.pending_resizes.remove(&id);
                    self.pending_host_resize_resyncs.remove(&id);
                } else if schedule_post_exit_resync {
                    self.queue_fullscreen_exit_resync(id);
                }
            }
            RemoteWindowEvent::Destroyed { id } => {
                if self.debug {
                    eprintln!("debug: remote window destroyed id={id}");
                }
                if self.active_window_count() <= 1 && self.windows.contains_key(&id) {
                    self.retire_last_window(id);
                    self.empty_windows_since = Some(Instant::now());
                    event_loop.set_control_flow(ControlFlow::WaitUntil(
                        Instant::now() + WINDOW_REPLACEMENT_GRACE,
                    ));
                } else {
                    self.remove_window(id);
                }
            }
        }
    }
}

impl ViewerWindow {
    fn resize_to_remote_size(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        let size_changed = self.frame.width != width || self.frame.height != height;
        self.frame.resize(width, height);
        if !size_changed {
            return;
        }
        // First Configured wins: mirroring every Configured regresses Firefox
        // launch (repeated relayouts → flicker). See should_adopt_host_window.
        //
        // Post-adoption resize-back-to-content is handled in the FrameTile
        // path (where the real content rect lives) so we don't fire two
        // host inner_size requests for the same guest resize — one off the
        // buffer-size Configured and another off the trimmed-content tile.
        if !should_adopt_host_window(self.adopted_initial_size, true) {
            return;
        }
        self.adopted_initial_size = true;
        // First adoption is capped at 70% of monitor so monitor-sized guests
        // don't seize the screen. See initial_adopt_cap.
        if let Some(monitor) = self.window.current_monitor() {
            let scale = self.window.scale_factor().max(1.0);
            let mon_logical_w = f64::from(monitor.size().width.max(1)) / scale;
            let mon_logical_h = f64::from(monitor.size().height.max(1)) / scale;
            if let Some((target_w, target_h)) = initial_adopt_cap(
                f64::from(width),
                f64::from(height),
                mon_logical_w,
                mon_logical_h,
                f64::from(MIN_VIEWER_WIDTH),
                f64::from(MIN_VIEWER_HEIGHT),
            ) {
                let logical = LogicalSize::new(target_w, target_h);
                let physical: PhysicalSize<u32> = logical.to_physical(scale);
                self.pending_programmatic_resize = Some(physical);
                let _ = self.window.request_inner_size(logical);
                return;
            }
        }
        self.request_scaled_inner_size();
    }

    /// Post-adoption host inner_size sync. Called when the guest reports a new
    /// toplevel size *after* initial adoption — typically because the guest's
    /// min-size or layout constraints stopped it from following the user's
    /// shrink. We bump the host back up to whatever the guest could actually
    /// render so the visible frame fills the host inner instead of being
    /// letterboxed.
    ///
    /// Guards:
    /// - Skip while the user is actively dragging the host (manual move /
    ///   resize, or in fullscreen) — touching inner_size mid-gesture fights
    ///   the user.
    /// - Skip when the host inner is already close to the guest size — the
    ///   common case during a smooth user drag where the guest follows
    ///   exactly. `physical_size_close`'s ±32px tolerance matches the
    ///   programmatic-resize dedup, so a small rounding gap won't trigger a
    ///   pointless `request_inner_size`.
    /// - Mark the request as programmatic so the resulting `WindowEvent::Resized`
    ///   is swallowed by `consume_programmatic_resize` and we don't bounce
    ///   the size back to the server as a user resize.
    fn request_redraw(&mut self, debug: bool, reason: &str, frame_count: u64) {
        self.redraw_request_count = self.redraw_request_count.saturating_add(1);
        if debug && should_log_count(self.redraw_request_count) {
            eprintln!(
                "debug: redraw requested count={} reason={} frame={}",
                self.redraw_request_count, reason, frame_count
            );
        }
        self.redraw_pending = true;
        self.window.request_redraw();
    }

    fn render_pending_redraw(&mut self, debug: bool, frame_count: u64) {
        if !self.redraw_pending {
            return;
        }
        self.render(debug, "redraw-request", frame_count);
    }

    fn render(&mut self, debug: bool, reason: &str, frame_count: u64) {
        let (target_width, target_height) = self.render_size();
        let Some(width) = NonZeroU32::new(target_width) else {
            return;
        };
        let Some(height) = NonZeroU32::new(target_height) else {
            return;
        };
        if let Err(e) = self.surface.resize(width, height) {
            eprintln!("resize failed: {e}");
            return;
        }
        // Precompute the source rect before touching surface.buffer_mut(),
        // which borrows `self` mutably — calling `effective_content_rect()`
        // inside the copy call after the mutable borrow would conflict.
        let content_rect = self.effective_content_rect();
        let mut buffer = match self.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(e) => {
                eprintln!("buffer failed: {e}");
                return;
            }
        };
        let target_pixels = target_width as usize * target_height as usize;
        if buffer.len() != target_pixels {
            eprintln!(
                "softbuffer size mismatch: reason={} buffer={} pixels={}",
                reason,
                buffer.len(),
                target_pixels
            );
            return;
        }
        // Letterbox color: prefer the frame's running average (matches the
        // visible content's hue) but fall back to the host theme matte if
        // the frame buffer hasn't painted yet. Without this fallback a
        // resize that races ahead of a fresh frame tile flashes the user
        // with the hard-coded DEFAULT_MATTE_PIXEL (0x181818 dark gray),
        // which is what the user saw as "깜빡거리면서 늘어남" during macOS
        // Split View.
        let letterbox = if self.frame.matte == DEFAULT_MATTE_PIXEL {
            self.theme_matte
        } else {
            self.frame.matte
        };
        copy_frame_region_preserve_aspect(
            &self.frame.pixels,
            self.frame.width as usize,
            content_rect,
            letterbox,
            &mut buffer,
            target_width as usize,
            target_height as usize,
        );
        self.window.pre_present_notify();
        if let Err(e) = buffer.present() {
            eprintln!("present failed: {e}");
            return;
        }
        self.redraw_pending = false;
        self.show_after_first_present();
        self.render_count = self.render_count.saturating_add(1);
        if debug && should_log_count(self.render_count) {
            eprintln!(
                "debug: softbuffer present count={} reason={} frame={} size={}x{} pixels={} display={}x{}+{}+{}",
                self.render_count,
                reason,
                frame_count,
                target_width,
                target_height,
                target_pixels,
                self.frame.content.w,
                self.frame.content.h,
                self.frame.content.x,
                self.frame.content.y
            );
        }
    }

    fn render_size(&self) -> (u32, u32) {
        if !self.visible {
            return self.effective_display_size();
        }
        let size = self.window.inner_size();
        (size.width.max(1), size.height.max(1))
    }

    /// Source rect that the viewer treats as "the guest window content".
    ///
    /// Default branch returns `frame.content` — the alpha-bbox +
    /// uniform-padding trimmed sub-rect, which strips CSD shadow margins
    /// and uniformly-colored padding so a GTK toplevel reports the
    /// *visible* size to the host.
    ///
    /// Raw-frame branch (Firefox, Chrome, terminal emulators, …): these apps
    /// paint chrome or legitimate empty viewports to the surface edges, so
    /// trimming can false-positive on a chunk of uniform pixels. The trim then
    /// shifts or shrinks the source rect, breaking fullscreen layout and
    /// click-coordinate mapping. Returning the raw buffer rect keeps host and
    /// guest aligned 1:1 for those apps.
    fn effective_content_rect(&self) -> crate::viewer::frame::FrameRect {
        compute_effective_content_rect(
            self.uses_own_chrome,
            self.frame.width,
            self.frame.height,
            self.frame.content,
        )
    }

    fn effective_display_size(&self) -> (u32, u32) {
        let rect = self.effective_content_rect();
        clamp_display_size(rect.w, rect.h)
    }

    fn enable_ime_at_last_cursor(&self) {
        self.window.set_ime_allowed(true);
        self.update_ime_cursor_area(self.last_cursor.0, self.last_cursor.1);
    }

    fn set_view_scale(&mut self, scale: f32) {
        self.view_scale = clamp_view_scale(scale);
        self.request_scaled_inner_size();
        self.request_redraw(false, "view-scale", 0);
    }

    fn set_theme(&mut self, theme: Theme, debug: bool, frame_count: u64) {
        let theme_matte = matte_for_theme(Some(theme));
        if !should_apply_theme_change(self.theme_matte, theme_matte) {
            return;
        }
        self.theme_matte = theme_matte;
        self.request_redraw(debug, "theme-changed", frame_count);
    }

    fn request_scaled_inner_size(&mut self) {
        let (logical, physical) = self.scaled_inner_sizes();
        self.pending_programmatic_resize = Some(physical);
        let _ = self.window.request_inner_size(logical);
    }

    fn scaled_inner_sizes(&self) -> (LogicalSize<f64>, PhysicalSize<u32>) {
        let (display_width, display_height) = self.effective_display_size();
        let width = scaled_len(display_width, self.view_scale);
        let height = scaled_len(display_height, self.view_scale);
        let logical = LogicalSize::new(f64::from(width), f64::from(height));
        let physical: PhysicalSize<u32> = logical.to_physical(self.window.scale_factor());
        (logical, physical)
    }

    fn needs_host_resize_resync_after_remote(&self) -> bool {
        if is_window_fullscreen(&self.window) {
            return false;
        }
        let (_, desired) = self.scaled_inner_sizes();
        host_resize_resync_needed(self.window.inner_size(), desired)
    }

    fn request_scaled_inner_size_if_needed(&mut self, debug: bool, reason: &str) -> bool {
        if is_window_fullscreen(&self.window) {
            return false;
        }
        let (logical, physical) = self.scaled_inner_sizes();
        let current = self.window.inner_size();
        if !host_resize_resync_needed(current, physical) {
            return false;
        }
        self.pending_programmatic_resize = Some(physical);
        let _ = self.window.request_inner_size(logical);
        self.request_redraw(debug, reason, 0);
        if debug {
            eprintln!(
                "debug: host resize resync target={}x{} current={}x{} reason={reason}",
                physical.width, physical.height, current.width, current.height
            );
        }
        true
    }

    fn update_resize_cursor(&mut self, position: PhysicalPosition<f64>) {
        if self.move_state.is_dragging() {
            self.window.set_cursor(CursorIcon::Move);
            return;
        }
        let direction = resize_direction_at(position, self.window.inner_size());
        if self.hover_resize_direction == direction {
            return;
        }
        self.hover_resize_direction = direction;
        self.window
            .set_cursor(direction.map_or(CursorIcon::Default, CursorIcon::from));
    }

    fn clear_resize_cursor(&mut self) {
        if self.active_resize.is_some() || self.move_state.is_dragging() {
            return;
        }
        self.hover_resize_direction = None;
        self.window.set_cursor(CursorIcon::Default);
    }

    fn remember_left_press(&mut self, debug: bool, id: u64) {
        if self.move_state.is_dragging() {
            return;
        }
        if let Some(drag) = self.move_drag_from_current_cursor(debug, id) {
            self.move_state = MoveState::Pressed(drag);
        }
    }

    fn clear_pending_move(&mut self) {
        if !self.move_state.is_dragging() {
            self.move_state = MoveState::Idle;
        }
    }

    fn move_drag_from_current_cursor(&self, debug: bool, id: u64) -> Option<MoveDrag> {
        let position = self.last_window_cursor?;
        let start_outer = match self.window.outer_position() {
            Ok(position) => position,
            Err(e) => {
                if debug {
                    eprintln!("debug: move ignored id={id}: outer_position failed: {e}");
                }
                return None;
            }
        };
        Some(MoveDrag {
            start_cursor_window: position,
            press_outer: start_outer,
        })
    }

    fn begin_manual_move(&mut self, debug: bool, id: u64) -> bool {
        let Some(drag) = self.move_drag_from_current_cursor(debug, id) else {
            return false;
        };
        self.move_state = MoveState::Dragging(drag);
        self.window.set_cursor(CursorIcon::Move);
        if debug {
            eprintln!("debug: begin manual move id={id}");
        }
        true
    }

    #[cfg(not(target_os = "macos"))]
    fn begin_requested_move(&mut self, debug: bool, id: u64) -> bool {
        let (drag, released) = match self.move_state {
            MoveState::Dragging(_) => return true,
            MoveState::Pressed(drag) => (drag, false),
            MoveState::Released { drag, at } if at.elapsed() <= REMOTE_MOVE_RELEASE_GRACE => {
                (drag, true)
            }
            MoveState::Released { .. } | MoveState::Idle => {
                self.move_state = MoveState::Idle;
                if debug {
                    eprintln!("debug: move request ignored id={id}: no active left press");
                }
                return false;
            }
        };
        self.move_state = MoveState::Dragging(drag);
        self.window.set_cursor(CursorIcon::Move);
        if debug {
            eprintln!("debug: begin requested move id={id}");
        }
        if let Some(position) = self.last_window_cursor {
            self.update_manual_move(position, debug, id);
        }
        if released {
            self.move_state = MoveState::Idle;
            self.clear_resize_cursor();
            if !self.snap_to_cursor_left_right_edge(debug, id) && edge_snap_enabled() {
                self.snap_to_cursor_tile(debug, id);
            }
        }
        true
    }

    fn update_manual_move(
        &mut self,
        position: PhysicalPosition<f64>,
        debug: bool,
        id: u64,
    ) -> bool {
        let MoveState::Dragging(drag) = self.move_state else {
            return false;
        };
        self.last_titlebar_click = None;
        let new_outer =
            compute_manual_move_outer(drag.press_outer, drag.start_cursor_window, position);
        self.window.set_outer_position(new_outer);
        self.move_state = MoveState::Dragging(drag);
        self.last_screen_cursor = Some(PhysicalPosition::new(
            f64::from(new_outer.x) + position.x,
            f64::from(new_outer.y) + position.y,
        ));
        if debug {
            eprintln!(
                "debug: manual move id={id} outer={}x{}",
                new_outer.x, new_outer.y
            );
        }
        true
    }

    fn finish_manual_move(&mut self, debug: bool, id: u64) -> bool {
        match self.move_state {
            MoveState::Dragging(_) => self.move_state = MoveState::Idle,
            #[cfg(not(target_os = "macos"))]
            MoveState::Pressed(drag) => {
                // Hold Pressed-at-release as Released for a short grace so a
                // belated MoveRequested from the guest can still adopt the
                // drag.
                self.move_state = MoveState::Released {
                    drag,
                    at: Instant::now(),
                };
                return false;
            }
            #[cfg(target_os = "macos")]
            MoveState::Pressed(_) => {
                // macOS doesn't honour guest MoveRequested (AppKit drives the
                // drag), so a Pressed left-up has nothing to upgrade to.
                self.move_state = MoveState::Idle;
                return false;
            }
            #[cfg(not(target_os = "macos"))]
            MoveState::Released { .. } => return false,
            MoveState::Idle => return false,
        }
        if let Some(position) = self.last_window_cursor {
            self.hover_resize_direction = None;
            self.update_resize_cursor(position);
        } else {
            self.clear_resize_cursor();
        }
        if !self.snap_to_cursor_left_right_edge(debug, id) && edge_snap_enabled() {
            self.snap_to_cursor_tile(debug, id);
        }
        if debug {
            eprintln!("debug: finish manual move id={id}");
        }
        true
    }

    fn snap_to_cursor_tile(&mut self, debug: bool, id: u64) -> bool {
        let Some(cursor_screen) = self.last_screen_cursor else {
            return false;
        };
        let outer_position = match self.window.outer_position() {
            Ok(position) => position,
            Err(e) => {
                if debug {
                    eprintln!(
                        "debug: cursor edge snap ignored id={id}: outer_position failed: {e}"
                    );
                }
                return false;
            }
        };
        self.snap_to_cursor_edge_tile_at(cursor_screen, outer_position, debug, id)
    }

    fn snap_to_cursor_edge_tile_at(
        &mut self,
        cursor_screen: PhysicalPosition<f64>,
        outer_position: PhysicalPosition<i32>,
        debug: bool,
        id: u64,
    ) -> bool {
        let Some(monitor) = self.window.current_monitor() else {
            return false;
        };
        let Some(rect) = cursor_edge_tile_rect(
            cursor_screen,
            outer_position,
            self.window.outer_size(),
            monitor.position(),
            self.tile_screen_size,
        ) else {
            return false;
        };

        self.apply_edge_snap_rect(rect, debug, id, "cursor edge tile")
    }

    fn apply_edge_snap_rect(
        &mut self,
        rect: EdgeSnapRect,
        debug: bool,
        id: u64,
        label: &str,
    ) -> bool {
        let scale = self.window.scale_factor().max(1.0);
        let (logical_width, logical_height) =
            physical_to_logical_size(rect.size.width, rect.size.height, scale);
        let logical = LogicalSize::new(logical_width, logical_height);
        let physical: PhysicalSize<u32> = logical.to_physical(scale);
        let logical_as_physical = PhysicalSize::new(logical_width as u32, logical_height as u32);
        self.pending_tile_remote_resize =
            Some(self.remote_size_for_inner_size(logical_as_physical));
        self.pending_programmatic_resize = Some(physical);
        let _ = self.window.request_inner_size(logical);
        self.window.set_outer_position(rect.position);
        self.request_redraw(debug, "window-tile", 0);
        if debug {
            eprintln!(
                "debug: {label} id={id} outer={}x{} size={}x{}",
                rect.position.x, rect.position.y, rect.size.width, rect.size.height
            );
        }
        true
    }

    fn snap_to_cursor_left_right_edge(&mut self, debug: bool, id: u64) -> bool {
        let Some(cursor_screen) = self.last_screen_cursor else {
            return false;
        };
        let Ok(outer_position) = self.window.outer_position() else {
            return false;
        };
        let Some(monitor) = self.window.current_monitor() else {
            return false;
        };
        let scale = self.window.scale_factor().max(1.0);
        let top_inset_px = (macos_menubar_inset_logical_pt() * scale).round().max(0.0) as u32;
        let Some(rect) = cursor_left_right_tile_rect(
            cursor_screen,
            outer_position,
            self.window.outer_size(),
            monitor.position(),
            monitor.size(),
            top_inset_px,
        ) else {
            return false;
        };
        self.apply_edge_snap_rect(rect, debug, id, "left-right edge snap")
    }

    fn take_pending_tile_remote_resize(&mut self) -> Option<(u32, u32)> {
        self.pending_tile_remote_resize.take()
    }

    fn take_titlebar_toggle_on_left_press(&mut self, debug: bool, id: u64) -> bool {
        let (x, y) = self.sync_cursor_from_window_position();
        if debug && y <= TITLEBAR_DOUBLE_CLICK_HEIGHT_PX {
            eprintln!(
                "debug: host titlebar press id={id} x={x} y={y} frame={}x{} max_hit={} double_area={}",
                self.frame.width,
                self.frame.height,
                is_titlebar_maximize_button(x, y, self.frame.width),
                is_titlebar_double_click_area(x, y, self.frame.width)
            );
        }
        if is_titlebar_maximize_button(x, y, self.frame.width) {
            self.suppress_next_left_release = true;
            self.last_titlebar_click = None;
            if debug {
                eprintln!("debug: host titlebar maximize button id={id} x={x} y={y}");
            }
            return true;
        }

        if !is_titlebar_double_click_area(x, y, self.frame.width) {
            self.last_titlebar_click = None;
            return false;
        }

        let now = Instant::now();
        let double_click = is_titlebar_double_click(
            self.last_titlebar_click,
            now,
            x,
            y,
            TITLEBAR_DOUBLE_CLICK_INTERVAL,
            TITLEBAR_DOUBLE_CLICK_DISTANCE_PX,
        );

        self.last_titlebar_click = Some(TitlebarClick { at: now, x, y });
        if !double_click {
            return false;
        }

        self.suppress_next_left_release = true;
        self.last_titlebar_click = None;
        if debug {
            eprintln!("debug: host titlebar double-click id={id} x={x} y={y}");
        }
        true
    }

    fn consume_suppressed_left_release(&mut self) -> bool {
        if !self.suppress_next_left_release {
            return false;
        }
        self.suppress_next_left_release = false;
        true
    }

    fn begin_manual_resize(&mut self, debug: bool, id: u64) -> bool {
        let Some(direction) = self.hover_resize_direction else {
            return false;
        };
        let Some(_cursor) = self.last_window_cursor else {
            return false;
        };
        let start_outer = match self.window.outer_position() {
            Ok(position) => position,
            Err(e) => {
                if debug {
                    eprintln!("debug: resize ignored id={id}: outer_position failed: {e}");
                }
                return false;
            }
        };
        self.active_resize = Some(ResizeDrag {
            direction,
            start_outer,
            start_size: self.window.inner_size(),
        });
        self.window.set_cursor(CursorIcon::from(direction));
        if debug {
            eprintln!("debug: begin manual resize id={id} direction={direction:?}");
        }
        true
    }

    fn update_manual_resize(
        &mut self,
        position: PhysicalPosition<f64>,
        debug: bool,
        id: u64,
    ) -> bool {
        let Some(drag) = self.active_resize else {
            return false;
        };
        let outer = self.window.outer_position().unwrap_or(drag.start_outer);
        let cursor_x = f64::from(outer.x) + position.x;
        let cursor_y = f64::from(outer.y) + position.y;

        let (mut left, mut top, mut right, mut bottom) =
            apply_resize_direction(&drag, cursor_x, cursor_y);

        clamp_manual_resize(drag.direction, &mut left, &mut top, &mut right, &mut bottom);
        let width = (right - left).round() as u32;
        let height = (bottom - top).round() as u32;

        if uses_west_edge(drag.direction) || uses_north_edge(drag.direction) {
            self.window.set_outer_position(PhysicalPosition::new(
                left.round() as i32,
                top.round() as i32,
            ));
        }
        let _ = self
            .window
            .request_inner_size(PhysicalSize::new(width, height));
        self.request_redraw(debug, "manual-resize", 0);
        if debug {
            eprintln!("debug: manual resize id={id} size={width}x{height}");
        }
        true
    }

    fn finish_manual_resize(&mut self, debug: bool, id: u64) -> bool {
        if self.active_resize.take().is_none() {
            return false;
        }
        self.clear_pending_move();
        if let Some(position) = self.last_window_cursor {
            self.hover_resize_direction = None;
            self.update_resize_cursor(position);
        } else {
            self.clear_resize_cursor();
        }
        if debug {
            eprintln!("debug: finish manual resize id={id}");
        }
        true
    }

    fn consume_programmatic_resize(&mut self, size: PhysicalSize<u32>) -> bool {
        consume_programmatic_resize_gate(&mut self.pending_programmatic_resize, size)
    }

    fn remote_size_for_inner_size(&self, size: PhysicalSize<u32>) -> (u32, u32) {
        let content = self.effective_content_rect();
        compute_remote_size(
            size,
            self.view_scale,
            content.w,
            content.h,
            self.frame.width,
            self.frame.height,
        )
    }

    /// Compute the remote-side (guest) size that matches the current host
    /// inner_size, returning `Some((width, height))` when it differs from
    /// the last size we received from the server (so the caller can decide
    /// whether to enqueue an `InputEvent::Resize`).
    ///
    /// Used both by the `WindowEvent::Resized` user-resize path and by the
    /// fullscreen-exit handler that has to recover sync after the guard
    /// suppressed resize-forwarding during fullscreen.
    fn resize_request_for_current_inner_size(&self) -> Option<(u32, u32)> {
        let size = self.window.inner_size();
        let scale = self.window.scale_factor().max(1.0);
        let logical: LogicalSize<u32> = size.to_logical(scale);
        let logical_as_physical = PhysicalSize::new(logical.width.max(1), logical.height.max(1));
        let (rw, rh) = self.remote_size_for_inner_size(logical_as_physical);
        (rw != self.frame.width || rh != self.frame.height).then_some((rw, rh))
    }

    fn map_cursor_to_frame(&self, x: f64, y: f64) -> (i32, i32) {
        let size = self.window.inner_size();
        cursor_to_frame_coords(
            x,
            y,
            (size.width.max(1), size.height.max(1)),
            self.effective_content_rect(),
            (self.frame.width, self.frame.height),
        )
    }

    fn sync_cursor_from_window_position(&mut self) -> (i32, i32) {
        if let Some(position) = self.last_window_cursor {
            let (x, y) = self.map_cursor_to_frame(position.x, position.y);
            self.last_cursor = (x, y);
            self.update_ime_cursor_area(x, y);
        }
        self.last_cursor
    }

    fn sync_cursor_for_button_event(&mut self, debug: bool, id: u64) -> (i32, i32) {
        let (x, y) = self.sync_cursor_from_window_position();
        if debug {
            eprintln!("debug: host button cursor id={id} frame={x}x{y}");
        }
        (x, y)
    }

    fn show_after_first_present(&mut self) {
        if self.visible {
            return;
        }
        self.window.set_visible(true);
        self.window.focus_window();
        self.visible = true;
    }

    /// Set both the AppKit/winit title and the cached `title` string in lockstep —
    /// callers should never touch `self.title` directly.
    fn set_window_title(&mut self, title: String) {
        self.window.set_title(&title);
        self.title = title;
    }

    /// Counterpart to `show_after_first_present`: hide the OS window and drop
    /// the `visible` cache in one step.
    fn hide(&mut self) {
        self.window.set_visible(false);
        self.visible = false;
    }

    /// One-shot mark used by both `Configured` and the first content-rect
    /// growth: prevents the guest's later relayouts from snapping the host
    /// window back to its preferred size. See the `adopted_initial_size` doc
    /// on the struct for the full story.
    fn mark_adopted_initial_size(&mut self) {
        self.adopted_initial_size = true;
    }

    /// Cache the window-local cursor position only. Used by CursorMoved
    /// paths that may short-circuit on `update_manual_move` /
    /// `update_manual_resize` / hover before computing the frame mapping.
    fn set_last_window_cursor(&mut self, position: PhysicalPosition<f64>) {
        self.last_window_cursor = Some(position);
    }

    /// Map a window-local cursor to frame coordinates and cache the result
    /// in `last_cursor`. Returns the cached (x, y).
    fn cache_frame_cursor(&mut self, position: PhysicalPosition<f64>) -> (i32, i32) {
        let (x, y) = self.map_cursor_to_frame(position.x, position.y);
        self.last_cursor = (x, y);
        (x, y)
    }

    /// Cache both the window-local cursor and its frame mapping in lockstep.
    /// For paths that always commit both (mouse wheel, button fallback) and
    /// have no manual-move/resize short-circuit.
    fn cache_cursor_at(&mut self, position: PhysicalPosition<f64>) -> (i32, i32) {
        self.set_last_window_cursor(position);
        self.cache_frame_cursor(position)
    }

    fn update_ime_cursor_area(&self, x: i32, y: i32) {
        let size = self.window.inner_size();
        let content = self.effective_content_rect();
        let mapping = aspect_fit_rect(content.w, content.h, size.width.max(1), size.height.max(1));
        let local_x = f64::from(mapping.x)
            + (f64::from((x - content.x as i32).max(0)) * f64::from(mapping.w.max(1))
                / f64::from(content.w.max(1)))
            .round();
        let local_y = f64::from(mapping.y)
            + (f64::from((y - content.y as i32).max(0)) * f64::from(mapping.h.max(1))
                / f64::from(content.h.max(1)))
            .round();
        self.window.set_ime_cursor_area(
            LogicalPosition::new(local_x, local_y),
            LogicalSize::new(1.0, 20.0),
        );
    }
}

fn window_title(title: &str) -> &str {
    if title.is_empty() { "vbox" } else { title }
}

/// Partition a snapshot of pending remote resizes into "ready" (due
/// time is past `now`) and "still waiting" buckets, returning the
/// soonest `next_due` from the waiting set. Pulled out of
/// `flush_due_remote_resizes` so the test pins the partition without
/// holding a winit event loop.
#[allow(dead_code)] // exercised by tests; production inlines retain()
fn partition_pending_resizes(
    pending: &[(u64, PendingRemoteResize)],
    now: Instant,
) -> (
    Vec<(u64, u32, u32)>,
    Vec<(u64, PendingRemoteResize)>,
    Option<Instant>,
) {
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    let mut next_due: Option<Instant> = None;
    for &(id, resize) in pending {
        if resize.due <= now {
            ready.push((id, resize.width, resize.height));
        } else {
            waiting.push((id, resize));
            next_due = Some(next_due.map_or(resize.due, |due| due.min(resize.due)));
        }
    }
    (ready, waiting, next_due)
}

/// Pure decision for "is this click the second half of a double-click
/// on the host titlebar?". Three gates a real macOS double-click must
/// pass: it has to be within the documented interval (default ~500ms),
/// within the slop distance on both axes, and we need to have observed
/// a prior click. Splitting from `take_titlebar_toggle_on_left_press`
/// lets us drive all four arms without a real Instant clock.
fn is_titlebar_double_click(
    last: Option<TitlebarClick>,
    now: Instant,
    x: i32,
    y: i32,
    interval: Duration,
    slop: i32,
) -> bool {
    last.is_some_and(|click| {
        now.duration_since(click.at) <= interval
            && (x - click.x).abs() <= slop
            && (y - click.y).abs() <= slop
    })
}

/// Decide whether a new matte color should trigger a redraw. We only
/// redraw when the value actually changes — otherwise a stream of
/// `WindowEvent::ThemeChanged` (which macOS does occasionally fire for
/// the same theme during a wake-from-sleep) would needlessly thrash
/// the softbuffer.
fn should_apply_theme_change(current: u32, next: u32) -> bool {
    current != next
}

/// Clamp a view-scale request to the supported [`MIN_VIEW_SCALE`,
/// `MAX_VIEW_SCALE`] range. Splitting from [`set_view_scale`] lets the
/// test pin every shape the operator can produce (zoom in past max,
/// zoom out past min, exactly-at-boundary, default 1.0) without owning
/// a real ViewerWindow.
fn clamp_view_scale(scale: f32) -> f32 {
    scale.clamp(MIN_VIEW_SCALE, MAX_VIEW_SCALE)
}

/// Pure debounce-bucket builder for [`queue_remote_resize`]. Splitting
/// the geometry-and-deadline construction out lets a test pin every
/// shape (zero clamping, deadline arithmetic) without holding a
/// ViewerApp.
fn build_pending_remote_resize(
    width: u32,
    height: u32,
    debounce: Duration,
    now: Instant,
) -> PendingRemoteResize {
    PendingRemoteResize {
        width: width.max(1),
        height: height.max(1),
        due: now + debounce,
    }
}

fn build_fullscreen_exit_resync_due(delay: Duration, now: Instant) -> Instant {
    now + delay
}

fn build_host_resize_resync_due(delay: Duration, now: Instant) -> Instant {
    now + delay
}

/// Should we suppress the Released half of a Ctrl+Cmd+Space chord that
/// we previously swallowed for the macOS emoji picker? If we forward
/// the Released to the guest without forwarding the Pressed, the guest
/// sees an unbalanced state and may snap out of an in-progress IME.
fn is_emoji_picker_release_to_swallow(
    state: ElementState,
    physical: PhysicalKey,
    modifiers: ModifiersState,
) -> bool {
    state == ElementState::Released
        && matches!(physical, PhysicalKey::Code(KeyCode::Space))
        && modifiers.super_key()
        && modifiers.control_key()
}

fn guest_modifier_refcount_should_forward(
    counts: &mut HashMap<u32, usize>,
    keycode: u32,
    state: ElementState,
) -> bool {
    match state {
        ElementState::Pressed => {
            let count = counts.entry(keycode).or_insert(0);
            let should_forward = *count == 0;
            *count = count.saturating_add(1);
            should_forward
        }
        ElementState::Released => {
            let Some(count) = counts.get_mut(&keycode) else {
                return false;
            };
            if *count > 1 {
                *count -= 1;
                return false;
            }
            counts.remove(&keycode);
            true
        }
    }
}

/// Pick the dedup key the "guest ships its own chrome" warning should
/// track. We prefer the `app_id` when present (it's stable across
/// window titles); fall back to the title when the guest hasn't yet
/// reported an app_id. Splitting it out keeps the warn-once logic
/// independently testable.
fn guest_chrome_dedup_key<'a>(app_id: &'a str, title: &'a str) -> &'a str {
    if app_id.is_empty() { title } else { app_id }
}

/// Build the one-shot stderr warning emitted the first time we see a
/// guest app that draws its own chrome. The message wording branches
/// on `host_chrome_enabled` so operators get the most actionable hint
/// for their current setup.
fn guest_chrome_warning_message(label: &str, host_chrome_enabled: bool) -> String {
    if host_chrome_enabled {
        format!(
            "[vbox] note: {label} ships its own header bar / tab strip. \
            Stacked with the host macOS titlebar it may look heavy or \
            fight fullscreen resizing. If it gets in the way, run with \
            {HOST_CHROME_ENV}=0 to fall back to the borderless layout."
        )
    } else {
        format!(
            "[vbox] note: {label} ships its own header bar / tab strip; \
            with {HOST_CHROME_ENV}=0 the host titlebar is hidden, so the \
            guest chrome is what you see and clicks at the very top of \
            the window go to the guest, not macOS window controls."
        )
    }
}

/// Pure scale picker for the host-side view zoom shortcuts. Operators
/// hit `+`/`=` to zoom in, `-`/`_` to zoom out, and `0` to reset. Any
/// other character returns `None` so the caller knows the shortcut
/// wasn't a zoom command and can let the key fall through to the
/// guest. The `*` / `/` math is captured here so a step-size tweak
/// only touches one place.
fn next_view_scale(text: &str, current: f32) -> Option<f32> {
    match text {
        "+" | "=" => Some(current * VIEW_SCALE_STEP),
        "-" | "_" => Some(current / VIEW_SCALE_STEP),
        "0" => Some(1.0),
        _ => None,
    }
}

/// Pure decision for "is this KeyEvent the macOS `Cmd+T` new-tab
/// shortcut, on a terminal window?". Takes the subset of KeyEvent
/// fields that matter (winit forbids struct-literal construction of
/// `KeyEvent`, so the helper takes the parts the caller can pass).
fn is_terminal_new_tab_shortcut(
    state: ElementState,
    repeat: bool,
    physical_key: &PhysicalKey,
    modifiers: &ModifiersState,
    target_is_terminal: bool,
) -> bool {
    state == ElementState::Pressed
        && !repeat
        && modifiers.super_key()
        && !modifiers.control_key()
        && !modifiers.alt_key()
        && !modifiers.shift_key()
        && matches!(*physical_key, PhysicalKey::Code(KeyCode::KeyT))
        && target_is_terminal
}

pub(crate) fn is_terminal_window_title(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    lower.contains("terminal")
        || lower.contains("ptyxis")
        || title.contains("터미널")
        || title.contains('@')
}

impl ApplicationHandler<ViewerEvent> for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.ensure_keepalive_window(event_loop);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(id) = self.window_ids.get(&window_id).copied() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                self.release_window_guest_keys(id);
                self.send_input(InputEvent::Close { id });
            }
            WindowEvent::Focused(focused) => {
                if focused {
                    if let Some(view) = self.windows.get(&id) {
                        view.enable_ime_at_last_cursor();
                    }
                } else {
                    self.release_window_guest_keys(id);
                }
                if self.debug {
                    eprintln!("debug: viewer focused id={id} focused={focused}");
                }
                self.send_input(InputEvent::Focus { id, focused });
            }
            WindowEvent::CursorMoved { position, .. } => {
                let Some((x, y)) = self.windows.get_mut(&id).and_then(|view| {
                    view.set_last_window_cursor(position);
                    if view.update_manual_move(position, self.debug, id) {
                        return None;
                    }
                    if view.update_manual_resize(position, self.debug, id) {
                        return None;
                    }
                    view.update_resize_cursor(position);
                    if view.hover_resize_direction.is_some() {
                        return None;
                    }
                    let (x, y) = view.cache_frame_cursor(position);
                    view.update_ime_cursor_area(x, y);
                    Some((x, y))
                }) else {
                    return;
                };
                self.send_input(InputEvent::PointerMotion { id, x, y });
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(view) = self.windows.get_mut(&id) {
                    view.clear_resize_cursor();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (delta_x, delta_y, kind) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(dx, dy) => (
                        f64::from(dx) * SCROLL_LINE_TO_AXIS_UNITS,
                        f64::from(dy) * SCROLL_LINE_TO_AXIS_UNITS,
                        "line",
                    ),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        let scale = self
                            .windows
                            .get(&id)
                            .map_or(1.0, |v| v.window.scale_factor().max(1.0));
                        (pos.x / scale, pos.y / scale, "pixel")
                    }
                };
                if self.debug {
                    eprintln!(
                        "debug: mouse wheel id={id} kind={kind} dx={delta_x:.2} dy={delta_y:.2}"
                    );
                }
                if delta_x == 0.0 && delta_y == 0.0 {
                    return;
                }
                let motion = self.windows.get_mut(&id).map(|view| {
                    let position = view.last_window_cursor.unwrap_or_else(|| {
                        let size = view.window.inner_size();
                        PhysicalPosition::new(
                            f64::from(size.width) / 2.0,
                            f64::from(size.height) / 2.0,
                        )
                    });
                    view.cache_cursor_at(position)
                });
                if let Some((x, y)) = motion {
                    self.send_input(InputEvent::PointerMotion { id, x, y });
                }
                self.send_input(InputEvent::PointerScroll {
                    id,
                    delta_x_millis: ((-delta_x) * 1000.0).round() as i32,
                    delta_y_millis: ((-delta_y) * 1000.0).round() as i32,
                });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let cursor = self.windows.get_mut(&id).map_or((0, 0), |view| {
                    view.sync_cursor_for_button_event(self.debug, id)
                });
                if button == MouseButton::Left {
                    if state == ElementState::Pressed {
                        // Borderless-only hack: hit-test the top strip of
                        // the guest frame and treat clicks on the spot
                        // where GTK header bars draw their maximize
                        // button as a maximize toggle on the host window.
                        // With the macOS titlebar enabled, the standard
                        // NSWindow zoom button already does this and the
                        // hack would race with the guest's own click
                        // handler — the clock app's "fullscreen" button
                        // ends up zooming both the host NSWindow *and*
                        // the guest, leaving content drawn at the wrong
                        // offset. Skip it when host chrome is on.
                        let handled_titlebar_toggle = !self.host_chrome_enabled
                            && self.windows.get_mut(&id).is_some_and(|view| {
                                view.take_titlebar_toggle_on_left_press(self.debug, id)
                            });
                        if handled_titlebar_toggle {
                            if let Some(view) = self.windows.get(&id) {
                                view.enable_ime_at_last_cursor();
                            }
                            self.send_input(InputEvent::Focus { id, focused: true });
                            self.send_input(InputEvent::ToggleMaximize { id });
                            // Mirror the toggle onto the host NSWindow so the
                            // wrapper geometry actually grows/shrinks. Without
                            // this the guest expands its frame buffer to
                            // monitor-size but the host window stays at its
                            // user-chosen size, which the user perceives as
                            // "maximize doesn't work".
                            if let Some(view) = self.windows.get(&id) {
                                let new_state = !view.window.is_maximized();
                                view.window.set_maximized(new_state);
                            }
                            return;
                        }
                        if let Some(view) = self.windows.get_mut(&id) {
                            view.remember_left_press(self.debug, id);
                        }
                    } else if self
                        .windows
                        .get_mut(&id)
                        .is_some_and(ViewerWindow::consume_suppressed_left_release)
                    {
                        return;
                    }
                    let mut move_resize = None;
                    let handled_move = match state {
                        ElementState::Pressed if host_move_modifier_active(self.modifiers) => self
                            .windows
                            .get_mut(&id)
                            .is_some_and(|view| view.begin_manual_move(self.debug, id)),
                        ElementState::Released => self.windows.get_mut(&id).is_some_and(|view| {
                            let handled = view.finish_manual_move(self.debug, id);
                            move_resize = view.take_pending_tile_remote_resize();
                            handled
                        }),
                        ElementState::Pressed => false,
                    };
                    if handled_move {
                        if let Some((width, height)) = move_resize {
                            self.queue_remote_resize(id, width, height);
                        }
                        return;
                    }

                    let handled_resize = match state {
                        ElementState::Pressed => self
                            .windows
                            .get_mut(&id)
                            .is_some_and(|view| view.begin_manual_resize(self.debug, id)),
                        ElementState::Released => self
                            .windows
                            .get_mut(&id)
                            .is_some_and(|view| view.finish_manual_resize(self.debug, id)),
                    };
                    if handled_resize {
                        if let Some(view) = self.windows.get(&id) {
                            view.enable_ime_at_last_cursor();
                        }
                        self.send_input(InputEvent::Focus { id, focused: true });
                        return;
                    }
                }
                if let Some(button) = mouse_button_code(button) {
                    let (x, y) = cursor;
                    if state == ElementState::Pressed {
                        if let Some(view) = self.windows.get(&id) {
                            view.enable_ime_at_last_cursor();
                        }
                        self.send_input(InputEvent::Focus { id, focused: true });
                    }
                    self.send_input(InputEvent::PointerMotion { id, x, y });
                    self.send_input(InputEvent::PointerButton {
                        id,
                        button,
                        state: match state {
                            ElementState::Pressed => InputButtonState::Pressed,
                            ElementState::Released => InputButtonState::Released,
                        },
                    });
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_keyboard(id, event),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                if self.debug {
                    eprintln!("debug: modifiers changed {:?}", self.modifiers);
                }
            }
            WindowEvent::Ime(ime) => self.handle_ime(id, ime),
            WindowEvent::ThemeChanged(theme) => {
                if let Some(view) = self.windows.get_mut(&id) {
                    if self.debug {
                        eprintln!("debug: theme changed id={id} theme={theme:?}");
                    }
                    view.set_theme(theme, self.debug, self.frame_count);
                }
            }
            WindowEvent::Resized(size) => {
                let mut resize = None;
                let mut fullscreen_to_send: Option<bool> = None;
                let mut schedule_post_exit_resync = false;
                let mut cancel_post_exit_resync = false;
                self.pending_host_resize_resyncs.remove(&id);
                if let Some(view) = self.windows.get_mut(&id) {
                    // While the host window is in our borderless fullscreen,
                    // winit reports an OS-driven Resized to the screen size.
                    // Forwarding that to the guest as an InputEvent::Resize
                    // makes the server flip mode back to Normal (and emit a
                    // FullscreenChanged(false) echo), which kicks off a
                    // mode-flip loop: fullscreen → resize → normal → echo →
                    // fullscreen exit → another resize → … . Treat host
                    // fullscreen resizes as a side-effect of the fullscreen
                    // state, not a user resize gesture.
                    let in_fullscreen = is_window_fullscreen(&view.window);
                    if self.debug {
                        eprintln!(
                            "trace fs-exit: client.WindowEvent::Resized id={id} size={}x{} in_fullscreen={in_fullscreen} last_host_fs={} frame={}x{} pending_prog={:?}",
                            size.width,
                            size.height,
                            view.last_host_fullscreen,
                            view.frame.width,
                            view.frame.height,
                            view.pending_programmatic_resize,
                        );
                    }

                    // Detect macOS native-fullscreen toggles (green window
                    // button / Cmd+Ctrl+F). winit doesn't emit a dedicated
                    // event for that path — only a Resized to the screen
                    // size — so the only signal we have is that
                    // `is_window_fullscreen` flipped while we weren't
                    // looking. Forward the new state to the guest so it
                    // can reconfigure its toplevel; without this, the host
                    // sits in native fullscreen while the guest keeps its
                    // small Normal-mode size, painting a tiny window
                    // inside a huge grey backdrop.
                    if in_fullscreen != view.last_host_fullscreen {
                        if self.debug {
                            eprintln!(
                                "trace fs-exit: client.Resized fullscreen_toggle_detected id={id} new_fs={in_fullscreen}",
                            );
                        }
                        fullscreen_to_send = Some(in_fullscreen);
                        view.last_host_fullscreen = in_fullscreen;
                        view.pending_programmatic_resize = None;
                        if !in_fullscreen {
                            // Mirrors the FullscreenChanged(false) path:
                            // once the host has left fullscreen, push the
                            // current inner size back to the guest so it
                            // catches up to the post-restore geometry
                            // (the guest still holds the pre-fullscreen
                            // size and would otherwise re-emit it as a
                            // Configured, snapping the host back).
                            schedule_post_exit_resync = true;
                            if self.debug {
                                eprintln!(
                                    "trace fs-exit: client.Resized schedule_post_exit_resync id={id}",
                                );
                            }
                        } else {
                            cancel_post_exit_resync = true;
                        }
                    }

                    if !schedule_post_exit_resync
                        && !in_fullscreen
                        && view.visible
                        && !view.consume_programmatic_resize(size)
                    {
                        // Dedup against the size the server last told us — see
                        // resize_to_remote_size for the inverse half of the
                        // feedback loop. Without this, a fractional scale or
                        // OS clamp can re-classify a programmatic resize as a
                        // user gesture and bounce it back to the server.
                        if let Some(pair) = view.resize_request_for_current_inner_size() {
                            resize = Some(pair);
                            view.enable_ime_at_last_cursor();
                        }
                    }
                    view.request_redraw(self.debug, "window-resized", self.frame_count);
                }
                if let Some(fullscreen) = fullscreen_to_send {
                    self.send_input(InputEvent::SetFullscreen { id, fullscreen });
                }
                if cancel_post_exit_resync {
                    self.pending_fullscreen_exit_resyncs.remove(&id);
                    self.pending_resizes.remove(&id);
                    self.pending_host_resize_resyncs.remove(&id);
                }
                if schedule_post_exit_resync {
                    self.queue_fullscreen_exit_resync(id);
                }
                if let Some((width, height)) = resize {
                    self.queue_remote_resize(id, width, height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(view) = self.windows.get_mut(&id) {
                    view.render_pending_redraw(self.debug, self.frame_count);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.flush_due_remote_resizes(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: ViewerEvent) {
        match event {
            ViewerEvent::Message(msg) => self.handle_message(event_loop, msg),
            ViewerEvent::Disconnected(reason) => {
                eprintln!("viewer disconnected: {reason}");
                event_loop.exit();
            }
            #[cfg(unix)]
            ViewerEvent::DumpWindows => self.dump_windows_to_stderr(),
        }
    }
}

impl ViewerApp {
    fn send_input(&self, event: InputEvent) {
        let _ = self
            .outbound_tx
            .send(vbox_proto::Message::InputEvent(event));
    }

    /// Collect a snapshot of every live viewer window for the
    /// `./vbox windows` dump. The retired-list is excluded — those are
    /// orphans waiting for the empty-window grace timeout and would
    /// just clutter the dump.
    pub(crate) fn collect_viewer_snapshots(&self) -> Vec<ViewerWindowSnapshot> {
        let mut ids: Vec<u64> = self
            .windows
            .keys()
            .copied()
            .filter(|id| !self.retired_window_ids.contains(id))
            .collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                self.windows.get(&id).map(|view| {
                    let pending_remote = self
                        .pending_resizes
                        .get(&id)
                        .map(|pending| (pending.width, pending.height));
                    snapshot_from_parts(
                        id,
                        &view.window,
                        (view.frame.width, view.frame.height),
                        view.last_host_fullscreen,
                        view.pending_programmatic_resize,
                        pending_remote,
                        view.view_scale,
                    )
                })
            })
            .collect()
    }

    pub(crate) fn retired_window_count(&self) -> usize {
        self.retired_window_ids.len()
    }

    pub(crate) fn host_chrome_debug_flag(&self) -> bool {
        self.host_chrome_enabled
    }

    fn dump_windows_to_stderr(&self) {
        let mut stderr = std::io::stderr().lock();
        if let Err(e) = dump_viewer_windows(self, &mut stderr) {
            eprintln!("window-dump: error writing dump: {e}");
        }
    }

    fn maybe_warn_about_guest_chrome(&mut self, app_id: &str, title: &str) {
        let Some(label) = guest_app_uses_own_chrome(app_id, title) else {
            return;
        };
        let key = guest_chrome_dedup_key(app_id, title).to_string();
        if !self.warned_chrome_apps.insert(key) {
            return;
        }
        eprintln!(
            "{}",
            guest_chrome_warning_message(label, self.host_chrome_enabled)
        );
    }

    fn queue_remote_resize(&mut self, id: u64, width: u32, height: u32) {
        let resize =
            build_pending_remote_resize(width, height, REMOTE_RESIZE_DEBOUNCE, Instant::now());
        self.pending_resizes.insert(id, resize);
        if self.debug {
            eprintln!(
                "debug: queued remote resize id={} size={}x{}",
                id, resize.width, resize.height
            );
        }
    }

    fn queue_fullscreen_exit_resync(&mut self, id: u64) {
        let due = build_fullscreen_exit_resync_due(FULLSCREEN_EXIT_RESYNC_DELAY, Instant::now());
        self.pending_resizes.remove(&id);
        self.pending_host_resize_resyncs.remove(&id);
        self.pending_fullscreen_exit_resyncs.insert(id, due);
        if self.debug {
            eprintln!(
                "trace fs-exit: client.queue_fullscreen_exit_resync id={id} delay_ms={}",
                FULLSCREEN_EXIT_RESYNC_DELAY.as_millis()
            );
        }
    }

    fn queue_host_resize_resync(&mut self, id: u64) {
        let due = build_host_resize_resync_due(HOST_RESIZE_RESYNC_DELAY, Instant::now());
        self.pending_host_resize_resyncs.insert(id, due);
        if self.debug {
            eprintln!(
                "debug: queue host resize resync id={id} delay_ms={}",
                HOST_RESIZE_RESYNC_DELAY.as_millis()
            );
        }
    }

    fn send_remote_resize_now(&mut self, id: u64, width: u32, height: u32, reason: &str) {
        if let Some(view) = self.windows.get_mut(&id) {
            view.remote_resize_in_flight = Some((width, height));
        }
        if self.debug {
            eprintln!("debug: send remote resize id={id} size={width}x{height} reason={reason}");
        }
        self.send_input(InputEvent::Focus { id, focused: true });
        self.send_input(InputEvent::Resize { id, width, height });
    }

    fn flush_due_remote_resizes(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut next_due: Option<Instant> = None;
        let mut ready_fullscreen_resyncs = Vec::new();
        let mut ready_host_resyncs = Vec::new();
        let mut ready = Vec::new();

        self.pending_fullscreen_exit_resyncs.retain(|id, due| {
            if *due <= now {
                ready_fullscreen_resyncs.push(*id);
                return false;
            }
            next_due = Some(next_due.map_or(*due, |known| known.min(*due)));
            true
        });

        self.pending_resizes.retain(|id, resize| {
            if resize.due <= now {
                ready.push((*id, resize.width, resize.height));
                return false;
            }
            next_due = Some(next_due.map_or(resize.due, |due| due.min(resize.due)));
            true
        });

        self.pending_host_resize_resyncs.retain(|id, due| {
            if *due <= now {
                ready_host_resyncs.push(*id);
                return false;
            }
            next_due = Some(next_due.map_or(*due, |known| known.min(*due)));
            true
        });

        for id in ready_fullscreen_resyncs {
            let Some((width, height)) = self
                .windows
                .get(&id)
                .and_then(ViewerWindow::resize_request_for_current_inner_size)
            else {
                if self.debug {
                    eprintln!("trace fs-exit: client.fullscreen_exit_resync noop id={id}");
                }
                continue;
            };
            if self.debug {
                eprintln!(
                    "trace fs-exit: client.fullscreen_exit_resync send id={id} size={width}x{height}",
                );
            }
            self.send_remote_resize_now(id, width, height, "fullscreen-exit-resync");
        }

        for (id, width, height) in ready {
            self.send_remote_resize_now(id, width, height, "debounced");
        }

        for id in ready_host_resyncs {
            let Some(view) = self.windows.get_mut(&id) else {
                continue;
            };
            view.request_scaled_inner_size_if_needed(self.debug, "host-resize-resync");
        }

        if let Some(started_at) = self.empty_windows_since {
            let exit_at = started_at + WINDOW_REPLACEMENT_GRACE;
            if now >= exit_at {
                self.remove_retired_windows();
                if self.debug {
                    eprintln!("debug: exiting after empty-window grace");
                }
                event_loop.exit();
                return;
            }
            next_due = Some(next_due.map_or(exit_at, |due| due.min(exit_at)));
        }

        if let Some(due) = next_due {
            event_loop.set_control_flow(ControlFlow::WaitUntil(due));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn handle_ime(&mut self, id: u64, ime: Ime) {
        if shortcut_modifiers_active(self.modifiers)
            && matches!(ime, Ime::Preedit(_, _) | Ime::Commit(_))
        {
            self.clear_stale_shortcut_modifiers_for_text(id, "ime-text");
        }
        if shortcut_modifiers_active(self.modifiers)
            && matches!(ime, Ime::Preedit(_, _) | Ime::Commit(_))
        {
            if self.debug {
                eprintln!("debug: ignore macos ime text while shortcut modifiers active");
            }
            return;
        }

        match ime {
            Ime::Enabled => {
                if self.debug {
                    eprintln!("debug: macos ime enabled id={id}");
                }
                // macOS captures the IME-switch chord (Cmd+Space, or
                // whatever the user has configured) without always
                // delivering the corresponding modifier-release event to
                // winit. If we leave `self.modifiers` showing Cmd/Ctrl
                // held, every subsequent English keypress falls into the
                // `shortcut_modifiers_active` suppress branch in
                // handle_keyboard and the text is silently dropped — the
                // exact symptom of "한국어에서 영어 전환시 영어 입력 안돼".
                // Clear stale Cmd/Ctrl bits here; if the user actually
                // held them, the next ModifiersChanged or KeyEvent will
                // restore the true state immediately.
                if self.modifiers.super_key() || self.modifiers.control_key() {
                    if self.debug {
                        eprintln!(
                            "debug: clearing stuck modifiers on Ime::Enabled super={} ctrl={}",
                            self.modifiers.super_key(),
                            self.modifiers.control_key()
                        );
                    }
                    self.clear_shortcut_modifier_cache(id, "ime-enabled", true);
                }
                // Belt-and-braces: drain any pending Hangul/Kana state so
                // the new IME context starts clean. flush_composer is a
                // no-op when the composer is already idle.
                self.flush_composer(id);
            }
            Ime::Preedit(text, cursor) => {
                let (cursor_begin, cursor_end) = ime_cursor_range(cursor);
                if self.debug {
                    eprintln!(
                        "debug: macos ime preedit chars={} cursor={:?}",
                        text.chars().count(),
                        cursor
                    );
                }
                if ime::can_compose(&text) || (text.is_empty() && !self.composer_is_empty(id)) {
                    if self.debug {
                        eprintln!("debug: input composer owns IME preedit");
                    }
                    return;
                }
                self.flush_composer(id);
                self.send_input(InputEvent::Preedit {
                    id,
                    text,
                    cursor_begin,
                    cursor_end,
                });
            }
            Ime::Commit(text) => {
                if self.debug {
                    if emoji::contains_emoji(&text) {
                        let clusters = emoji::split_clusters(&text);
                        eprintln!(
                            "debug: macos ime commit chars={} clusters={} emoji=[{}]",
                            text.chars().count(),
                            clusters.len(),
                            emoji::codepoint_dump(&text)
                        );
                    } else {
                        eprintln!("debug: macos ime commit chars={}", text.chars().count());
                    }
                }
                if !text.is_empty() {
                    self.send_host_text(id, &text);
                }
            }
            Ime::Disabled => {
                if self.debug {
                    eprintln!("debug: macos ime disabled");
                }
                self.flush_composer(id);
                self.send_input(InputEvent::Preedit {
                    id,
                    text: String::new(),
                    cursor_begin: -1,
                    cursor_end: -1,
                });
            }
        }
    }

    fn handle_keyboard(&mut self, id: u64, event: KeyEvent) {
        if self.debug {
            eprintln!(
                "debug: keyboard state={:?} repeat={} physical={:?} logical={:?} text={:?}",
                event.state, event.repeat, event.physical_key, event.logical_key, event.text
            );
        }

        if let Some(keycode) = guest_modifier_keycode(event.physical_key, &event.logical_key) {
            if self.debug {
                eprintln!("debug: modifier key forwarded keycode={keycode}");
            }
            self.send_tracked_guest_modifier_key(id, keycode, event.state);
            return;
        }

        if event.state == ElementState::Pressed
            && !event.repeat
            && is_window_dump_shortcut(self.modifiers, &event.logical_key)
        {
            // Cmd+Option+W: dump every viewer window's state to
            // stderr (which client.log captures). Same diagnostic as
            // SIGUSR1 from `./vbox windows`, just keyboard-driven so
            // the operator can hit it while reproducing a bug
            // without leaving the viewer.
            if self.debug {
                eprintln!("debug: window-dump shortcut id={id}");
            }
            self.dump_windows_to_stderr();
            return;
        }

        if event.state == ElementState::Pressed
            && !event.repeat
            && is_fullscreen_shortcut(self.modifiers, &event.logical_key)
        {
            // Send the target state, not a toggle, and don't touch the
            // host window locally — the server echoes FullscreenChanged
            // back and apply_window_fullscreen converges from there.
            // This keeps host and guest aligned even when the server
            // treats the request as a no-op (already in that mode,
            // stale id, etc.).
            let fullscreen = self
                .windows
                .get(&id)
                .is_some_and(|view| !is_window_fullscreen(&view.window));
            if self.debug {
                eprintln!("debug: fullscreen shortcut id={id} target={fullscreen}");
            }
            self.send_input(InputEvent::SetFullscreen { id, fullscreen });
            return;
        }

        // Yield Ctrl+Cmd+Space to macOS so its Character Viewer (emoji
        // picker) can come up. If we let the shortcut path forward it, the
        // guest receives Ctrl+Space and consumes the chord as its own
        // shortcut before any emoji can be picked.
        if event.state == ElementState::Pressed
            && is_macos_emoji_picker_shortcut(event.physical_key, self.modifiers)
        {
            if self.debug {
                eprintln!("debug: swallow Ctrl+Cmd+Space for macOS emoji picker");
            }
            self.flush_composer(id);
            return;
        }
        if is_emoji_picker_release_to_swallow(event.state, event.physical_key, self.modifiers) {
            if self.debug {
                eprintln!("debug: swallow Ctrl+Cmd+Space release (picker chord)");
            }
            return;
        }

        if self.handle_terminal_macos_new_tab_shortcut(id, &event) {
            return;
        }

        if event.state == ElementState::Pressed
            && printable_keyboard_text(event.text.as_deref()).is_some()
        {
            self.clear_stale_shortcut_modifiers_for_text(id, "keyboard-text");
        }

        let keyboard_command_modifiers = keyboard_command_modifiers_active(self.modifiers);
        if keyboard_command_modifiers {
            if event.state == ElementState::Pressed
                && shortcut_modifiers_active(self.modifiers)
                && self.handle_view_shortcut(id, &event.logical_key)
            {
                return;
            }

            if let Some(keycode) =
                shortcut_keycode(event.physical_key).or_else(|| named_keycode(&event.logical_key))
            {
                if event.state == ElementState::Pressed {
                    self.flush_composer(id);
                } else if !self.pressed_guest_keys.contains_key(&keycode) {
                    return;
                }
                if self.debug {
                    eprintln!("debug: shortcut key forwarded keycode={keycode}");
                }
                self.send_tracked_guest_key(id, keycode, event.state);
                return;
            }
        }

        if let Some(keycode) = named_keycode(&event.logical_key) {
            if event.state == ElementState::Pressed {
                if keycode == KEY_BACKSPACE && !self.composer_is_empty(id) {
                    let actions = self.composer_backspace(id);
                    self.send_composer_actions(id, actions);
                    return;
                }
                self.flush_composer(id);
            }
            self.send_tracked_guest_key(id, keycode, event.state);
            return;
        }

        if event.state != ElementState::Pressed {
            if let Some(keycode) = shortcut_keycode(event.physical_key)
                .filter(|keycode| self.pressed_guest_keys.contains_key(keycode))
            {
                self.send_tracked_guest_key(id, keycode, ElementState::Released);
            }
            return;
        }

        if keyboard_command_modifiers {
            if self.debug {
                if let Some(text) = event.text.as_deref().filter(|text| !text.is_empty()) {
                    eprintln!(
                        "debug: suppress shortcut text chars={}",
                        text.chars().count()
                    );
                }
            }
            return;
        }

        let Some(text) = printable_keyboard_text(event.text.as_deref()) else {
            return;
        };

        if self.debug {
            eprintln!("debug: keyboard text commit chars={}", text.chars().count());
        }
        if ime::can_compose(&text) {
            if self.debug {
                eprintln!("debug: skip keyboard composable text; waiting for IME event");
            }
            return;
        }
        self.send_host_text(id, &text);
    }

    fn handle_terminal_macos_new_tab_shortcut(&mut self, id: u64, event: &KeyEvent) -> bool {
        let target_is_terminal = self
            .windows
            .get(&id)
            .is_some_and(|view| is_terminal_window_title(&view.title));
        if !is_terminal_new_tab_shortcut(
            event.state,
            event.repeat,
            &event.physical_key,
            &self.modifiers,
            target_is_terminal,
        ) {
            return false;
        }

        self.flush_composer(id);
        let injected_ctrl = !self.pressed_guest_keys.contains_key(&KEY_LEFTCTRL);
        if injected_ctrl {
            self.send_guest_key(id, KEY_LEFTCTRL, InputKeyState::Pressed);
        }
        self.send_guest_key(id, KEY_LEFTSHIFT, InputKeyState::Pressed);
        self.send_guest_key(id, 20, InputKeyState::Pressed);
        self.send_guest_key(id, 20, InputKeyState::Released);
        self.send_guest_key(id, KEY_LEFTSHIFT, InputKeyState::Released);
        if injected_ctrl {
            self.send_guest_key(id, KEY_LEFTCTRL, InputKeyState::Released);
        }

        if self.debug {
            eprintln!("debug: mapped macos terminal Cmd+T to guest Ctrl+Shift+T");
        }
        true
    }

    fn handle_view_shortcut(&mut self, id: u64, key: &Key) -> bool {
        let Key::Character(text) = key else {
            return false;
        };
        let Some(view) = self.windows.get_mut(&id) else {
            return false;
        };
        let Some(next_scale) = next_view_scale(text.as_str(), view.view_scale) else {
            return false;
        };
        view.set_view_scale(next_scale);
        if self.debug {
            eprintln!("debug: view scale id={} scale={:.2}", id, view.view_scale);
        }
        true
    }

    fn send_tracked_guest_key(&mut self, id: u64, keycode: u32, state: ElementState) {
        let (id, state) = match state {
            ElementState::Pressed => {
                self.pressed_guest_keys.insert(keycode, id);
                (id, InputKeyState::Pressed)
            }
            ElementState::Released => {
                let Some(pressed_id) = self.pressed_guest_keys.remove(&keycode) else {
                    if self.debug {
                        eprintln!("debug: suppress unmatched key release keycode={keycode}");
                    }
                    return;
                };
                (pressed_id, InputKeyState::Released)
            }
        };

        self.send_input(InputEvent::Key { id, keycode, state });
    }

    fn send_tracked_guest_modifier_key(&mut self, id: u64, keycode: u32, state: ElementState) {
        if !guest_modifier_refcount_should_forward(
            &mut self.pressed_guest_modifier_counts,
            keycode,
            state,
        ) {
            return;
        }
        self.send_tracked_guest_key(id, keycode, state);
    }

    fn send_guest_key(&self, id: u64, keycode: u32, state: InputKeyState) {
        self.send_input(InputEvent::Key { id, keycode, state });
    }

    fn release_window_guest_keys(&mut self, id: u64) {
        if self.pressed_guest_keys.is_empty() {
            return;
        }

        let keycodes: Vec<u32> = self
            .pressed_guest_keys
            .iter()
            .filter_map(|(keycode, pressed_id)| (*pressed_id == id).then_some(*keycode))
            .collect();
        if self.debug {
            eprintln!("debug: release pressed guest keys count={}", keycodes.len());
        }
        for keycode in keycodes {
            self.pressed_guest_keys.remove(&keycode);
            self.pressed_guest_modifier_counts.remove(&keycode);
            self.send_input(InputEvent::Key {
                id,
                keycode,
                state: InputKeyState::Released,
            });
        }
    }

    fn release_guest_key_if_pressed(&mut self, keycode: u32, reason: &str) -> bool {
        self.pressed_guest_modifier_counts.remove(&keycode);
        let Some(pressed_id) = self.pressed_guest_keys.remove(&keycode) else {
            return false;
        };
        if self.debug {
            eprintln!("debug: release guest key keycode={keycode} reason={reason}");
        }
        self.send_input(InputEvent::Key {
            id: pressed_id,
            keycode,
            state: InputKeyState::Released,
        });
        true
    }

    fn clear_shortcut_modifier_cache(&mut self, id: u64, reason: &str, release_guest_ctrl: bool) {
        if self.debug {
            eprintln!(
                "debug: clearing shortcut modifiers reason={reason} super={} ctrl={}",
                self.modifiers.super_key(),
                self.modifiers.control_key()
            );
        }
        self.modifiers = ModifiersState::empty();
        if release_guest_ctrl {
            self.release_guest_key_if_pressed(KEY_LEFTCTRL, reason);
        }
        self.flush_composer(id);
    }

    fn clear_stale_shortcut_modifiers_for_text(&mut self, id: u64, reason: &str) -> bool {
        if !shortcut_modifiers_active(self.modifiers)
            || self.pressed_guest_keys.contains_key(&KEY_LEFTCTRL)
        {
            return false;
        }
        self.clear_shortcut_modifier_cache(id, reason, false);
        true
    }

    fn send_host_text(&mut self, id: u64, text: &str) {
        if ime::can_compose(text) {
            if self.debug {
                eprintln!("debug: input composer chars={}", text.chars().count());
            }
            let actions = self.composer_push_text(id, text);
            self.send_composer_actions(id, actions);
            return;
        }
        self.flush_composer(id);
        self.send_input(InputEvent::Text {
            id,
            text: text.to_string(),
        });
    }

    fn flush_composer(&mut self, id: u64) {
        let actions = self.composer_flush(id);
        self.send_composer_actions(id, actions);
    }

    fn composer_is_empty(&self, id: u64) -> bool {
        self.windows
            .get(&id)
            .map_or(true, |view| view.composer.is_empty())
    }

    fn composer_backspace(&mut self, id: u64) -> Vec<ComposerAction> {
        self.windows
            .get_mut(&id)
            .map(|view| view.composer.backspace())
            .unwrap_or_default()
    }

    fn composer_push_text(&mut self, id: u64, text: &str) -> Vec<ComposerAction> {
        self.windows
            .get_mut(&id)
            .map(|view| view.composer.push_text(text))
            .unwrap_or_default()
    }

    fn composer_flush(&mut self, id: u64) -> Vec<ComposerAction> {
        self.windows
            .get_mut(&id)
            .map(|view| view.composer.flush())
            .unwrap_or_default()
    }

    fn send_composer_actions(&self, id: u64, actions: Vec<ComposerAction>) {
        for action in actions {
            match action {
                ComposerAction::Commit(text) if !text.is_empty() => {
                    if self.debug {
                        eprintln!("debug: composer commit '{text}'");
                    }
                    self.send_input(InputEvent::Text { id, text });
                }
                ComposerAction::Preedit(Some(text)) => {
                    if self.debug {
                        eprintln!("debug: composer preedit '{text}'");
                    }
                    let cursor = usize_to_i32(text.len());
                    self.send_input(InputEvent::Preedit {
                        id,
                        text,
                        cursor_begin: cursor,
                        cursor_end: cursor,
                    });
                }
                ComposerAction::Preedit(None) => {
                    self.send_input(InputEvent::Preedit {
                        id,
                        text: String::new(),
                        cursor_begin: -1,
                        cursor_end: -1,
                    });
                }
                ComposerAction::Commit(_) => {}
            }
        }
    }
}

fn viewer_window_attributes(
    title: &str,
    icon: Option<winit::window::Icon>,
    host_chrome: bool,
) -> WindowAttributes {
    let attrs = WindowAttributes::default()
        .with_title(title)
        .with_window_icon(icon)
        .with_visible(false)
        .with_resizable(true)
        .with_min_inner_size(PhysicalSize::new(MIN_VIEWER_WIDTH, MIN_VIEWER_HEIGHT))
        .with_inner_size(PhysicalSize::new(
            INITIAL_HIDDEN_WINDOW_SIZE,
            INITIAL_HIDDEN_WINDOW_SIZE,
        ));

    // macOS chrome layout is decided once at ViewerApp construction
    // (see `ViewerApp::host_chrome_enabled`) and passed in here so every
    // toplevel in a given session is consistent. Default: a standard
    // macOS titlebar above the guest content, similar to Parallels.
    // Turning it off falls back to the older transparent borderless
    // layout, useful when a guest app paints its own decorations all the
    // way to the window edges.
    #[cfg(target_os = "macos")]
    let attrs = if host_chrome {
        attrs
    } else {
        attrs
            .with_decorations(false)
            .with_transparent(true)
            .with_title_hidden(true)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
    };

    #[cfg(not(target_os = "macos"))]
    let _ = host_chrome;
    #[cfg(not(target_os = "macos"))]
    let attrs = attrs.with_decorations(false);

    attrs
}

fn configure_host_window(_window: &Window) {
    // The viewer used to disable the NSWindow shadow to look more like a
    // bare guest surface. Now that we render with the standard macOS
    // titlebar (Parallels-style), keep the default shadow so the window
    // visually pops off the desktop.
}

fn set_initial_window_position(window: &Window, id: u64) {
    let offset = id.saturating_sub(1).min(8).try_into().unwrap_or(0);
    let offset = offset * INITIAL_WINDOW_CASCADE_PX;
    window.set_outer_position(PhysicalPosition::new(
        INITIAL_WINDOW_X + offset,
        INITIAL_WINDOW_Y + offset,
    ));
}

pub(crate) fn matte_for_theme(theme: Option<Theme>) -> u32 {
    match theme {
        Some(Theme::Light) => LIGHT_MATTE_PIXEL,
        Some(Theme::Dark) | None => DARK_MATTE_PIXEL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_window_title_detection_covers_ptyxis_titles() {
        assert!(is_terminal_window_title("user@fedora-linux-42:~"));
        assert!(is_terminal_window_title("Ptyxis"));
        assert!(is_terminal_window_title("터미널"));
        assert!(!is_terminal_window_title("Firefox"));
    }

    // ---- window_title -----------------------------------------------------
    //
    // Story: when the guest gives us an empty title (some GTK4 apps until
    // their first xdg_toplevel.set_title call) we show "vbox" so the
    // window doesn't appear nameless in Mission Control. Any non-empty
    // title falls through verbatim.

    #[test]
    fn window_title_replaces_empty_with_vbox() {
        assert_eq!(window_title(""), "vbox");
    }

    #[test]
    fn window_title_keeps_non_empty_value_verbatim() {
        assert_eq!(window_title("GNOME Calculator"), "GNOME Calculator");
        // Whitespace-only titles are unusual but real — keep them as-is so
        // the operator can spot a misbehaving guest rather than swallowing
        // the bug behind a "vbox" fallback.
        assert_eq!(window_title("   "), "   ");
    }

    // ---- compute_remote_size ----------------------------------------------
    //
    // Story: when the host window is resized, we need to tell the guest
    // compositor the matching guest-side size. The math undoes the
    // viewer's view_scale and adds back the "hidden" frame chrome (the
    // server-side titlebar lives outside the visible content rect, so
    // the guest sees a larger window than the host shows).

    #[test]
    fn compute_remote_size_unscaled_passes_inner_through() {
        // scale=1.0, no hidden chrome → guest gets exactly the host's
        // inner size.
        let inner = PhysicalSize::new(1024, 768);
        assert_eq!(
            compute_remote_size(inner, 1.0, 1024, 768, 1024, 768),
            (1024, 768)
        );
    }

    #[test]
    fn compute_remote_size_adds_hidden_chrome() {
        // Frame is 1024x800, content is 1024x768 → 32px of "hidden"
        // height (server-side titlebar). Guest must see 800 even though
        // the host only renders 768.
        let inner = PhysicalSize::new(1024, 768);
        assert_eq!(
            compute_remote_size(inner, 1.0, 1024, 768, 1024, 800),
            (1024, 800)
        );
    }

    #[test]
    fn compute_remote_size_has_no_hidden_chrome_when_content_is_raw_frame() {
        // Firefox/Chrome pass the effective raw frame as their content
        // rect. That makes hidden chrome 0, so fullscreen-exit resync
        // does not add a phantom titlebar height.
        let inner = PhysicalSize::new(1808, 1139);
        assert_eq!(
            compute_remote_size(inner, 1.0, 1860, 1191, 1860, 1191),
            (1808, 1139)
        );
    }

    #[test]
    fn compute_remote_size_divides_by_view_scale() {
        // The viewer is at 2x scale (retina) — host inner is 2048x1536
        // but the guest only thinks about logical pixels (1024x768).
        let inner = PhysicalSize::new(2048, 1536);
        assert_eq!(
            compute_remote_size(inner, 2.0, 1024, 768, 1024, 768),
            (1024, 768)
        );
    }

    #[test]
    fn compute_remote_size_clamps_zero_inner_to_one_pixel() {
        // A 0x0 inner size can briefly happen mid-resize on macOS; we
        // must not return 0x0 (the guest's wl_compositor refuses zero
        // dimensions). 1x1 is the smallest legal value.
        let inner = PhysicalSize::new(0, 0);
        assert_eq!(compute_remote_size(inner, 1.0, 0, 0, 0, 0), (1, 1));
    }

    #[test]
    fn compute_remote_size_view_scale_floor_avoids_div_by_zero() {
        // view_scale=0.0 would divide-by-zero; the helper clamps to
        // 0.01 internally. Result must be finite.
        let inner = PhysicalSize::new(100, 100);
        let (w, h) = compute_remote_size(inner, 0.0, 100, 100, 100, 100);
        assert!(w > 0 && h > 0, "got {w}x{h}");
    }

    // ---- is_terminal_new_tab_shortcut -----------------------------------
    //
    // Story: macOS users instinctively press Cmd+T to open a new
    // terminal tab. The default Wayland-side guest terminal doesn't
    // map Cmd+T (no Super-as-tab convention in GNOME terminals), so we
    // translate to Ctrl+Shift+T. The translation is gated on three
    // signals — the right modifier combo, the right physical key, and a
    // window title that smells like a terminal — otherwise we'd
    // accidentally hijack Cmd+T in unrelated apps.

    fn super_mods() -> ModifiersState {
        let mut m = ModifiersState::empty();
        m |= ModifiersState::SUPER;
        m
    }

    #[test]
    fn terminal_new_tab_matches_cmd_t_press_on_terminal() {
        assert!(is_terminal_new_tab_shortcut(
            ElementState::Pressed,
            false,
            &PhysicalKey::Code(KeyCode::KeyT),
            &super_mods(),
            true
        ));
    }

    #[test]
    fn terminal_new_tab_ignores_release_event() {
        // We only translate on the down stroke — releases are inert. A
        // duplicate Released event would otherwise inject a second
        // Ctrl+Shift+T into the guest on every Cmd+T.
        assert!(!is_terminal_new_tab_shortcut(
            ElementState::Released,
            false,
            &PhysicalKey::Code(KeyCode::KeyT),
            &super_mods(),
            true
        ));
    }

    #[test]
    fn terminal_new_tab_ignores_key_repeat() {
        // Holding Cmd+T must NOT auto-fire new-tab — that would spam
        // the terminal with tabs as long as the key is held.
        assert!(!is_terminal_new_tab_shortcut(
            ElementState::Pressed,
            true,
            &PhysicalKey::Code(KeyCode::KeyT),
            &super_mods(),
            true
        ));
    }

    #[test]
    fn terminal_new_tab_requires_only_super() {
        // Cmd+Shift+T (which IS the guest binding) must not re-translate
        // — that would double up. Same for Cmd+Ctrl+T and Cmd+Alt+T.
        for extra in [
            ModifiersState::SHIFT,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
        ] {
            let mut mods = ModifiersState::SUPER;
            mods |= extra;
            assert!(
                !is_terminal_new_tab_shortcut(
                    ElementState::Pressed,
                    false,
                    &PhysicalKey::Code(KeyCode::KeyT),
                    &mods,
                    true
                ),
                "should reject extra modifier: {extra:?}"
            );
        }
    }

    #[test]
    fn terminal_new_tab_requires_super_modifier() {
        // Plain T without any modifier is just typing a letter — never
        // translate it.
        assert!(!is_terminal_new_tab_shortcut(
            ElementState::Pressed,
            false,
            &PhysicalKey::Code(KeyCode::KeyT),
            &ModifiersState::empty(),
            true
        ));
    }

    #[test]
    fn terminal_new_tab_ignores_other_letter_keys() {
        // Cmd+Y / Cmd+G / Cmd+anything-else must not trigger.
        assert!(!is_terminal_new_tab_shortcut(
            ElementState::Pressed,
            false,
            &PhysicalKey::Code(KeyCode::KeyY),
            &super_mods(),
            true
        ));
    }

    // ---- next_view_scale -------------------------------------------------
    //
    // Story: Cmd+Plus zooms in, Cmd+Minus zooms out, Cmd+0 resets — same
    // bindings users expect from a browser. The math is multiplicative
    // so steps stay perceptually uniform across the full scale range.

    #[test]
    fn view_scale_plus_zooms_in() {
        // Both `+` (Shift-numpad) and `=` (the unshifted key) map to
        // zoom in — accommodates US vs international keyboards.
        let next = next_view_scale("+", 1.0).unwrap();
        assert!(next > 1.0);
        let next_eq = next_view_scale("=", 1.0).unwrap();
        assert!(next_eq > 1.0);
        // Both must produce the same value (no asymmetry).
        assert!((next - next_eq).abs() < f32::EPSILON);
    }

    #[test]
    fn view_scale_minus_zooms_out() {
        let next = next_view_scale("-", 1.0).unwrap();
        assert!(next < 1.0);
        let next_under = next_view_scale("_", 1.0).unwrap();
        assert!(next_under < 1.0);
    }

    #[test]
    fn view_scale_zero_resets_to_one() {
        // The reset target is exactly 1.0 regardless of the prior scale.
        assert_eq!(next_view_scale("0", 2.5), Some(1.0));
        assert_eq!(next_view_scale("0", 0.3), Some(1.0));
    }

    #[test]
    fn view_scale_unknown_character_returns_none() {
        // Letter "x" or any non-shortcut character must let the key fall
        // through to the guest — None tells the caller to keep going.
        assert!(next_view_scale("x", 1.0).is_none());
        assert!(next_view_scale("", 1.0).is_none());
        assert!(next_view_scale("a", 1.0).is_none());
    }

    #[test]
    fn view_scale_zoom_is_reversible() {
        // Zoom in then zoom out from the same starting point should
        // land back on (close to) 1.0 — sanity check that the helper
        // uses inverse multiplicative steps in both directions.
        let zoomed_in = next_view_scale("+", 1.0).unwrap();
        let back = next_view_scale("-", zoomed_in).unwrap();
        assert!((back - 1.0).abs() < 1e-5, "expected ~1.0, got {back}");
    }

    // ---- guest_chrome_dedup_key + guest_chrome_warning_message ----------
    //
    // Story: the viewer shows a one-time stderr hint for apps that ship
    // their own header bar (Firefox, VS Code, etc.). The dedup key
    // tracks who we've warned about; the warning text varies based on
    // whether host chrome is enabled. We pin both pure helpers so a
    // future wording or app-list update is caught.

    #[test]
    fn dedup_key_prefers_app_id_when_available() {
        // app_id is the stable identity — title can change as the user
        // navigates inside the app, but app_id stays put.
        let key = guest_chrome_dedup_key("org.mozilla.firefox", "Mozilla Firefox");
        assert_eq!(key, "org.mozilla.firefox");
    }

    #[test]
    fn dedup_key_falls_back_to_title_when_app_id_blank() {
        // First-frame race: the guest may have a title before the
        // xdg_toplevel.set_app_id call lands. Use the title so we
        // still emit the warning rather than letting "" key it out.
        assert_eq!(
            guest_chrome_dedup_key("", "Firefox Nightly"),
            "Firefox Nightly"
        );
    }

    #[test]
    fn warning_message_with_host_chrome_enabled_recommends_disabling() {
        let msg = guest_chrome_warning_message("Firefox", true);
        assert!(msg.contains("Firefox"));
        assert!(msg.contains(HOST_CHROME_ENV));
        assert!(msg.contains("=0"));
        // Wording must point at the borderless fall-back path.
        assert!(msg.contains("borderless"));
    }

    #[test]
    fn warning_message_with_host_chrome_disabled_explains_quirk() {
        let msg = guest_chrome_warning_message("VS Code", false);
        assert!(msg.contains("VS Code"));
        // The "titlebar is hidden" wording is what tells the user to
        // expect guest-owned clicks at the top of the window.
        assert!(msg.contains("titlebar is hidden"));
    }

    // ---- is_emoji_picker_release_to_swallow ------------------------------
    //
    // Story: macOS's Cmd+Ctrl+Space chord opens the Character Viewer.
    // The Pressed half is intentionally swallowed (it's a host-OS
    // gesture, not for the guest); the Released half must ALSO be
    // swallowed to keep the press/release pair balanced. Test the
    // exact combination — the helper must fire only when state ==
    // Released AND Space AND both Cmd and Ctrl modifiers are held.

    #[test]
    fn emoji_picker_release_swallowed_on_ctrl_cmd_space() {
        let mods = ModifiersState::SUPER | ModifiersState::CONTROL;
        assert!(is_emoji_picker_release_to_swallow(
            ElementState::Released,
            PhysicalKey::Code(KeyCode::Space),
            mods,
        ));
    }

    #[test]
    fn emoji_picker_release_not_swallowed_on_pressed_event() {
        // Pressed half is handled by `is_macos_emoji_picker_shortcut`;
        // this helper only governs the Released half.
        let mods = ModifiersState::SUPER | ModifiersState::CONTROL;
        assert!(!is_emoji_picker_release_to_swallow(
            ElementState::Pressed,
            PhysicalKey::Code(KeyCode::Space),
            mods,
        ));
    }

    #[test]
    fn emoji_picker_release_not_swallowed_without_both_modifiers() {
        for mods in [
            ModifiersState::SUPER,
            ModifiersState::CONTROL,
            ModifiersState::empty(),
        ] {
            assert!(
                !is_emoji_picker_release_to_swallow(
                    ElementState::Released,
                    PhysicalKey::Code(KeyCode::Space),
                    mods,
                ),
                "should not swallow with {mods:?}",
            );
        }
    }

    #[test]
    fn emoji_picker_release_not_swallowed_on_other_key() {
        // Cmd+Ctrl+A release is not the picker chord — let it through.
        let mods = ModifiersState::SUPER | ModifiersState::CONTROL;
        assert!(!is_emoji_picker_release_to_swallow(
            ElementState::Released,
            PhysicalKey::Code(KeyCode::KeyA),
            mods,
        ));
    }

    #[test]
    fn guest_modifier_refcount_holds_shared_ctrl_until_all_sources_release() {
        let mut counts = HashMap::new();

        assert!(guest_modifier_refcount_should_forward(
            &mut counts,
            KEY_LEFTCTRL,
            ElementState::Pressed
        ));
        assert!(!guest_modifier_refcount_should_forward(
            &mut counts,
            KEY_LEFTCTRL,
            ElementState::Pressed
        ));
        assert!(!guest_modifier_refcount_should_forward(
            &mut counts,
            KEY_LEFTCTRL,
            ElementState::Released
        ));
        assert!(guest_modifier_refcount_should_forward(
            &mut counts,
            KEY_LEFTCTRL,
            ElementState::Released
        ));
        assert!(counts.is_empty());
    }

    #[test]
    fn guest_modifier_refcount_suppresses_unmatched_release() {
        let mut counts = HashMap::new();

        assert!(!guest_modifier_refcount_should_forward(
            &mut counts,
            KEY_LEFTCTRL,
            ElementState::Released
        ));
        assert!(counts.is_empty());
    }

    // ---- build_pending_remote_resize ------------------------------------
    //
    // Story: every user-driven host window resize gets bucketed into a
    // PendingRemoteResize with a debounce deadline. We push the actual
    // Resize wire frame only after the deadline elapses to keep the
    // guest from re-laying-out mid-drag.

    #[test]
    fn pending_remote_resize_carries_width_height_and_deadline() {
        let now = Instant::now();
        let debounce = Duration::from_millis(80);
        let resize = build_pending_remote_resize(1024, 768, debounce, now);
        assert_eq!(resize.width, 1024);
        assert_eq!(resize.height, 768);
        assert_eq!(resize.due, now + debounce);
    }

    #[test]
    fn pending_remote_resize_clamps_zero_to_one() {
        // wl_compositor refuses zero dimensions; we clamp 0 → 1 here
        // so a momentary 0x0 inner_size during a macOS resize gesture
        // doesn't kill the surface on the guest.
        let resize = build_pending_remote_resize(0, 0, Duration::from_millis(80), Instant::now());
        assert_eq!(resize.width, 1);
        assert_eq!(resize.height, 1);
    }

    #[test]
    fn pending_remote_resize_keeps_large_values() {
        // A 4K-ish drag must pass through verbatim — clamping must be
        // strictly "≥ 1", not a clamp to some artificial ceiling.
        let resize =
            build_pending_remote_resize(3840, 2160, Duration::from_millis(80), Instant::now());
        assert_eq!(resize.width, 3840);
        assert_eq!(resize.height, 2160);
    }

    #[test]
    fn host_resize_resync_due_uses_fixed_delay() {
        let now = Instant::now();
        let due = build_host_resize_resync_due(Duration::from_millis(180), now);
        assert_eq!(due, now + Duration::from_millis(180));
    }

    #[test]
    fn host_resize_resync_only_grows_when_guest_refuses_shrink() {
        let current = PhysicalSize::new(1650, 1470);
        let desired = PhysicalSize::new(1798, 1618);
        assert!(host_resize_resync_needed(current, desired));
    }

    #[test]
    fn host_resize_resync_ignores_close_or_smaller_targets() {
        let current = PhysicalSize::new(1650, 1470);
        assert!(!host_resize_resync_needed(
            current,
            PhysicalSize::new(1660, 1460)
        ));
        assert!(!host_resize_resync_needed(
            current,
            PhysicalSize::new(1200, 900)
        ));
    }

    #[test]
    fn release_guest_key_if_pressed_sends_release_to_original_window() {
        let (tx, rx) = mpsc::channel();
        let mut app = ViewerApp::new(tx, None, false, PhysicalSize::new(1920, 1080));
        app.pressed_guest_keys.insert(KEY_LEFTCTRL, 42);
        app.pressed_guest_modifier_counts.insert(KEY_LEFTCTRL, 2);

        assert!(app.release_guest_key_if_pressed(KEY_LEFTCTRL, "test"));

        match rx.try_recv().expect("release event") {
            Message::InputEvent(InputEvent::Key { id, keycode, state }) => {
                assert_eq!(id, 42);
                assert_eq!(keycode, KEY_LEFTCTRL);
                assert_eq!(state, InputKeyState::Released);
            }
            other => panic!("expected key release, got {other:?}"),
        }
        assert!(!app.pressed_guest_keys.contains_key(&KEY_LEFTCTRL));
        assert!(
            !app.pressed_guest_modifier_counts
                .contains_key(&KEY_LEFTCTRL)
        );
    }

    #[test]
    fn stale_shortcut_modifiers_clear_for_printable_text_only_without_tracked_ctrl() {
        let (tx, _rx) = mpsc::channel();
        let mut app = ViewerApp::new(tx, None, false, PhysicalSize::new(1920, 1080));
        app.modifiers = ModifiersState::SUPER;

        assert!(app.clear_stale_shortcut_modifiers_for_text(1, "test"));

        assert!(!shortcut_modifiers_active(app.modifiers));
    }

    #[test]
    fn stale_shortcut_modifiers_preserve_real_tracked_ctrl_shortcut() {
        let (tx, _rx) = mpsc::channel();
        let mut app = ViewerApp::new(tx, None, false, PhysicalSize::new(1920, 1080));
        app.modifiers = ModifiersState::SUPER;
        app.pressed_guest_keys.insert(KEY_LEFTCTRL, 1);

        assert!(!app.clear_stale_shortcut_modifiers_for_text(1, "test"));

        assert!(shortcut_modifiers_active(app.modifiers));
    }

    #[test]
    fn fullscreen_exit_resync_due_uses_fixed_delay() {
        let now = Instant::now();
        let due = build_fullscreen_exit_resync_due(Duration::from_millis(180), now);
        assert_eq!(due, now + Duration::from_millis(180));
    }

    // ---- compute_effective_content_rect / clamp_display_size ------------
    //
    // Story: most GTK guests need the buffer-trimmed content rect (so
    // CSD shadow padding isn't included in click maths); Firefox /
    // Chrome paint their own chrome to the buffer edges and need the
    // raw frame rect for 1:1 click alignment. The helper picks the
    // right rect based on `uses_own_chrome`.

    #[test]
    fn effective_content_uses_content_rect_for_standard_guests() {
        let content = crate::viewer::frame::FrameRect::new(8, 8, 1024, 768);
        let rect = compute_effective_content_rect(false, 1040, 784, content);
        assert_eq!(rect.x, 8);
        assert_eq!(rect.y, 8);
        assert_eq!(rect.w, 1024);
        assert_eq!(rect.h, 768);
    }

    #[test]
    fn effective_content_uses_raw_frame_for_own_chrome_apps() {
        // Firefox: even if the trim suggested a smaller content rect,
        // we ignore it and return the full buffer (0,0,W,H). Otherwise
        // clicks land below the visible row.
        let content = crate::viewer::frame::FrameRect::new(10, 50, 800, 600);
        let rect = compute_effective_content_rect(true, 1024, 768, content);
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
        assert_eq!(rect.w, 1024);
        assert_eq!(rect.h, 768);
    }

    #[test]
    fn effective_content_clamps_zero_frame_to_one() {
        // A degenerate 0x0 frame mid-resize must still produce a 1x1
        // rect so softbuffer doesn't refuse the surface.
        let content = crate::viewer::frame::FrameRect::new(0, 0, 0, 0);
        let rect = compute_effective_content_rect(true, 0, 0, content);
        assert_eq!(rect.w, 1);
        assert_eq!(rect.h, 1);
    }

    #[test]
    fn own_chrome_display_size_uses_raw_frame_not_trimmed_content() {
        let content = crate::viewer::frame::FrameRect::new(16, 44, 960, 540);
        let rect = compute_effective_content_rect(true, 1280, 720, content);
        assert_eq!(clamp_display_size(rect.w, rect.h), (1280, 720));
    }

    #[test]
    fn clamp_display_size_passes_through_positive_values() {
        assert_eq!(clamp_display_size(800, 600), (800, 600));
        assert_eq!(clamp_display_size(1, 1), (1, 1));
    }

    #[test]
    fn clamp_display_size_promotes_zeros_to_one() {
        assert_eq!(clamp_display_size(0, 0), (1, 1));
        assert_eq!(clamp_display_size(0, 600), (1, 600));
        assert_eq!(clamp_display_size(800, 0), (800, 1));
    }

    // ---- clamp_view_scale -----------------------------------------------
    //
    // Story: a zoom shortcut keeps multiplying by VIEW_SCALE_STEP — left
    // unchecked it eventually overflows past usable scale. We clamp to
    // the documented [MIN_VIEW_SCALE, MAX_VIEW_SCALE] range so
    // operators can't accidentally fly the viewport into oblivion.

    #[test]
    fn clamp_view_scale_passes_through_in_range() {
        assert_eq!(clamp_view_scale(1.0), 1.0);
        assert_eq!(clamp_view_scale(0.5), 0.5);
        assert_eq!(clamp_view_scale(1.5), 1.5);
    }

    #[test]
    fn clamp_view_scale_caps_at_max() {
        // Pretend the operator hammered Cmd+Plus 50 times — we should
        // never exceed MAX_VIEW_SCALE.
        let huge = MAX_VIEW_SCALE * 10.0;
        assert_eq!(clamp_view_scale(huge), MAX_VIEW_SCALE);
    }

    #[test]
    fn clamp_view_scale_floors_at_min() {
        // Same for the other direction — Cmd+Minus 50 times must floor
        // at MIN_VIEW_SCALE.
        assert_eq!(clamp_view_scale(0.001), MIN_VIEW_SCALE);
        // Negative values are nonsensical but the clamp must handle them
        // without producing NaN or panicking.
        assert_eq!(clamp_view_scale(-1.0), MIN_VIEW_SCALE);
    }

    // ---- should_apply_theme_change --------------------------------------

    #[test]
    fn theme_change_skipped_when_matte_unchanged() {
        // Same matte → no redraw. macOS sends ThemeChanged spuriously
        // on wake-from-sleep with the same theme; we ignore those.
        assert!(!should_apply_theme_change(
            DARK_MATTE_PIXEL,
            DARK_MATTE_PIXEL
        ));
        assert!(!should_apply_theme_change(
            LIGHT_MATTE_PIXEL,
            LIGHT_MATTE_PIXEL
        ));
    }

    #[test]
    fn theme_change_applied_when_matte_differs() {
        // Actual light→dark or dark→light flips must redraw so the
        // viewer's matte (the area outside the guest's content rect)
        // matches the new system theme.
        assert!(should_apply_theme_change(
            LIGHT_MATTE_PIXEL,
            DARK_MATTE_PIXEL
        ));
        assert!(should_apply_theme_change(
            DARK_MATTE_PIXEL,
            LIGHT_MATTE_PIXEL
        ));
    }

    // ---- is_titlebar_double_click ---------------------------------------
    //
    // Story: the host titlebar double-click toggles fullscreen the way
    // macOS expects. We have to recognise "two clicks, close in time
    // AND close in space" with a tolerance for jittery touchpads. The
    // helper makes the timing + slop gate testable without a real
    // Instant clock.

    fn click_at(now: Instant, x: i32, y: i32) -> TitlebarClick {
        TitlebarClick { at: now, x, y }
    }

    #[test]
    fn double_click_requires_a_prior_click() {
        // No prior click in memory → never a double-click (it's the
        // first half of one, at most).
        assert!(!is_titlebar_double_click(
            None,
            Instant::now(),
            10,
            5,
            Duration::from_millis(500),
            24
        ));
    }

    #[test]
    fn double_click_matches_when_close_in_time_and_space() {
        let prior_at = Instant::now();
        let now = prior_at + Duration::from_millis(300);
        assert!(is_titlebar_double_click(
            Some(click_at(prior_at, 100, 10)),
            now,
            105, // within slop
            12,  // within slop
            Duration::from_millis(500),
            24
        ));
    }

    #[test]
    fn double_click_rejects_when_interval_exceeded() {
        let prior_at = Instant::now();
        let now = prior_at + Duration::from_millis(800);
        assert!(!is_titlebar_double_click(
            Some(click_at(prior_at, 100, 10)),
            now,
            100,
            10,
            Duration::from_millis(500),
            24
        ));
    }

    #[test]
    fn double_click_rejects_when_x_distance_exceeded() {
        let prior_at = Instant::now();
        let now = prior_at + Duration::from_millis(100);
        assert!(!is_titlebar_double_click(
            Some(click_at(prior_at, 100, 10)),
            now,
            200, // way past slop
            10,
            Duration::from_millis(500),
            24
        ));
    }

    #[test]
    fn double_click_rejects_when_y_distance_exceeded() {
        let prior_at = Instant::now();
        let now = prior_at + Duration::from_millis(100);
        assert!(!is_titlebar_double_click(
            Some(click_at(prior_at, 100, 10)),
            now,
            100,
            200, // way past slop
            Duration::from_millis(500),
            24
        ));
    }

    // ---- apply_resize_direction -----------------------------------------
    //
    // Story: when the operator drags a window edge or corner, we
    // translate the cursor delta into a new (left, top, right, bottom)
    // rectangle. Each direction must update only the corresponding
    // edges — drag the east edge, top stays put. A bug here makes
    // manual-resize feel wrong; we pin every combination.

    fn drag_from(direction: winit::window::ResizeDirection) -> ResizeDrag {
        ResizeDrag {
            direction,
            start_outer: PhysicalPosition::new(100, 200),
            start_size: PhysicalSize::new(800, 600),
        }
    }

    #[test]
    fn resize_east_moves_only_the_right_edge() {
        use winit::window::ResizeDirection;
        let (l, t, r, b) = apply_resize_direction(&drag_from(ResizeDirection::East), 950.0, 250.0);
        assert_eq!(l, 100.0);
        assert_eq!(t, 200.0);
        assert_eq!(r, 950.0);
        assert_eq!(b, 800.0, "bottom must remain at start_outer.y + height");
    }

    #[test]
    fn resize_west_moves_only_the_left_edge() {
        use winit::window::ResizeDirection;
        let (l, t, r, b) = apply_resize_direction(&drag_from(ResizeDirection::West), 50.0, 250.0);
        assert_eq!(l, 50.0);
        assert_eq!(r, 900.0, "right stays at start_outer.x + width");
        assert_eq!(t, 200.0);
        assert_eq!(b, 800.0);
    }

    #[test]
    fn resize_north_moves_only_the_top_edge() {
        use winit::window::ResizeDirection;
        let (l, t, r, b) = apply_resize_direction(&drag_from(ResizeDirection::North), 250.0, 150.0);
        assert_eq!(t, 150.0);
        assert_eq!(b, 800.0);
        assert_eq!(l, 100.0);
        assert_eq!(r, 900.0);
    }

    #[test]
    fn resize_south_moves_only_the_bottom_edge() {
        use winit::window::ResizeDirection;
        let (_l, t, _r, b) =
            apply_resize_direction(&drag_from(ResizeDirection::South), 250.0, 850.0);
        assert_eq!(t, 200.0);
        assert_eq!(b, 850.0);
    }

    #[test]
    fn resize_northeast_moves_top_and_right() {
        use winit::window::ResizeDirection;
        let (l, t, r, b) =
            apply_resize_direction(&drag_from(ResizeDirection::NorthEast), 950.0, 150.0);
        assert_eq!(l, 100.0);
        assert_eq!(t, 150.0);
        assert_eq!(r, 950.0);
        assert_eq!(b, 800.0);
    }

    #[test]
    fn resize_southwest_moves_bottom_and_left() {
        use winit::window::ResizeDirection;
        let (l, t, r, b) =
            apply_resize_direction(&drag_from(ResizeDirection::SouthWest), 50.0, 850.0);
        assert_eq!(l, 50.0);
        assert_eq!(t, 200.0);
        assert_eq!(r, 900.0);
        assert_eq!(b, 850.0);
    }

    // ---- partition_pending_resizes -------------------------------------
    //
    // Story: each tick of the event loop walks the pending-resize
    // bucket. Entries whose due time has elapsed get flushed to the
    // wire; the rest stay in the bucket. The `next_due` value is what
    // the event loop will WaitUntil — picking the soonest remaining
    // deadline gives the lowest-latency flush without busy-waiting.

    fn pending(width: u32, height: u32, due: Instant) -> PendingRemoteResize {
        PendingRemoteResize { width, height, due }
    }

    // ---- cursor_to_frame_coords -----------------------------------------
    //
    // Story: host cursor pixels need to translate to guest frame pixels
    // so the guest's mouse maps to the right widget. The math undoes
    // the aspect-fit letterboxing inside the host window, scales by
    // the content-vs-display ratio, and clamps to legal frame
    // indices. Drive it on the geometries operators actually hit.

    fn rect(x: u32, y: u32, w: u32, h: u32) -> crate::viewer::frame::FrameRect {
        crate::viewer::frame::FrameRect::new(x, y, w, h)
    }

    #[test]
    fn cursor_top_left_in_host_maps_to_content_origin() {
        // Host window exactly fits content (no letterbox). Top-left of
        // host = origin of content.
        let coords = cursor_to_frame_coords(0.0, 0.0, (800, 600), rect(0, 0, 800, 600), (800, 600));
        assert_eq!(coords, (0, 0));
    }

    #[test]
    fn cursor_bottom_right_in_host_maps_to_max_frame_index() {
        // Host bottom-right corner → last valid frame index, NOT
        // beyond. Without the clamp the guest sees an out-of-bounds
        // pointer and may panic.
        let coords =
            cursor_to_frame_coords(799.0, 599.0, (800, 600), rect(0, 0, 800, 600), (800, 600));
        assert_eq!(coords, (799, 599));
    }

    #[test]
    fn cursor_outside_letterbox_clamps_to_frame_edge() {
        // 800x600 host with 800x400 content (16:8 letterbox top/bottom).
        // Cursor at y=599 lands below the content rect and must clamp
        // to the bottom-most legal frame y.
        let coords =
            cursor_to_frame_coords(400.0, 599.0, (800, 600), rect(0, 0, 800, 400), (800, 400));
        // Frame is 800×400; max y is 399.
        assert_eq!(coords.1, 399);
    }

    #[test]
    fn cursor_negative_host_pixel_clamps_to_origin() {
        // A negative host pixel (winit can briefly deliver these on
        // macOS during fast cursor moves) must clamp to (0, 0).
        let coords =
            cursor_to_frame_coords(-5.0, -5.0, (800, 600), rect(0, 0, 800, 600), (800, 600));
        assert_eq!(coords, (0, 0));
    }

    #[test]
    fn cursor_zero_sized_inner_does_not_panic() {
        // Inner clamped to (1,1) upstream; the helper must not panic
        // on the minimum valid size.
        let coords = cursor_to_frame_coords(0.0, 0.0, (1, 1), rect(0, 0, 1, 1), (1, 1));
        assert_eq!(coords, (0, 0));
    }

    // ---- matches_pending_programmatic_resize ---------------------------
    //
    // Story: when we programmatically resize the host window we set a
    // one-shot gate so the subsequent Resized event isn't classified as
    // a user gesture (which would round-trip back to the guest). The
    // gate is fired with a ±32px slop because winit can shift the
    // delivered size from the requested size during DPI rounding.

    #[test]
    fn programmatic_resize_no_pending_means_user_gesture() {
        let actual = PhysicalSize::new(1024, 768);
        assert!(!matches_pending_programmatic_resize(None, actual));
    }

    #[test]
    fn programmatic_resize_exact_match_consumes_pending() {
        let actual = PhysicalSize::new(1024, 768);
        assert!(matches_pending_programmatic_resize(Some(actual), actual));
    }

    #[test]
    fn programmatic_resize_within_slop_consumes_pending() {
        // ±32px slop — a 5px drift is fine.
        let pending = PhysicalSize::new(1024, 768);
        let actual = PhysicalSize::new(1029, 763);
        assert!(matches_pending_programmatic_resize(Some(pending), actual));
    }

    #[test]
    fn programmatic_resize_consumes_single_axis_os_clamp() {
        let pending = PhysicalSize::new(2312, 2262);
        let actual = PhysicalSize::new(2312, 2194);
        assert!(matches_pending_programmatic_resize(Some(pending), actual));
    }

    #[test]
    fn programmatic_resize_gate_clears_on_match() {
        let mut pending = Some(PhysicalSize::new(1024, 768));
        assert!(consume_programmatic_resize_gate(
            &mut pending,
            PhysicalSize::new(1029, 763)
        ));
        assert_eq!(pending, None);
    }

    #[test]
    fn programmatic_resize_gate_clears_stale_far_resize() {
        let mut pending = Some(PhysicalSize::new(1024, 768));
        assert!(!consume_programmatic_resize_gate(
            &mut pending,
            PhysicalSize::new(1600, 1200)
        ));
        assert_eq!(pending, None);
    }

    // ---- physical_to_logical_size --------------------------------------
    //
    // Story: edge-snap targets arrive in physical pixels; macOS's
    // `request_inner_size` expects logical points. On a 2x retina,
    // 1920 physical → 960 logical. On a non-retina display, 1920 →
    // 1920 (scale=1.0). The clamp at scale ≥ 1.0 guards against an
    // impossible-but-defensive 0 from a buggy winit.

    #[test]
    fn physical_to_logical_unscaled_passes_through() {
        let (w, h) = physical_to_logical_size(1920, 1080, 1.0);
        assert_eq!((w, h), (1920.0, 1080.0));
    }

    #[test]
    fn physical_to_logical_retina_halves() {
        let (w, h) = physical_to_logical_size(1920, 1080, 2.0);
        assert_eq!((w, h), (960.0, 540.0));
    }

    #[test]
    fn physical_to_logical_fractional_scale_rounds() {
        // 1.5x is unusual but plausible on some Linux desktops; the
        // round() should land on the nearest whole logical pixel.
        let (w, _) = physical_to_logical_size(1500, 900, 1.5);
        assert_eq!(w, 1000.0);
    }

    #[test]
    fn physical_to_logical_floors_scale_at_one() {
        // Defensive: a scale below 1.0 would otherwise inflate the
        // logical size past the physical — pointless and confusing.
        let (w, h) = physical_to_logical_size(800, 600, 0.5);
        assert_eq!((w, h), (800.0, 600.0));
    }

    #[test]
    fn physical_to_logical_promotes_zero_to_one_pixel() {
        // A momentary 0 physical width during a resize must produce
        // a 1-pixel logical width — never zero (winit refuses).
        let (w, h) = physical_to_logical_size(0, 0, 2.0);
        assert_eq!((w, h), (1.0, 1.0));
    }

    #[test]
    fn programmatic_resize_far_outside_slop_is_user_gesture() {
        let pending = PhysicalSize::new(1024, 768);
        let actual = PhysicalSize::new(1920, 1080);
        assert!(!matches_pending_programmatic_resize(Some(pending), actual));
    }

    #[test]
    fn cursor_inside_content_with_offset_origin_returns_offset_coords() {
        // Content rect starts at (10, 5) — operators with guests that
        // trim CSD shadows see this. The mapping must add the offset
        // back so the guest receives true content coordinates.
        let coords =
            cursor_to_frame_coords(0.0, 0.0, (800, 600), rect(10, 5, 800, 600), (820, 610));
        assert_eq!(coords, (10, 5));
    }

    #[test]
    fn partition_returns_empty_buckets_for_empty_input() {
        let (ready, waiting, next_due) = partition_pending_resizes(&[], Instant::now());
        assert!(ready.is_empty());
        assert!(waiting.is_empty());
        assert!(next_due.is_none());
    }

    #[test]
    fn partition_flushes_only_due_entries() {
        let now = Instant::now();
        let due_past = now - Duration::from_millis(10);
        let due_future = now + Duration::from_millis(100);
        let input = vec![
            (1u64, pending(1024, 768, due_past)),
            (2u64, pending(800, 600, due_future)),
        ];

        let (ready, waiting, next_due) = partition_pending_resizes(&input, now);

        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0], (1, 1024, 768));
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].0, 2);
        // Soonest remaining due — exactly the future entry's deadline.
        assert_eq!(next_due, Some(due_future));
    }

    #[test]
    fn partition_picks_earliest_waiting_due_when_multiple() {
        let now = Instant::now();
        let close = now + Duration::from_millis(20);
        let far = now + Duration::from_millis(200);
        let input = vec![
            (1u64, pending(100, 100, far)),
            (2u64, pending(200, 200, close)),
            (3u64, pending(300, 300, far)),
        ];

        let (_ready, _waiting, next_due) = partition_pending_resizes(&input, now);

        // Three pending; all still waiting. Next wake-up should be the
        // soonest deadline so the loop doesn't oversleep the first
        // flush.
        assert_eq!(next_due, Some(close));
    }

    #[test]
    fn partition_flushes_all_when_every_entry_is_due() {
        let now = Instant::now();
        let due = now - Duration::from_millis(1);
        let input = vec![
            (1u64, pending(800, 600, due)),
            (2u64, pending(1024, 768, due)),
        ];

        let (ready, waiting, next_due) = partition_pending_resizes(&input, now);

        assert_eq!(ready.len(), 2);
        assert!(waiting.is_empty());
        assert!(next_due.is_none(), "no more entries → no next_due");
    }

    #[test]
    fn partition_treats_exactly_due_as_ready() {
        // The flush rule is `due <= now`. An entry whose due is the
        // exact same Instant as `now` must flush — otherwise the
        // operator's slider drag would never quiesce in pathological
        // clocks where Instant resolution is coarse.
        let now = Instant::now();
        let input = vec![(7u64, pending(100, 100, now))];

        let (ready, waiting, _) = partition_pending_resizes(&input, now);

        assert_eq!(ready.len(), 1);
        assert!(waiting.is_empty());
    }

    #[test]
    fn resize_corner_directions_pin_remaining_two_edges() {
        // Sanity check on the remaining two corners — NorthWest and
        // SouthEast — for symmetry coverage.
        use winit::window::ResizeDirection;
        let (l, t, r, b) =
            apply_resize_direction(&drag_from(ResizeDirection::NorthWest), 50.0, 150.0);
        assert_eq!((l, t, r, b), (50.0, 150.0, 900.0, 800.0));

        let (l, t, r, b) =
            apply_resize_direction(&drag_from(ResizeDirection::SouthEast), 950.0, 850.0);
        assert_eq!((l, t, r, b), (100.0, 200.0, 950.0, 850.0));
    }

    #[test]
    fn double_click_accepts_negative_drift_within_slop() {
        // Slop is |x - prev_x| ≤ slop on both signs. Make sure
        // the helper handles negative drift symmetrically.
        let prior_at = Instant::now();
        let now = prior_at + Duration::from_millis(100);
        assert!(is_titlebar_double_click(
            Some(click_at(prior_at, 100, 10)),
            now,
            95,
            5,
            Duration::from_millis(500),
            24
        ));
    }

    #[test]
    fn terminal_new_tab_skipped_outside_terminal_windows() {
        // The same Cmd+T in Firefox or Calculator must pass through —
        // those apps own their own Cmd+T behaviour.
        assert!(!is_terminal_new_tab_shortcut(
            ElementState::Pressed,
            false,
            &PhysicalKey::Code(KeyCode::KeyT),
            &super_mods(),
            false
        ));
    }

    #[test]
    fn matte_follows_system_theme() {
        assert_eq!(matte_for_theme(Some(Theme::Dark)), DARK_MATTE_PIXEL);
        assert_eq!(matte_for_theme(Some(Theme::Light)), LIGHT_MATTE_PIXEL);
        assert_eq!(matte_for_theme(None), DARK_MATTE_PIXEL);
    }

    #[test]
    fn host_chrome_off_uses_borderless_attributes() {
        let attrs = viewer_window_attributes("vbox", None, false);
        assert!(!attrs.decorations);
        if cfg!(target_os = "macos") {
            assert!(attrs.transparent);
        } else {
            assert!(!attrs.transparent);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn host_chrome_on_uses_visible_decorated_window() {
        let attrs = viewer_window_attributes("vbox", None, true);
        assert!(attrs.decorations);
        assert!(!attrs.transparent);
    }
}
