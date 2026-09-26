import {
  Buttons,
  ClientKind,
  Features,
  PROTOCOL_VERSION,
  Tool,
  decode,
  encode,
  type Contact,
  type Message,
  type Pen,
  type StreamConfig,
} from "./protocol.ts";
import { HoverFilter } from "./hover.ts";

const params = new URLSearchParams(location.search);
/** `?mouse=1` treats mouse input as a pen, for testing on a desktop browser. */
const MOUSE_AS_PEN = params.has("mouse");

const canvas = document.getElementById("screen") as HTMLCanvasElement;
const ctx = canvas.getContext("2d", { alpha: false, desynchronized: true })!;
const overlay = document.getElementById("overlay")!;
const statusEl = document.getElementById("status")!;
const hud = document.getElementById("hud")!;
hud.hidden = !params.has("hud");

let ws: WebSocket | null = null;
let decoder: VideoDecoder | null = null;
let config: StreamConfig | null = null;
let waitingForKey = true;
let rttMs = 0;
let framesDrawn = 0;
/** Decode submit time per chunk timestamp, for the HUD's decode latency. */
const submitted = new Map<number, number>();
let decodeMs: number[] = [];
let drawMs: number[] = [];

function send(msg: Message) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(encode(msg));
}

function setStatus(text: string | null) {
  overlay.hidden = text === null && document.fullscreenElement !== null;
  statusEl.textContent = text ?? "";
}

// ---- video ---------------------------------------------------------------

function resetDecoder() {
  if (decoder && decoder.state !== "closed") decoder.close();
  decoder = null;
  waitingForKey = true;
  if (!config) return;
  const c = config;
  decoder = new VideoDecoder({
    output(frame) {
      const t1 = performance.now();
      const t0 = submitted.get(frame.timestamp);
      if (t0 !== undefined) {
        decodeMs.push(t1 - t0);
        submitted.delete(frame.timestamp);
      }
      ctx.drawImage(frame, 0, 0);
      drawMs.push(performance.now() - t1);
      send({ type: "Ack", pts_us: BigInt(frame.timestamp) });
      frame.close();
      framesDrawn++;
    },
    error(e) {
      console.warn("decoder error, resyncing", e);
      resetDecoder();
      send({ type: "RequestKeyframe" });
    },
  });
  // Annex-B input: no `description`, SPS/PPS arrive in-band with each keyframe.
  decoder.configure({ codec: c.codec, optimizeForLatency: true, codedWidth: c.width, codedHeight: c.height });
}

async function onStreamConfig(c: StreamConfig) {
  const support = await VideoDecoder.isConfigSupported({ codec: c.codec, optimizeForLatency: true });
  if (!support.supported) {
    setStatus(`This browser cannot decode ${c.codec}`);
    return;
  }
  config = c;
  canvas.width = c.width;
  canvas.height = c.height;
  resetDecoder();
  send({ type: "RequestKeyframe" });
  setStatus(null);
}

function onVideo(keyframe: boolean, pts: bigint, data: Uint8Array) {
  if (!decoder || decoder.state !== "configured") return;
  if (waitingForKey) {
    if (!keyframe) return;
    waitingForKey = false;
  }
  if (submitted.size > 120) submitted.clear(); // chunks the decoder dropped
  submitted.set(Number(pts), performance.now());
  decoder.decode(new EncodedVideoChunk({ type: keyframe ? "key" : "delta", timestamp: Number(pts), data }));
}

// ---- connection ------------------------------------------------------------

function connect() {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const sock = new WebSocket(`${scheme}://${location.host}/ws${location.search}`);
  sock.binaryType = "arraybuffer";
  ws = sock;
  setStatus("Connecting…");

  sock.onopen = () => {
    const dpr = window.devicePixelRatio || 1;
    send({
      type: "Hello",
      hello: {
        proto_ver: PROTOCOL_VERSION,
        client_kind: ClientKind.Web,
        features:
          Features.Pen |
          Features.Hover |
          Features.Eraser |
          Features.BarrelButton |
          Features.Tilt |
          Features.Touch |
          Features.Acks,
        screen_w: Math.round(screen.width * dpr),
        screen_h: Math.round(screen.height * dpr),
        dpi: Math.round(96 * dpr),
        codecs: ["avc1.42E034", "avc1.42E033", "avc1.42E02A"],
      },
    });
    setStatus("Waiting for video…");
  };
  sock.onmessage = (ev) => {
    const msg = decode(new Uint8Array(ev.data as ArrayBuffer));
    if (!msg) return;
    switch (msg.type) {
      case "StreamConfig":
        void onStreamConfig(msg.config);
        break;
      case "Video":
        onVideo(msg.video.keyframe, msg.video.pts_us, msg.video.data);
        break;
      case "Pong":
        rttMs = (performance.now() * 1000 - Number(msg.t)) / 1000;
        break;
    }
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    ws = null;
    config = null;
    resetDecoder();
    setStatus("Disconnected, retrying…");
    setTimeout(connect, 1000);
  };
}

