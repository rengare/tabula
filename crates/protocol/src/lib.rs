//! tabula wire protocol.
//!
//! Every message is `tag: u8` followed by little-endian fields. The encoding
//! is independent of the transport: one WebSocket binary message carries one
//! protocol message, and a raw TCP transport prefixes each message with its
//! length as a `u32`. `docs/protocol.md` is the normative spec; the golden
//! fixtures in `fixtures/` pin the byte layout for every client implementation.

use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 1;

/// Default port for both the HTTP/WebSocket and the raw TCP transport.
pub const DEFAULT_PORT: u16 = 7543;

mod tag {
    pub const HELLO: u8 = 0x01;
    pub const PEN: u8 = 0x02;
    pub const TOUCH: u8 = 0x03;
    pub const PING: u8 = 0x04;
    pub const REQUEST_KEYFRAME: u8 = 0x05;
    pub const ACK: u8 = 0x06;

    pub const STREAM_CONFIG: u8 = 0x81;
    pub const VIDEO: u8 = 0x82;
    pub const PONG: u8 = 0x83;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClientKind {
    Web = 0,
    Native = 1,
}

/// Capabilities a client advertises in [`Hello`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features(pub u32);

impl Features {
    pub const PEN: Self = Self(1 << 0);
    pub const HOVER: Self = Self(1 << 1);
    pub const ERASER: Self = Self(1 << 2);
    pub const BARREL_BUTTON: Self = Self(1 << 3);
    pub const TILT: Self = Self(1 << 4);
    pub const TOUCH: Self = Self(1 << 5);
    /// The client sends [`Message::Ack`] for every decoded frame, which
    /// lets the server limit frames in flight to what the client keeps up with.
    pub const ACKS: Self = Self(1 << 6);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Features {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub proto_ver: u16,
    pub client_kind: ClientKind,
    pub features: Features,
    /// Physical pixels of the client's display area.
    pub screen_w: u16,
    pub screen_h: u16,
    pub dpi: u16,
    /// Decodable codecs, most preferred first, e.g. `"avc1.42E01F"`.
    pub codecs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tool {
    Pen = 0,
    Eraser = 1,
}

/// Pen button bits in [`Pen::buttons`].
pub mod buttons {
    pub const BARREL: u8 = 1 << 0;
    pub const BARREL2: u8 = 1 << 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pen {
    /// Position normalized to the video frame: 0 = left/top, 65535 = right/bottom.
    pub x: u16,
    pub y: u16,
    /// 0 = no pressure, 65535 = full pressure.
    pub pressure: u16,
    /// Degrees, -90..=90.
    pub tilt_x: i8,
    pub tilt_y: i8,
    pub tool: Tool,
    pub buttons: u8,
    /// Tip touches the surface.
    pub contact: bool,
    /// Pen is detected (hovering or touching). `false` means it left proximity.
    pub in_range: bool,
}

/// One finger on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contact {
    /// Stable for as long as the finger stays down.
    pub id: u8,
    /// Normalized like [`Pen::x`] / [`Pen::y`].
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    pub width: u16,
    pub height: u16,
    /// WebCodecs-style codec string, e.g. `"avc1.42E01F"`.
    pub codec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub pts_us: u64,
    pub keyframe: bool,
    /// One Annex-B H.264 access unit.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Hello(Hello),
    Pen(Pen),
    /// All fingers currently touching; empty when none are.
    Touch(Vec<Contact>),
    Ping { t: u64 },
    RequestKeyframe,
    /// The frame with this `pts_us` has been decoded.
    Ack { pts_us: u64 },
    StreamConfig(StreamConfig),
    Video(Video),
    Pong { t: u64 },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("empty message")]
    Empty,
    /// Receivers must ignore unknown tags so newer peers can add messages.
    #[error("unknown message tag {0:#04x}")]
    UnknownTag(u8),
    #[error("message truncated")]
    Truncated,
    #[error("invalid value for {0}")]
    Invalid(&'static str),
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer(Vec::with_capacity(16));
        match self {
            Message::Hello(h) => {
                w.u8(tag::HELLO);
                w.u16(h.proto_ver);
                w.u8(h.client_kind as u8);
                w.u32(h.features.0);
                w.u16(h.screen_w);
                w.u16(h.screen_h);
                w.u16(h.dpi);
                w.u8(h.codecs.len() as u8);
                for c in &h.codecs {
                    w.str8(c);
                }
            }
            Message::Pen(p) => {
                w.u8(tag::PEN);
                w.u16(p.x);
                w.u16(p.y);
                w.u16(p.pressure);
                w.u8(p.tilt_x as u8);
                w.u8(p.tilt_y as u8);
                w.u8(p.tool as u8);
                w.u8(p.buttons);
                w.u8(p.contact as u8 | (p.in_range as u8) << 1);
            }
            Message::Touch(contacts) => {
                w.u8(tag::TOUCH);
                w.u8(contacts.len().min(255) as u8);
                for c in contacts.iter().take(255) {
                    w.u8(c.id);
                    w.u16(c.x);
                    w.u16(c.y);
                }
            }
            Message::Ping { t } => {
                w.u8(tag::PING);
                w.u64(*t);
            }
            Message::RequestKeyframe => w.u8(tag::REQUEST_KEYFRAME),
            Message::Ack { pts_us } => {
                w.u8(tag::ACK);
                w.u64(*pts_us);
            }
            Message::StreamConfig(s) => {
                w.u8(tag::STREAM_CONFIG);
                w.u16(s.width);
                w.u16(s.height);
                w.str8(&s.codec);
            }
            Message::Video(v) => {
                w.0.reserve(v.data.len() + 10);
                w.u8(tag::VIDEO);
                w.u64(v.pts_us);
                w.u8(v.keyframe as u8);
                w.0.extend_from_slice(&v.data);
            }
            Message::Pong { t } => {
                w.u8(tag::PONG);
                w.u64(*t);
            }
        }
        w.0
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let (&tag, rest) = buf.split_first().ok_or(DecodeError::Empty)?;
        let mut r = Reader(rest);
        let msg = match tag {
            tag::HELLO => {
                let proto_ver = r.u16()?;
                let client_kind = match r.u8()? {
                    0 => ClientKind::Web,
                    1 => ClientKind::Native,
                    _ => return Err(DecodeError::Invalid("client_kind")),
                };
                let features = Features(r.u32()?);
                let screen_w = r.u16()?;
                let screen_h = r.u16()?;
                let dpi = r.u16()?;
                let n = r.u8()?;
                let codecs = (0..n).map(|_| r.str8()).collect::<Result<_, _>>()?;
                Message::Hello(Hello {
                    proto_ver,
                    client_kind,
                    features,
                    screen_w,
                    screen_h,
                    dpi,
                    codecs,
                })
            }
            tag::PEN => {
                let x = r.u16()?;
                let y = r.u16()?;
                let pressure = r.u16()?;
                let tilt_x = r.u8()? as i8;
                let tilt_y = r.u8()? as i8;
                let tool = match r.u8()? {
                    0 => Tool::Pen,
                    1 => Tool::Eraser,
                    _ => return Err(DecodeError::Invalid("tool")),
                };
                let buttons = r.u8()?;
                let flags = r.u8()?;
                Message::Pen(Pen {
                    x,
                    y,
                    pressure,
                    tilt_x,
                    tilt_y,
                    tool,
                    buttons,
                    contact: flags & 1 != 0,
                    in_range: flags & 2 != 0,
                })
            }
            tag::TOUCH => {
                let n = r.u8()?;
                let contacts = (0..n)
                    .map(|_| Ok(Contact { id: r.u8()?, x: r.u16()?, y: r.u16()? }))
                    .collect::<Result<_, DecodeError>>()?;
                Message::Touch(contacts)
            }
            tag::PING => Message::Ping { t: r.u64()? },
            tag::REQUEST_KEYFRAME => Message::RequestKeyframe,
            tag::ACK => Message::Ack { pts_us: r.u64()? },
            tag::STREAM_CONFIG => Message::StreamConfig(StreamConfig {
                width: r.u16()?,
                height: r.u16()?,
                codec: r.str8()?,
            }),
            tag::VIDEO => Message::Video(Video {
                pts_us: r.u64()?,
                keyframe: r.u8()? != 0,
                data: r.rest().to_vec(),
            }),
            tag::PONG => Message::Pong { t: r.u64()? },
            other => return Err(DecodeError::UnknownTag(other)),
        };
        Ok(msg)
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// UTF-8 string with a u8 length prefix; longer strings are truncated to 255 bytes.
    fn str8(&mut self, s: &str) {
        let mut end = s.len().min(255);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.u8(end as u8);
        self.0.extend_from_slice(&s.as_bytes()[..end]);
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, tail) = self.0.split_first_chunk::<N>().ok_or(DecodeError::Truncated)?;
        self.0 = tail;
        Ok(*head)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn str8(&mut self) -> Result<String, DecodeError> {
        let len = self.u8()? as usize;
        if self.0.len() < len {
            return Err(DecodeError::Truncated);
        }
        let (s, tail) = self.0.split_at(len);
        self.0 = tail;
        String::from_utf8(s.to_vec()).map_err(|_| DecodeError::Invalid("utf-8 string"))
    }
    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }
}

#[cfg(test)]
mod tests;
