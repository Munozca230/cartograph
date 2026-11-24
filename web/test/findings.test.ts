import { describe, expect, it } from "vitest";
import type { Finding, GraphResponse } from "../src/api/types";
import { findingLabel, findingLines, findingsOnPlan, holderOf, isProblem, placeOf, problemCount } from "../src/model/findings";
import { buildPlan } from "../src/model/plan";
import { initialState, store } from "../src/state/store";
import { findingsFor, issuesOf, problemsOf } from "../src/state/plan";

/** A /api/graph body of dirs from [src, dst, weight] triples (parents from the paths). */
function graph(edges: [string, string, number][]): GraphResponse {
  const ids = [...new Set(edges.flatMap(([s, d]) => [s, d]))].map((p) => `dir:${p}`);
  return {
    generation: 1,
    nodes: ids.map((id) => {
      const path = id.slice(4);
      const i = path.lastIndexOf("/");
      return {
        id,
        kind: "dir" as const,
        name: path.slice(i + 1),
        path,
        parent: i < 0 ? "repo:" : `dir:${path.slice(0, i)}`,
        loc: 100,
        files: 3,
        langs: { typescript: 3 },
        fan_in: 0,
        fan_out: 0,
        unresolved: 0,
        expandable: true,
      };
    }),
    edges: edges.map(([s, d, w]) => ({ src: `dir:${s}`, dst: `dir:${d}`, weight: w, confidence: { exact: 0, resolved: w, heuristic: 0 } })),
    externals: [],
    ignored: [],
  };
}

function finding(over: Partial<Finding>): Finding {
  return {
    id: "cycle:file:src/ui/a.ts",
    kind: "cycle",
    severity: "warning",
    violation: false,
    title: "x",
    nodes: [],
    edges: [],
    evidence: [],
    metrics: {},
    ...over,
  };
}

const g = graph([
  ["src/ui", "src/core", 5],
  ["src/core", "src/infra", 2],
  ["src/infra", "src/core", 1],
]);