setInterval(() => send({ type: "Ping", t: BigInt(Math.round(performance.now() * 1000)) }), 1000);

let lastFrames = 0;
setInterval(() => {
  const median = (v: number[]) => (v.length ? v.sort((a, b) => a - b)[Math.floor(v.length / 2)].toFixed(1) : "-");
  const queue = decoder?.decodeQueueSize ?? 0;
  hud.textContent = `${framesDrawn - lastFrames} fps · rtt ${rttMs.toFixed(1)} ms · decode ${median(decodeMs)} ms · draw ${median(drawMs)} ms · queue ${queue}`;
  decodeMs = [];
  drawMs = [];
  lastFrames = framesDrawn;
}, 1000);

// ---- pen input ---------------------------------------------------------------

/** Maps a client position to 0..65535 within the letterboxed video area. */
function normalize(clientX: number, clientY: number): [number, number] {
  const r = canvas.getBoundingClientRect();
  const scale = Math.min(r.width / canvas.width, r.height / canvas.height);
  const w = canvas.width * scale;
  const h = canvas.height * scale;
  const x = (clientX - r.left - (r.width - w) / 2) / w;
  const y = (clientY - r.top - (r.height - h) / 2) / h;
  const clamp = (v: number) => Math.round(Math.min(1, Math.max(0, v)) * 65535);
  return [clamp(x), clamp(y)];
}

function isPen(e: PointerEvent) {
  return e.pointerType === "pen" || (MOUSE_AS_PEN && e.pointerType === "mouse");
}

function penFrom(e: PointerEvent, inRange: boolean): Pen {
  const [x, y] = normalize(e.clientX, e.clientY);
  // Pointer Events: buttons bit 5 (32) is the eraser, bit 1 (2) the barrel button.
  const eraser = (e.buttons & 32) !== 0 || e.button === 5;
  const contact = inRange && (e.buttons & 1) !== 0;
  const pressure = e.pointerType === "mouse" ? (contact ? 0.5 : 0) : e.pressure;
  return {
    x,
    y,
    pressure: Math.round(Math.min(1, Math.max(0, pressure)) * 65535),
    tilt_x: Math.round(e.tiltX || 0),
    tilt_y: Math.round(e.tiltY || 0),
    tool: eraser ? Tool.Eraser : Tool.Pen,
    buttons: (e.buttons & 2) !== 0 ? Buttons.Barrel : 0,
    contact: contact || (eraser && inRange && e.pressure > 0),
    in_range: inRange,
  };
}

// ---- touch -------------------------------------------------------------------

/** Touch is ignored this long after the last pen event, so a resting palm doesn't click. */
const PALM_GRACE_MS = 300;
/**
 * Not every browser sends pointerleave when a hovering pen leaves range, so
 * range is derived from how recently the pen was seen instead: a pen in
 * range keeps producing hover moves.
 */
const PEN_RANGE_TIMEOUT_MS = 1000;
let penInRange = false;
let penContact = false;
let penLastSeen = -Infinity;
let lastPen: Pen | null = null;
/** Fingers currently down, by pointerId. */
const touches = new Map<number, Contact>();

// "pen": fingers are ignored entirely (rest your hand on the screen while drawing).
// "touch": fingers act as a touchscreen, with palm rejection while the pen is near.
type InputMode = "pen" | "touch";
const modeButtons = {
  pen: document.getElementById("mode-pen")!,
  touch: document.getElementById("mode-touch")!,
};
let inputMode: InputMode = "touch";

function setInputMode(mode: InputMode) {
  inputMode = mode;
  for (const [m, button] of Object.entries(modeButtons)) button.setAttribute("aria-pressed", String(m === mode));
  if (mode === "pen") cancelTouches();
  try {
    localStorage.setItem("tabula.inputMode", mode);
  } catch {
    /* storage unavailable; the choice just isn't remembered */
  }
}

for (const [mode, button] of Object.entries(modeButtons)) {
  button.addEventListener("click", () => setInputMode(mode as InputMode));
}
try {
  const saved = localStorage.getItem("tabula.inputMode");
  if (saved === "pen" || saved === "touch") inputMode = saved;
} catch {
  /* default mode */
}

function palmRejected() {
  return inputMode === "pen" || penContact || performance.now() - penLastSeen < PALM_GRACE_MS;
}

