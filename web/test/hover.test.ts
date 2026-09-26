import { test } from "node:test";
import assert from "node:assert/strict";
import { HoverFilter } from "../src/hover.ts";

const NO_SMOOTHING = { minCutoff: 1e9, beta: 0, dCutoff: 1 };
const run = (f: HoverFilter, pts: [number, number][], contact = false) =>
  pts.map(([x, y], i) => f.filter(x, y, contact, i * 0.01));

// Hover samples recorded with a palm on the screen: y spikes and decays back.
test("drops a spike and its decay", () => {
  const out = run(new HoverFilter(NO_SMOOTHING), [9498, 17539, 13766, 9777, 9800].map((y) => [39000, y]));
  assert.deepEqual(out.map((p) => p !== null), [true, false, false, true, true]);
});

test("drops a spike that lasts two samples", () => {
  const out = run(new HoverFilter(NO_SMOOTHING), [[8371, 1864], [15621, 2343], [15500, 2300], [9177, 2388]]);
  assert.deepEqual(out.map((p) => p !== null), [true, false, false, true]);
});

test("follows a real move made of small steps", () => {
  const f = new HoverFilter(NO_SMOOTHING);
  for (let x = 0; x < 30000; x += 1500) assert.ok(f.filter(x, 5000, false, x / 1e5));
});

test("jumps to a new place confirmed by three samples", () => {
  const out = run(new HoverFilter(NO_SMOOTHING), [[1000, 1000], [30000, 30000], [30300, 30100], [30500, 30200]]);
  assert.deepEqual(out.map((p) => p !== null), [true, false, false, true]);
  assert.deepEqual(out[3], { x: 30500, y: 30200 });
});

test("smooths jitter while hovering still", () => {
  const f = new HoverFilter();
  const jittery = Array.from({ length: 60 }, (_, i): [number, number] => [20000 + (i % 2 ? 1500 : -1500), 20000]);
  const out = run(f, jittery).filter((p) => p !== null);
  const tail = out.slice(-20).map((p) => p!.x);
  assert.ok(Math.max(...tail) - Math.min(...tail) < 600, `still jittering: ${Math.min(...tail)}..${Math.max(...tail)}`);
});

test("never filters or smooths tip contact", () => {
  const f = new HoverFilter();
  assert.ok(f.filter(1000, 1000, false, 0));
  assert.deepEqual(f.filter(40000, 40000, true, 0.01), { x: 40000, y: 40000 });
  assert.deepEqual(f.filter(40100, 40000, true, 0.02), { x: 40100, y: 40000 });
});
