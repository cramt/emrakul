use std::{os::fd::OwnedFd, path::Path, time::Duration};

use anyhow::{Context, bail};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            dmabuf::Dmabuf,
            format::FormatSet,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportDma, ImportMemWl,
            element::{
                Id, Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                utils::select_dmabuf_feedback,
            },
            gles::GlesRenderer,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
    },
    desktop::{
        PopupManager,
        utils::{OutputPresentationFeedback, surface_presentation_feedback_flags_from_states},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{LoopHandle, RegistrationToken},
        drm::{
            Device as _,
            control::{ModeTypeFlags, connector, crtc},
        },
        input::Libinput,
        rustix::fs::OFlags,
        wayland_protocols::wp::{
            linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags,
            presentation_time::server::wp_presentation_feedback,
        },
    },
    utils::{DeviceFd, Logical, Scale, Size},
    wayland::{
        dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
        presentation::Refresh,
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use crate::{
    config::{Config, Mode},
    state::Emrakul,
};

/// 8-bit only for now. 10-bit is where HDR starts, and that is its own
/// milestone: it needs the colour pipeline to mean something first.
const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Abgr8888, Fourcc::Argb8888];

/// The placeholder Home: whatever no client covers. A fullscreen app covers
/// all of it, so this only shows when nothing is mapped.
const HOME_COLOUR: [f32; 4] = [0.08, 0.10, 0.18, 1.0];

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type Feedback = Option<OutputPresentationFeedback>;
type Element = WaylandSurfaceRenderElement<GlesRenderer>;

pub struct Backend {
    session: LibSeatSession,
    libinput: Libinput,
    render_node: DrmNode,
    renderer: GlesRenderer,
    outputs: DrmOutputManager<Allocator, Exporter, Feedback, DrmDeviceFd>,
    screen: Option<Screen>,
    redraw: Redraw,
}

/// The TV, once its connector has been set up.
struct Screen {
    output: Output,
    drm_output: DrmOutput<Allocator, Exporter, Feedback, DrmDeviceFd>,
    frame_duration: Duration,
    feedback: SurfaceFeedback,
    /// What filled the primary plane last frame, so changes can be logged.
    scanout: Option<Scanout>,
    foreground_presentation: Option<String>,
}

/// The two dmabuf feedbacks a client surface can be told. `scanout` adds a
/// preferred tranche of formats the primary plane can show as is, which is
/// what gets a client to allocate buffers that skip composition entirely.
/// Each surface gets whichever matches how its last frame was shown.
struct SurfaceFeedback {
    render: DmabufFeedback,
    scanout: DmabufFeedback,
}

/// Whether a frame went through the GPU or the client's own buffer went
/// straight to the screen. Direct scanout is the point of having one
/// fullscreen thing: no copy, no added latency, the GPU free for the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scanout {
    Composited,
    Direct,
}

/// Where the next frame stands. Rendering is driven by damage, not a timer:
/// an idle home screen costs nothing until a client commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redraw {
    Idle,
    /// A render is scheduled on the event loop's idle queue.
    Queued,
    /// A frame is on its way to the screen. Rendering again before its vblank
    /// would fail, so new damage only marks it `dirty`.
    AwaitingVblank {
        dirty: bool,
    },
}

/// Event sources [`Backend::open`] creates, inserted once the state exists.
pub struct Sources {
    session: smithay::backend::session::libseat::LibSeatSessionNotifier,
    drm: smithay::backend::drm::DrmDeviceNotifier,
    libinput: LibinputInputBackend,
}