/** Takes a pen that went quiet out of proximity, so the desktop doesn't keep it forever. */
setInterval(() => {
  if (penInRange && !penContact && lastPen && performance.now() - penLastSeen > PEN_RANGE_TIMEOUT_MS) {
    penInRange = false;
    hoverFilter.reset();
    send({ type: "Pen", pen: { ...lastPen, contact: false, pressure: 0, in_range: false } });
  }
}, 250);

function sendTouches() {
  send({ type: "Touch", contacts: [...touches.values()] });
}

function freeTouchId(): number | undefined {
  const used = new Set([...touches.values()].map((c) => c.id));
  for (let id = 0; id < 255; id++) if (!used.has(id)) return id;
  return undefined;
}

function onTouch(e: PointerEvent) {
  e.preventDefault();
  const known = touches.get(e.pointerId);
  switch (e.type) {
    case "pointerdown": {
      if (palmRejected()) return;
      const id = freeTouchId();
      if (id === undefined) return;
      canvas.setPointerCapture(e.pointerId);
      const [x, y] = normalize(e.clientX, e.clientY);
      touches.set(e.pointerId, { id, x, y });
      break;
    }
    case "pointermove": {
      if (!known) return;
      [known.x, known.y] = normalize(e.clientX, e.clientY);
      break;
    }
    default: // up, cancel, leave
      if (!known) return;
      touches.delete(e.pointerId);
  }
  sendTouches();
}

function cancelTouches() {
  if (touches.size === 0) return;
  touches.clear();
  sendTouches();
}

// ---- pen & dispatch -------------------------------------------------------------

const hoverFilter = new HoverFilter();
/**
 * Chrome reports the pen leaving range right after every lift and entering
 * again ~10 ms later; a short grace period keeps that from reaching the
 * desktop as a proximity out/in on every stroke.
 */
const LEAVE_GRACE_MS = 80;
let leaveTimer: ReturnType<typeof setTimeout> | undefined;

function onPointer(e: PointerEvent) {
  if (e.pointerType === "touch") return onTouch(e);
  if (!isPen(e)) return;
  e.preventDefault();
  const inRange = e.type !== "pointerleave" && e.type !== "pointercancel" && e.type !== "pointerout";
  penLastSeen = performance.now();
  if (!inRange) {
    if (penInRange && leaveTimer === undefined) {
      leaveTimer = setTimeout(() => {
        leaveTimer = undefined;
        penInRange = false;
        penContact = false;
        hoverFilter.reset();
        if (lastPen) send({ type: "Pen", pen: { ...lastPen, contact: false, pressure: 0, in_range: false } });
      }, LEAVE_GRACE_MS);
    }
    return;
  }
  clearTimeout(leaveTimer);
  leaveTimer = undefined;
  if (!penInRange) cancelTouches();
  penInRange = true;
  if (e.type === "pointerdown") canvas.setPointerCapture(e.pointerId);
  const samples = e.type === "pointermove" && e.getCoalescedEvents ? e.getCoalescedEvents() : [e];
  for (const s of samples.length ? samples : [e]) {
    const pen = penFrom(s, true);
    // Button and contact changes always go through; hover glitches don't.
    const changed = !lastPen || pen.contact !== lastPen.contact || pen.buttons !== lastPen.buttons || pen.tool !== lastPen.tool;
    if (!hoverFilter.accept(pen.x, pen.y, pen.contact) && !changed) continue;
    lastPen = pen;
    send({ type: "Pen", pen });
  }
  penContact = lastPen?.contact ?? false;
}

for (const type of ["pointerdown", "pointermove", "pointerup", "pointercancel", "pointerleave"]) {
  canvas.addEventListener(type, onPointer as EventListener, { passive: false });
}
canvas.addEventListener("contextmenu", (e) => e.preventDefault());

// ---- fullscreen & wake lock -------------------------------------------------------

document.getElementById("start")!.addEventListener("click", async () => {
  try {
    await document.documentElement.requestFullscreen({ navigationUI: "hide" });
  } catch {
    /* not allowed, e.g. desktop iframe; keep going windowed */
  }
  try {
    await navigator.wakeLock?.request("screen");
  } catch {
    /* optional */
  }
  if (config) setStatus(null);
  overlay.hidden = config !== null;
});

document.addEventListener("fullscreenchange", () => {
  if (!document.fullscreenElement && config) {
    statusEl.textContent = "";
    overlay.hidden = false;
  }
});

setInputMode(inputMode);

if (!("VideoDecoder" in window)) {
  setStatus("This browser has no WebCodecs support (needs Chrome/Edge 94+, Firefox 130+, or a secure origin)");
} else {
  connect();
}
