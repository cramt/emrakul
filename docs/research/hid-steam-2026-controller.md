# The 2026 Steam Controller through `hid-steam`

Question ([#2](https://github.com/cramt/emrakul/issues/2)): what does the 2026 Steam Controller look like to userspace through Linux 7.3's `hid-steam`? Which input devices, which event codes, how lizard mode works, and when the driver unregisters the device.

Everything here comes from reading `drivers/hid/hid-steam.c` at **v7.3-rc4**, which is byte-identical to torvalds `master` as of 2026-10-02. Line numbers refer to that file:
<https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/hid/hid-steam.c?h=v7.3-rc4>

The driver calls the 2026 controller **Ibex** (`STEAM_QUIRK_IBEX`). `Documentation/hid/` at v7.3-rc4 does not mention Steam or Valve hardware, so the driver source is the only primary doc.

## TL;DR for emrakul

- With the default `lizard_mode=1`, the gamepad evdev node **exists but stays silent**, Steam button included, until the user holds **Menu** for 0.45 s to toggle into "gamepad mode". Set `hid_steam.lizard_mode=0` and events flow from the moment the controller connects.
- Gamepad and motion sensors are **two separate evdev nodes**. The trackpads are **not** separate nodes. They are plain `ABS_HAT0*` / `ABS_HAT1*` axes on the gamepad node, with no `ABS_MT_*`.
- Steam button is `BTN_MODE` (0x13c). The "..." quick-access button is `BTN_BASE` (0x126).
- If anything opens the controller's hidraw node, the driver **unregisters the gamepad node, the sensors node and the battery**. They come back with new `eventN` numbers when the last hidraw opener closes.
- Ganymede runs `linux_zen` **7.2.7**, which has no Ibex support at all: no 0x1304 in `hid-ids.h`, no `IBEX` in `hid-steam.c` at v7.2 or v7.2.7. None of this applies until ganymede runs 7.3 or later.
- Rumble (`FF_RUMBLE`) exists only with `CONFIG_STEAM_FF=y`, which defaults to off. The zen kernel on luna has `# CONFIG_STEAM_FF is not set`.

## Device IDs and variants

`drivers/hid/hid-ids.h` (v7.3-rc4) and the id table at line 2736:

| PID | Name in driver | `driver_data` quirks |
|---|---|---|
| 0x1302 | Steam Controller (2026) wired | `IBEX` |
| 0x1303 | Steam Controller (2026) BLE (`HID_BLUETOOTH_DEVICE`) | `IBEX \| BLE` |
| 0x1304 | Steam Controller (2026) Puck (`..._PROTEUS`) | `IBEX \| WIRELESS` |
| 0x1305 | Steam Machine internal receiver (`..._NEREID`) | `IBEX \| WIRELESS` |

The puck shares the `WIRELESS` quirk with the 2015 dongle. That flag drives the evdev name, the connect-status query at probe, and the sensor open/close behaviour.

## Which HID interfaces get a `steam_device`

`steam_is_valve_interface()` (line 1589) documents the layout:

- Wired 2026: one unified interface.
- Puck: 7 USB interfaces. 0–1 are internal comms (not HID). **2–5 are four controller slots.** 6 is a "basic pogo pin interface". The pogo interface is recognised by its first collection usage `0xFF000002`, and it is ignored by being started with plain `HID_CONNECT_DEFAULT`, the same treatment as non-Valve interfaces.
- BLE: a single HID interface, always treated as the controller.

Each puck slot interface (2–5) is probed separately and gets its own `steam_device` and its own virtual hidraw node, whether or not a controller is paired into that slot. Evdev nodes appear only when a controller connects to a slot.

### Puck connect and disconnect

- At probe, `WIRELESS` makes the driver ask the dongle for the connection state over feature report 2 (`REPORT_ID_FEATURES_DONGLE`, command `ID_DONGLE_GET_WIRELESS_STATE`). It registers immediately if the state is `CONNECT`.
- At runtime, input report `0x79` (`REPORT_ID_WIRELESS_EVENT`) with payload 1 or 2 means disconnect or connect. It schedules `work_connect`, which registers or unregisters the evdev nodes and battery (`steam_raw_event`, line 2530).
- Any input or battery report that arrives while no input device exists is also treated as a connect event.

So a controller going to sleep and waking up makes its evdev nodes disappear and reappear. Their `eventN` numbers can change.

## Registered devices

`steam_register()` (line 1372) creates, in order:

1. A **power_supply**, for `WIRELESS | IBEX`, so every 2026 variant gets one. It is named `steam-<serial>`, with properties present, status, scope, voltage_now, current_now, capacity and temp (line 1045). It is fed from input report `0x43`.
2. The **gamepad evdev node** (`steam_input_register`, line 1099).
3. The **motion sensors evdev node** (`steam_sensors_register`, line 1227).

### Gamepad node

- `name`: `"Wireless Steam Controller"` if `WIRELESS` (puck, Nereid), else `"Steam Controller"` (wired, BLE).
- `uniq` = controller serial. `phys` is the HID device's phys. `vendor` is 0x28de and `product` is the HID PID, so the puck's controllers show `product=0x1304`.
- `EV_KEY`, `EV_ABS`, and `EV_FF` (only with `CONFIG_STEAM_FF`).

Buttons. The bit positions come from `steam_ibex_button_mappings` (line 2393), which reads input reports 0x42 and 0x45. Labels are the driver's own comments.

| Physical | Code | Value |
|---|---|---|
| A / B / X / Y | `BTN_A` / `BTN_B` / `BTN_X` / `BTN_Y` | 0x130 / 0x131 / 0x133 / 0x134 |
| **Steam logo** | **`BTN_MODE`** | **0x13c** |
| **Quick access ("...")** | **`BTN_BASE`** | **0x126** |
| Menu (right, ≡) | `BTN_START` | 0x13b |
| View (left) | `BTN_SELECT` | 0x13a |
| Left / right shoulder | `BTN_TL` / `BTN_TR` | 0x136 / 0x137 |
| Left / right trigger fully pressed | `BTN_TL2` / `BTN_TR2` | 0x138 / 0x139 |
| Left / right stick click | `BTN_THUMBL` / `BTN_THUMBR` | 0x13d / 0x13e |
| Left / right trackpad **pressed** (click) | `BTN_THUMB` / `BTN_THUMB2` | 0x121 / 0x122 |
| D-pad up/down/left/right | `BTN_DPAD_UP/DOWN/LEFT/RIGHT` | 0x220–0x223 |
| Left / right top grip | `BTN_GRIPL` / `BTN_GRIPR` | 0x224 / 0x225 |
| Left / right bottom grip | `BTN_GRIPL2` / `BTN_GRIPR2` | 0x226 / 0x227 |

The report also carries bits that the driver decodes but does **not** forward: left and right pad touched, left and right stick touched, left and right grip touch, and left and right pad pressure (u16).

Axes (`steam_input_register` IBEX branch, and `steam_ibex_axis_mappings`):

| Axis | Meaning | Range | Res (units/mm) |
|---|---|---|---|
| `ABS_X` / `ABS_Y` | left stick (Y inverted to evdev convention) | ±32767 | 6553 |
| `ABS_RX` / `ABS_RY` | right stick (Y inverted) | ±32767 | 6553 |
| `ABS_HAT2Y` / `ABS_HAT2X` | left / right trigger analog, uncalibrated | 0–32767 | 5461 |
| `ABS_HAT0X` / `ABS_HAT0Y` | **left trackpad** position | ±32767, fuzz 256 | 1638 |
| `ABS_HAT1X` / `ABS_HAT1Y` | **right trackpad** position | ±32767, fuzz 256 | 1638 |

### Trackpads

The trackpads are single-contact absolute axes on the gamepad node. The driver has **no `ABS_MT_*`, no `BTN_TOUCH`, no `INPUT_PROP_POINTER`** and no separate touchpad device. In `steam_do_ibex_input_event` (line 2441), a pad's X/Y is reported when its "touched" bit is set and forced to `0,0` when the bit is clear. Touch-down and lift therefore have to be inferred: (0,0) means not touched, which collides with a finger resting dead-centre. A click is `BTN_THUMB` / `BTN_THUMB2`. Pressure is in the report but not exposed.

### Motion sensors node

- `name`: `"Steam Controller Motion Sensors"`, with `INPUT_PROP_ACCELEROMETER`, `EV_MSC/MSC_TIMESTAMP` and the same `uniq` as the gamepad.
- Accelerometer: `ABS_X/Y/Z`, ±32768, 16384 units/g, fuzz 128. Gyro: `ABS_RX/RY/RZ`, ±32768, 16 units/°/s.
- `MSC_TIMESTAMP` comes from the controller's own u32 timestamp at report offset 30.
- Open and close (line 1227): on **puck and BLE**, opening the node writes `SETTING_IMU_MODE = raw accel | raw gyro` and closing writes 0, so the IMU only streams while something holds the node open. On **wired**, there is no open/close hook.

## Lizard mode

`static bool lizard_mode = true;` (line 55), exposed as `module_param_cb(lizard_mode, …, 0644)` (line 2732): *"Enable mouse and keyboard emulation (lizard mode) when the gamepad is not in use"*. It can be changed at runtime through `/sys/module/hid_steam/parameters/lizard_mode`. A write reapplies the mode to every registered controller that has no hidraw client.

### What it emulates

Lizard mode is controller firmware behaviour. The driver only toggles it (`steam_set_lizard_mode`, line 880):

- Enable: `ID_SET_DEFAULT_DIGITAL_MAPPINGS` ("enable esc, enter, cursors") plus `ID_LOAD_DEFAULT_SETTINGS`. The header comment (lines 10–16) describes the emulation: the right pad works as a mouse, the shoulders are mouse buttons, A/B are Enter/Escape, "and so on". That comment dates from the 2015 controller, and the driver does not say which keys the 2026 firmware maps.
- Disable on Ibex/Deck: `ID_CLEAR_DIGITAL_MAPPINGS`, plus `SETTING_LIZARD_MODE = 0` and `SETTING_STEAM_WATCHDOG_ENABLE = 0`.

The emulated keyboard and mouse events reach userspace through hid-core's generic `hid-input`, not through hid-steam's evdev nodes. The Valve interface is started with `HID_CONNECT_DEFAULT & ~HID_CONNECT_HIDRAW`, which keeps `HIDINPUT`, and the puck has no separate keyboard or mouse interfaces. So any lizard keyboard and mouse nodes must come from collections in the slot interface's own report descriptor. *That last step is an inference from the code. I could not dump a real report descriptor because I had no access to ganymede.* Those nodes belong to hid-core, so the hidraw unregister described below does **not** remove them.

### How it gates the gamepad: the important part

In `steam_do_ibex_input_event` and `steam_do_ibex_sensors_event`:

```c
if (!steam->gamepad_mode && lizard_mode)
        return;
```

With the default `lizard_mode=1`, the gamepad and sensor nodes get **no events at all**, not even `BTN_MODE`, until `gamepad_mode` is switched on. Unlike the 2015 controller, opening the evdev node does **not** turn lizard mode off on Ibex. `steam_input_open` (line 909) skips the Deck and Ibex quirks on purpose.

`gamepad_mode` toggles when **Menu (`BTN_START`) is held for 450 ms** (`schedule_delayed_work(&steam->mode_switch, 45 * HZ / 100)`) while no hidraw client is open. `steam_mode_switch_cb` (line 1487) then:

- Entering gamepad mode: disables lizard mode, and the gamepad node starts reporting.
- Leaving gamepad mode: re-enables lizard mode, sends release/zero for every button and stick/pad axis plus the gyro axes, and freezes the accelerometer.
- In both directions: plays a trackpad haptic pulse as feedback.
- Returns immediately if `lizard_mode` is 0, so the toggle does nothing in that configuration.

`gamepad_mode` lives in `steam_device`, which exists for the life of the HID interface. On the puck that means per slot. It survives controller disconnect and reconnect and hidraw open and close, and it resets only on re-probe, for example when the puck is replugged or the module reloads.

With **`lizard_mode=0`**, `steam_register` calls `steam_set_lizard_mode(steam, false)` on every (re)registration. The firmware emulation and the Steam watchdog are off, and both nodes report from the moment the controller connects. **That is the setting emrakul wants**: `boot.extraModprobeConfig = "options hid_steam lizard_mode=0";`. The kernel cmdline form is `hid_steam.lizard_mode=0`.

## hidraw: when the driver unregisters

Since commit [cd33a91d37eb](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/commit/?id=cd33a91d37eb4d7c6ce56aa7f4688066309808eb) "HID: steam: Fully unregister controller when hidraw is opened" (merged for 7.3):

> previously detached the evdev nodes when the hidraw is opened. However, this isn't sufficient to avoid FEATURE reports from conflicting, so we change to fully unregistering the controller internally, leaving only the hidraw active until it's closed.

The mechanism:

1. **There is no hidraw on the real interface.** `steam_probe` (line 1735) starts the Valve interface with `HID_CONNECT_DEFAULT & ~HID_CONNECT_HIDRAW`. It then creates a virtual "client" `hid_device` (`steam_create_client_hid`, line 1706) with the same bus, VID, PID, name and phys, in group `HID_GROUP_STEAM`, and connects **only hidraw** on it. Steam, SDL and other hidraw users see a normal-looking `/dev/hidrawN` for each slot.
2. **Open.** The first `open()` of that hidraw reaches `steam_client_ll_open` (line 1650): `client_opened++` and `schedule_work(work_connect)`. In `steam_work_connect_cb` (line 1461), `connected && !opened` is now false, so `steam_unregister()` removes the **battery, the sensors node and the gamepad node**, cancels rumble and mode-switch work, and drops the device from the lizard-param list.
3. **While open.** `steam_raw_event` forwards every raw report to the client hidraw. Input reports are not parsed. Feature and output requests from the hidraw user pass through to the hardware. The driver sends no commands of its own, and the Menu-hold mode switch is disabled.
4. **Close.** When the last opener closes, `steam_client_ll_close` (line 1664) runs `client_opened--` and reschedules `work_connect`. If the controller is still connected, `steam_register()` runs again: it re-reads the serial and attributes, **reapplies `lizard_mode`** (overwriting whatever the hidraw client configured), and creates **new** battery, gamepad and sensors devices with new `eventN` numbers.

The trigger is any `open()` of the virtual hidraw node, including a short probe-and-close. Each puck slot has its own hidraw node and its own `client_opened` count, so opening one slot's hidraw only affects the controller in that slot.

### Wired vs BLE vs puck

| | Wired 0x1302 | BLE 0x1303 | Puck 0x1304 |
|---|---|---|---|
| evdev name | Steam Controller | Steam Controller | Wireless Steam Controller |
| Sensors node | yes, IMU always on | yes, IMU on only while node open | yes, IMU on only while node open |
| Battery power_supply | yes | yes | yes (per connected controller) |
| Nodes appear | at probe | at probe | on wireless connect (0x79 or first input report) |
| hidraw unregister | yes | yes | yes, per slot |
| Lizard gating, Menu-hold toggle | same | same | same |
| Rumble (`CONFIG_STEAM_FF`) | yes | yes | yes |

## Haptics and rumble

- **Rumble** is compiled only with `CONFIG_STEAM_FF` (Kconfig: "Steam Deck force feedback support", `bool`, default n). When enabled, the gamepad node gets `EV_FF` / `FF_RUMBLE` through `input_ff_create_memless` with `steam_play_effect` (line 860).
- `strong_magnitude` drives the left motor and `weak_magnitude` the right.
- On Ibex the driver sends **output report 0x80** (`REPORT_ID_HAPTIC_RUMBLE`) with `hid_hw_output_report`, using intensity 0, left gain 2 and right gain 0 (`steam_haptic_rumble`, line 793).
- Updates are coalesced. The first update is sent immediately, and while either magnitude is non-zero the current state is resent every 50 ms (`steam_coalesce_rumble_cb`, line 844).
- Trackpad **haptic pulses** (output report 0x81) are used only internally, as feedback for the Menu-hold mode toggle. Userspace has no API for them. Reports 0x82–0x85 (command, LFO tone, log sweep, script) are defined but unused.
- On NixOS: luna's zen kernel reports `# CONFIG_STEAM_FF is not set`, and nixpkgs `common-config.nix` does not set it. Rumble from evdev therefore needs `boot.kernelPatches` / `structuredExtraConfig` with `STEAM_FF = yes`.

## What this means for "emrakul reads the puck through evdev while nothing opens hidraw"

The plan holds, with three conditions:

1. **Kernel ≥ 7.3** on ganymede. 7.2.7-zen has no Ibex support, so the puck would bind to hid-generic and expose only vendor-page HID.
2. **`lizard_mode=0`**. Otherwise the gamepad node is silent until someone holds Menu, and the firmware keyboard and mouse emulation is live.
3. **Nothing opens the virtual hidraw.** The obvious offenders are the Steam client and SDL's HIDAPI Steam drivers. Moonlight uses SDL for gamepads, so whether SDL's HIDAPI driver for this controller is on by default, and whether the hidraw node is user-accessible (udev `uaccess` rules such as `steam-devices`), needs its own check. That check is outside this ticket. If anything does open it, emrakul loses the Steam button until that process closes the node. Then the nodes come back under new numbers, so emrakul has to handle hotplug through udev/libinput rather than holding on to `eventN`.

Also: rely on `uniq` (the serial) or `phys` to tie the gamepad and sensors nodes together, not on node numbers.

## Sources

- `drivers/hid/hid-steam.c` @ v7.3-rc4: <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/hid/hid-steam.c?h=v7.3-rc4>
- `drivers/hid/hid-ids.h` @ v7.3-rc4 (Valve PIDs 0x1302–0x1305)
- `drivers/hid/Kconfig` @ v7.3-rc4 (`HID_STEAM`, `STEAM_FF`)
- `include/uapi/linux/input-event-codes.h` @ v7.3-rc4 (numeric codes)
- Commit cd33a91d37eb "HID: steam: Fully unregister controller when hidraw is opened"
- Commit 0a80b4e8ec6a "HID: steam: Initial 2026 Steam Controller support"
- Commit cfe8ee25fe12 "HID: steam: Zero out inputs when disabling gamepad mode"
- Series cover letter, v2 0/6 "HID: steam: Add 2026 Steam Controller support", Vicki Pfau, 2026-08-05. Read through the lore mirror at <https://lore-kernel.gnuweeb.org/linux-input/20260806022653.93939-1-vi@endrift.com/>, because lore.kernel.org sits behind a bot wall.
- v7.2 / v7.2.7 `hid-steam.c` and `hid-ids.h`: no Ibex or 0x1304 (negative check)
- `Documentation/hid/*.rst` @ v7.3-rc4: no Steam or Valve content (negative check)
