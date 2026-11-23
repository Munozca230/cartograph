import { describe, expect, it } from "vitest";
import { formatLoc, locUnit, SPRING_VIA, viaLabel, viaTip } from "../src/ui/format";

describe("formatLoc", () => {
  it("says thousands as people say them", () => {
    expect(formatLoc(840)).toBe("840");
    expect(formatLoc(1300)).toBe("1,3 mil");
    expect(formatLoc(191_500)).toBe("191,5 mil");
  });

  it("says millions in millions, not in thousands", () => {
    expect(formatLoc(1_500_000)).toBe("1,5 millones");
    expect(formatLoc(12_000_000)).toBe("12 millones");
    expect(formatLoc(1_000_000)).toBe("1 millón");
    expect(formatLoc(1_040_000)).toBe("1 millón");
    // Rounds to 1000 thousands: that is a million.
    expect(formatLoc(999_960)).toBe("1 millón");
  });

  it("puts 'de' between millions and the unit", () => {
    expect(`${formatLoc(1_500_000)} ${locUnit(1_500_000)}`).toBe("1,5 millones de líneas");
    expect(`${formatLoc(1300)} ${locUnit(1300)}`).toBe("1,3 mil líneas");
  });
});

describe("viaLabel and viaTip", () => {
  it("say a Spring bean in Spanish and keep a bridge's key", () => {
    expect(viaLabel(SPRING_VIA)).toBe("bean de Spring");
    expect(viaTip(SPRING_VIA)).toContain("Bean de Spring");
    expect(viaLabel("ipc invoke users:get")).toBe("ipc invoke users:get");
    expect(viaTip("ipc invoke users:get")).toBe("Puente, no una llamada: ipc invoke users:get");
    expect(viaLabel("x".repeat(40))).toHaveLength(36);
  });
});
