# Steam Controller (2026) through hid-steam on Linux 7.3

Captured on ganymede, 2026-10-02: kernel `7.3.0-rc4`, NVIDIA 580.178.04 (loads fine on 7.3-rc), puck `28de:1304` on USB, one controller paired. Raw `/proc/bus/input/devices` snapshots and `evtest` logs are in [steam-controller-7.3-captures.tar.xz](steam-controller-7.3-captures.tar.xz).

## Devices

| Node name | What it is | When it exists |
| --- | --- | --- |
| `Valve Software Steam Controller Puck` ×4 | hid-core's generic keyboard+mouse on the puck's interfaces 2–5 (one per receiver slot): lizard mode's emulation | always, with or without a controller connected, in both lizard modes |
| `Wireless Steam Controller` | hid-steam's gamepad | while a controller is connected |
| `Steam Controller Motion Sensors` | hid-steam's IMU | while a controller is connected; streams only while opened |

The gamepad and motion nodes come and go with the wireless link (the controller sleeps after a while; dmesg logs `connected`/`disconnected`). Read them by hotplug.

Opening the puck's hidraw node unregisters the gamepad: Steam (still autostarted in ganymede's Plasma session) did exactly that, and hid-steam re-registered the gamepad the moment Steam exited.

## Lizard mode

- **`lizard_mode=1` (default):** until Menu is held, only the emulation node talks. Right trackpad → `REL_X/REL_Y` mouse, its click → `BTN_LEFT`, A → `KEY_ENTER`, B → `KEY_ESC`. The left stick sends nothing; the gamepad is silent. Holding Menu sends `KEY_ESC` with autorepeat for ~0.45 s, then hid-steam flips to gamepad mode: `BTN_START` appears on the gamepad and the emulation node falls silent.
- **`lizard_mode=0`:** the emulation nodes stay but send nothing; everything arrives on the gamepad. Settable at runtime through `/sys/module/hid_steam/parameters/lizard_mode`.

## Gamepad map (`lizard_mode=0`)

| Control | evdev |
| --- | --- |
| A / B / X / Y | `BTN_SOUTH` / `BTN_EAST` / `BTN_NORTH` / `BTN_WEST` |
| D-pad | `BTN_DPAD_UP/DOWN/LEFT/RIGHT` (buttons, not a hat) |
| Bumpers | `BTN_TL` / `BTN_TR` |
| Triggers, full pull | `BTN_TL2` / `BTN_TR2` |
| Triggers, analog | left `ABS_HAT2Y`, right `ABS_HAT2X`, 0…32767 |
| Left / right stick | `ABS_X/ABS_Y` / `ABS_RX/ABS_RY`, −32767…32767; clicks `BTN_THUMBL` / `BTN_THUMBR` |
| Left / right trackpad position | `ABS_HAT0X/Y` / `ABS_HAT1X/Y`, −32767…32767 |
| Left / right trackpad click | `BTN_THUMB` / `BTN_THUMB2` |
| View / Menu | `BTN_SELECT` / `BTN_START` |
| Steam | `BTN_MODE` |
| Quick access (…) | `BTN_BASE` |
| Back grips | codes 548–551 (`BTN_GRIPL`, `BTN_GRIPR`, `BTN_GRIPL2`, `BTN_GRIPR2`; evtest 1.36 doesn't name them). All four fire; which physical grip is which is unconfirmed, since the presses arrived as 548, 549, 550, 551 and the exact press order wasn't controlled. |
| Motion node | accel `ABS_X/Y/Z`, gyro `ABS_RX/RY/RZ`, `MSC_TIMESTAMP` |

Trackpad touch has no key event of its own: a touch shows up as the pad's position axes moving.

**Trackpad y grows upwards, stick y downwards.** hid-steam negates the sticks' y (`ABS_Y`, `ABS_RY`) to evdev's down-is-positive, but reports both pads' y (`ABS_HAT0Y`, `ABS_HAT1Y`) as the controller sends it: up is positive. Confirmed on the couch: with the pad read as down-positive, the pointer moved the wrong way vertically.

**Sticks never go quiet.** Each stick axis reports at ~210 Hz with the controller at rest, wandering by about ±500 of 32767. Anything treating stick motion as activity (idle, Home navigation) needs a deadzone.
