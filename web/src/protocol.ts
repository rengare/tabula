// Mirror of crates/protocol; see docs/protocol.md. Checked against the
// golden fixtures in ../fixtures by test/protocol.test.ts.

export const PROTOCOL_VERSION = 1;

export const ClientKind = { Web: 0, Native: 1 } as const;
export type ClientKind = (typeof ClientKind)[keyof typeof ClientKind];

export const Features = {
  Pen: 1 << 0,
  Hover: 1 << 1,
  Eraser: 1 << 2,
  BarrelButton: 1 << 3,
  Tilt: 1 << 4,
  Touch: 1 << 5,
  Acks: 1 << 6,
} as const;

export const Tool = { Pen: 0, Eraser: 1 } as const;
export type Tool = (typeof Tool)[keyof typeof Tool];

export const Buttons = { Barrel: 1 << 0, Barrel2: 1 << 1 } as const;

export type Hello = {
  proto_ver: number;
  client_kind: ClientKind;
  features: number;
  screen_w: number;
  screen_h: number;
  dpi: number;
  codecs: string[];
};

export type Pen = {
  x: number;
  y: number;
  pressure: number;
  tilt_x: number;
  tilt_y: number;
  tool: Tool;
  buttons: number;
  contact: boolean;
  in_range: boolean;
};

export type StreamConfig = { width: number; height: number; codec: string };
export type Video = { pts_us: bigint; keyframe: boolean; data: Uint8Array };

export type Message =
  | { type: "Hello"; hello: Hello }
  | { type: "Pen"; pen: Pen }
  | { type: "Ping"; t: bigint }
  | { type: "RequestKeyframe" }
  | { type: "Ack"; pts_us: bigint }
  | { type: "StreamConfig"; config: StreamConfig }
  | { type: "Video"; video: Video }
  | { type: "Pong"; t: bigint };

const Tag = {
  Hello: 0x01,
  Pen: 0x02,
  Ping: 0x04,
  RequestKeyframe: 0x05,
  Ack: 0x06,
  StreamConfig: 0x81,
  Video: 0x82,
  Pong: 0x83,
} as const;

export class DecodeError extends Error {}

class Writer {
  private buf: Uint8Array<ArrayBuffer> = new Uint8Array(64);
  private view = new DataView(this.buf.buffer);
  private len = 0;

  private reserve(n: number) {
    if (this.len + n <= this.buf.length) return;
    const bigger = new Uint8Array(Math.max(this.buf.length * 2, this.len + n));
    bigger.set(this.buf);
    this.buf = bigger;
    this.view = new DataView(bigger.buffer);
  }
  u8(v: number) {
    this.reserve(1);
    this.view.setUint8(this.len++, v);
  }
  i8(v: number) {
    this.reserve(1);
    this.view.setInt8(this.len++, v);
  }
  u16(v: number) {
    this.reserve(2);
    this.view.setUint16(this.len, v, true);
    this.len += 2;
  }
  u32(v: number) {
    this.reserve(4);
    this.view.setUint32(this.len, v, true);
    this.len += 4;
  }
  u64(v: bigint) {
    this.reserve(8);
    this.view.setBigUint64(this.len, v, true);
    this.len += 8;
  }
  bytes(b: Uint8Array) {
    this.reserve(b.length);
    this.buf.set(b, this.len);
    this.len += b.length;
  }
  str8(s: string) {
    let b = new TextEncoder().encode(s);
    if (b.length > 255) {
      let end = 255;
      while (end > 0 && (b[end] & 0xc0) === 0x80) end--; // don't split a UTF-8 sequence
      b = b.subarray(0, end);
    }
    this.u8(b.length);
    this.bytes(b);
  }
  finish(): Uint8Array<ArrayBuffer> {
    return this.buf.slice(0, this.len);
  }
}

class Reader {
  private buf: Uint8Array;
  private view: DataView;
  private pos = 0;
  constructor(buf: Uint8Array) {
    this.buf = buf;
    this.view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  }
  private need(n: number) {
    if (this.pos + n > this.buf.length) throw new DecodeError("message truncated");
  }
  u8() {
    this.need(1);
    return this.view.getUint8(this.pos++);
  }
  i8() {
    this.need(1);
    return this.view.getInt8(this.pos++);
  }
  u16() {
    this.need(2);
    const v = this.view.getUint16(this.pos, true);
    this.pos += 2;
    return v;
  }
  u32() {
    this.need(4);
    const v = this.view.getUint32(this.pos, true);
    this.pos += 4;
    return v;
  }
  u64() {
    this.need(8);
    const v = this.view.getBigUint64(this.pos, true);
    this.pos += 8;
    return v;
  }
  str8() {
    const n = this.u8();
    this.need(n);
    const s = new TextDecoder("utf-8", { fatal: true }).decode(this.buf.subarray(this.pos, this.pos + n));
    this.pos += n;
    return s;
  }
  rest() {
    const r = this.buf.subarray(this.pos);
    this.pos = this.buf.length;
    return r;
  }
}

