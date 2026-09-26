// Cleans up hover positions. With a palm resting on the screen, the tablet's
// hover position (sensed through the same capacitive panel) spikes by
// centimeters for a few samples and jitters by ~1 cm around the true position.
// Tip contact positions are unaffected and pass through untouched.
//
// Two stages, hover only:
// 1. Outlier rejection: a jump larger than HOVER_JUMP is only believed once
//    two more samples land near it.
// 2. One Euro filter (Casiez et al., CHI 2012): strong smoothing while the
//    pen moves slowly, little lag when it moves fast.

/** Largest hover step accepted without confirmation, in normalized units (~4% of the width). */
export const HOVER_JUMP = 2600;
/** Samples near a jump's target needed before believing it (the jump itself included). */
const CONFIRMATIONS = 3;

export type OneEuroParams = {
  /** Cutoff in Hz at rest; lower removes more jitter. */
  minCutoff: number;
  /** How fast the cutoff rises with speed (per normalized unit/s); higher means less lag. */
  beta: number;
  /** Cutoff in Hz for the speed estimate. */
  dCutoff: number;
};

export const HOVER_SMOOTHING: OneEuroParams = { minCutoff: 1.5, beta: 0.0004, dCutoff: 1.0 };

/** Coalesced samples can share a timestamp; treat them as this far apart. */
const MIN_DT = 0.004;

class OneEuro {
  private x: number | null = null;
  private dx = 0;
  private t = 0;
  private p: OneEuroParams;
  constructor(p: OneEuroParams) {
    this.p = p;
  }

  private static alpha(cutoff: number, dt: number) {
    const tau = 1 / (2 * Math.PI * cutoff);
    return 1 / (1 + tau / dt);
  }

  filter(x: number, t: number): number {
    if (this.x === null) {
      this.x = x;
      this.t = t;
      return x;
    }
    const dt = Math.max(t - this.t, MIN_DT);
    this.t = t;
    const rawDx = (x - this.x) / dt;
    this.dx += OneEuro.alpha(this.p.dCutoff, dt) * (rawDx - this.dx);
    const cutoff = this.p.minCutoff + this.p.beta * Math.abs(this.dx);
    this.x += OneEuro.alpha(cutoff, dt) * (x - this.x);
    return this.x;
  }

  reset() {
    this.x = null;
    this.dx = 0;
  }
}

type Point = { x: number; y: number };

export class HoverFilter {
  private last: Point | null = null;
  private pending: Point[] = [];
  private fx: OneEuro;
  private fy: OneEuro;

  constructor(smoothing: OneEuroParams = HOVER_SMOOTHING) {
    this.fx = new OneEuro(smoothing);
    this.fy = new OneEuro(smoothing);
  }

  /**
   * The position to send for this sample, or null to drop it.
   * `t` is the sample time in seconds.
   */
  filter(x: number, y: number, contact: boolean, t: number): Point | null {
    const p = { x, y };
    const near = (a: Point) => Math.hypot(a.x - x, a.y - y) <= HOVER_JUMP;
    if (contact) {
      // The tip position is reliable; start smoothing afresh from it.
      this.last = p;
      this.pending = [];
      this.resetSmoothing();
      return p;
    }
    if (!this.last) {
      this.last = p;
      this.resetSmoothing();
    } else if (near(this.last)) {
      this.last = p;
      this.pending = [];
    } else {
      const tail = this.pending.at(-1);
      this.pending = tail && near(tail) ? [...this.pending, p] : [p];
      if (this.pending.length < CONFIRMATIONS) return null;
      // A real move to a new place: jump there instead of gliding.
      this.last = p;
      this.pending = [];
      this.resetSmoothing();
    }
    return { x: Math.round(this.fx.filter(x, t)), y: Math.round(this.fy.filter(y, t)) };
  }

  reset() {
    this.last = null;
    this.pending = [];
    this.resetSmoothing();
  }

  private resetSmoothing() {
    this.fx.reset();
    this.fy.reset();
  }
}
