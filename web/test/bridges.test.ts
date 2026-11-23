// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import type { Rect } from "../src/model/plan";
import { bridgeLines } from "../src/ui/PlanView";

const cards = new Map<string, Rect>([
  ["file:src/renderer/App.tsx", { x: 0, y: 0, w: 100, h: 40 }],
  ["file:src/preload/index.ts", { x: 300, y: 0, w: 100, h: 40 }],
  ["file:src/main/ipc.ts", { x: 300, y: 200, w: 100, h: 40 }],
]);

describe("bridges over the plan", () => {
  it("draws a curved line from card border to card border, and skips what is not a card", () => {
    const lines = bridgeLines(
      [
        { src: "file:src/renderer/App.tsx", dst: "file:src/preload/index.ts", weight: 2 },
        { src: "file:src/preload/index.ts", dst: "file:src/main/ipc.ts", weight: 1 },
        { src: "file:src/preload/index.ts", dst: "dir:src/hidden", weight: 1 },
        { src: "file:src/main/ipc.ts", dst: "file:src/main/ipc.ts", weight: 1 },
      ],
      cards,
    );
    expect(lines.map((l) => l.key)).toEqual(["file:src/renderer/App.tsx=>file:src/preload/index.ts", "file:src/preload/index.ts=>file:src/main/ipc.ts"]);
    // Leaves the first card on its right border and reaches the second on its left one.
    expect(lines[0]!.d.startsWith("M100,20 Q")).toBe(true);
    expect(lines[0]!.d.endsWith(" 300,20")).toBe(true);
    expect(lines[0]!.title).toContain("2 puentes");
    // Straight down: bottom border to top border.
    expect(lines[1]!.d.startsWith("M350,40 Q")).toBe(true);
    expect(lines[1]!.d.endsWith(" 350,200")).toBe(true);
  });
});
