# tabula

Use an Android pen tablet as a second (or mirrored) monitor and a pressure-sensitive drawing tablet for Linux. It works with any desktop (GNOME, KDE, COSMIC, …) because it doesn't use compositor-specific APIs:

- **Virtual monitor:** the kernel's `vkms` driver, configured through configfs. Your desktop sees an ordinary hotplugged monitor. For mirror mode, set it to mirror in display settings.
- **Capture:** tabula reads the monitor's scanout buffer directly from DRM.
- **Pen input:** a `uinput` virtual tablet (pressure, tilt, eraser).
- **Tablet side:** a web page (WebCodecs + Pointer Events), so there's nothing to install. A native app can be added later using the same [protocol](docs/protocol.md).

## Status

- Milestone 0 (virtual monitor + capture): works. COSMIC needs a cosmic-comp fix for display-only DRM devices whose EGL render node belongs to another GPU (vkms through Mesa's kmsro); without it the vkms output stays black and the compositor logs `NoDevice` renderer errors. GNOME and KDE are untested.
- Milestone 1 (encode + web viewer + streaming): works end to end on a real tablet over USB (~40 fps with motion, 6–8 ms round trip).
- Milestone 2 (pen through uinput): works on COSMIC; pressure, tilt, eraser and barrel buttons. Capture polls for new buffers every 4 ms so flips aren't missed.
- Next: LAN mode (HTTPS + pairing), hardware encoder, touch.

## Performance

Measured with `tabula run --stats` and `node tools/bench.ts` (hovers the virtual pen in a circle and counts frames), mirroring a 1920×1200 laptop to a Lenovo TB336FU (MediaTek MT8755) over USB:

| Stage | p50 / p95 |
|---|---|
| COSMIC page flips on the virtual monitor | 60/s |
| Frames sent without flow control | 60/s |
| Grab (detect new buffer → copied) | 0.8 / 1.0 ms |
| Encode (x264, 1920×1200) | 4.8 / 6.5 ms |
| Pen sample → frame showing it sent | 18 / 22 ms |
| USB round trip | ~6 ms |
| Decode on the tablet (Chrome WebCodecs) | ~20 ms |

Chrome's hardware decoder on this tablet tops out around 36–41 fps regardless of resolution, which is a fixed per-frame cost in Chrome's WebCodecs path on Android. The client acknowledges every decoded frame (`Ack`) and the server keeps at most `--max-in-flight` (default 2) frames unacknowledged, skipping capture instead of queueing. That keeps latency bounded; without it the decoder queue grew by ~25 frames per second (over a second of lag). A native client using MediaCodec directly should reach the full 60 fps.

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

The virtual monitor starts at 1024×768. Set its mode in display settings:

- **Extend:** use a mode with the tablet's aspect ratio, e.g. 2560×1600 or 1920×1200 for a 16:10 tablet.
- **Mirror:** use the same mode *and scale* as the mirrored display. COSMIC draws the mirrored display's logical area at the target's scale without stretching it, so a 1920×1200 laptop at 110% needs `Virtual-1` at 1920×1200 and 110%. Otherwise the picture is letterboxed inside the frame. On COSMIC: `cosmic-randr mode Virtual-1 1920 1200 --scale 1.1`.

Rebuilding the binary drops its capability, so rerun `setup` after a rebuild.

## Pen

When the tablet connects, tabula creates a uinput tablet named **tabula pen** (screen tablet, pressure, tilt, eraser, barrel buttons). `tabula setup` installs a udev rule that lets the logged-in user create it; `--view-only` disables it.

Pen positions cover the whole streamed monitor, so the desktop has to map the device onto that monitor:

- **Mirror:** map it to the mirrored display. COSMIC maps tablets to the built-in display by default, so nothing needs to be set.
- **Extend:** map it to the virtual monitor.
  - GNOME: Settings → Wacom Tablet → Map to Monitor
  - KDE: System Settings → Drawing Tablet → Map to screen
  - COSMIC: in `~/.config/cosmic/com.system76.CosmicComp/v1/input_devices`, add `"tabula pen": (state: Enabled, map_to_output: Some("Virtual-1"))`
