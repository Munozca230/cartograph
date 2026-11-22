import { describe, expect, it } from "vitest";
import type { TreeNode } from "../src/api/types";
import { buildHierarchy } from "../src/model/hierarchy";
import { dirKindByName, dominantKind, emptyMix, fileKind, kindOf, kindsOf } from "../src/model/kind";

describe("fileKind", () => {
  it("tells code, tests, scripts, styles and the rest apart", () => {
    const cases: [string, string][] = [
      ["src/app/page.tsx", "code"],
      ["src/lib/db.ts", "code"],
      ["main.go", "code"],
      ["crates/core/src/lib.rs", "code"],
      ["game/player.gd", "code"],
      ["game/level.tscn", "code"],
      ["src/app/page.test.tsx", "test"],
      ["src/app/page.spec.ts", "test"],
      ["pkg/server_test.go", "test"],
      ["app/test_models.py", "test"],
      ["src/main/java/AppTest.java", "test"],
      ["tests/fixtures/data.json", "test"],
      ["src/__tests__/a.ts", "test"],
      ["scripts/release.mjs", "script"],
      ["tools/gen.py", "script"],
      ["bin/run", "script"],
      ["deploy.sh", "script"],
      ["ci/build.ps1", "script"],
      ["Makefile", "script"],
      ["build.mjs", "script"],
      ["src/ui/App.css", "style"],
      ["styles/theme.scss", "style"],
      ["package.json", "data"],
      ["README.md", "data"],
      [".github/workflows/ci.yml", "data"],
      ["vite.config.ts", "data"],
      [".eslintrc.js", "data"],
      ["src/assets/logo.svg", "data"],
      ["LICENSE", "data"],
    ];
    for (const [path, kind] of cases) expect([path, fileKind(path)]).toEqual([path, kind]);
  });

  it("matches folder names exactly, never as part of a longer name", () => {
    expect(fileKind("src/testing-utils/x.ts")).toBe("code");
    expect(fileKind("src/latest/x.ts")).toBe("code");
    expect(fileKind("src/descripts/x.ts")).toBe("code");
    expect(fileKind("src/Arc.ts")).toBe("code");
    expect(fileKind("src/rc.ts")).toBe("code");
  });

  it("treats spec/ folders as tests only for code files", () => {
    expect(fileKind("spec/models/user_spec.rb")).toBe("test");
    expect(fileKind("spec/helpers/setup.ts")).toBe("test");
    expect(fileKind("specs/api.js")).toBe("test");
    expect(fileKind("spec/openapi.yaml")).toBe("data");
    expect(fileKind("docs/specs/design.md")).toBe("data");
    expect(fileKind("spec/jest.config.js")).toBe("data");
    // The other test folders keep everything inside them, fixtures included.
    expect(fileKind("tests/fixtures/data.json")).toBe("test");
  });

  it("treats test support folders inside spec/ as tests, whatever the file", () => {
    expect(fileKind("spec/fixtures/user.json")).toBe("test");
    expect(fileKind("spec/support/schema.yaml")).toBe("test");
    expect(fileKind("specs/mocks/api.json")).toBe("test");
    expect(fileKind("spec/__snapshots__/a.snap")).toBe("test");
    expect(fileKind("src/__snapshots__/App.test.tsx.snap")).toBe("test");
    // Support folders outside spec or test folders are not tests.
    expect(fileKind("src/fixtures/seed.json")).toBe("data");
  });

  it("takes a Java/Kotlin/C# integration test by its IT suffix, not any name ending in IT", () => {
    expect(fileKind("src/main/java/OrderServiceIT.java")).toBe("test");
    expect(fileKind("src/main/kotlin/Api2IT.kt")).toBe("test");
    expect(fileKind("src/main/java/OrderDAOIT.java")).toBe("test");
    expect(fileKind("src/main/kotlin/RestAPIIT.kt")).toBe("test");
    expect(fileKind("src/main/java/AUDIT.java")).toBe("code");
    expect(fileKind("src/main/java/EDIT.cs")).toBe("code");
    expect(fileKind("src/main/java/BIT.cs")).toBe("code");
    expect(fileKind("src/main/java/IT.java")).toBe("code");
    expect(fileKind("src/main/java/UserServiceIT.java")).toBe("test");
    expect(fileKind("src/main/java/S3IT.java")).toBe("test");
    // A word that ends in IT is no IT suffix.
    expect(fileKind("src/main/java/OrderAUDIT.java")).toBe("code");
    expect(fileKind("src/main/java/OrderEDIT.kt")).toBe("code");
  });

  it("keeps bin/ and tools/ inside a source tree as code", () => {
    expect(fileKind("src/bin/main.rs")).toBe("code");
    expect(fileKind("src/main/mcp/tools/search.ts")).toBe("code");
    expect(fileKind("tools/lint.ts")).toBe("script");
  });

  it("puts tests before scripts and scripts before the extension", () => {
    expect(fileKind("scripts/check.test.mjs")).toBe("test");
    expect(fileKind("src/test/unit/scripts/a.test.ts")).toBe("test");
    expect(fileKind("scripts/theme.css")).toBe("script");
    expect(fileKind("scripts/README.md")).toBe("script");
    // Godot's code folder is `scripts/`.
    expect(fileKind("scripts/view/dig_hud.gd")).toBe("code");
    expect(fileKind("tools/capture.gd")).toBe("script");
  });

  it("is case-insensitive on folder names and extensions", () => {
    expect(fileKind("Tests/a.ts")).toBe("test");
    expect(fileKind("src/App.CSS")).toBe("style");
  });
});

