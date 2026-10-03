//! emrakul's xdg-desktop-portal backend: `org.freedesktop.impl.portal.RemoteDesktop`,
//! version 2, so a program asking the portal for remote input (KDE Connect,
//! turning a phone into a touchpad and keyboard) gets a connection to
//! emrakul's EIS socket.
//!
//! Every request is granted without asking. The TV has one user and no
//! way to answer a dialog from the couch, and the only program asking is
//! one the user already paired a phone with.

#[path = "../eis_socket.rs"]
mod eis_socket;

use std::{
    collections::HashMap,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};

use zbus::{
    fdo, interface,
    object_server::{ObjectServer, SignalEmitter},
    zvariant::{self, ObjectPath, OwnedObjectPath, OwnedValue},
};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.emrakul";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// The portal's device types bitmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceTypes(u32);

impl DeviceTypes {
    const KEYBOARD: u32 = 1;
    const POINTER: u32 = 2;
    /// No touch screen: emrakul's EIS seat doesn't offer one.
    const AVAILABLE: Self = Self(Self::KEYBOARD | Self::POINTER);

    /// What a request for `requested` gets: no more than there is, and
    /// everything there is when it doesn't say.
    fn granted(requested: Option<u32>) -> Self {
        Self(requested.unwrap_or(Self::AVAILABLE.0) & Self::AVAILABLE.0)
    }
}

/// Where a session is in CreateSession, SelectDevices, Start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Created,
    Selected(DeviceTypes),
    Started(DeviceTypes),
}

