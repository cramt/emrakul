//! Drives emrakul the way KDE Connect's remote input does, for checking
//! remote input without a phone: the RemoteDesktop portal (CreateSession,
//! SelectDevices, Start, ConnectToEIS), then a libei sender that moves the
//! pointer, clicks, types and scrolls.
//!
//! Text is typed the way kdeconnect types it: each character's keysym is
//! looked up on any level of the keymap emrakul hands out, and its keycode
//! sent alone, with no Shift.
//!
//! ```sh
//! cargo run --example remote-input -- --move 200,100 --click --text 'Hi there!' --scroll 120
//! ```
//!
//! `--to x,y` puts the pointer there, in client pixels, over the absolute
//! pointer. `--direct` skips the portal and connects to `$XDG_RUNTIME_DIR/emrakul-eis`.

use std::{
    collections::HashMap,
    io,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use futures_util::StreamExt;
use reis::{PendingRequestResult, ei};
use smithay::input::keyboard::xkb;
use zbus::zvariant::{OwnedValue, Value};

#[derive(Default)]
struct Plan {
    direct: bool,
    moves: Vec<(f32, f32)>,
    to: Option<(f32, f32)>,
    click: bool,
    text: Option<String>,
    scroll: Option<f32>,
}

fn plan() -> anyhow::Result<Plan> {
    let mut plan = Plan::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--direct" => plan.direct = true,
            "--click" => plan.click = true,
            "--move" => {
                let by = args.next().context("--move dx,dy")?;
                let (x, y) = by.split_once(',').context("--move dx,dy")?;
                plan.moves.push((x.parse()?, y.parse()?));
            }
            "--to" => {
                let at = args.next().context("--to x,y")?;
                let (x, y) = at.split_once(',').context("--to x,y")?;
                plan.to = Some((x.parse()?, y.parse()?));
            }
            "--text" => plan.text = Some(args.next().context("--text TEXT")?),
            "--scroll" => plan.scroll = Some(args.next().context("--scroll DY")?.parse()?),
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    Ok(plan)
}

/// One portal request: calls `method` and waits for its Response.
async fn request(
    connection: &zbus::Connection,
    portal: &zbus::Proxy<'_>,
    token: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> anyhow::Result<HashMap<String, OwnedValue>> {
    let sender = connection.unique_name().context("no unique name")?;
    let sender = sender.trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = zbus::Proxy::new(
        connection,
        "org.freedesktop.portal.Desktop",
        path,
        "org.freedesktop.portal.Request",
    )
    .await?;
    let mut responses = request.receive_signal("Response").await?;
    portal.call_method(method, body).await?;
    let response = responses.next().await.context("no Response")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = response.body().deserialize()?;
    anyhow::ensure!(code == 0, "{method} answered {code}");
    Ok(results)
}

async fn portal_eis() -> anyhow::Result<OwnedFd> {
    let connection = zbus::Connection::session().await?;
    let portal = zbus::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.RemoteDesktop",
    )
    .await?;
    let options = |pairs: &[(&'static str, Value<'static>)]| {
        pairs.iter().cloned().collect::<HashMap<&str, Value>>()
    };
    let created = request(
        &connection,
        &portal,
        "t1",
        "CreateSession",
        &(options(&[
            ("handle_token", "t1".into()),
            ("session_handle_token", "remote_input_example".into()),
        ]),),
    )
    .await?;
    let session: String = created
        .get("session_handle")
        .context("no session_handle")?
        .try_clone()?
        .try_into()?;
    let session = zbus::zvariant::ObjectPath::try_from(session)?;
    // kdeconnect asks for everything, touch screen included.
    request(
        &connection,
        &portal,
        "t2",
        "SelectDevices",
        &(
            &session,
            options(&[("handle_token", "t2".into()), ("types", 7u32.into())]),
        ),
    )
    .await?;
    let started = request(
        &connection,
        &portal,
        "t3",
        "Start",
        &(&session, "", options(&[("handle_token", "t3".into())])),
    )
    .await?;
    eprintln!("started: {started:?}");
    let reply = portal
        .call_method("ConnectToEIS", &(&session, options(&[])))
        .await?;
    let fd: zbus::zvariant::OwnedFd = reply.body().deserialize()?;
    // The session lives as long as this connection does.
    std::mem::forget(connection);
    Ok(fd.into())
}

#[derive(Default)]
struct Device {
    interfaces: HashMap<String, reis::Object>,
    done: bool,
    resumed: bool,
}

struct Client {
    context: ei::Context,
    serial: u32,
    /// The capabilities to bind on each seat, collected until its Done.
    bind_mask: HashMap<ei::Seat, u64>,
    devices: HashMap<ei::Device, Device>,
    keymap: Option<xkb::Keymap>,
}

impl Client {
    fn dispatch(&mut self) -> anyhow::Result<()> {
        // Ok(0) is nothing to read yet; emrakul hanging up is an error.
        match self.context.read() {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err.into()),
        }
        while let Some(result) = self.context.pending_event() {
            let PendingRequestResult::Request(event) = result else {
                continue;
            };
            match event {
                ei::Event::Handshake(handshake, ei::handshake::Event::HandshakeVersion { .. }) => {
                    handshake.handshake_version(1);
                    handshake.name("remote-input-example");
                    handshake.context_type(ei::handshake::ContextType::Sender);
                    for interface in [
                        "ei_callback",
                        "ei_connection",
                        "ei_seat",
                        "ei_device",
                        "ei_pingpong",
                        "ei_keyboard",
                        "ei_pointer",
                        "ei_pointer_absolute",
                        "ei_button",
                        "ei_scroll",
                    ] {
                        handshake.interface_version(interface, 1);
                    }
                    handshake.finish();
                }
                ei::Event::Handshake(_, ei::handshake::Event::Connection { serial, .. }) => {
                    self.serial = serial;
                }
                ei::Event::Connection(_, ei::connection::Event::Ping { ping }) => ping.done(0),
                ei::Event::Connection(
                    _,
                    ei::connection::Event::Disconnected { explanation, .. },
                ) => {
                    anyhow::bail!("disconnected: {explanation:?}")
                }
                ei::Event::Seat(seat, ei::seat::Event::Capability { mask, interface }) => {
                    if [
                        "ei_keyboard",
                        "ei_pointer",
                        "ei_pointer_absolute",
                        "ei_button",
                        "ei_scroll",
                    ]
                    .contains(&interface.as_str())
                    {
                        let bits = self.bind_mask.entry(seat).or_default();
                        *bits |= mask;
                    }
                }
                ei::Event::Seat(seat, ei::seat::Event::Done) => {
                    seat.bind(self.bind_mask.get(&seat).copied().unwrap_or(0));
                }
                ei::Event::Seat(_, ei::seat::Event::Device { device }) => {
                    self.devices.insert(device, Device::default());
                }
                ei::Event::Device(device, event) => {
                    let data = self.devices.entry(device).or_default();
                    match event {
                        ei::device::Event::Interface { object } => {
                            data.interfaces
                                .insert(object.interface().to_owned(), object);
                        }
                        ei::device::Event::Done => data.done = true,
                        ei::device::Event::Resumed { serial } => {
                            data.resumed = true;
                            self.serial = serial;
                        }
                        _ => {}
                    }
                }
                ei::Event::Keyboard(_, ei::keyboard::Event::Keymap { size, keymap, .. }) => {
                    let context = xkb::Context::new(0);
                    // SAFETY: emrakul sends a sealed memfd of `size` bytes.
                    self.keymap = unsafe {
                        xkb::Keymap::new_from_fd(
                            &context,
                            keymap,
                            size as usize,
                            xkb::KEYMAP_FORMAT_TEXT_V1,
                            0,
                        )
                    }?;
                }
                _ => {}
            }
        }
        let _ = self.context.flush();
        Ok(())
    }

    fn interface<T: reis::Interface>(&self) -> Option<(ei::Device, T)> {
        self.devices.iter().find_map(|(device, data)| {
            let object = data.interfaces.get(T::NAME)?.clone().downcast()?;
            Some((device.clone(), object))
        })
    }

    fn frame(&mut self, device: &ei::Device) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        device.frame(self.serial, now);
        let _ = self.context.flush();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// kdeconnect's `Xkb::keycodeFromKeysym`: the first keycode with the
/// keysym on any level, as an evdev code.
fn kdeconnect_keycode(keymap: &xkb::Keymap, c: char) -> Option<u32> {
    let sym = xkb::utf32_to_keysym(c as u32);
    (keymap.min_keycode().raw()..keymap.max_keycode().raw()).find_map(|raw| {
        let code = xkb::Keycode::new(raw);
        (0..keymap.num_levels_for_key(code, 0))
            .any(|level| keymap.key_get_syms_by_level(code, 0, level).contains(&sym))
            .then_some(raw - 8)
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let plan = plan()?;
    let stream = if plan.direct {
        let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR")?);
        UnixStream::connect(dir.join("emrakul-eis"))?
    } else {
        UnixStream::from(portal_eis().await?)
    };
    let context = ei::Context::new(stream)?;
    let _ = context.flush();
    let mut client = Client {
        context,
        serial: 0,
        bind_mask: HashMap::new(),
        devices: HashMap::new(),
        keymap: None,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        client.dispatch()?;
        let ready = client.devices.len() >= 3
            && client.devices.values().all(|d| d.done && d.resumed)
            && client.keymap.is_some();
        if ready {
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "no devices from emrakul");
        std::thread::sleep(Duration::from_millis(10));
    }
    for data in client.devices.values() {
        eprintln!("device: {:?}", data.interfaces.keys().collect::<Vec<_>>());
    }
    for (sequence, device) in client
        .devices
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .iter()
        .enumerate()
    {
        device.start_emulating(client.serial, sequence as u32);
    }

    if let Some((device, pointer)) = client.interface::<ei::Pointer>() {
        for (dx, dy) in &plan.moves {
            // In ten steps, like a finger across a touchpad.
            for _ in 0..10 {
                pointer.motion_relative(dx / 10.0, dy / 10.0);
                client.frame(&device);
            }
        }
    }
    if let Some((x, y)) = plan.to
        && let Some((device, pointer)) = client.interface::<ei::PointerAbsolute>()
    {
        pointer.motion_absolute(x, y);
        client.frame(&device);
    }
    if plan.click
        && let Some((device, button)) = client.interface::<ei::Button>()
    {
        const BTN_LEFT: u32 = 0x110;
        button.button(BTN_LEFT, ei::button::ButtonState::Press);
        client.frame(&device);
        button.button(BTN_LEFT, ei::button::ButtonState::Released);
        client.frame(&device);
    }
    if let Some(text) = &plan.text
        && let Some((device, keyboard)) = client.interface::<ei::Keyboard>()
    {
        let keymap = client.keymap.clone().context("no keymap")?;
        for c in text.chars() {
            let Some(code) = kdeconnect_keycode(&keymap, c) else {
                eprintln!("no key for {c:?}");
                continue;
            };
            keyboard.key(code, ei::keyboard::KeyState::Press);
            client.frame(&device);
            keyboard.key(code, ei::keyboard::KeyState::Released);
            client.frame(&device);
        }
    }
    if let Some(dy) = plan.scroll
        && let Some((device, scroll)) = client.interface::<ei::Scroll>()
    {
        scroll.scroll(0.0, dy);
        client.frame(&device);
    }
    for device in client.devices.keys() {
        device.stop_emulating(client.serial);
    }
    let _ = client.context.flush();
    std::thread::sleep(Duration::from_millis(200));
    eprintln!("done");
    Ok(())
}