describe("folder kinds", () => {
  const file = (path: string): TreeNode => ({ id: `file:${path}`, kind: "file", name: path.slice(path.lastIndexOf("/") + 1), path, loc: 10, fan_in: 0, fan_out: 0, unresolved: 0 });
  const dir = (path: string, children: TreeNode[]): TreeNode => ({ id: path ? `dir:${path}` : "repo:", kind: path ? "dir" : "repo", name: path.slice(path.lastIndexOf("/") + 1) || "repo", path, loc: 0, fan_in: 0, fan_out: 0, unresolved: 0, children });
  const tree = dir("", [
    dir("src", [
      dir("src/ui", [file("src/ui/a.tsx"), file("src/ui/b.tsx"), file("src/ui/a.css")]),
      dir("src/shared", [file("src/shared/x.ts")]),
      dir("src/test", [file("src/test/a.test.ts"), file("src/test/b.test.ts"), file("src/test/c.test.ts"), file("src/test/d.test.ts"), file("src/test/e.test.ts"), file("src/test/f.test.ts")]),
    ]),
    dir("docs", [file("docs/a.md"), file("docs/b.md"), file("docs/run.sh")]),
    dir("scripts", [file("scripts/a.mjs")]),
    dir("tied", [file("tied/a.css"), file("tied/b.json")]),
    dir("tied2", [dir("tied2/s", [file("tied2/s/a.css"), file("tied2/s/b.css")]), file("tied2/c.json")]),
    dir("empty", []),
    dir("tests", []),
    dir("apispec", [dir("apispec/spec", [file("apispec/spec/openapi.yaml")])]),
    dir("rb", [dir("rb/spec", [file("rb/spec/user_spec.rb"), file("rb/spec/order_spec.rb")])]),
  ]);
  const h = buildHierarchy(tree);
  const k = kindsOf(h);

  it("gives every file and folder a kind", () => {
    for (const id of h.nodes.keys()) expect(k.kind.has(id)).toBe(true);
  });

  it("decides a folder by what it shows when opened, not by its biggest subfolder", () => {
    // src/test has more files than the rest of src together, but src shows two code folders and one of tests.
    expect(k.kind.get("dir:src")).toBe("code");
    expect(k.mix.get("dir:src")).toEqual({ code: 3, test: 6, script: 0, style: 1, data: 0 });
    expect(k.kind.get("dir:src/test")).toBe("test");
    expect(k.kind.get("dir:docs")).toBe("data");
    expect(k.kind.get("dir:scripts")).toBe("script");
  });

  it("does not let config and data files outvote the code they sit next to", () => {
    const cfg = ["package.json", "package-lock.json", "tsconfig.json", "README.md", "DESIGN.md", "vite.config.ts", "vitest.config.ts", "eslint.config.js"];
    const src = Array.from({ length: 12 }, (_, i) => file(`web/src/m${i}.ts`));
    const web = buildHierarchy(
      dir("", [dir("web", [...cfg.map((f) => file(`web/${f}`)), dir("web/src", src), dir("web/test", [file("web/test/a.test.ts")])])]),
    );
    expect(kindsOf(web).kind.get("dir:web")).toBe("code");
    // Mostly data underneath: still data.
    expect(k.kind.get("dir:docs")).toBe("data");
  });

  it("breaks a tie by the files underneath, then by the order of the kinds", () => {
    expect(k.kind.get("dir:tied")).toBe("style");
    expect(k.kind.get("dir:tied2")).toBe("style");
  });

  it("names an empty folder by its own name", () => {
    expect(k.kind.get("dir:empty")).toBe("code");
    expect(k.kind.get("dir:tests")).toBe("test");
    expect(dirKindByName("a/scripts")).toBe("script");
  });

  it("decides a spec folder by its files, as for any folder; by name alone, as a code file inside it would be", () => {
    // A spec folder holding a written spec is data, like the file.
    expect(k.kind.get("dir:apispec/spec")).toBe("data");
    expect(k.kind.get("dir:rb/spec")).toBe("test");
    expect(dirKindByName("spec")).toBe(fileKind("spec/a.rb"));
    expect(dirKindByName("spec/fixtures")).toBe("test");
    expect(dirKindByName("src/fixtures")).toBe("code");
    expect(dirKindByName("tools")).toBe("script");
  });

  it("falls back to the path for ids the tree doesn't know", () => {
    expect(kindOf(k, "file:new/x.test.ts", "new/x.test.ts", false)).toBe("test");
    expect(kindOf(null, "dir:scripts", "scripts", true)).toBe("script");
    expect(kindOf(k, "dir:src", "src", true)).toBe("code");
  });

  it("picks the dominant kind of a mix, code on an empty one", () => {
    expect(dominantKind(emptyMix())).toBe("code");
    expect(dominantKind({ ...emptyMix(), data: 2, test: 2 })).toBe("test");
  });
});