/// A call made out of order.
#[derive(Debug, PartialEq, Eq)]
struct OutOfOrder(&'static str);

impl Session {
    fn select(self, requested: Option<u32>) -> Result<Self, OutOfOrder> {
        match self {
            Session::Created | Session::Selected(_) => {
                Ok(Session::Selected(DeviceTypes::granted(requested)))
            }
            Session::Started(_) => Err(OutOfOrder("devices are selected before Start")),
        }
    }

    /// The spec lets a session start without selecting devices, which
    /// selects all of them.
    fn start(self) -> Result<(Self, DeviceTypes), OutOfOrder> {
        let devices = match self {
            Session::Created => DeviceTypes::AVAILABLE,
            Session::Selected(devices) => devices,
            Session::Started(_) => return Err(OutOfOrder("the session has already started")),
        };
        Ok((Session::Started(devices), devices))
    }

    fn connect(self) -> Result<(), OutOfOrder> {
        match self {
            Session::Started(_) => Ok(()),
            Session::Created | Session::Selected(_) => {
                Err(OutOfOrder("ConnectToEIS comes after Start"))
            }
        }
    }
}

type Sessions = Arc<Mutex<HashMap<OwnedObjectPath, Session>>>;
type Results = HashMap<String, OwnedValue>;

/// The portal's response codes.
const SUCCESS: u32 = 0;
const OTHER: u32 = 2;

struct RemoteDesktop {
    sessions: Sessions,
}

impl RemoteDesktop {
    /// Moves the session at `path` on, or answers OTHER: the frontend
    /// turns that into a failed request for the client.
    fn advance<T>(
        &self,
        path: &ObjectPath<'_>,
        step: impl FnOnce(Session) -> Result<(Session, T), OutOfOrder>,
    ) -> Result<T, u32> {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(&OwnedObjectPath::from(path.clone())) else {
            tracing::warn!(%path, "no such session");
            return Err(OTHER);
        };
        match step(*session) {
            Ok((next, value)) => {
                *session = next;
                Ok(value)
            }
            Err(OutOfOrder(why)) => {
                tracing::warn!(%path, why, "refused");
                Err(OTHER)
            }
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl RemoteDesktop {
    async fn create_session(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        let path = OwnedObjectPath::from(session_handle);
        let session = SessionObject {
            path: path.clone(),
            sessions: self.sessions.clone(),
        };
        if let Err(err) = server.at(&path, session).await {
            tracing::warn!(%err, %path, "exporting the session");
            return (OTHER, Results::new());
        }
        tracing::info!(app_id, %path, "session created");
        self.sessions.lock().unwrap().insert(path, Session::Created);
        (SUCCESS, Results::new())
    }

    async fn select_devices(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        _app_id: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let requested = options
            .get("types")
            .and_then(|v| v.downcast_ref::<u32>().ok());
        match self.advance(&session_handle, |s| Ok((s.select(requested)?, ()))) {
            Ok(()) => (SUCCESS, Results::new()),
            Err(code) => (code, Results::new()),
        }
    }

    async fn start(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        match self.advance(&session_handle, Session::start) {
            Ok(devices) => {
                tracing::info!(app_id, devices = devices.0, "granted remote input");
                let results = Results::from([
                    ("devices".to_owned(), OwnedValue::from(devices.0)),
                    ("clipboard_enabled".to_owned(), OwnedValue::from(false)),
                ]);
                (SUCCESS, results)
            }
            Err(code) => (code, Results::new()),
        }
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<zvariant::OwnedFd> {
        self.advance(&session_handle, |s| Ok((s, s.connect()?)))
            .map_err(|_| fdo::Error::Failed("the session hasn't started".into()))?;
        let path = eis_socket::path()
            .ok_or_else(|| fdo::Error::Failed("XDG_RUNTIME_DIR is unset".into()))?;
        let stream = UnixStream::connect(&path).map_err(|err| {
            fdo::Error::Failed(format!("connecting to {}: {err}", path.display()))
        })?;
        tracing::info!(app_id, "connected to emrakul's EIS");
        Ok(std::os::fd::OwnedFd::from(stream).into())
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        DeviceTypes::AVAILABLE.0
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

/// `org.freedesktop.impl.portal.Session`, at each session's path.
struct SessionObject {
    path: OwnedObjectPath,
    sessions: Sessions,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionObject {
    async fn close(&self, #[zbus(object_server)] server: &ObjectServer) {
        self.sessions.lock().unwrap().remove(&self.path);
        let _ = server.remove::<Self, _>(&self.path).await;
        tracing::info!(path = %self.path, "session closed");
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "emrakul_portal=info".into()),
        )
        .init();
    let portal = RemoteDesktop {
        sessions: Sessions::default(),
    };
    let _connection = zbus::connection::Builder::session()?
        .serve_at(PATH, portal)?
        .name(BUS_NAME)?
        .build()
        .await?;
    tracing::info!(BUS_NAME, "serving");
    std::future::pending::<()>().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn introspection(interface: &impl zbus::object_server::Interface) -> String {
        let mut xml = String::new();
        interface.introspect_to_writer(&mut xml, 0);
        xml
    }

    /// The frontend calls these by the spec's names, which aren't what
    /// zbus would make of the Rust ones (`ConnectToEis`, `Version`).
    #[test]
    fn the_interfaces_use_the_specs_names() {
        let portal = introspection(&RemoteDesktop {
            sessions: Sessions::default(),
        });
        for name in [
            r#"method name="CreateSession""#,
            r#"method name="SelectDevices""#,
            r#"method name="Start""#,
            r#"method name="ConnectToEIS""#,
            r#"property name="AvailableDeviceTypes""#,
            r#"property name="version""#,
        ] {
            assert!(portal.contains(name), "{name} missing from {portal}");
        }
        let session = introspection(&SessionObject {
            path: OwnedObjectPath::try_from("/s").unwrap(),
            sessions: Sessions::default(),
        });
        for name in [
            r#"method name="Close""#,
            r#"signal name="Closed""#,
            r#"property name="version""#,
        ] {
            assert!(session.contains(name), "{name} missing from {session}");
        }
    }

    #[test]
    fn a_request_for_a_touch_screen_too_gets_keyboard_and_pointer() {
        // kdeconnect asks for 7: keyboard, pointer and touch screen.
        assert_eq!(DeviceTypes::granted(Some(7)), DeviceTypes(3));
        assert_eq!(DeviceTypes::granted(Some(1)), DeviceTypes(1));
    }

    #[test]
    fn a_session_starts_with_what_was_selected() {
        let selected = Session::Created.select(Some(7)).unwrap();
        assert_eq!(
            selected.start(),
            Ok((Session::Started(DeviceTypes(3)), DeviceTypes(3)))
        );
    }

    #[test]
    fn a_session_started_without_selecting_gets_everything() {
        assert_eq!(Session::Created.start().unwrap().1, DeviceTypes::AVAILABLE);
    }

    #[test]
    fn eis_is_only_handed_out_once_started() {
        assert!(Session::Created.connect().is_err());
        assert!(Session::Selected(DeviceTypes(3)).connect().is_err());
        assert!(Session::Started(DeviceTypes(3)).connect().is_ok());
    }

    #[test]
    fn a_started_session_cannot_start_or_reselect() {
        let started = Session::Started(DeviceTypes(3));
        assert!(started.start().is_err());
        assert!(started.select(Some(1)).is_err());
    }
}
