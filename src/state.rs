use std::{ffi::OsString, sync::Arc};

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::utils::on_commit_buffer_handler},
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output,
    delegate_presentation, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupUngrabStrategy, Space, Window,
        find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{Seat, SeatHandler, SeatState, keyboard::XkbConfig, pointer::CursorImageStatus},
    reexports::{
        calloop::{
            Interest, LoopHandle, LoopSignal, Mode as CalloopMode, PostAction, generic::Generic,
        },
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle, SERIAL_COUNTER, Serial, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, with_states},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::{OutputHandler, OutputManagerState},
        presentation::PresentationState,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};

use crate::{
    config::Config,
    drm::Backend,
    lifecycle::{Home, Session},
    recency::Recency,
};

pub struct Emrakul {
    pub config: Config,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Emrakul>,
    pub loop_signal: LoopSignal,
    pub clock: Clock<Monotonic>,
    pub socket_name: OsString,

    /// Every toplevel a client has opened, oldest first. Only the last one is
    /// mapped in `space`: one thing on the TV at a time, and whatever opened
    /// most recently is it. Closing it brings back the one before.
    pub toplevels: Vec<Window>,
    pub space: Space<Window>,
    /// Menus and `<select>` dropdowns. Drawn above the toplevel they belong
    /// to, and only while that toplevel is the foreground.
    pub popups: PopupManager,

    pub session: Session,
    pub recency: Recency,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    _globals: Globals,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<Emrakul>,
    pub seat: Seat<Emrakul>,

    pub backend: Backend,
}

impl Emrakul {
    pub fn new(
        config: Config,
        display: Display<Emrakul>,
        loop_handle: LoopHandle<'static, Emrakul>,
        loop_signal: LoopSignal,
        backend: Backend,
    ) -> anyhow::Result<Self> {
        let dh = display.handle();
        let clock = Clock::new();

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, backend.seat_name());
        seat.add_keyboard(XkbConfig::default(), 400, 30)?;

        let socket_name = Self::listen(display, &loop_handle)?;

        Ok(Self {
            config,
            loop_handle,
            loop_signal,
            socket_name,
            toplevels: Vec::new(),
            space: Space::default(),
            session: Session::Home(Home::default()),
            recency: Recency::load(Recency::default_path()?)?,
            popups: PopupManager::default(),
            compositor_state: CompositorState::new::<Self>(&dh),
            xdg_shell_state: XdgShellState::new::<Self>(&dh),
            shm_state: ShmState::new::<Self>(&dh, vec![]),
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            _globals: Globals {
                _xdg_decoration: XdgDecorationState::new::<Self>(&dh),
                _xdg_output: OutputManagerState::new_with_xdg_output::<Self>(&dh),
                _presentation: PresentationState::new::<Self>(&dh, clock.id() as u32),
                _viewporter: ViewporterState::new::<Self>(&dh),
            },
            data_device_state: DataDeviceState::new::<Self>(&dh),
            seat_state,
            seat,
            clock,
            backend,
            display_handle: dh,
        })
    }

    fn listen(
        display: Display<Emrakul>,
        loop_handle: &LoopHandle<'static, Emrakul>,
    ) -> anyhow::Result<OsString> {
        let socket = ListeningSocketSource::new_auto()?;
        let name = socket.socket_name().to_os_string();
        loop_handle
            .insert_source(socket, |stream, _, state| {
                if let Err(err) = state
                    .display_handle
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(?err, "dropping a client that failed to connect");
                }
            })
            .map_err(|e| anyhow::anyhow!("inserting the wayland socket: {e}"))?;
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, CalloopMode::Level),
                |_, display, state| {
                    // SAFETY: the display is never dropped while the loop runs.
                    unsafe { display.get_mut().dispatch_clients(state)? };
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("inserting the wayland display: {e}"))?;
        Ok(name)
    }

    /// Map the newest toplevel fullscreen, unmap the rest, and give it the
    /// keyboard. The single place the foreground changes. While an app is
    /// ending, Home is already on screen, so nothing is mapped.
    pub fn restack(&mut self) {
        let foreground = match self.session {
            Session::Ending(_) => None,
            Session::Home(_) | Session::Running(_) => self.toplevels.last().cloned(),
        };
        for window in self.space.elements().cloned().collect::<Vec<_>>() {
            self.space.unmap_elem(&window);
            // A menu left open on an app that just lost the screen would come
            // back with it, stale.
            if Some(&window) != foreground.as_ref()
                && let Some(toplevel) = window.toplevel()
            {
                let root = toplevel.wl_surface();
                for (popup, _) in PopupManager::popups_for_surface(root) {
                    let _ = PopupManager::dismiss_popup(root, &popup);
                }
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(window) = &foreground {
            self.space.map_element(window.clone(), (0, 0), true);
        }
        let focus = foreground
            .as_ref()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        if let Some(keyboard) = self.seat.get_keyboard() {
            // A popup grab ignores focus changes until its popups are gone,
            // and the foreground changing is more important than any menu.
            keyboard.unset_grab(self);
            keyboard.set_focus(self, focus, serial);
        }
        self.backend.request_redraw(&self.loop_handle);
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Some(output) = self.backend.output_size() else {
            return;
        };
        let offset = get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        popup.with_pending_state(|state| {
            state.geometry = unconstrained_popup_geometry(&state.positioner, output, offset);
        });
    }

    fn fullscreen(&self, toplevel: &ToplevelSurface) {
        let size = self.backend.output_size();
        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.states.set(xdg_toplevel::State::Activated);
            state.size = size;
        });
    }
}

