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
- Shows exactly one toplevel, fullscreen and with no decorations. The newest
  one wins, and closing it brings back the one before.
- Speaks enough Wayland for real clients: shm, dmabuf with per-surface scanout
  feedback, presentation-time, viewporter, xdg-output.
- Reserves Ctrl+Alt+Backspace (quit) and Ctrl+Alt+F1–F12 (switch VT). Every
  other key goes to the foreground client.

Next: the 2026 Steam Controller through the kernel's `hid-steam` driver (Linux
7.3+), and a home screen drawn by the compositor itself.

## Configuration

```toml
# A by-path name: cardN numbering follows probe order and can swap between boots.
device = "/dev/dri/by-path/pci-0000:01:00.0-card"
connector = "HDMI-A-1"
mode = "3840x2160@60"   # optional; omitted = the display's preferred mode
launch = ["foot"]       # optional; started once the compositor is up
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
screen.

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
- The Smithay rev is pinned to the one niri 26.04 ships, because that build was
  seen driving this exact TV before emrakul existed.
