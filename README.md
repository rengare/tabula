# tabula

Use an Android pen tablet as a second (or mirrored) monitor and a pressure-sensitive drawing tablet for Linux. It works with any desktop (GNOME, KDE, COSMIC, …) because it doesn't use compositor-specific APIs:

- **Virtual monitor:** the kernel's `vkms` driver, configured through configfs. Your desktop sees an ordinary hotplugged monitor. For mirror mode, set it to mirror in display settings.
- **Capture:** tabula reads the monitor's scanout buffer directly from DRM.
- **Pen input:** a `uinput` virtual tablet (pressure, tilt, eraser).
- **Tablet side:** a web page (WebCodecs + Pointer Events), so there's nothing to install. A native app can be added later using the same [protocol](docs/protocol.md).

## Status

- Milestone 0 (virtual monitor + capture): works. COSMIC needs a cosmic-comp fix for display-only DRM devices whose EGL render node belongs to another GPU (vkms through Mesa's kmsro); without it the vkms output stays black and the compositor logs `NoDevice` renderer errors. GNOME and KDE are untested.
- Milestone 1 (encode + web viewer + streaming): works end to end on a real tablet over USB (~40 fps with motion, 6–8 ms round trip).
- Next: pen input through uinput (milestone 2).

## Build

```fish
cd web; and npm install; and npm run build; and cd ..   # the web client is embedded in the binary
cargo build --release
```

## Try it

```fish
# Pipeline test, no virtual monitor needed:
./target/release/tabula run --test-pattern
# then open http://localhost:7543/?hud=1 (on the tablet over USB, `adb reverse` is set up automatically)

# Real virtual monitor:
sudo ./target/release/tabula setup      # loads vkms, creates the monitor, grants CAP_SYS_ADMIN
./target/release/tabula capture-test    # writes tabula-frame.png from the virtual monitor
./target/release/tabula run
sudo ./target/release/tabula teardown   # removes the monitor
```

On the tablet, connect over USB with USB debugging enabled, start `tabula run`, then open `http://localhost:7543` in Chrome and tap **Enter fullscreen**. `?hud=1` shows fps and round-trip time. `?mouse=1` treats the mouse as a pen, for testing on a desktop.

The virtual monitor starts at 1024×768. Pick a mode closer to your tablet's resolution in display settings. Rebuilding the binary drops its capability, so rerun `setup` after a rebuild.

## Map the pen to the tablet's monitor

Once the pen is added, map its input device to the virtual monitor in your desktop's settings:

- GNOME: Settings → Wacom Tablet → Map to Monitor
- KDE: System Settings → Drawing Tablet → Map to screen
- COSMIC: `map_to_output` in the input config
