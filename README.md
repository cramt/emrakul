# emrakul

A Wayland compositor that turns a PC plugged into a TV into an appliance: one
fullscreen thing on screen at a time, driven from the couch. Built on
[Smithay](https://github.com/Smithay/smithay), in one crate, configured by one
TOML file.

Its first home is `ganymede`, a GTX 1050 Ti laptop on an LG OLED. Games are
streamed in from a desktop over Moonlight, so emrakul is not a gaming
compositor. It is a TV shell that happens to be able to show a game stream.

## Status

**M0, pixels on the TV: done.** On ganymede, emrakul sets 3840x2160@60 on the
NVIDIA card, renders with GLES on that same card, and shows the newest client
fullscreen. mpv plays a 60 fps test pattern there at a measured 60.0003 Hz with
0 dropped and 0 mistimed frames.

What it does today:

- Opens one configured DRM card and connector, and sets the configured mode or
  the display's preferred one.
- Lists every app (installed desktop entries from `XDG_DATA_HOME` and
  `XDG_DATA_DIRS`) on a placeholder Home, most recently launched first, then
  by name. Entries marked `Hidden`, `NoDisplay` or `OnlyShowIn` aren't apps.
  The order lives in `$XDG_STATE_HOME/emrakul/recent`, so it survives restarts.
- Runs one app at a time. Going Home asks it to close (`xdg_toplevel.close`,
  or the entry's `X-Emrakul-Quit` command if it has one), then sends its
  process group SIGTERM after 5 s and SIGKILL 5 s after that. Whenever the
  app's process exits, however it exits, Home comes back.
- Shows exactly one toplevel, fullscreen and with no decorations. The newest
  one wins, and closing it brings back the one before.
- Speaks enough Wayland for real clients: shm, dmabuf with per-surface scanout
  feedback, presentation-time, viewporter, xdg-output, and xdg popups, so a
  web app's `<select>` dropdowns and context menus show above it, kept on the
  screen. They close when their app leaves the screen.
- Reserves Ctrl+Alt+Backspace (quit), Ctrl+Alt+F1–F12 (switch VT) and
  Ctrl+Alt+H (go Home, the keyboard's Steam button). Every other key goes
  to the foreground client, except on Home itself, where arrows move the focus
  and Enter launches. Home is a solid colour for now; the focused app's name
  is only in the log.
- Reads gamepads (the Steam Controller through the kernel's `hid-steam`
  driver, Linux 7.3+) straight from their evdev nodes, following udev as they
  come and go with the wireless link. The Steam button (`BTN_MODE`) goes Home
  from anywhere. On Home, a stand-in mapping until the full controller table
  is decided: D-pad, or the left stick pushed past half way, moves the focus,
  and A launches. Over an app, only the Steam button does anything so far.

Next: Home drawn for real: names, icons and focus.

## Configuration

```toml
# A by-path name: cardN numbering follows probe order and can swap between boots.
device = "/dev/dri/by-path/pci-0000:01:00.0-card"
connector = "HDMI-A-1"
mode = "3840x2160@60"   # optional; omitted = the display's preferred mode
launch = ["foot"]       # optional; started once the compositor is up, outside
                        # the app lifecycle (a test hook)
```

Run it with `emrakul --config config.toml` inside a logind session that owns
the seat. On NixOS, use the module:

```nix
services.emrakul = {
  enable = true;
  user = "cramt";
  settings = {
    device = "/dev/dri/by-path/pci-0000:01:00.0-card";
    connector = "HDMI-A-1";
    mode = "3840x2160@60";
  };
};
```

The module runs emrakul as a system service on a VT, with a PAM login session,
and switches that VT's getty off. It refuses to evaluate alongside a display
manager, because whichever starts second gets no DRM master and shows a black
screen. It also sets `hid_steam lizard_mode=0`, without which the
gamepad node stays silent, and takes the Steam Controller's hidraw nodes
away from the seat's user, since anything opening one makes `hid-steam`
unregister the gamepad.

## Development

```sh
nix develop -c cargo test
nix develop -c cargo clippy --all-targets -- -D warnings
nix build
```

`RUST_LOG=emrakul=debug` logs which plane each frame went out on, and why the
foreground surface was or wasn't scanned out directly. Add `emrakul=trace` for
every redraw transition.

## Hardware notes: ganymede (GTX 1050 Ti, nvidia 580)

- **TV EDID.** The LG only advertises HDMI 2.x (4K60, HDR10, VRR) once *HDMI
  Deep Colour* is enabled for its input. Before that, it reports 4K30 at most.
- **No VRR.** The TV advertises 40–120 Hz VRR, but the connector reports
  `vrr_capable = 0`. NVIDIA only does HDMI VRR on Turing and newer.
- **No overlay planes.** Smithay's reference compositor disables them on
  NVIDIA because using one breaks the output, and emrakul does the same. The
  primary plane is still available for direct scanout.
- **Direct scanout doesn't happen with mpv**, so frames are composited, which
  holds 4K60 without drops. mpv's Vulkan path allocates compressed
  block-linear buffers (modifier `0x0300000000cdb014`: kind `0xdb`,
  compression 1). The primary plane only accepts uncompressed kind `0xfe`
  (`0x03000000004fe010`–`…015`), and nvidia-drm refuses them in `addfb2`
  ("Invalid format modifier for framebuffer object"). The scanout tranche in
  the dmabuf feedback only lists the plane's modifiers, but NVIDIA's Vulkan
  WSI allocates compressed buffers anyway. mpv's OpenGL path is never even
  offered for scanout. Revisit if a client or driver update starts honouring
  the scanout tranche.
- **4K60 YouTube decodes on the card.** Chromium 154 with nvidia-vaapi-driver
  0.0.18 decodes 4K60 VP9 on NVDEC under emrakul: `VaapiVideoDecoder`, about
  26% decoder load, 23% CPU, 0–1.6% dropped frames. It needs
  `LIBVA_DRIVER_NAME=nvidia` and `--enable-features=AcceleratedVideoDecodeLinuxGL,VaapiOnNvidiaGPUs
  --ignore-gpu-blocklist --use-gl=angle --use-angle=gl`. Without those flags
  Chromium decodes in software (`VpxVideoDecoder`): it still reaches 60 fps,
  but at 77% CPU and 2–3% dropped frames. Measurements are in
  [#14](https://github.com/cramt/emrakul/issues/14).
- **Steam Controller on 7.3.** hid-steam binds the puck (`28de:1304`) and the
  NVIDIA driver loads on 7.3-rc4. The gamepad node only exists while the
  controller is awake, anything opening the puck's hidraw unregisters it, and
  with `lizard_mode=0` every control arrives on the gamepad (Steam is
  `BTN_MODE`). Full map: [docs/hardware/steam-controller-7.3.md](docs/hardware/steam-controller-7.3.md).
- The Smithay rev is pinned to the one niri 26.04 ships, because that build was
  seen driving this exact TV before emrakul existed.
