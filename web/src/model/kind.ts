// What each card is: source code, tests, scripts, styles, or the rest (config, data, docs).
// Decided from the path alone (the server only names programming languages, so css, json and
// md files arrive without one), with generic rules: no repository is special-cased. A folder
// takes the kind most of its files have and keeps its folder shape on the plan.
import type { Hierarchy } from "./hierarchy";

export type Kind = "code" | "test" | "script" | "style" | "data";

/** In legend order; also the tie-break order of a folder's dominant kind. */
export const KINDS: readonly Kind[] = ["code", "test", "script", "style", "data"];

/** Names shown in the legend and the tooltip. */
export const KIND_LABEL: Record<Kind, string> = {
  code: "Código",
  test: "Pruebas",
  script: "Scripts",
  style: "Estilos",
  data: "Config, datos y docs",
};

/** Short names for counts in the tooltip ("12 pruebas · 3 código"). */
export const KIND_SHORT: Record<Kind, string> = {
  code: "código",
  test: "pruebas",
  script: "scripts",
  style: "estilos",
  data: "config/datos",
};

/** "Carpeta de …" in the tooltip. */
export const FOLDER_LABEL: Record<Kind, string> = {
  code: "Carpeta de código",
  test: "Carpeta de pruebas",
  script: "Carpeta de scripts",
  style: "Carpeta de estilos",
  data: "Carpeta de config, datos y docs",
};

export type KindMix = Record<Kind, number>;

const CODE_EXT = new Set([
  "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "vue", "svelte", "astro",
  "py", "pyi", "rs", "go", "java", "kt", "kts", "scala", "groovy",
  "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "cs", "fs", "vb",
  "gd", "tscn", "gdshader", "shader", "glsl", "hlsl", "wgsl",
  "rb", "php", "swift", "m", "mm", "dart", "lua", "ex", "exs", "erl", "hs", "ml", "clj", "elm", "r", "jl", "zig", "nim",
  "html", "htm",
]);
const STYLE_EXT = new Set(["css", "scss", "sass", "less", "styl", "pcss"]);
const SCRIPT_EXT = new Set(["sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd"]);
/** Folders whose files are tests, wherever they are. */
const TEST_DIRS = new Set(["test", "tests", "__tests__", "__test__", "e2e", "__mocks__", "__fixtures__", "__snapshots__"]);
/**
 * Folders whose code files are tests (RSpec, Jasmine); elsewhere "spec" often means a written
 * spec (`spec/openapi.yaml`). Their test support folders hold tests whatever the file.
 */
const SPEC_DIRS = new Set(["spec", "specs"]);
const SPEC_SUPPORT = new Set(["fixtures", "support", "mocks", "factories", "helpers"]);

/** Words that end in IT: `OrderAUDIT` is an audit, not an integration test of `OrderAUD`. */
const IT_WORDS = new Set([
  "AUDIT", "BIT", "EDIT", "UNIT", "LIMIT", "SUBMIT", "COMMIT", "CREDIT", "DEBIT", "EXIT", "INIT", "SPLIT", "KIT",
  "HIT", "FIT", "DIGIT", "DEPOSIT", "PROFIT", "VISIT", "PERMIT", "TRANSIT", "OMIT", "EMIT", "REMIT", "QUIT", "SUIT", "WIT",
]);

/**
 * A Java/Kotlin/C# integration test: `UserServiceIT`, `S3IT`, `OrderDAOIT`, `RestAPIIT`. The
 * name before IT needs a lowercase letter or a digit (not `AUDIT`, `BIT`, `IT`), and the capitals
 * right before IT must not spell a word with it (not `OrderAUDIT`).
 */
function integrationTest(name: string): boolean {
  const m = /^(.*?)([A-Z]*)IT\.(java|kt|cs)$/.exec(name);
  if (!m) return false;
  const [, head, caps] = m as unknown as [string, string, string];
  if (!/[a-z0-9]/.test(head + caps)) return false;
  return caps === "" || !IT_WORDS.has(`${caps}IT`);
}
/** Godot code: never a "script" for being in `scripts/` (Godot's own name for its code folder). */
const GODOT_EXT = new Set(["gd", "tscn", "gdshader"]);
/** Folders whose files are scripts, wherever they are. */
const SCRIPT_DIRS = new Set(["scripts", "script"]);
/** Folders whose files are scripts only at the top level (inside a source tree they are usually code). */
const TOP_SCRIPT_DIRS = new Set(["bin", "tools"]);
/** Script files without an extension. */
const SCRIPT_NAMES = new Set(["makefile", "justfile", "rakefile", "gnumakefile"]);

/** Tool configuration written as code: `vite.config.ts`, `.eslintrc.js`, `jest.setup.js`… */
const CONFIG_CODE = /\.(config|conf|setup)\.(js|cjs|mjs|ts|cts|mts)$|^\.[\w.-]*rc\.(js|cjs|mjs|ts|cts|mts)$/i;
const TEST_FILE = /\.(test|spec|e2e|bench)\.[^.]+$|^test_.+\.py$|_test\.(py|go)$|(Test|Tests)\.(java|kt|cs)$|_spec\.rb$/;