describe("findings on the plan", () => {
  it("lands each location on the card that holds it", () => {
    const plan = buildPlan(g);
    const where = new Set(plan.nodes.map((n) => n.id));
    expect(holderOf(where, "file:src/ui/deep/a.ts")).toBe("dir:src/ui");
    expect(holderOf(where, "dir:src/core")).toBe("dir:src/core");
    expect(holderOf(where, "file:README.md")).toBeNull();
  });

  it("marks the drawn edges a problem finding proves, never info ones", () => {
    const plan = buildPlan(g);
    const layer = finding({
      id: "layer_violation:ui->infra",
      kind: "layer_violation",
      severity: "error",
      violation: true,
      nodes: ["file:src/ui/app.ts"],
      edges: [{ src: "file:src/ui/app.ts", dst: "file:src/core/x.ts", line: 3 }],
      metrics: { imports: 1 },
    });
    const hub = finding({ id: "hub:file:src/core/x.ts", kind: "hub", severity: "info", nodes: ["file:src/core/x.ts"], edges: [{ src: "file:src/ui/a.ts", dst: "file:src/core/x.ts" }], metrics: { share: 0.123 } });
    const on = findingsOnPlan(plan, [layer, hub]);
    const key = plan.edges.find((e) => e.src === "dir:src/ui" && e.dst === "dir:src/core")!.key;
    expect(on.byEdge.get(key)).toEqual([layer]);
    expect(on.byId.get("dir:src/ui")).toEqual([layer]);
    expect(on.byId.get("dir:src/core")).toEqual([hub]);
    // An edge inside one card proves nothing on this view.
    const inside = findingsOnPlan(plan, [finding({ edges: [{ src: "file:src/ui/a.ts", dst: "file:src/ui/b.ts" }] })]);
    expect(inside.byEdge.size).toBe(0);
  });

  it("feeds the dots and the pill with problems only, apart from indexing issues", () => {
    store.set({
      ...initialState,
      graph: g,
      findings: {
        generation: 1,
        findings: [
          finding({ nodes: ["file:src/ui/a.ts", "file:src/ui/b.ts"], metrics: { files: 2 } }),
          finding({ id: "hub:file:src/core/x.ts", kind: "hub", severity: "info", nodes: ["file:src/core/x.ts"], metrics: { share: 0.2 } }),
        ],
        summary: { total: 2, violations: 0, by_severity: {}, by_kind: {} },
        config: { origin: null, error: null },
        total: 2,
        truncated: false,
      },
    });
    const s = store.get();
    // One finding with two locations on the same card counts once.
    expect(problemsOf(s).get("dir:src/ui")).toBe(1);
    expect(problemsOf(s).get("dir:src/core")).toBeUndefined();
    expect(issuesOf(s).get("dir:src/ui")).toBeUndefined();
    expect(findingsFor(s).byId.get("dir:src/core")?.length).toBe(1);
    store.set(initialState);
  });

  it("counts problems from the summary, not from the findings that came back", () => {
    const resp = {
      generation: 1,
      findings: [finding({}), finding({ id: "hub:x", severity: "info" })],
      summary: { total: 4012, violations: 3, by_severity: { error: 3, warning: 4000, info: 9 }, by_kind: {} },
      config: { origin: null, error: null },
      total: 4012,
      truncated: true,
    };
    expect(problemCount(resp)).toBe(4003);
    expect(problemCount({ ...resp, summary: { ...resp.summary, by_severity: {} } })).toBe(0);
  });

  it("gives each detail line its own key and the place it refers to", () => {
    const a = finding({ id: "orphan:file:src/ui/a.ts", kind: "orphan", nodes: ["file:src/ui/a.ts"] });
    const b = finding({ id: "orphan:file:src/ui/b.ts", kind: "orphan", nodes: ["file:src/ui/b.ts"] });
    const self = finding({ id: "orphan:dir:src/ui", kind: "orphan", nodes: ["dir:src/ui"] });
    const lines = findingLines([a, b, self], 4, "dir:src/ui");
    expect(new Set(lines.map((l) => l.key)).size).toBe(3);
    expect(lines.map((l) => l.where)).toEqual(["src/ui/a.ts", "src/ui/b.ts", undefined]);
    // The "more" line has a key of its own too.
    const capped = findingLines([a, b, self], 1, "dir:src/ui");
    expect(capped.map((l) => l.key)).toEqual(["orphan:file:src/ui/a.ts", "more"]);
  });

  it("marks only the nodes of a finding whose edges were cut", () => {
    const plan = buildPlan(g);
    const cut = finding({
      id: "layer_violation:ui->core",
      kind: "layer_violation",
      severity: "error",
      violation: true,
      nodes: ["file:src/ui/app.ts"],
      edges: [{ src: "file:src/ui/app.ts", dst: "file:src/core/x.ts" }],
      edges_more: 120,
    });
    const on = findingsOnPlan(plan, [cut]);
    expect(on.byEdge.size).toBe(0);
    expect(on.byId.get("dir:src/ui")).toEqual([cut]);
  });

  it("places a repo: location on the plan's root and words it as a package", () => {
    const plan = buildPlan(g);
    const root = finding({ id: "orphan:repo:", kind: "orphan", nodes: ["repo:"] });
    expect(findingsOnPlan(plan, [root]).byId.get("repo:")).toEqual([root]);
    expect(findingLabel(root)).toBe("Paquete que nadie importa");
    expect(placeOf("repo:")).toBe("raíz");
    expect(placeOf("dir:src/ui")).toBe("src/ui/");
  });

  it("words findings in Spanish, problems first", () => {
    expect(findingLabel(finding({ metrics: { files: 2 } }))).toBe("Ciclo: dos archivos se importan entre sí");
    expect(findingLabel(finding({ metrics: { files: 4 }, violation: true }))).toBe("Regla rota: Ciclo entre 4 archivos");
    expect(findingLabel(finding({ id: "layer_violation:ui->infra", kind: "layer_violation", violation: true, metrics: { imports: 2 } }))).toBe(
      "Regla rota: la capa ui no puede depender de infra (2 imports)",
    );
    expect(findingLabel(finding({ kind: "hub", metrics: { share: 0.3421 } }))).toBe("Cuello de botella: en el 34.2 % de los caminos de dependencias");
    expect(findingLabel(finding({ kind: "orphan", severity: "info", nodes: ["file:a.ts"] }))).toBe("Solo lo usan las pruebas");
    expect(findingLabel(finding({ kind: "orphan", nodes: ["dir:pkg/x"] }))).toBe("Paquete que nadie importa");
    expect(findingLabel(finding({ kind: "hidden_coupling", severity: "info", metrics: { together: 11 } }))).toBe(
      "Acoplamiento oculto: cambian juntos en 11 commits sin importarse",
    );
    expect(findingLabel(finding({ kind: "http_unmatched_call", id: "http_unmatched_call:desktop:POST /api/reports/generate" }))).toBe(
      "Llamada HTTP sin ruta en su servidor: POST /api/reports/generate",
    );
    expect(findingLabel(finding({ kind: "http_unused_route", severity: "info", id: "http_unused_route:DELETE /users/{}" }))).toBe(
      "Ruta HTTP que ningún repositorio llama: DELETE /users/{}",
    );
    expect(findingLabel(finding({ kind: "contract_missing_field", id: "contract_missing_field:api:POST /api/items/tags" }))).toBe(
      "El cuerpo de la ruta exige un campo que ningún cliente envía: POST /api/items/tags",
    );
    const lines = findingLines(
      [finding({ kind: "hub", severity: "info", metrics: { share: 0.1 } }), finding({ metrics: { files: 3 } }), finding({ kind: "deep_chain", severity: "info", metrics: { depth: 12 } }), finding({ kind: "sdp", severity: "info" })],
      2,
    );
    expect(lines[0]).toMatchObject({ text: "Ciclo entre 3 archivos", alert: true });
    expect(lines.at(-1)).toEqual({ key: "more", text: "y 2 hallazgos más", alert: false });
    expect(isProblem(finding({ severity: "info" }))).toBe(false);
  });
});
