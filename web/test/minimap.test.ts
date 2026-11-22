import { describe, expect, it } from "vitest";
import { minimapFrame, minimapToPlan } from "../src/ui/planGeometry";

describe("minimap", () => {
  it("maps a click to the plan as drawn, also when a thin plan clamps the minimap's size", () => {
    for (const [width, height] of [
      [2000, 10], // wide and flat: the height is clamped to 36 px
      [10, 2000], // tall and narrow: the width is clamped to 48 px
      [1600, 900],
    ] as const) {
      const f = minimapFrame(width, height);
      expect(f.w).toBeGreaterThanOrEqual(48);
      expect(f.h).toBeGreaterThanOrEqual(36);
      // The middle of the minimap is the middle of the plan.
      const mid = minimapToPlan(f, f.w / 2, f.h / 2);
      expect(mid.x).toBeCloseTo(width / 2, 6);
      expect(mid.y).toBeCloseTo(height / 2, 6);
      // The plan's corners land inside the minimap, where `meet` draws them.
      const lw = width + 2 * f.pad;
      const lh = height + 2 * f.pad;
      expect(f.ox * 2 + lw * f.scale).toBeCloseTo(f.w, 6);
      expect(f.oy * 2 + lh * f.scale).toBeCloseTo(f.h, 6);
      const origin = minimapToPlan(f, f.ox + f.pad * f.scale, f.oy + f.pad * f.scale);
      expect(origin.x).toBeCloseTo(0, 6);
      expect(origin.y).toBeCloseTo(0, 6);
    }
  });
});