function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i <= 0 ? "" : name.slice(i + 1).toLowerCase();
}

/** The kind of a file from its repo-relative path (forward slashes). */
export function fileKind(path: string): Kind {
  const parts = path.split("/").filter(Boolean);
  const name = parts[parts.length - 1] ?? "";
  const dirs = parts.slice(0, -1).map((d) => d.toLowerCase());
  const ext = extOf(name);
  if (TEST_FILE.test(name) || integrationTest(name) || dirs.some((d) => TEST_DIRS.has(d))) return "test";
  const spec = dirs.findIndex((d) => SPEC_DIRS.has(d));
  if (spec >= 0) {
    if (dirs.slice(spec + 1).some((d) => SPEC_SUPPORT.has(d))) return "test";
    if (CODE_EXT.has(ext) && !CONFIG_CODE.test(name)) return "test";
  }
  // Godot keeps its game code in `scripts/`: GDScript and scenes there are code.
  if ((!GODOT_EXT.has(ext) && dirs.some((d) => SCRIPT_DIRS.has(d))) || (dirs.length > 0 && TOP_SCRIPT_DIRS.has(dirs[0]!))) return "script";
  if (SCRIPT_EXT.has(ext) || SCRIPT_NAMES.has(name.toLowerCase())) return "script";
  if (STYLE_EXT.has(ext)) return "style";
  if (CONFIG_CODE.test(name)) return "data";
  // A loose .mjs/.cjs at the top of the repo is a build or tooling script.
  if (dirs.length === 0 && (ext === "mjs" || ext === "cjs")) return "script";
  if (CODE_EXT.has(ext)) return "code";
  return "data";
}

/**
 * The kind of a folder from its path alone (one the tree doesn't know, or an empty one): the
 * kind a source file inside it would have, so it follows the same rules as files. A folder
 * the tree knows takes the kind of what it holds instead (kindsOf).
 */
export function dirKindByName(path: string): Kind {
  return fileKind(`${path}/_.ts`);
}

export function emptyMix(): KindMix {
  return { code: 0, test: 0, script: 0, style: 0, data: 0 };
}

/** The kind most of a mix is; ties go to the first in KINDS. An empty mix is code. */
export function dominantKind(mix: KindMix): Kind {
  let best: Kind = "code";
  for (const k of KINDS) if (mix[k] > mix[best]) best = k;
  return best;
}

export interface Kinds {
  /** Kind of every file and folder of the tree, by id. */
  kind: Map<string, Kind>;
  /** Files per kind under every folder (a file counts once, for itself). */
  mix: Map<string, KindMix>;
}

/**
 * Kinds for a whole tree, bottom-up. A folder is what most of the things you see when you
 * open it are: each child (file or non-empty folder) votes with its kind. Ties go to the kind
 * with more files under the folder, then to the first in KINDS. So `src` with `ui`, `shared`
 * and a big `test` folder stays code, while `src/test` is tests. Data (config, lockfiles,
 * docs) wins only when most files under the folder are data, so a package folder with a
 * dozen config files beside its `src/` is code.
 */
export function kindsOf(h: Hierarchy): Kinds {
  const kind = new Map<string, Kind>();
  const mix = new Map<string, KindMix>();
  const visit = (id: string): KindMix => {
    const n = h.nodes.get(id)!;
    if (n.kind === "file") {
      const k = fileKind(n.path);
      kind.set(id, k);
      const m = emptyMix();
      m[k] = 1;
      mix.set(id, m);
      return m;
    }
    const m = emptyMix();
    const votes = emptyMix();
    for (const c of n.children) {
      const cm = visit(c);
      for (const k of KINDS) m[k] += cm[k];
      if (KINDS.some((k) => cm[k] > 0)) votes[kind.get(c)!] += 1;
    }
    // Config, lockfiles and docs pile up next to the code they serve (`web/` with a dozen
    // config files and one `src/`): data wins the vote only when most files under the folder
    // are data too.
    const total = KINDS.reduce((s, k) => s + m[k], 0);
    if (votes.data > 0 && m.data * 2 <= total && KINDS.some((k) => k !== "data" && votes[k] > 0)) votes.data = 0;
    const top = Math.max(...KINDS.map((k) => votes[k]));
    const tied = KINDS.filter((k) => votes[k] === top);
    const best = tied.reduce((a, b) => (m[b] > m[a] ? b : a));
    kind.set(id, top > 0 ? best : dirKindByName(n.path));
    mix.set(id, m);
    return m;
  };
  visit(h.root);
  return { kind, mix };
}

/** The kind of a plan card: from the tree when it knows the id, else from the path. */
export function kindOf(kinds: Kinds | null, id: string, path: string, isDir: boolean): Kind {
  return kinds?.kind.get(id) ?? (isDir ? dirKindByName(path) : fileKind(path));
}
