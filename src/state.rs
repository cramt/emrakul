use std::{ffi::OsString, sync::Arc};

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::utils::on_commit_buffer_handler},
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output,
    delegate_presentation, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{Space, Window},
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
    utils::{Clock, Monotonic, SERIAL_COUNTER, Serial},
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
        for window in self.space.elements().cloned().collect::<Vec<_>>() {
            self.space.unmap_elem(&window);
        }
        let serial = SERIAL_COUNTER.next_serial();
        let foreground = match self.session {
            Session::Ending(_) => None,
            Session::Home(_) | Session::Running(_) => self.toplevels.last().cloned(),
        };
        if let Some(window) = &foreground {
            self.space.map_element(window.clone(), (0, 0), true);
        }
        let focus = foreground
            .as_ref()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, focus, serial);
        }
        self.backend.request_redraw(&self.loop_handle);
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

    // Popups (menus, tooltips) aren't part of the couch UI. Leaving them
    // unconfigured means clients never get to map them.
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
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
