// Rejects hover position glitches. With a palm resting on the screen, the
// tablet's hover position (sensed through the same capacitive panel) spikes
// by centimeters for a few samples and snaps back. Tip contact positions are
// unaffected, so only hover samples are filtered.

/** Largest hover step accepted without confirmation, in normalized units (~4% of the width). */
export const HOVER_JUMP = 2600;

export class HoverFilter {
  private last: { x: number; y: number } | null = null;
  private pending: { x: number; y: number } | null = null;

  /** Whether to send this sample. */
  accept(x: number, y: number, contact: boolean): boolean {
    const p = { x, y };
    const near = (a: { x: number; y: number }) => Math.hypot(a.x - x, a.y - y) <= HOVER_JUMP;
    // A big jump is only believed once the next sample lands near it too;
    // single spikes and their decay never get there.
    if (contact || !this.last || near(this.last) || (this.pending && near(this.pending))) {
      this.last = p;
      this.pending = null;
      return true;
    }
    this.pending = p;
    return false;
  }

  reset() {
    this.last = null;
    this.pending = null;
  }
}