impl Backend {
    /// Take the seat and the configured card, and get a renderer on it.
    pub fn open(config: &Config) -> anyhow::Result<(Self, Sources)> {
        let (mut session, session_notifier) = LibSeatSession::new()
            .context("taking the seat (is this running inside a logind session?)")?;

        // libseat wants the real node; the config names a stable by-path link.
        let path = config
            .device
            .canonicalize()
            .with_context(|| format!("resolving {}", config.device.display()))?;
        let fd = session
            .open(
                &path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .with_context(|| format!("opening {}", path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));

        let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true).context("initializing DRM")?;
        let gbm = GbmDevice::new(fd.clone()).context("initializing GBM")?;

        let render_node = DrmNode::from_file(&fd)
            .ok()
            .and_then(|node| node.node_with_type(NodeType::Render)?.ok())
            .context("finding the card's render node")?;

        // SAFETY: the GBM device outlives the display, as both live in `self`.
        let egl = unsafe { EGLDisplay::new(gbm.clone()) }.context("initializing EGL")?;
        let context = EGLContext::new_with_priority(&egl, ContextPriority::High)
            .context("creating the EGL context")?;
        // SAFETY: the context is current only on this thread.
        let renderer =
            unsafe { GlesRenderer::new(context) }.context("creating the GLES renderer")?;
        let render_formats = renderer.egl_context().dmabuf_render_formats().clone();

        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(render_node).into());
        let outputs = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            COLOR_FORMATS.iter().copied(),
            render_formats,
        );

        let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
            session.clone().into(),
        );
        libinput
            .udev_assign_seat(&session.seat())
            .map_err(|()| anyhow::anyhow!("assigning libinput to {}", session.seat()))?;
        let libinput_backend = LibinputInputBackend::new(libinput.clone());

        Ok((
            Self {
                session,
                libinput,
                render_node,
                renderer,
                outputs,
                screen: None,
                redraw: Redraw::Idle,
            },
            Sources {
                session: session_notifier,
                drm: drm_notifier,
                libinput: libinput_backend,
            },
        ))
    }

    pub fn seat_name(&self) -> String {
        self.session.seat()
    }

    /// Check a client's dmabuf against the renderer and claim it for this card.
    ///
    /// The claim (`set_node`) is what direct scanout hangs on: the framebuffer
    /// exporter only turns buffers from our render node into scanout
    /// framebuffers, and an unclaimed buffer is refused without a word, so
    /// the client gets composited forever.
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        let imported = self.renderer.import_dmabuf(dmabuf, None).is_ok();
        if imported {
            dmabuf.set_node(self.render_node);
        }
        imported
    }

    pub fn output(&self) -> Option<&Output> {
        self.screen.as_ref().map(|s| &s.output)
    }

    /// What a fullscreen client should size itself to.
    pub fn output_size(&self) -> Option<Size<i32, Logical>> {
        let mode = self.output()?.current_mode()?;
        Some(mode.size.to_logical(1))
    }

    /// Open an input node through the session, so the seat decides who gets
    /// it and revokes it on a VT switch.
    pub fn open_input(&mut self, node: &Path) -> anyhow::Result<OwnedFd> {
        self.session
            .open(
                node,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .with_context(|| format!("opening {}", node.display()))
    }

    pub fn close_input(&mut self, fd: OwnedFd) {
        if let Err(err) = self.session.close(fd) {
            tracing::warn!(?err, "closing an input node");
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(?err, vt, "switching VT");
        }
    }

    pub fn request_redraw(&mut self, loop_handle: &LoopHandle<'static, Emrakul>) {
        tracing::trace!(redraw = ?self.redraw, "redraw requested");
        match self.redraw {
            Redraw::Idle => {
                self.redraw = Redraw::Queued;
                loop_handle.insert_idle(|state| state.render());
            }
            Redraw::Queued | Redraw::AwaitingVblank { dirty: true } => {}
            Redraw::AwaitingVblank { dirty: false } => {
                self.redraw = Redraw::AwaitingVblank { dirty: true }
            }
        }
    }
}

/// Bring up the TV and wire the backend into the event loop.
pub fn start(state: &mut Emrakul, sources: Sources) -> anyhow::Result<()> {
    let dh = state.display_handle.clone();

    state
        .shm_state
        .update_formats(state.backend.renderer.shm_formats());
    let feedback = DmabufFeedbackBuilder::new(
        state.backend.render_node.dev_id(),
        state.backend.renderer.dmabuf_formats(),
    )
    .build()
    .context("building dmabuf feedback")?;
    state.dmabuf_global = Some(
        state
            .dmabuf_state
            .create_global_with_default_feedback::<Emrakul>(&dh, &feedback),
    );

    let screen = connect_screen(state)?;
    state.space.map_output(&screen.output, (0, 0));
    state.backend.screen = Some(screen);

    let handle = state.loop_handle.clone();
    insert(
        &handle,
        sources.drm,
        |event, metadata, state: &mut Emrakul| match event {
            DrmEvent::VBlank(_crtc) => state.on_vblank(metadata),
            DrmEvent::Error(err) => tracing::error!(?err, "DRM error"),
        },
    )?;
    insert(
        &handle,
        sources.libinput,
        |event, _, state: &mut Emrakul| state.on_input(event),
    )?;
    insert(&handle, sources.session, |event, _, state: &mut Emrakul| {
        state.on_session(event)
    })?;

    state.backend.request_redraw(&state.loop_handle);
    Ok(())
}

