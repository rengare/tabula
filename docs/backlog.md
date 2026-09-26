# Backlog

Not scheduled yet, roughly in priority order.

## Native Android client

A Kotlin app next to the web client, talking to the same server.

- **Why:** Chrome's WebCodecs decoder on the test tablet (Lenovo TB336FU, MediaTek MT8755) tops out around 36–41 fps at any resolution and adds ~20 ms of decode latency. MediaCodec used directly, with `KEY_LOW_LATENCY` and output straight to a `Surface`, should reach 60 fps. `MotionEvent` also gives raw stylus data (hover, eraser, buttons, tilt/orientation, historical samples) without the browser's quirks, such as the missing `pointerleave` after the pen leaves hover range.
- **Transport:** raw TCP with a `u32` length prefix per message (`docs/protocol.md`), over `adb reverse` or the LAN. The server needs a `transport/tcp` adapter next to the WebSocket one; `crates/session` stays unchanged.
- **Protocol:** implement against `docs/protocol.md` and check it with the golden fixtures in `fixtures/`, like the TypeScript client does. Send `Hello` with `client_kind = Native` and the `acks` feature.
- **Build setup:** this laptop is aarch64 and Google ships `aapt2` for x86_64 Linux only. Options: run Google's `aapt2` through FEX (already the x86 binfmt handler here), or use Ubuntu's arm64 `aapt2` via `android.aapt2FromMavenOverride`. `ANDROID_HOME` currently points at a macOS path (`/Users/ren/Android/Sdk`) from the dotfiles.

## Other

- **Privileged capture helper:** every rebuild of `tabula` drops `CAP_SYS_ADMIN` and needs `setcap` again. Move the DRM capture into a small helper that rarely changes and hands dma-buf fds to the unprivileged server.
- **Hover smoothing:** the hover glitch filter (`web/src/hover.ts`) leaves a few smaller jumps in sustained palm noise. If they're still visible, add light smoothing for hover only, at the cost of some cursor lag.
- **GPU color conversion:** with the hardware encoder, most of the remaining CPU cost is the RGB→NV12 `videoconvert`. Import the dma-buf and convert on the GPU (or hand it to the encoder directly).
- **Iris hardware encoder:** on kernel 7.2.5 the Qualcomm Iris encoder hung under load, and the driver logged a UBSAN out-of-bounds read at `iris_buffer.c:932` (`metadata_idx` reaches 32 in `iris_set_ts_metadata` and isn't wrapped until the next write). Report it to linux-media, then reconsider making `--encoder v4l2` the default.
- **COSMIC tablet proximity:** cosmic-comp calls `set_grab` before `proximity_in` when the pen enters over a surface without tablet support (`src/input/mod.rs`), so smithay logs "set_grab called with an out of proximity tool" and the pointer emulation grab only starts on the next motion. Worth an upstream issue.
- **License files:** `Cargo.toml` declares `MIT OR Apache-2.0`, but `LICENSE-MIT` / `LICENSE-APACHE` aren't in the repo yet.
- **Test on GNOME and KDE:** the virtual monitor and capture are compositor-independent by design, but only COSMIC has been tested.
