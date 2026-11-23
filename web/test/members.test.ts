import { describe, expect, it } from "vitest";
import type { WorkspaceMember } from "../src/api/types";
import { memberBadge, memberLine, membersByPath } from "../src/model/members";

const m = (name: string, extra: Partial<WorkspaceMember> = {}): WorkspaceMember => ({
  name,
  path: name,
  root: `/w/${name}`,
  git: "ok",
  branch: "main",
  head: "0123456789abcdef",
  detached: false,
  ...extra,
});

describe("workspace members", () => {
  it("are keyed by their folder with distinct hues", () => {
    const by = membersByPath({ workspace: { members: [m("api"), m("lib"), m("web")] } });
    expect([...by.keys()]).toEqual(["api", "lib", "web"]);
    const hues = [...by.values()].map((x) => x.hue);
    expect(new Set(hues).size).toBe(3);
    expect(hues.every((h) => h >= 0 && h < 360)).toBe(true);
  });

  it("are none for a single repo", () => {
    expect(membersByPath({}).size).toBe(0);
    expect(membersByPath(null).size).toBe(0);
  });

  it("say their branch, a detached head or no git", () => {
    expect(memberBadge(m("api"))).toBe("main");
    expect(memberBadge(m("api", { detached: true, branch: null }))).toBe("01234567");
    expect(memberBadge(m("api", { git: "no_git", branch: null, head: null }))).toBe("sin git");
    expect(memberLine(m("api", { path: "backend-api" }))).toBe("Repositorio api · carpeta backend-api · rama main");
    expect(memberLine(m("lib", { git: null, branch: null }))).toBe("Repositorio lib");
  });
});