fn insert<S, F>(
    handle: &LoopHandle<'static, Emrakul>,
    source: S,
    callback: F,
) -> anyhow::Result<RegistrationToken>
where
    S: smithay::reexports::calloop::EventSource + 'static,
    F: FnMut(S::Event, &mut S::Metadata, &mut Emrakul) -> S::Ret + 'static,
{
    handle
        .insert_source(source, callback)
        .map_err(|e| anyhow::anyhow!("inserting an event source: {}", e.error))
}

fn connect_screen(state: &mut Emrakul) -> anyhow::Result<Screen> {
    let backend = &mut state.backend;
    let wanted = &state.config.connector;

    let mut scanner: DrmScanner = DrmScanner::new();
    let scan = scanner
        .scan_connectors(backend.outputs.device())
        .context("scanning connectors")?;
    let mut seen = Vec::new();
    let mut found = None;
    for event in scan {
        if let DrmScanEvent::Connected { connector, crtc } = event {
            let name = connector_name(&connector);
            if &name == wanted {
                found = Some((connector, crtc));
            } else {
                seen.push(name);
            }
        }
    }
    let Some((connector, crtc)) = found else {
        bail!(
            "connector {wanted} is not connected (connected: {})",
            seen.join(", ")
        );
    };
    let crtc: crtc::Handle = crtc.with_context(|| format!("no free CRTC can drive {wanted}"))?;

    let drm_mode = pick_mode(connector.modes(), state.config.mode).with_context(|| match state
        .config
        .mode
    {
        Some(mode) => format!("{wanted} offers no {mode} mode"),
        None => format!("{wanted} offers no modes"),
    })?;
    tracing::info!(connector = wanted, mode = ?drm_mode, "setting up the screen");

    let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        wanted.clone(),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "emrakul".into(),
            model: wanted.clone(),
            serial_number: String::new(),
        },
    );
    let wl_mode = WlMode::from(drm_mode);
    output.create_global::<Emrakul>(&state.display_handle);
    output.set_preferred(wl_mode);
    output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));

    let device = backend.outputs.device();
    let mut planes = device.planes(&crtc).context("querying the CRTC's planes")?;
    // Smithay's own reference compositor refuses overlay planes on NVIDIA:
    // using one there breaks the output. The primary plane still scans out a
    // fullscreen client directly, which is the case that matters here.
    if driver_is_nvidia(device) {
        planes.overlay.clear();
    }

    let drm_output = backend
        .outputs
        .lock()
        .initialize_output::<_, Element>(
            crtc,
            drm_mode,
            &[connector.handle()],
            &output,
            Some(planes),
            &mut backend.renderer,
            &DrmOutputRenderElements::default(),
        )
        .map_err(|e| anyhow::anyhow!("initializing {wanted}: {e}"))?;

    let feedback = surface_feedback(backend.render_node, &backend.renderer, &drm_output)?;
    let frame_duration = Duration::from_secs_f64(1_000.0 / wl_mode.refresh as f64);
    Ok(Screen {
        output,
        feedback,
        drm_output,
        frame_duration,
        scanout: None,
        foreground_presentation: None,
    })
}

fn connector_name(connector: &connector::Info) -> String {
    format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    )
}

fn driver_is_nvidia(device: &DrmDevice) -> bool {
    device.get_driver().is_ok_and(|driver| {
        driver
            .name()
            .to_string_lossy()
            .to_lowercase()
            .contains("nvidia")
            || driver
                .description()
                .to_string_lossy()
                .to_lowercase()
                .contains("nvidia")
    })
}

