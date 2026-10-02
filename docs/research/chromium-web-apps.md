# Chromium web apps on emrakul (Wayland, NVIDIA)

Research for [#4](https://github.com/cramt/emrakul/issues/4). This doc covers how a
**web app** (Chromium opening one URL fullscreen) behaves on emrakul running on
ganymede (GTX 1050 Ti GP107M on the 580 legacy driver, i5-8300H with UHD 630,
NixOS). It answers each question in the ticket, says which Wayland protocols
emrakul must add, and gives a verdict on 4K YouTube decode.

Versions: nixpkgs ships Chromium **154.0.8037.57**. Chromium source links point at
`main` at commit [`949bb59c771e`](https://github.com/chromium/chromium/tree/949bb59c771e033b5d8786739aee3a3f9388dc92)
unless they say otherwise. In the links below, `CR/` stands for
`https://github.com/chromium/chromium/blob/949bb59c771e033b5d8786739aee3a3f9388dc92/`.
Facts marked **(observed)** were measured on ganymede on 2026-10-02.

## TL;DR

- **Protocols emrakul must add for web apps: none are hard blockers.** Chromium
  starts with what emrakul already has. It requires `wl_compositor`, `wl_shm`,
  `xdg_wm_base`, at least one `wl_output` (v2 or later) that has sent `done`, and
  `wl_subcompositor`. Smithay's `CompositorState` provides the last one.
- **emrakul must fix one thing it already does: configure `xdg_popup`s.** `<select>`
  dropdowns and menus are xdg_popups. emrakul leaves them unconfigured today, so
  they never appear. Chromium doesn't hang, but those dropdowns are unusable.
- **Not needed:** `zwp_idle_inhibit` (no-op without it, and the TV never blanks),
  `text-input-v3` (typing falls back to `wl_keyboard`), and `xdg-activation` (no-op
  without it, which suits a kiosk).
- **Pointer protocols** (cursor-shape, pointer constraints) only matter once
  emrakul has a pointer. Fractional scale is optional at 1x.
- **4K60 YouTube on the 1050 Ti is possible but experimental.** Chromium blocks
  VA-API on `nvidia-drm` unless `VaapiOnNvidiaGPUs` is enabled. nvidia-vaapi-driver
  0.0.18, which ganymede already has, claims Chromium support. Nobody reports it
  working on Pascal or 580, and two open bugs hit YouTube VP9. AV1 must be blocked
  either way. If hardware decode fails, 4K60 VP9 falls to software decode on 4
  cores, which isn't realistic, so the fallback is 1080p.
- **Steam Controller hidraw risk: safe on Chromium 145 and later.** Chromium only
  opens hidraw for allowlisted DualShock 4, Xbox-over-BT and HID-haptic pads. Valve
  `28de` isn't on the list.

## 1. The invocation

```sh
chromium \
  --ozone-platform=wayland \
  --user-data-dir="$XDG_STATE_HOME/emrakul/web/<app>" \
  --kiosk --app=https://www.youtube.com/tv \
  --no-first-run --noerrdialogs --disable-session-crashed-bubble
```

### `--ozone-platform=wayland` is mandatory on M154

- `--ozone-platform-hint` was removed in Cr-Commit-Position #1496532, around M141
  (commit `8f2c7d1c85`). The flag no longer exists in
  `ui/ozone/public/ozone_switches.cc`, so the nixpkgs wrapper's
  `--ozone-platform-hint=auto`, added only when `NIXOS_OZONE_WL` is set
  ([nixpkgs `chromium/default.nix`](https://github.com/NixOS/nixpkgs/blob/master/pkgs/applications/networking/browsers/chromium/default.nix)),
  does nothing.
- In M154, if `--ozone-platform` isn't passed, Chromium picks Wayland only when
  `XDG_SESSION_TYPE=wayland` (`ui/linux/display_server_utils.cc`,
  `SetOzonePlatformForLinuxIfNeeded`).
- Either pass the flag or have emrakul's session export `XDG_SESSION_TYPE=wayland`.
  Main after 2026-09-30 probes the sockets instead, but that isn't in M154.

### `--app` vs `--kiosk`

Every Chromium browser window is one `xdg_toplevel`.

**`--app=URL`**
- Opens a `TYPE_APP` window with no tab strip
  ([`CR/chrome/browser/ui/extensions/application_launch.cc#L608-L616`](https://github.com/chromium/chromium/blob/949bb59c771e033b5d8786739aee3a3f9388dc92/chrome/browser/ui/extensions/application_launch.cc#L608-L616)).
- A `target=_blank` link opens a **second, normal tabbed toplevel**, either an
  existing one or a new one (`CR/chrome/browser/ui/navigator/browser_navigator.cc#L298-L310`).
- `window.open` popups are also new toplevels.
- emrakul shows the newest toplevel and returns to the previous one when it
  closes. That works for these windows, but they come with browser UI.

**`--kiosk`**
- A Linux desktop flag: "not Chrome OS kiosk mode" (`CR/chrome/common/chrome_switches.h#L478-L479`).
- Makes `IsRunningInAppMode()` true (`CR/chrome/browser/app_mode/app_mode_utils.cc#L102-L106`).
- Starts fullscreen (`CR/chrome/browser/ui/startup/startup_browser_creator_impl.cc#L241-L248`).
- Suppresses the "press Esc to exit fullscreen" bubble.
- Refuses to leave fullscreen (`CR/chrome/browser/ui/exclusive_access/fullscreen_controller.cc#L395`, `#L638-L642`).

**`--app` + `--kiosk` together** give one chromeless fullscreen toplevel per web
app.

**Fullscreen on the wire**
- Chromium calls `xdg_toplevel.set_fullscreen(NULL)`, so the compositor picks the
  output (`CR/ui/ozone/platform/wayland/host/xdg_toplevel.cc#L136-L141`).
- A window created fullscreen requests it immediately after role creation
  (`CR/ui/ozone/platform/wayland/host/wayland_toplevel_window.cc#L81-L99`).
- Chromium waits for the first configure before drawing (`CR/ui/ozone/platform/wayland/host/wayland_window.cc#L597-L602`).
- It accepts a fullscreen size it didn't ask for (`wayland_toplevel_window.cc#L543-L551`).
- emrakul already forces fullscreen on every toplevel, so this matches.

### Matching windows by `app_id`

- For `--app` windows, `--class` is ignored. The app_id is
  `<exe>-<host>_<path>-<profile dir basename>`, with illegal characters replaced
  by `_`. For example, `--app=https://example.com/` gives `chrome-example.com__-Default`.
- Sources: `CR/chrome/browser/ui/views/frame/browser_native_widget_aura_linux.cc#L67-L75`,
  `CR/chrome/browser/shell_integration_linux.cc#L370-L388`, and
  `CR/chrome/browser/web_applications/os_integration/web_app_shortcut_linux.cc#L390-L401`.
- `CHROME_WEB_APP_DESKTOP_ID_PREFIX` prepends a prefix, which gives emrakul a stable
  thing to match on.
- Ordinary browser windows use `--class`, or the desktop file's base name if it
  isn't set.

## 2. Profiles: one `--user-data-dir` per web app

- `--user-data-dir` is "where the browser will look for all of its state"
  (`CR/chrome/common/chrome_switches.h#L767-L769`). Cookies persist there, so
  logins survive restarts. `--incognito` and `--guest` throw that state away; don't
  use them.
- **ProcessSingleton** locks the dir with `SingletonLock`, `SingletonSocket` and
  `SingletonCookie` (`CR/chrome/browser/process_singleton_posix.cc#L766-L770`). A
  second launch against the same dir hands its command line to the running
  process and exits (`#L940-L980`).
  - **Shared dir:** every web app is one Chromium process. Its windows are
    separate toplevels, but the `chromium` that emrakul spawned exits immediately,
    so emrakul can't track or kill an app through its PID.
  - **Dir per web app:** each app is its own process, with its own login, and
    killing the PID ends exactly that app. This is the right model for "at most
    one app runs at a time".
- **Cost of per-app dirs:** you log into Google separately for YouTube and any
  other Google web app. Use one dir per site, not per URL.

## 3. `xdg_toplevel.close`

**What close does**
- `xdg_toplevel.close` → `PlatformWindowDelegate::OnCloseRequest` → `Widget::Close()`
  → `BrowserView::OnWindowCloseRequested` (`CR/ui/ozone/platform/wayland/host/xdg_toplevel.cc#L271-L275`,
  `CR/chrome/browser/ui/views/frame/browser_view.cc#L4582-L4610`).
- That **runs `beforeunload` first**, so a page can refuse or delay the close. Then
  it hides the window and closes the tabs.
- After the last window closes, the process exits. Background mode is compiled
  in, but it only keeps the process alive if an extension or app with background
  permission exists (`CR/chrome/browser/background/extensions/background_mode_manager.cc#L825-L828`).

**SIGTERM is the reliable way to go Home**
- It calls `chrome::SessionEnding()`, which flushes state (cookies, prefs), skips
  beforeunload, and exits with code 0 (`CR/chrome/browser/chrome_browser_main_posix.cc#L100-L110`,
  `CR/chrome/browser/lifetime/application_lifetime_desktop.cc#L375-L436`).
- SIGINT and SIGHUP go through `AttemptExit()`, which beforeunload can block.

**Recommendation:** send `xdg_toplevel.close`, and SIGTERM the process if it hasn't
exited after about a second. Or skip close and SIGTERM straight away; logins
survive either way.

## 4. Wayland protocols: required vs opportunistic

From `CR/ui/ozone/platform/wayland/host/wayland_connection.cc`.

### Required

Startup fails or hangs without these.

| Global | Requirement | Source |
|---|---|---|
| `wl_compositor` | Any version, binds up to v4. "No wl_compositor object" makes init fail. | `wayland_connection.cc#L271-L282`, `#L635-L643` |
| `wl_shm` | v1. Init fails without it. | same, `host/wayland_shm.cc#L14` |
| `xdg_wm_base` | Any version, binds up to v6. "No Wayland shell found". | same, `#L651-L661` |
| `wl_output` | v2 or later, and it must send `done`. Init repeats roundtrips with **no timeout** until one output is ready, so without it Chromium hangs. | `#L256-L266`, `#L361-L368`, `host/wayland_output.cc#L244-L250` |
| `wl_subcompositor` | Not checked at init, but bubbles (permission and other prompts) are always subsurfaces. Release builds crash on the null proxy. Smithay's `CompositorState` advertises it. | `host/wayland_bubble.cc#L140-L161`, `host/wayland_surface.cc#L494-L502` |

### Opportunistic

Chromium binds these if they're present. Without them:

| Protocol | Behavior without it | Matters on a TV? |
|---|---|---|
| `wl_seat` | Logs a warning and has no input. | emrakul has it. |
| `wl_data_device_manager` | No clipboard or drag and drop. | emrakul has it. |
| `zwp_linux_dmabuf_v1` (v1–v4) | GPU raster falls back to shm. At bind time Chromium waits up to **500 ms** for default-feedback `done` (`host/wayland_zwp_linux_dmabuf.cc#L92-L99`). The feedback `main_device` also becomes Chromium's render node, which matters for decode (§5). | Yes, emrakul has it. Make sure feedback sends `done`. |
| `zwp_idle_inhibit_manager_v1` | `SetScreenSaverSuspended` returns false and does nothing (`host/wayland_screen.cc#L463-L474`). | **No.** See below. |
| `zwp_text_input_manager_v3` | Logs "text-input-v3 not available." and continues (`wayland_connection.cc#L414-L425`). Key events still flow through `wl_keyboard` into Chromium's own key handling (`host/wayland_event_source.cc#L302-L305`, `host/wayland_input_method_context.cc#L345-L365`). The IME is on by default (`WaylandTextInputV3`, `ui/base/ui_base_features.cc#L129`), and nixpkgs' `--enable-wayland-ime=true` falls back the same way. | **No**, unless emrakul wants an on-screen keyboard. |
| `xdg_activation_v1` | `Activate()` skips the token dance (`host/wayland_toplevel_window.cc#L353-L372`). `window.focus()` can't steal focus. | **No.** Without it is better for a kiosk. Focus comes from `wl_keyboard.enter`, which emrakul must send to the foreground toplevel (`#L557-L565`). |
| `wp_fractional_scale_v1` | Integer `wl_output` scale. | No, at 1x. |
| `wp_cursor_shape_v1`, pointer constraints, relative pointer | Not bound. | Only once emrakul has a pointer. |
| `wp_linux_drm_syncobj_v1` | Bound only on kernel 6.11 or later. Otherwise implicit sync. | Nice to have with NVIDIA, not required. |
| `wp_content_type_v1` | Sets VIDEO on surfaces with video (`host/wayland_surface.cc#L189-L200`). | Could tell the TV it's showing video. Low priority. |
| `wp_single_pixel_buffer_v1` | Only used by overlay delegation, which is **off by default** (`ui/ozone/common/features.cc#L11`). | No. Video is composited into Chromium's main buffer, so there are no video subsurfaces to scan out. |
| primary selection | No middle-click paste. | No. |

### Idle inhibit specifically

- Video does request it. Blink's `VideoWakeLock` asks for `kPreventDisplaySleep`
  (`CR/third_party/blink/renderer/core/html/media/video_wake_lock.cc#L142`). On
  Linux that calls `display::Screen::SuspendScreenSaver()`
  (`CR/services/device/wake_lock/power_save_blocker/power_save_blocker_linux.cc#L320-L330`),
  which reaches `zwp_idle_inhibit` on the keyboard-focused window
  (`host/zwp_idle_inhibit_manager.cc#L54-L77`).
- emrakul doesn't blank, so nothing reads that signal. Add the protocol only if
  emrakul gains a screensaver or DPMS-off timer. At that point it becomes the
  "don't blank during video" signal.

### Popups: the one real gap

UI-to-role mapping (`CR/ui/ozone/platform/wayland/host/wayland_window_factory.cc#L24-L53`):

| Chromium UI | Role |
|---|---|
| Menus, tooltips, HTML `<select>` dropdowns | `xdg_popup` |
| Permission and other bubbles | `wl_subsurface` |
| Popups with no parent | new toplevel |

What happens today:
- emrakul's `new_popup` is a no-op, so popups are never configured. Chromium holds
  their frames until configure (`host/wayland_frame_manager.cc#L231-L237`), so they
  never draw.
- They don't wedge anything. `xdg_popup.grab` is only sent with
  `--use-wayland-explicit-grab` (`host/xdg_popup.cc#L291-L296`).
- Keys still reach the invisible menu: views menus via an Env pre-target handler,
  and `<select>` via its parent. Esc closes it.
- Net effect: a `<select>` opens invisibly and arrow keys change it blind.

Fix: configure popups at their positioner geometry, constrained to the output,
and draw them above the toplevel. Jellyfin's settings pages use `<select>`.

## 5. Hardware video decode

### What the hardware can do (observed)

`vainfo --display drm --device ...` on ganymede:

| Node | Driver | Decode profiles |
|---|---|---|
| `renderD128` (NVIDIA, `pci-0000:01:00.0`) | nvidia-vaapi-driver, "VA-API NVDEC driver [direct backend]", installed via `hardware.nvidia.videoAcceleration` (default true, [nixpkgs `nvidia.nix`](https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/hardware/video/nvidia.nix)) | H.264, HEVC Main/Main10/Main12, **VP9 Profile 0 and 2**, MPEG-2, VC-1, JPEG. **No AV1.** |
| `renderD129` (Intel UHD 630, `pci-0000:00:02.0`) | none installed. `vaInitialize` fails. | With `intel-media-driver` 26.2.4 run ad hoc: H.264, HEVC Main/Main10, VP9 Profile 0/2. **No AV1.** |

The TV (HDMI-A-1) is wired to the NVIDIA card, and emrakul renders and scans out
there.

### Chromium's side

- **Built with VA-API.** nixpkgs only sets `use_vaapi = false` on aarch64
  ([`common.nix`](https://github.com/NixOS/nixpkgs/blob/master/pkgs/applications/networking/browsers/chromium/common.nix)),
  and the wrapper puts `libva` on `LD_LIBRARY_PATH`.
- **NVIDIA is gated off by default.**
  [`media/gpu/vaapi/vaapi_wrapper.cc#L123-L131`](https://github.com/chromium/chromium/blob/main/media/gpu/vaapi/vaapi_wrapper.cc#L123-L131)
  skips any DRM node whose kernel driver is `nvidia-drm` unless `kVaapiOnNvidiaGPUs`
  is enabled. On x86_64 that feature is `FEATURE_DISABLED_BY_DEFAULT`
  ([`media/base/media_switches.cc#L1056`](https://github.com/chromium/chromium/blob/main/media/base/media_switches.cc#L1056)).
  The comment cites crbug.com/1492880, "driver landscape on x64 linux tends to be
  shaky". Chromium's [docs/gpu/vaapi.md](https://github.com/chromium/chromium/blob/main/docs/gpu/vaapi.md)
  still calls NVIDIA unsupported.
- **The render node follows the compositor.** On Wayland, Chromium sets
  `--render-node-override` from linux-dmabuf feedback `main_device`
  (`ui/ozone/platform/wayland/host/wayland_zwp_linux_dmabuf.cc`,
  `WaylandConnection::SetRenderNodePath`). emrakul advertises the NVIDIA node, so
  VA-API targets NVIDIA. With the feature off, that means **no hardware decode at
  all**; Chromium does not fall through to Intel.
  `--hardware-video-device-path=/dev/dri/renderD129` would force Intel, but then
  decoded frames have to cross GPUs into NVIDIA EGL. That path is unverified, and
  likely fails or falls back to a copy.
- **No Vulkan Video path.** Chromium has no Vulkan Video decoder on Linux:
  `media/gpu/` has only vaapi, v4l2, windows, mac and chromeos.

### nvidia-vaapi-driver and Chromium

- The README's Chrome section now documents a working single-buffer export path
  ([README#chrome](https://github.com/elFarto/nvidia-vaapi-driver#chrome)). It
  shipped in [v0.0.18](https://github.com/elFarto/nvidia-vaapi-driver/releases/tag/v0.0.18)
  on 2026-08-31, and ganymede has that version.
- The README's recipe:

  ```sh
  LIBVA_DRIVER_NAME=nvidia chromium --ozone-platform=wayland \
    --enable-features=AcceleratedVideoDecodeLinuxGL,VaapiOnNvidiaGPUs \
    --ignore-gpu-blocklist --use-gl=angle --use-angle=gl
  ```

- It was proven on an RTX 4090, driver 610, KWin, and needed an ANGLE fix
  ([#440](https://github.com/elFarto/nvidia-vaapi-driver/issues/440)).
- Nobody reports Pascal or 580.
- Two open issues hit exactly the YouTube case:
  - [#463](https://github.com/elFarto/nvidia-vaapi-driver/issues/463): YouTube
    plays black video with the README flags.
  - [#462](https://github.com/elFarto/nvidia-vaapi-driver/issues/462): VP9 decode
    in browsers hangs until reboot.

### What YouTube sends

- 4K60 only exists as VP9 or AV1; H.264 tops out at 1080p.
- If Chromium reports AV1 as decodable, it is, in software via dav1d, and YouTube
  will serve AV1. The 1050 Ti can't decode AV1.
- YouTube's playback setting offers Auto, "Prefer AV1 for SD" and "Always prefer
  AV1", with no "never" option. The only sources for that are secondary, for
  example [9to5google](https://9to5google.com/2020/02/05/netflix-android-av1-streaming/).
- To block AV1 only, use [enhanced-h264ify](https://github.com/alextrv/enhanced-h264ify)
  with just the AV1 box ticked. Its defaults also block VP9, which caps playback at
  1080p. It can be force-installed declaratively through
  `programs.chromium.extensions`.

### Verdict for 4K60 YouTube on ganymede

1. **Hardware VP9 4K60 on NVDEC is the only route to 4K60.** It needs
   `VaapiOnNvidiaGPUs` plus the README flags, with AV1 blocked. It's experimental
   on this exact stack (Pascal, 580, Smithay) and must be bench-tested before
   anyone relies on it.
   - To check it's decoding on the card: `chrome://media-internals` should show
     `VaapiVideoDecoder` (not `VpxVideoDecoder` or `Dav1dVideoDecoder`), and
     `nvidia-smi dmon -s u` should show load in the `dec` column.
2. **Software decode:** 4K60 VP9, and AV1 even more so, on a 4-core/8-thread
   mobile Coffee Lake at 45 W is not realistic for sustained playback. Expect drops.
   1080p60 VP9 in software is fine.
3. **Intel iHD with cross-GPU import** is a third option, but needs
   `intel-media-driver` added and `--hardware-video-device-path`. Unverified.
4. **Jellyfin:** Jellyfin can transcode on luna's GPU to H.264 or HEVC, so it
   doesn't depend on any of this.

## 6. Couch navigation

### YouTube

- **Desktop site:** keyboard shortcuts (k or space to play or pause, j/l to seek
  ±10 s, f for fullscreen, c for captions, Shift+N/P for next and previous, / for
  search) work once the player has focus
  ([support.google.com/youtube/answer/7631406](https://support.google.com/youtube/answer/7631406)).
  Browsing the grid needs Tab or a pointer. This is not a 10-foot UI.
- **youtube.com/tv (leanback)** is the real couch UI, with D-pad and gamepad
  support.
  - **(observed, curl)** With a desktop Chrome user agent it returns a 5.7 KB stub
    that redirects to `youtube.com/?app=desktop`.
  - With a Tizen TV user agent it serves the full app, titled "YouTube TV".
  - So the YouTube web app needs `--user-agent=<a TV UA>`.

### Jellyfin web

- Arrow and D-pad keys only navigate in the TV layout
  ([`src/scripts/keyboardNavigation.js`](https://github.com/jellyfin/jellyfin-web/blob/master/src/scripts/keyboardNavigation.js),
  gated on `layoutManager.tv`).
- Set the layout to TV under Display. It is stored in localStorage under the
  `layout` key, so it persists in the per-app profile.
- Gamepad support is off by default (`appSettings.enableGamepad`). Turn it on in
  Settings → Controls.
- With gamepad on, [`gamepadtokey.js`](https://github.com/jellyfin/jellyfin-web/blob/master/src/scripts/gamepadtokey.js)
  maps A to Enter, B to Escape, and the D-pad and left stick to arrows.
- It reads the standard Gamepad API mapping.

### Steam Controller through the Gamepad API

- Chromium reads gamepads through evdev and joydev nodes tagged
  `ID_INPUT_JOYSTICK`. hid-steam's gamepad device qualifies.
- Chromium only gained a **standard mapping** for hid-steam controllers in commit
  [`8dd3f5a534`](https://github.com/chromium/chromium/commit/8dd3f5a534)
  (Cr-Commit-Position #1695735, which is M155 since M155 branched at #1697595).
- On M154 the pad shows up with no standard mapping, so Jellyfin's button indices
  may come out wrong.
- The simplest path is for **emrakul to translate the controller into key events**
  for the foreground client: arrows, Enter, Escape. That works on both sites with
  no Gamepad API involved. The YouTube TV UI also takes keyboard input.

### hidraw risk: does Chromium take the Steam Controller from hid-steam?

Ticket 2 found that any `open()` of the controller's hidraw node makes hid-steam
unregister its gamepad, motion and battery devices until the last close. Chromium's
Linux gamepad fetcher watches both the `input` and `hidraw` udev subsystems
([`device/gamepad/gamepad_platform_data_fetcher_linux.cc`](https://github.com/chromium/chromium/blob/main/device/gamepad/gamepad_platform_data_fetcher_linux.cc),
`OnAddedToProvider`).

**1. Which devices get hidraw opened: only allowlisted ones, and Valve isn't on the list.**
- Since commit [`de69492965`](https://github.com/chromium/chromium/commit/de6949296597b6ab53a9c510c2fbd2afad9a0644),
  "Allowlist hidraw gamepads on Linux/ChromeOS" (Cr-Commit-Position #1556830, which
  is **M145** since M145 branched at #1568190), `RefreshHidrawDevice` reads
  `HID_ID` from the udev parent *before* opening anything.
- It returns early unless the device is one of:
  - a DualShock 4: `054c:05c4`, `054c:09cc`, Scuf `7725` ([`dualshock4_controller.cc`](https://github.com/chromium/chromium/blob/main/device/gamepad/dualshock4_controller.cc))
  - a Bluetooth Xbox pad: Microsoft `02e0/02fd/0b05/0b13/0b20/0b22` ([`xbox_hid_controller.cc`](https://github.com/chromium/chromium/blob/main/device/gamepad/xbox_hid_controller.cc))
  - a HID-haptic pad: XSkills GameCube adapter, Stadia ([`hid_haptic_gamepad.cc`](https://github.com/chromium/chromium/blob/main/device/gamepad/hid_haptic_gamepad.cc))
- The gate is the `AllowlistHidrawGamepads` feature, enabled by default
  ([`device/gamepad/public/cpp/gamepad_features.cc#L48`](https://github.com/chromium/chromium/blob/main/device/gamepad/public/cpp/gamepad_features.cc#L48)).
- Valve `28de` isn't on the list.
- The only fall-through is a hidraw node with **no** `HID_ID` on its parent. That
  node gets opened, and its IDs checked afterwards.
- **(observed)** ganymede's controller nodes `hidraw1`–`hidraw5` all carry
  `HID_ID=0003:000028DE:00001304` ("Valve Software Steam Controller Puck"), so
  they're skipped.
- Before M145, Chromium opened **every** hidraw node to read its IDs (the commit
  message says so). **Pin Chromium ≥ 145.** nixpkgs' 154 is fine.

**2. When enumeration happens: lazily, on first Gamepad API use, then for the rest of the process.**
- The `GamepadProvider`, and with it the udev watcher and enumeration, is created
  in `GamepadService::EnsureProvider()`. That is called from `ConsumerBecameActive`
  ([`device/gamepad/gamepad_service.cc#L66-L77`](https://github.com/chromium/chromium/blob/main/device/gamepad/gamepad_service.cc)),
  which a renderer triggers by starting gamepad polling
  ([`gamepad_monitor.cc`](https://github.com/chromium/chromium/blob/main/device/gamepad/gamepad_monitor.cc)).
  Blink does that when a page calls `navigator.getGamepads()` or listens for
  `gamepadconnected`.
- Browser startup only runs `StartUp()`, which creates the fetcher factory and
  opens nothing.
- Once created, the provider lives until the browser exits. Pausing it stops
  polling but keeps the fds open.
- Jellyfin with gamepad enabled, and YouTube TV, will both trigger it. That's
  harmless given the allowlist.

**3. What turns it off**
- No flag is needed on M145 and later.
- **Belt-and-braces (declarative):** don't grant the session user the controller's
  hidraw nodes. Nothing on the TV needs them, since emrakul reads hid-steam's
  evdev devices.
  - Today `programs.steam` / `hardware.steam-hardware` installs
    `60-steam-input.rules`, which tags `KERNEL=="hidraw*", ATTRS{idVendor}=="28de"`
    with `uaccess` **(observed:** hidraw1–5 are `0660 root:root` plus an ACL**)**.
  - If ganymede keeps Steam installed, add a later rule that removes `uaccess` for
    `28de` hidraw, or don't enable steam-hardware there. Then neither Chromium nor
    any other process run as the user can open them.
- **What doesn't work:** `--disable-features=AllowlistHidrawGamepads` makes it
  *worse*, because it brings back open-everything. There's no switch to disable
  the Gamepad API itself.
- **WebHID** (`navigator.hid`) is a separate path. It reads report descriptors
  from sysfs during enumeration and only opens a node on `Connect`
  ([`services/device/hid/hid_service_linux.cc`](https://github.com/chromium/chromium/blob/main/services/device/hid/hid_service_linux.cc)).
  Connect only happens after a user grants access in a chooser, which can't
  appear here. To rule it out declaratively, set the policy
  `programs.chromium.extraOpts.DefaultWebHidGuardSetting = 2`.

## What emrakul should do, in order

1. **Configure and draw `xdg_popup`s**, or `<select>` and menus are invisible.
2. Send `wl_keyboard.enter` to the foreground toplevel, which already happens with
   keyboard focus. Translate Steam Controller input into keys for web apps.
3. Launch web apps with `--ozone-platform=wayland --kiosk --app=URL`, one
   `--user-data-dir` per site. Close them with `xdg_toplevel.close` followed by
   SIGTERM.
4. Make sure dmabuf feedback sends `done`, or Chromium waits 500 ms at every launch.
5. Later: `zwp_idle_inhibit` if emrakul gains blanking, cursor-shape and pointer
   constraints with a pointer, and text-input-v3 for an on-screen keyboard.
6. Bench the NVDEC recipe in §5 for 4K60 VP9 before promising 4K YouTube.
