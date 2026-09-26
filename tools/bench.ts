// End-to-end benchmark without a tablet: connects like the web client,
// hovers the virtual pen in a circle (in range, never touching, so nothing
// gets clicked) and counts the video frames that come back. Run the server
// with `--stats` to see per-stage latencies for the same run.
//
//   node tools/bench.ts [seconds] [ws://localhost:7543/ws]
//
// Note: this moves the real cursor while it runs.
import { ClientKind, Features, PROTOCOL_VERSION, Tool, decode, encode } from "../web/src/protocol.ts";

const seconds = Number(process.argv[2] ?? 10);
const url = process.argv[3] ?? "ws://localhost:7543/ws";
const PEN_HZ = 120;

const ws = new WebSocket(url);
ws.binaryType = "arraybuffer";
let frames = 0;
let bytes = 0;
let perSecond: number[] = [];
let lastCount = 0;
const rtts: number[] = [];

ws.onmessage = (ev) => {
  const m = decode(new Uint8Array(ev.data as ArrayBuffer));
  if (m?.type === "Video") {
    frames++;
    bytes += m.video.data.length;
  } else if (m?.type === "Pong") {
    rtts.push(performance.now() - Number(m.t) / 1000);
  }
};

ws.onopen = () => {
  ws.send(encode({
    type: "Hello",
    hello: {
      proto_ver: PROTOCOL_VERSION,
      client_kind: ClientKind.Web,
      features: Features.Pen | Features.Hover,
      screen_w: 2560,
      screen_h: 1600,
      dpi: 254,
      codecs: ["avc1.42E033"],
    },
  }));
  const start = performance.now();
  const pen = setInterval(() => {
    const t = (performance.now() - start) / 1000;
    const a = t * Math.PI; // half a turn per second
    ws.send(encode({
      type: "Pen",
      pen: {
        x: Math.round(32768 + 12000 * Math.cos(a)),
        y: Math.round(32768 + 12000 * Math.sin(a)),
        pressure: 0,
        tilt_x: 0,
        tilt_y: 0,
        tool: Tool.Pen,
        buttons: 0,
        contact: false,
        in_range: true,
      },
    }));
  }, 1000 / PEN_HZ);
  const tick = setInterval(() => {
    perSecond.push(frames - lastCount);
    lastCount = frames;
    ws.send(encode({ type: "Ping", t: BigInt(Math.round(performance.now() * 1000)) }));
  }, 1000);
  setTimeout(() => {
    clearInterval(pen);
    clearInterval(tick);
    ws.send(encode({
      type: "Pen",
      pen: { x: 0, y: 0, pressure: 0, tilt_x: 0, tilt_y: 0, tool: Tool.Pen, buttons: 0, contact: false, in_range: false },
    }));
    const steady = perSecond.slice(1); // first second includes startup
    const avg = steady.reduce((a, b) => a + b, 0) / Math.max(1, steady.length);
    rtts.sort((a, b) => a - b);
    console.log(`fps per second: ${perSecond.join(" ")}`);
    console.log(`avg ${avg.toFixed(1)} fps, min ${Math.min(...steady)}, ${(bytes * 8 / seconds / 1e6).toFixed(2)} Mbit/s`);
    if (rtts.length) console.log(`ws round trip p50 ${rtts[Math.floor(rtts.length / 2)].toFixed(1)} ms`);
    setTimeout(() => process.exit(0), 200);
  }, seconds * 1000);
};