/// Protocol globals that only have to exist. The delegate macros answer their
/// requests; nothing reads these back.
pub struct Globals {
    _xdg_decoration: XdgDecorationState,
    _xdg_output: OutputManagerState,
    _presentation: PresentationState,
    _viewporter: ViewporterState,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl CompositorHandler for Emrakul {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client is inserted with ClientState")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        if let Some(window) = self
            .toplevels
            .iter()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        {
            window.on_commit();
            let toplevel = window.toplevel().expect("found by its toplevel").clone();
            let initial_configure_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .is_some_and(|data| data.lock().unwrap().initial_configure_sent)
            });
            if !initial_configure_sent {
                toplevel.send_configure();
            }
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            tracing::debug!(geometry = ?popup.with_pending_state(|s| s.geometry), "popup configured");
            // Only fails for a popup whose parent is already gone.
            let _ = popup.send_configure();
        }

        self.backend.request_redraw(&self.loop_handle);
    }
}

impl BufferHandler for Emrakul {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Emrakul {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for Emrakul {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self.backend.import_dmabuf(&dmabuf) {
            let _ = notifier.successful::<Self>();
        } else {
            notifier.failed();
        }
    }
}

impl XdgShellHandler for Emrakul {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::debug!(app_id = ?toplevel_app_id(&surface), "new toplevel");
        self.fullscreen(&surface);
        self.toplevels.push(Window::new_wayland_window(surface));
        self.restack();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        tracing::debug!("toplevel destroyed");
        self.toplevels
            .retain(|w| w.toplevel().is_none_or(|t| t != &surface));
        self.restack();
    }

    // Everything on the TV is fullscreen, so every request to be something
    // else gets the same answer.
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        self.fullscreen(&surface);
        surface.send_pending_configure();
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.fullscreen(&surface);
        surface.send_pending_configure();
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.fullscreen(&surface);
        surface.send_pending_configure();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?err, "tracking a popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| state.positioner = positioner);
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    // The keyboard follows the topmost popup until the client closes it, so
    // the controller's arrows walk a dropdown and Escape dismisses it.
    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let Ok(mut grab) = self.popups.grab_popup(root, popup, &seat, serial) else {
            tracing::debug!("popup grab denied");
            return;
        };
        let Some(keyboard) = seat.get_keyboard() else {
            return;
        };
        // Another grab already holds the keyboard and this popup isn't part
        // of it: it doesn't get to steal input.
        if keyboard.is_grabbed()
            && !(keyboard.has_grab(serial)
                || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
        {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        tracing::debug!("popup grabbed the keyboard");
        keyboard.set_focus(self, grab.current_grab(), serial);
        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
    }
}

impl XdgDecorationHandler for Emrakul {
    // "Server side" with nothing drawn: no title bars on a TV.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel
            .with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ServerSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.new_decoration(toplevel.clone());
        toplevel.send_pending_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.new_decoration(toplevel.clone());
        toplevel.send_pending_configure();
    }
}

