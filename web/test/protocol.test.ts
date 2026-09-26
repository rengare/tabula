// Decodes and re-encodes the golden fixtures written by crates/protocol, so
// the TypeScript implementation cannot drift from the Rust one.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ClientKind, Features, Tool, Buttons, PROTOCOL_VERSION, decode, encode, type Message } from "../src/protocol.ts";

const expected: Record<string, Message> = {
  hello: {
    type: "Hello",
    hello: {
      proto_ver: PROTOCOL_VERSION,
      client_kind: ClientKind.Web,
      features: Features.Pen | Features.Hover | Features.Tilt,
      screen_w: 2560,
      screen_h: 1600,
      dpi: 274,
      codecs: ["avc1.42E01F", "avc1.42E033"],
    },
  },
  pen: {
    type: "Pen",
    pen: {
      x: 32768,
      y: 1000,
      pressure: 40000,
      tilt_x: -30,
      tilt_y: 45,
      tool: Tool.Eraser,
      buttons: Buttons.Barrel,
      contact: true,
      in_range: true,
    },
  },
  ping: { type: "Ping", t: 0x0102030405060708n },
  request_keyframe: { type: "RequestKeyframe" },
  stream_config: { type: "StreamConfig", config: { width: 1920, height: 1200, codec: "avc1.42E033" } },
  video: { type: "Video", video: { pts_us: 16667n, keyframe: true, data: new Uint8Array([0, 0, 0, 1, 0x67, 0x42]) } },
  pong: { type: "Pong", t: 42n },
};

function fixture(name: string): Uint8Array {
  const hex = readFileSync(new URL(`../../fixtures/${name}.hex`, import.meta.url), "utf8").trim();
  return Uint8Array.from(hex.match(/../g)!.map((b) => parseInt(b, 16)));
}

for (const [name, msg] of Object.entries(expected)) {
  test(`fixture ${name}`, () => {
    const bytes = fixture(name);
    assert.deepEqual(decode(bytes), msg);
    assert.deepEqual(encode(msg), bytes);
  });
}

test("unknown tags are ignored", () => {
  assert.equal(decode(new Uint8Array([0x7f, 1, 2])), null);
});

test("truncated messages throw", () => {
  assert.throws(() => decode(new Uint8Array([0x04, 1, 2])));
});
