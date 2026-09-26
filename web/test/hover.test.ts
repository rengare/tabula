import { test } from "node:test";
import assert from "node:assert/strict";
import { HoverFilter } from "../src/hover.ts";

// Hover samples recorded with a palm on the screen: y spikes and decays back.
test("drops a spike and its decay", () => {
  const f = new HoverFilter();
  const ys = [9498, 17539, 13766, 9777, 9800];
  assert.deepEqual(ys.map((y) => f.accept(39000, y, false)), [true, false, false, true, true]);
});

test("drops oscillating noise", () => {
  const f = new HoverFilter();
  const pts = [[10012, 42336], [17049, 42368], [13686, 43118], [10400, 42400]];
  assert.deepEqual(pts.map(([x, y]) => f.accept(x, y, false)), [true, false, false, true]);
});

test("follows a real move made of small steps", () => {
  const f = new HoverFilter();
  for (let x = 0; x < 30000; x += 1500) assert.ok(f.accept(x, 5000, false));
});

test("accepts a jump confirmed by the next sample", () => {
  const f = new HoverFilter();
  assert.ok(f.accept(1000, 1000, false));
  assert.ok(!f.accept(30000, 30000, false));
  assert.ok(f.accept(30500, 30200, false));
});

test("never filters tip contact", () => {
  const f = new HoverFilter();
  assert.ok(f.accept(1000, 1000, false));
  assert.ok(f.accept(40000, 40000, true));
});