/// The configured mode, or the display's preferred one.
///
/// Several modes answer to one nominal rate (60 and 59.94 both round to 60),
/// so this takes the one whose real refresh is closest to the request.
fn pick_mode(
    modes: &[smithay::reexports::drm::control::Mode],
    wanted: Option<Mode>,
) -> Option<smithay::reexports::drm::control::Mode> {
    let Some(wanted) = wanted else {
        return modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(modes.first())
            .copied();
    };
    let target_mhz = i64::from(wanted.refresh) * 1000;
    modes
        .iter()
        .filter(|m| m.size() == (wanted.width, wanted.height))
        .filter(|m| m.vrefresh() == wanted.refresh)
        .min_by_key(|m| (refresh_mhz(m) - target_mhz).abs())
        .copied()
}

fn refresh_mhz(mode: &smithay::reexports::drm::control::Mode) -> i64 {
    let (htotal, vtotal) = (i64::from(mode.hsync().2), i64::from(mode.vsync().2));
    if htotal == 0 || vtotal == 0 {
        return 0;
    }
    i64::from(mode.clock()) * 1_000_000 / (htotal * vtotal)
}

impl Emrakul {
    pub fn render(&mut self) {
        let Emrakul {
            backend,
            space,
            clock,
            ..
        } = self;
        let Some(screen) = backend.screen.as_mut() else {
            backend.redraw = Redraw::Idle;
            return;
        };
        let output = &screen.output;
        let scale = Scale::from(1.0);

        // ScanoutCandidate is what lets the DRM compositor put a client's buffer
        // straight on the primary plane. Window::render_elements marks surfaces
        // Unspecified, which rules that out before it is even tried.
        let elements: Vec<Element> = space
            .elements()
            .filter_map(|window| Some((window, window.toplevel()?.wl_surface().clone())))
            .flat_map(|(window, surface)| {
                let location =
                    space.element_location(window).unwrap_or_default() - window.geometry().loc;
                // Front to back, so popups come first to land above their
                // toplevel. Same placement as Smithay's own Window rendering.
                let popups: Vec<Element> = PopupManager::popups_for_surface(&surface)
                    .flat_map(|(popup, offset)| {
                        let location =
                            location + window.geometry().loc + offset - popup.geometry().loc;
                        render_elements_from_surface_tree(
                            &mut backend.renderer,
                            popup.wl_surface(),
                            location.to_physical_precise_round::<_, i32>(scale),
                            scale,
                            1.0,
                            Kind::Unspecified,
                        )
                    })
                    .collect();
                let toplevel: Vec<Element> = render_elements_from_surface_tree(
                    &mut backend.renderer,
                    &surface,
                    location.to_physical_precise_round::<_, i32>(scale),
                    scale,
                    1.0,
                    Kind::ScanoutCandidate,
                );
                popups.into_iter().chain(toplevel)
            })
            .collect();

        backend.redraw = match screen.drm_output.render_frame(
            &mut backend.renderer,
            &elements,
            HOME_COLOUR,
            FrameFlags::DEFAULT,
        ) {
            Ok(frame) if !frame.is_empty => {
                let scanout = match frame.primary_element {
                    PrimaryPlaneElement::Swapchain(_) => Scanout::Composited,
                    PrimaryPlaneElement::Element(_) => Scanout::Direct,
                };
                if screen.scanout != Some(scanout) {
                    tracing::info!(?scanout, "primary plane changed");
                    screen.scanout = Some(scanout);
                }
                // Why the foreground surface was or wasn't scanned out, each
                // time that answer changes. The first thing to read when a
                // fullscreen client is unexpectedly being composited.
                let foreground = space.elements().last().and_then(|w| w.toplevel()).map(|t| {
                    let id = Id::from_wayland_resource(t.wl_surface());
                    let state = frame.states.element_render_state(id);
                    format!("{:?}", state.map(|s| s.presentation_state))
                });
                if screen.foreground_presentation != foreground {
                    tracing::debug!(presentation = ?foreground, "foreground surface");
                    screen.foreground_presentation = foreground;
                }
                let mut feedback = OutputPresentationFeedback::new(output);
                for window in space.elements() {
                    window.take_presentation_feedback(
                        &mut feedback,
                        |_, _| Some(output.clone()),
                        |surface, _| {
                            surface_presentation_feedback_flags_from_states(
                                surface,
                                None,
                                &frame.states,
                            )
                        },
                    );
                }
                let SurfaceFeedback {
                    render: render_feedback,
                    scanout: scanout_feedback,
                } = &screen.feedback;
                for window in space.elements() {
                    window.send_dmabuf_feedback(
                        output,
                        |_, _| Some(output.clone()),
                        |surface, _| {
                            select_dmabuf_feedback(
                                surface,
                                &frame.states,
                                render_feedback,
                                scanout_feedback,
                            )
                        },
                    );
                }
                match screen.drm_output.queue_frame(Some(feedback)) {
                    Ok(()) => {
                        tracing::trace!(?scanout, "frame queued");
                        Redraw::AwaitingVblank { dirty: false }
                    }
                    Err(err) => {
                        tracing::warn!(?err, "queueing a frame");
                        Redraw::Idle
                    }
                }
            }
            Ok(_) => {
                tracing::trace!("frame had no damage");
                Redraw::Idle
            }
            Err(err) => {
                tracing::warn!(?err, "rendering a frame");
                Redraw::Idle
            }
        };

        let now = clock.now();
        for window in space.elements() {
            window.send_frame(output, now, Some(Duration::from_secs(1)), |_, _| {
                Some(output.clone())
            });
        }
    }