export function encode(msg: Message): Uint8Array<ArrayBuffer> {
  const w = new Writer();
  switch (msg.type) {
    case "Hello": {
      const h = msg.hello;
      w.u8(Tag.Hello);
      w.u16(h.proto_ver);
      w.u8(h.client_kind);
      w.u32(h.features);
      w.u16(h.screen_w);
      w.u16(h.screen_h);
      w.u16(h.dpi);
      w.u8(h.codecs.length);
      h.codecs.forEach((c) => w.str8(c));
      break;
    }
    case "Pen": {
      const p = msg.pen;
      w.u8(Tag.Pen);
      w.u16(p.x);
      w.u16(p.y);
      w.u16(p.pressure);
      w.i8(p.tilt_x);
      w.i8(p.tilt_y);
      w.u8(p.tool);
      w.u8(p.buttons);
      w.u8((p.contact ? 1 : 0) | (p.in_range ? 2 : 0));
      break;
    }
    case "Ping":
      w.u8(Tag.Ping);
      w.u64(msg.t);
      break;
    case "RequestKeyframe":
      w.u8(Tag.RequestKeyframe);
      break;
    case "Ack":
      w.u8(Tag.Ack);
      w.u64(msg.pts_us);
      break;
    case "StreamConfig":
      w.u8(Tag.StreamConfig);
      w.u16(msg.config.width);
      w.u16(msg.config.height);
      w.str8(msg.config.codec);
      break;
    case "Video":
      w.u8(Tag.Video);
      w.u64(msg.video.pts_us);
      w.u8(msg.video.keyframe ? 1 : 0);
      w.bytes(msg.video.data);
      break;
    case "Pong":
      w.u8(Tag.Pong);
      w.u64(msg.t);
      break;
  }
  return w.finish();
}

/** Returns `null` for unknown tags, which receivers must ignore. */
export function decode(buf: Uint8Array): Message | null {
  if (buf.length === 0) throw new DecodeError("empty message");
  const r = new Reader(buf.subarray(1));
  switch (buf[0]) {
    case Tag.Hello: {
      const proto_ver = r.u16();
      const client_kind = r.u8();
      if (client_kind !== ClientKind.Web && client_kind !== ClientKind.Native)
        throw new DecodeError("invalid client_kind");
      const features = r.u32();
      const screen_w = r.u16();
      const screen_h = r.u16();
      const dpi = r.u16();
      const n = r.u8();
      const codecs = Array.from({ length: n }, () => r.str8());
      return { type: "Hello", hello: { proto_ver, client_kind, features, screen_w, screen_h, dpi, codecs } };
    }
    case Tag.Pen: {
      const x = r.u16();
      const y = r.u16();
      const pressure = r.u16();
      const tilt_x = r.i8();
      const tilt_y = r.i8();
      const tool = r.u8();
      if (tool !== Tool.Pen && tool !== Tool.Eraser) throw new DecodeError("invalid tool");
      const buttons = r.u8();
      const flags = r.u8();
      return {
        type: "Pen",
        pen: { x, y, pressure, tilt_x, tilt_y, tool, buttons, contact: (flags & 1) !== 0, in_range: (flags & 2) !== 0 },
      };
    }
    case Tag.Ping:
      return { type: "Ping", t: r.u64() };
    case Tag.RequestKeyframe:
      return { type: "RequestKeyframe" };
    case Tag.Ack:
      return { type: "Ack", pts_us: r.u64() };
    case Tag.StreamConfig:
      return { type: "StreamConfig", config: { width: r.u16(), height: r.u16(), codec: r.str8() } };
    case Tag.Video:
      return { type: "Video", video: { pts_us: r.u64(), keyframe: r.u8() !== 0, data: r.rest() } };
    case Tag.Pong:
      return { type: "Pong", t: r.u64() };
    default:
      return null;
  }
}
