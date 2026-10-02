# Moonlight and Sunshine in a declarative, controller-only setup

Research for [#5](https://github.com/cramt/emrakul/issues/5). It answers how a Game (see
`CONTEXT.md`) is started and ended from emrakul on ganymede, how pairing can be done without a
keyboard, what of the Steam Controller reaches the gaming desktop, and how saturn's Sunshine
apps, box art and wake-up can be declared in nix.

## Versions read

Everything below is read from source, pinned to what the fleet actually builds:

| Component | Version | Source read |
|---|---|---|
| moonlight-qt | 6.1.0 (nixpkgs) | tag [`v6.1.0`](https://github.com/moonlight-stream/moonlight-qt/tree/v6.1.0); master at [`8369d1a`](https://github.com/moonlight-stream/moonlight-qt/tree/8369d1a0e11b999d4d1598f62ca5f6dea49602fb) where they differ |
| SDL | moonlight-qt links `sdl2-compat` 2.32.72, i.e. **SDL 3.4.16** underneath | tag [`release-3.4.16`](https://github.com/libsdl-org/SDL/tree/release-3.4.16) |
| Sunshine | 2026.914.233613 (nixpkgs) | tag [`v2026.914.233613`](https://github.com/LizardByte/Sunshine/tree/v2026.914.233613) |
| NixOS module | nixpkgs `26ef669c` (nixconf's lock) | [`nixos/modules/services/networking/sunshine.nix`](https://github.com/NixOS/nixpkgs/blob/26ef669cffa904b6f6832ab57b77892a37c1a671/nixos/modules/services/networking/sunshine.nix) |
| Linux `hid-steam` | ganymede and saturn run 7.2.7 | [`drivers/hid/hid-steam.c`](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-steam.c) on master |

`nix eval nixpkgs#moonlight-qt.buildInputs` lists `sdl2-compat-2.32.72-dev`, so every SDL hint
below is an SDL3 hint, read by SDL3 from the environment.

## Summary

- **Stream one app:** `moonlight stream saturn "<App name>"`. **Quit it:** `moonlight quit saturn`.
- **Exit codes are not a success signal.** Every GUI path ends in `Qt.quit()`, which is exit 0,
  including failures. Failures first show a modal dialog and wait for someone to dismiss it.
  Only argument errors exit 1.
- **Pairing can be headless** (`moonlight pair saturn --pin 1234` plus Sunshine's `/api/pin`), and
  it can also be skipped entirely by seeding both sides' state files with pre-generated certs.
- **`SDL_JOYSTICK_HIDAPI=0` keeps hidraw closed.** It holds, with two conditions. The kernel must
  be new enough to bind the 2026 controller (7.3; the fleet runs 7.2.7), and `hid-steam` must be
  in gamepad mode (`hid_steam.lizard_mode=0`).
- **Gyro and accelerometer reach Sunshine. Trackpads do not.** Sunshine auto-selects DualSense
  emulation because the client reports motion sensors.
- **Apps are declared** with `services.sunshine.applications.apps`, a free-form list rendered
  straight into `apps.json`. `image-path` takes an absolute PNG path, so a nix store path works.
- **Wake:** Moonlight sends Wake-on-LAN by itself on `stream`/`quit`/`pair`, but only after it has
  learned saturn's MAC, and it waits 30 s at most. Saturn needs `wakeOnLan` on its NIC and
  should suspend rather than power off.

## 1. Starting and ending a stream from the CLI

### Commands

The CLI has four actions: `list`, `quit`, `stream`, `pair`
([`commandlineparser.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/cli/commandlineparser.cpp)).

```sh
moonlight stream <host> "<app>" [options]   # host = name, UUID or IP; app matched case-insensitively by name
moonlight quit <host>                        # quits whatever app is running on the host
moonlight list <host> [--csv] [--verbose]    # headless, no window
moonlight pair <host> [--pin NNNN]
```

Options that matter for a TV:

- `--display-mode fullscreen|windowed|borderless`
- `--1080`, `--4K`, `--resolution WxH`, `--fps N`, `--bitrate Kbps`
- `--quit-after` / `--no-quit-after`: quit the host app when the session ends gracefully
- `--hdr`, `--video-codec auto|H.264|HEVC|AV1`, `--video-decoder auto|software|hardware`
- `--audio-config stereo|5.1-surround|7.1-surround`
- `--background-gamepad` / `--no-background-gamepad`
- `--keep-awake`, `--performance-overlay`, `--capture-system-keys never|fullscreen|always`

CLI options override the saved preferences for that run only. `StreamingPreferences::save()` is
only called from `SettingsView.qml`, so the CLI never writes them back.

### What `stream` does

From [`cli/startstream.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/cli/startstream.cpp)
and [`gui/CliStartStreamSegue.qml`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/gui/CliStartStreamSegue.qml):

1. Looks up the host. If it is already known, sends WoL immediately (see section 6). It waits
   up to **30 s** for the host and then **10 s** for the app list.
2. If the host is not paired, it fails with "Computer %1 has not been paired".
3. If no app or the same app is running, it starts the session. If a **different** app is
   running, it opens a Yes/No dialog ("Are you sure you want to quit %1?"). No exits; Yes quits
   the other app and then streams. This dialog needs input. To keep the path non-interactive,
   emrakul can run `moonlight quit saturn` before streaming a different game.
4. The stream window is SDL fullscreen. When the session ends, `StreamSegue.qml` calls
   `Qt.quit()` right away if there is no error text. Otherwise it opens the error dialog and quits
   when the dialog closes.

### How a stream ends, and the exit code for each case

| Cause | Behaviour | Exit code |
|---|---|---|
| Host app exits (game closed on saturn) | Sunshine sends termination reason `0x80030023` ("graceful", [`stream.cpp`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src/stream.cpp)) → `ML_ERROR_GRACEFUL_TERMINATION` → no dialog | 0 |
| `moonlight quit saturn` from another process | Host app is quit → same graceful termination of the running stream | 0 for both processes |
| Gamepad combo Start+Select+L1+R1 | Pushes `SDL_QUIT` ([`gamepad.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/streaming/input/gamepad.cpp)); host app keeps running unless `--quit-after`. Disabled with `NO_GAMEPAD_QUIT=1` | 0 |
| Keyboard quit combo (no keyboard here) | Same `SDL_QUIT` path | 0 |
| Connection lost, no video, host encoder error, etc. | `clConnectionTerminated` sets error text → **modal dialog, process waits** | 0 after dismiss |
| Host unreachable, not paired, app not found | `failed` signal → **modal dialog, process waits** | 0 after dismiss |
| Bad arguments | `showError` prints usage | **1** |
| `--help` / `--version` | | 0 |
| SIGTERM / SIGINT on **6.1.0** | No handler in 6.1.0, so the default action kills it. The stream is not stopped cleanly and the host app keeps running | killed by signal (143 in a shell) |
| SIGTERM on **master** (unreleased) | Graceful: interrupts the session, quits the app, exits; a second signal → `_Exit(1)` ([`main.cpp` @ master](https://github.com/moonlight-stream/moonlight-qt/blob/8369d1a0e11b999d4d1598f62ca5f6dea49602fb/app/main.cpp)) | 0, or 1 on second signal |

Consequences for emrakul:

- **Don't use the exit code to detect failure.** It is 0 in both cases. Failures are logged
  (`console.error(message)` and `SDL_LogError("Connection terminated: %d")`) to stderr, so
  emrakul can scrape stderr if it needs a reason.
- **Error dialogs block until dismissed.** They are gamepad-navigable: 6.1.0 calls
  `SdlGamepadKeyNavigation.enable()` in the CLI segues. The Steam button going Home (emrakul
  killing the process) also clears them.
- **Going Home while streaming** should run `moonlight quit saturn` (clean, frees saturn) and
  then wait for the stream process to exit, with SIGKILL as the fallback. On 6.1.0, SIGTERM
  alone leaves the game running on saturn.

## 2. Pairing without a keyboard, done once

Moonlight's side ([`cli/pair.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/cli/pair.cpp)):
`moonlight pair saturn --pin 1234` uses the given 4-digit PIN; without `--pin` it generates one
and shows it. It fails if already paired. Success and failure both open a dialog that must be
closed, then exit 0.

Sunshine's side ([`confighttp.cpp`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src/confighttp.cpp)):
the PIN is submitted over the authenticated web API. This version needs a pairing id and a CSRF
token:

```sh
# on saturn (or via its LAN address), web UI credentials required
TOKEN=$(curl -sku user:pass https://localhost:47990/api/csrf-token | jq -r .csrf_token)
ID=$(curl -sku user:pass https://localhost:47990/api/pin | jq -r '.pairings[0].id')
curl -sku user:pass -H "X-CSRF-Token: $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"pairing_id\":\"$ID\",\"pin\":\"1234\",\"name\":\"ganymede\"}" \
  https://localhost:47990/api/pin
```

That handles a one-off pair. A more declarative route is to **skip the PIN handshake**, because
pairing is nothing more than each side storing the other's certificate:

**Client state (ganymede)**: all of it is in one QSettings INI file. On Linux that is
`~/.config/Moonlight Game Streaming Project/Moonlight.conf`. The org and app names come from
[`main.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/main.cpp), and
Qt's native format on Linux is INI under `$XDG_CONFIG_HOME`. It holds:

- `certificate`, `key`, `uniqueid`: the client identity. A self-signed RSA-2048 cert,
  CN "NVIDIA GameStream Client", valid 20 years, generated on first run if missing
  ([`identitymanager.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/backend/identitymanager.cpp)).
- `hosts/N/{hostname,uuid,mac,localaddress,localport,manualaddress,…,srvcert,apps}`: known hosts.
  `srvcert` is the pinned server cert, and `mac` is what WoL uses
  ([`nvcomputer.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/backend/nvcomputer.cpp)).
- Streaming preferences.

The box-art cache sits in `~/.cache/Moonlight Game Streaming Project/Moonlight/boxart`
(`QStandardPaths::CacheLocation`, [`path.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/path.cpp)).
If `portable.dat` exists in the working directory, everything moves to the CWD instead.

**Server state (saturn)**: under Sunshine's config dir (`~/.config/sunshine` for the user service):

- `sunshine_state.json`: `root.uniqueid` (the server UUID Moonlight stores as `uuid`) and
  `root.named_devices[] = {name, cert, uuid, enabled}`, one per paired client
  ([`nvhttp.cpp`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src/nvhttp.cpp)).
- `credentials/cacert.pem` and `credentials/cakey.pem`: the server cert/key. Their paths can be
  overridden by the `cert` / `pkey` settings
  ([`config.cpp`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src/config.cpp)).

The declarative pairing plan:

1. Generate a client keypair/cert and a server keypair/cert once and store them in 1Password.
2. On saturn, point `services.sunshine.settings.cert`/`pkey` at the opnix-provided files. Seed
   `sunshine_state.json` with a fixed `uniqueid` and a `named_devices` entry holding the client
   cert.
3. On ganymede, seed `Moonlight.conf` with `certificate`/`key`/`uniqueid` and a `hosts/1` entry
   with saturn's `uuid`, `manualaddress`, `srvcert` and `mac`.

Both files are rewritten at runtime: Moonlight saves hosts and app lists, and Sunshine rewrites
its state on pairing changes. They must therefore be **seeded writable copies** (an activation
script or a tmpfiles `C` rule), not store symlinks. Both contain private keys, so they go
through opnix, never the store. The exact INI encoding of the `hosts` array (QSettings
`size=` plus `N\key=` lines, and how the PEM `QByteArray` is escaped) has not been verified here.
Generate one by pairing once and copy its shape.

## 3. Controller: does `SDL_JOYSTICK_HIDAPI=0` keep hidraw closed?

**Yes, and it is needed: without it SDL opens the 2026 controller's hidraw node by default.**

- SDL 3.4.16 has a dedicated HIDAPI driver for the 2026 controller ("Triton",
  [`SDL_hidapi_steam_triton.c`](https://github.com/libsdl-org/SDL/blob/release-3.4.16/src/joystick/hidapi/SDL_hidapi_steam_triton.c)),
  matching `28de:1302` (USB), `1303` (BLE), and `1304`/`1305` (dongles)
  ([`controller_list.h`](https://github.com/libsdl-org/SDL/blob/release-3.4.16/src/joystick/controller_list.h)).
  Its `IsEnabled()` reads `SDL_JOYSTICK_HIDAPI_STEAM`, which defaults to `SDL_JOYSTICK_HIDAPI`,
  which defaults to **on**. The older Steam driver behaves the same on desktop
  ([`SDL_hidapi_steam.c` L37–43](https://github.com/libsdl-org/SDL/blob/release-3.4.16/src/joystick/hidapi/SDL_hidapi_steam.c#L37-L43)).
  The header doc in `SDL_hints.h` says "(default) 0", but that only holds on Android/iOS/tvOS.
  The source wins.
- SDL opens hidraw only in `HIDAPI_SetupDeviceDriver` → `SDL_hid_open_path`, and only for a
  device matched by an **enabled** driver
  ([`SDL_hidapijoystick.c`](https://github.com/libsdl-org/SDL/blob/release-3.4.16/src/joystick/hidapi/SDL_hidapijoystick.c)).
  Enumeration reads sysfs (`uevent`, `report_descriptor`), not `/dev/hidraw*`
  ([`hidapi/linux/hid.c`](https://github.com/libsdl-org/SDL/blob/release-3.4.16/src/hidapi/linux/hid.c)).
  With the drivers disabled, no hidraw open happens.
- With the HIDAPI driver disabled, `HIDAPI_IsDevicePresent` is false, so the Linux evdev backend
  no longer skips the device (`SDL_JoystickHandledByAnotherDriver`) and picks up the `hid-steam`
  event node.
- moonlight-qt never overrides these hints. It only sets `…PS4_RUMBLE`/`…PS5_RUMBLE`
  ([`input.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/streaming/input/input.cpp)),
  so environment variables win.

Set `SDL_JOYSTICK_HIDAPI_STEAM=0` (the targeted one) and `SDL_JOYSTICK_HIDAPI=0` (belt and
braces) in Moonlight's environment. The same goes for any other SDL program emrakul launches.

When `hid-steam` sees a hidraw client, it stops forwarding input to its evdev devices
(`client_opened`) and tears down its input devices, as described in the driver header comment.
That is why any hidraw open is fatal to emrakul.

### Kernel preconditions

1. **Kernel version.** `hid-steam` support for the 2026 controller ("Ibex") landed in
   [`0a80b4e8ec6a`](https://github.com/torvalds/linux/commit/0a80b4e8ec6a) "HID: steam: Initial
   2026 Steam Controller support". It first appears in **v7.3-rc1**. It is not in v7.2.7, the
   kernel ganymede runs: v7.2.7's `hid-ids.h` has no `IBEX` ids. On 7.2.x the controller gets
   `hid-generic` with no `hid-steam` gamepad at all. ganymede needs ≥ 7.3 (currently rc5).
2. **Gamepad mode.** For Ibex (and Deck), `hid-steam` drops all gamepad and IMU reports while
   `!gamepad_mode && lizard_mode` (`steam_do_ibex_input_event`, `steam_do_ibex_sensors_event`).
   Gamepad mode is toggled by holding Start for 0.45 s, which is not appliance-friendly. Loading
   the module with `lizard_mode=0` makes the gate always open and turns off the kernel-side
   mouse/keyboard emulation. In nix: `boot.extraModprobeConfig = "options hid_steam lizard_mode=0";`.
   emrakul has to read the controller as a gamepad anyway, so it loses nothing.

### What SDL maps from the `hid-steam` evdev device

`hid-steam` registers two input devices with the same `uniq` (the serial): the gamepad
("Steam Controller") and "Steam Controller Motion Sensors" (`INPUT_PROP_ACCELEROMETER`).

SDL's evdev backend auto-generates a mapping (`LINUX_JoystickGetGamepadMapping` in
[`SDL_sysjoystick.c`](https://github.com/libsdl-org/SDL/blob/release-3.4.16/src/joystick/linux/SDL_sysjoystick.c)):

| Kernel code (Ibex) | SDL gamepad element |
|---|---|
| `BTN_A/B/X/Y`, `BTN_TL/TR`, `BTN_SELECT/START`, `BTN_THUMBL/R` | face, shoulders, back/start, stick clicks |
| `BTN_MODE` (Steam button) | guide |
| `BTN_DPAD_*` | d-pad |
| `ABS_X/Y`, `ABS_RX/RY` | left / right stick |
| `ABS_HAT2Y` / `ABS_HAT2X` | left / right trigger (analog) |
| `BTN_GRIPL/GRIPR/GRIPL2/GRIPR2` | paddles 1–4 |
| `ABS_HAT0X/Y`, `ABS_HAT1X/Y` (left / right trackpad position) | **raw joystick axes only, no gamepad element** (they have fuzz and resolution, so SDL treats them as analog axes, not hats) |
| `BTN_THUMB`, `BTN_THUMB2`, `BTN_BASE`, and the digital `BTN_TL2/TR2` | raw buttons only (the analog `ABS_HAT2*` already claim the triggers) |
| Motion Sensors node `ABS_X/Y/Z` (accel), `ABS_RX/RY/RZ` (gyro) | gamepad accel + gyro, paired to the gamepad by matching `uniq` (`GetSensor`) |

### What reaches Sunshine

Moonlight 6.1.0 announces each gamepad with `LiSendControllerArrivalEvent`
([`gamepad.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/streaming/input/gamepad.cpp)):

- Type: `LI_CTYPE_UNKNOWN`. 6.1.0 has no Steam type; master adds `LI_CTYPE_STEAM` for
  `28de:1302–1305`.
- Capabilities: `ANALOG_TRIGGERS`, `RUMBLE` (`hid-steam` sets `FF_RUMBLE` for Ibex), `ACCEL`,
  `GYRO`, `BATTERY_STATE`. **No `TOUCHPAD`**, because the evdev backend exposes no SDL
  touchpads.

Sunshine's auto profile selection
([`virtualhid_input.cpp`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src/platform/virtualhid_input.cpp),
`profile_for_metadata`) works like this. For an unknown type, it picks DualSense (`ds5`) when
`motion_as_ds4` is on (default) and the client reports accel or gyro. Otherwise it picks Xbox
Series. Defaults are in `config.cpp`; options are documented in
[`docs/configuration.md`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/docs/configuration.md).
Sunshine then asks the client for motion events at 100 Hz, and Moonlight forwards
`LI_MOTION_TYPE_ACCEL`/`GYRO`.

So on saturn the game sees a **virtual DualSense with working gyro/accel**, sticks, analog
triggers and rumble. **Trackpads do not survive.** They never become SDL touchpads on the evdev
path. The only SDL path that would expose them is the HIDAPI Triton driver, which needs hidraw.
Paddles reach Moonlight as SDL paddles. Whether they survive on a DS5 profile was not checked.
Set `gamepad = ds5` explicitly if auto-selection should not depend on the motion sensor being
seen.

### The Steam button and emrakul

The Steam button arrives as `BTN_MODE` on the same evdev node that Moonlight reads, and evdev
fans out to every reader unless someone `EVIOCGRAB`s it. So emrakul and Moonlight both see it.
Moonlight forwards it to saturn as Guide, which opens Steam's overlay there, while emrakul goes
Home. Whether emrakul should grab, or Moonlight should ignore the button, is a design question
for the input ticket. This doc only notes that both readers will see it.

## 4. Declaring apps in the NixOS module

[`sunshine.nix`](https://github.com/NixOS/nixpkgs/blob/26ef669cffa904b6f6832ab57b77892a37c1a671/nixos/modules/services/networking/sunshine.nix):

- `services.sunshine.applications = { env = attrsOf str; apps = listOf attrs; }`. The module
  renders it with `pkgs.formats.json` into `apps.json` and sets `settings.file_apps` to that
  store path. Each element of `apps` is free-form, so every key Sunshine reads can be used.
- `services.sunshine.settings` is free-form `key = value` into `sunshine.conf`. Setting either
  option makes the web UI unable to change that part.
- Also: `openFirewall` (TCP base−5, base, +1, +21; UDP +9, +10, +11, +13, +21 around 47989),
  `capSysAdmin` (KMS capture), `autoStart`. It runs as a **systemd user service**
  `wantedBy = graphical-session.target`. Sunshine therefore only runs while someone is logged in
  graphically on saturn, so saturn needs autologin into COSMIC for the stream to work after a
  wake.

App keys Sunshine parses ([`process.cpp`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src/process.cpp)):
`name`, `cmd`, `detached` (list), `prep-cmd` (list of `{do, undo, elevated}`), `image-path`,
`working-dir`, `output`, `elevated`, `auto-detach`, `wait-all`, `exit-timeout`,
`exclude-global-prep-cmd`. App ids are a CRC of name + image SHA-256. Moonlight's CLI selects
apps **by name**, so changing an app's id is harmless.

```nix
services.sunshine.applications.apps = [
  {
    name = "Hades II";
    cmd = "steam steam://rungameid/1145350";
    image-path = "${hades2Cover}";   # any absolute .png path
    auto-detach = true;
  }
];
```

## 5. Box art

How Sunshine resolves `image-path` (`validate_app_image_path` in `process.cpp`):

- The extension must be `.png` (case-insensitive) and the file must have a valid PNG signature.
  Anything else falls back to the default `box.png`.
- A relative path is tried under Sunshine's assets dir first, then as given. An absolute nix
  store path passes.

Moonlight fetches the image over HTTPS from `/appasset?appid=N`. That endpoint needs a paired
client cert, and Moonlight caches the image in its boxart dir. emrakul does not have to go
through Sunshine for Home tiles, though. The same store path can feed both `apps.json` on
saturn and emrakul's app list on ganymede.

Where the PNGs can come from, all fetchable with `fetchurl` at build time:

- **IGDB covers.** Sunshine's own web UI cover search uses this. It looks names up in
  [LizardByte/GameDB](https://github.com/LizardByte/GameDB) (`buckets/<x>.json`, then
  `games/<id>.json`) and downloads
  `https://images.igdb.com/igdb/image/upload/t_cover_big_2x/<slug>.png`, which is already a PNG
  ([`apps.html`](https://github.com/LizardByte/Sunshine/blob/v2026.914.233613/src_assets/common/assets/web/apps.html)).
- **Steam library capsules**: `https://shared.steamstatic.com/store_item_assets/steam/apps/<appid>/library_600x900_2x.jpg`
  (verified 200, `image/jpeg`). These are JPEG, so they need a PNG conversion step, e.g. a
  `runCommand` with imagemagick.
- **SteamGridDB** also works, but needs an API key, so it is less suited to pure fetches.

## 6. Waking saturn

Moonlight's own wake ([`computerseeker.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/backend/computerseeker.cpp),
`NvComputer::wake` in [`nvcomputer.cpp`](https://github.com/moonlight-stream/moonlight-qt/blob/v6.1.0/app/backend/nvcomputer.cpp)):

- `stream`, `quit`, `pair` and `list` all construct a `ComputerSeeker`, which calls `wake()` on a
  **known** host before polling.
- `wake()` needs a stored MAC. Sunshine only reports its real MAC in `serverinfo` to paired
  clients over HTTPS; over HTTP it reports `00:00:00:00:00:00`, which Moonlight ignores
  (`nvhttp.cpp`). Moonlight therefore learns the MAC after the first successful paired contact,
  or from a seeded `Moonlight.conf`.
- It sends the magic packet (6×FF + 16×MAC) as UDP to every known address and every interface
  broadcast address. Ports: 9, 47009, and the GFE ports 47998/47999/48000/48002/48010 offset by
  the host's HTTP port.
- `stream` waits **30 s** for the host to appear; `quit`/`pair` wait 10 s. A cold boot of saturn
  plus COSMIC autologin plus the Sunshine user service will likely take longer than 30 s. In that
  case the first attempt fails with "Failed to connect to saturn" (dialog, exit 0).

Saturn-side requirements:

- NIC WoL: `networking.interfaces.<if>.wakeOnLan.enable = true` (nixpkgs renders
  `WakeOnLan=magic` into a systemd `.link`), plus WoL enabled in firmware.
- Prefer suspend over power-off. Resume is well under 30 s, and the logged-in session (and so the
  Sunshine user service) survives.

For robustness, emrakul can send its own magic packet first (any WoL crate or `wakeonlan`), poll
`http://saturn:47989/serverinfo` until it answers, and only then exec `moonlight stream`. That
keeps the wait visible on Home instead of inside Moonlight's 30 s timeout.

## Open points for other tickets

- ganymede needs kernel ≥ 7.3 for `hid-steam` to bind the 2026 controller at all.
- `hid_steam.lizard_mode=0` should go in ganymede's config.
- Decide whether the Steam button is grabbed by emrakul or forwarded to saturn as Guide.
- The `Moonlight.conf` seed format needs one real pairing to copy from.
