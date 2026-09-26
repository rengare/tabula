# tabula wire protocol, version 1

This spec is the source of truth for every client (web now, native later). `crates/protocol` implements it, and `fixtures/*.hex` holds the exact bytes for one sample of each message. Client implementations must decode and encode those fixtures identically.

## Framing

Each message is `tag: u8` followed by its fields. All integers are little-endian. The transport decides framing:

| Transport | Framing |
|---|---|
| WebSocket (`/ws`) | one binary WS message = one protocol message |
| raw TCP (native clients, later) | `len: u32` LE, then `len` bytes of message |

Default port: 7543. Over USB, run `adb reverse tcp:7543 tcp:7543` and connect to `localhost:7543`.

`str8` = `len: u8` followed by `len` bytes of UTF-8.

Receivers **must ignore messages with unknown tags**. That lets newer peers add messages without breaking older ones.

## Session

1. The client connects and sends `Hello`.
2. The server checks `proto_ver` and closes the connection if it's incompatible. It then sends `StreamConfig`, followed by a keyframe `Video`.
3. After that, messages flow in both directions. The client sends `RequestKeyframe` whenever its decoder needs to resync (after an error or after dropping frames).

## Client → server

| Tag | Message | Fields |
|---|---|---|
| `0x01` | Hello | `proto_ver u16`, `client_kind u8` (0 web, 1 native), `features u32`, `screen_w u16`, `screen_h u16` (physical px), `dpi u16`, `n_codecs u8`, then `n_codecs × str8` codec strings, most preferred first |
| `0x02` | Pen | `x u16`, `y u16` (normalized to the video frame: 0 = left/top, 65535 = right/bottom), `pressure u16` (0–65535), `tilt_x i8`, `tilt_y i8` (degrees, −90..90), `tool u8` (0 pen, 1 eraser), `buttons u8` (bit0 barrel, bit1 second barrel), `flags u8` (bit0 contact, bit1 in_range) |
| `0x04` | Ping | `t u64`: an opaque client timestamp, echoed back in Pong |
| `0x05` | RequestKeyframe | none |

`features` bits: 0 pen, 1 hover, 2 eraser, 3 barrel button, 4 tilt, 5 touch.

Pen semantics: send one Pen message per input sample (use coalesced samples when available). When `in_range` = 0, the pen has left proximity. The server releases all buttons.

## Server → client

| Tag | Message | Fields |
|---|---|---|
| `0x81` | StreamConfig | `width u16`, `height u16`, `codec str8` (WebCodecs codec string, e.g. `avc1.42E033`) |
| `0x82` | Video | `pts_us u64`, `keyframe u8`, then the rest of the message: one H.264 access unit in Annex-B format. Keyframes include SPS/PPS. |
| `0x83` | Pong | `t u64` copied from the Ping |

The video is constrained-baseline H.264 without B-frames, so decode order equals display order. Both WebCodecs (`VideoDecoder`, Annex-B input, no `description`) and Android `MediaCodec` accept it as-is.