    fn on_vblank(&mut self, metadata: &mut Option<DrmEventMetadata>) {
        tracing::trace!(redraw = ?self.backend.redraw, "vblank");
        let Some(screen) = self.backend.screen.as_mut() else {
            return;
        };
        match screen.drm_output.frame_submitted() {
            Ok(Some(Some(mut feedback))) => {
                let (time, kind) = match metadata.as_ref().map(|m| m.time) {
                    Some(DrmEventTime::Monotonic(tp)) if !tp.is_zero() => (
                        tp.into(),
                        wp_presentation_feedback::Kind::Vsync
                            | wp_presentation_feedback::Kind::HwClock
                            | wp_presentation_feedback::Kind::HwCompletion,
                    ),
                    _ => (self.clock.now(), wp_presentation_feedback::Kind::Vsync),
                };
                let sequence = metadata.as_ref().map_or(0, |m| m.sequence);
                feedback.presented(
                    time,
                    Refresh::fixed(screen.frame_duration),
                    u64::from(sequence),
                    kind,
                );
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "finishing a frame"),
        }

        let dirty = matches!(self.backend.redraw, Redraw::AwaitingVblank { dirty: true });
        self.backend.redraw = Redraw::Idle;
        if dirty {
            self.backend.request_redraw(&self.loop_handle);
        }
    }

    fn on_session(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                self.backend.libinput.suspend();
                self.close_gamepads();
                self.backend.outputs.pause();
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                if self.backend.libinput.resume().is_err() {
                    tracing::error!("libinput failed to resume");
                }
                self.scan_gamepads();
                if let Err(err) = self.backend.outputs.lock().activate(false) {
                    tracing::error!(?err, "reactivating DRM");
                }
                // Whatever was in flight died with the pause.
                self.backend.redraw = Redraw::Idle;
                self.backend.request_redraw(&self.loop_handle);
            }
        }
    }
}

fn surface_feedback(
    render_node: DrmNode,
    renderer: &GlesRenderer,
    drm_output: &DrmOutput<Allocator, Exporter, Feedback, DrmDeviceFd>,
) -> anyhow::Result<SurfaceFeedback> {
    let render_formats = renderer.dmabuf_formats();
    let (scanout_device, scanout_formats) = drm_output.with_compositor(|compositor| {
        let surface = compositor.surface();
        let formats = surface
            .plane_info()
            .formats
            .intersection(&render_formats)
            .copied()
            .collect::<FormatSet>();
        (surface.device_fd().dev_id(), formats)
    });
    let builder = DmabufFeedbackBuilder::new(render_node.dev_id(), render_formats);
    Ok(SurfaceFeedback {
        render: builder
            .clone()
            .build()
            .context("building render feedback")?,
        scanout: builder
            .add_preference_tranche(
                scanout_device.context("reading the card's device id")?,
                Some(TrancheFlags::Scanout),
                scanout_formats,
            )
            .build()
            .context("building scanout feedback")?,
    })
}