impl SeatHandler for Emrakul {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        smithay::wayland::selection::data_device::set_data_device_focus(
            &self.display_handle,
            seat,
            client,
        );
    }
}

impl SelectionHandler for Emrakul {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Emrakul {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl smithay::input::dnd::DndGrabHandler for Emrakul {}
impl WaylandDndGrabHandler for Emrakul {}

impl OutputHandler for Emrakul {}

delegate_compositor!(Emrakul);
delegate_shm!(Emrakul);
delegate_dmabuf!(Emrakul);
delegate_xdg_shell!(Emrakul);
delegate_xdg_decoration!(Emrakul);
delegate_seat!(Emrakul);
delegate_data_device!(Emrakul);
delegate_output!(Emrakul);
delegate_presentation!(Emrakul);
delegate_viewporter!(Emrakul);

fn toplevel_app_id(toplevel: &ToplevelSurface) -> Option<String> {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()?
            .lock()
            .ok()?
            .app_id
            .clone()
    })
}

/// Where a popup goes once it is kept on the output. `offset` is the popup's
/// parent's position in toplevel coordinates (non-zero for a submenu), since
/// the positioner works in the parent's coordinates.
fn unconstrained_popup_geometry(
    positioner: &PositionerState,
    output: Size<i32, Logical>,
    offset: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    positioner.get_unconstrained_geometry(Rectangle::new(Point::default() - offset, output))
}

#[cfg(test)]
mod tests {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
        Anchor, ConstraintAdjustment, Gravity,
    };

    use super::*;

    #[test]
    fn dropdown_near_the_bottom_flips_above_its_select() {
        let dropdown = PositionerState {
            rect_size: (200, 300).into(),
            anchor_rect: Rectangle::new((100, 2000).into(), (150, 30).into()),
            anchor_edges: Anchor::BottomLeft,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideX,
            ..Default::default()
        };
        assert_eq!(
            unconstrained_popup_geometry(&dropdown, Size::new(3840, 2160), (0, 0).into()),
            Rectangle::new((100, 1700).into(), (200, 300).into())
        );
    }

    #[test]
    fn submenu_at_the_right_edge_opens_to_the_left_of_its_menu() {
        // A context menu at x=3600, 220 wide: its submenu would start at 3820
        // and run 250 past the edge, so it flips to the menu's left side.
        let submenu = PositionerState {
            rect_size: (250, 100).into(),
            anchor_rect: Rectangle::new((0, 40).into(), (220, 30).into()),
            anchor_edges: Anchor::TopRight,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::FlipX,
            ..Default::default()
        };
        assert_eq!(
            unconstrained_popup_geometry(&submenu, Size::new(3840, 2160), (3600, 100).into()),
            Rectangle::new((-250, 40).into(), (250, 100).into())
        );
    }

    #[test]
    fn popup_that_fits_stays_where_the_client_asked() {
        let menu = PositionerState {
            rect_size: (220, 400).into(),
            anchor_rect: Rectangle::new((500, 500).into(), (1, 1).into()),
            anchor_edges: Anchor::BottomRight,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::all(),
            ..Default::default()
        };
        assert_eq!(
            unconstrained_popup_geometry(&menu, Size::new(3840, 2160), (0, 0).into()),
            Rectangle::new((501, 501).into(), (220, 400).into())
        );
    }
}
