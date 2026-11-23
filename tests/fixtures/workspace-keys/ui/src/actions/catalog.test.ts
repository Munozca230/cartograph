import { describe, expect, it } from "vitest";
import { find } from "./catalog";

describe("catalog", () => {
  it("finds an action", () => {
    expect(find("READ-FILE")?.label).toBe("Read a file");
  });
  it("lists only some actions here", () => {
    const keys = ["READ-FILE", "WRITE-FILE", "COPY-FILE", "DELETE-FILE"];
    expect(keys.length).toBe(4);
  });
});
