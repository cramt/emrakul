mod config;
mod drm;
mod input;
mod state;

use std::{path::PathBuf, process::Command};

use anyhow::Context;
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
    tracing::info!(socket = ?state.socket_name, "listening");

    launch(&state);

    event_loop.run(None, &mut state, |state| {
        state.space.refresh();
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

fn launch(state: &Emrakul) {
    let Some(launch) = &state.config.launch else {
        return;
    };
    let spawned = Command::new(&launch.program)
        .args(&launch.args)
        .env("WAYLAND_DISPLAY", &state.socket_name)
        .env("XDG_SESSION_TYPE", "wayland")
        .env_remove("DISPLAY")
        .spawn()
        .with_context(|| format!("starting {}", launch.program));
    match spawned {
        // Reaped on its own thread so an exited client never lingers as a
        // zombie while the compositor runs.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(err) => tracing::error!("{err:#}"),
    }
}
