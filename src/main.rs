mod apps;
mod config;
mod drm;
mod gamepad;
mod idle;
mod input;
mod lifecycle;
mod recency;
mod state;

use std::path::PathBuf;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use crate::{config::Config, state::Emrakul};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "emrakul=info,smithay=warn".into()),
        )
        .init();

    let config = Config::load(&config_path()?)?;

    let mut event_loop: EventLoop<'static, Emrakul> = EventLoop::try_new()?;
    let display: Display<Emrakul> = Display::new()?;
    let (backend, sources) = drm::Backend::open(&config)?;
    let mut state = Emrakul::new(
        config,
        display,
        event_loop.handle(),
        event_loop.get_signal(),
        backend,
    )?;
    drm::start(&mut state, sources)?;
    state.watch_gamepads()?;
    state.arm_idle_timer();
    tracing::info!(socket = ?state.socket_name, "listening");

    if let Some(argv) = &state.config.launch {
        state.run_detached(argv);
    }
    state.enter_home();

    event_loop.run(None, &mut state, |state| {
        state.space.refresh();
        state.popups.cleanup();
        if let Err(err) = state.display_handle.flush_clients() {
            tracing::warn!(?err, "flushing clients");
        }
    })?;
    Ok(())
}

fn config_path() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("--config"), Some(path)) => Ok(path.into()),
        _ => anyhow::bail!("usage: emrakul --config <path/to/config.toml>"),
    }
}
