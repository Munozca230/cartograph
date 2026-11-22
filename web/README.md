# cartograph web: the layered plan

The local web UI of `cartograph serve`. It draws the repository as a layered plan you can read without reading code: one card per folder or file of the open view, open folders as nested boxes, dependencies pointing down, and the few edges that must point up to close a cycle drawn in magenta ("contra la corriente"). The screen is the plan alone: the explorer, legend, numbers, detail, libraries and unconnected nodes sit behind buttons. The art direction is in [DESIGN.md](./DESIGN.md).

## Run it

```sh
npm ci
npm run dev                                          # mock API (fixture A, a ~170-file Next.js SaaS)
CARTOGRAPH_FIXTURE=perf npm run dev                  # mock API, fixture B (~5k files)
CARTOGRAPH_API=http://127.0.0.1:7878 npm run dev     # proxy /api to a real `cartograph serve`
npm run build                                        # web/dist, relative asset URLs, servable from /
```

Mock options: `CARTOGRAPH_MOCK_WATCH_MS` sets how often a simulated save re-indexes (default 45000; 0 means "Sin vigilancia"). Open the app with `?debug` to get `window.__cartograph` (controller and store).

| Script | What it does |
|---|---|
| `npm run typecheck` | `tsc` in strict mode over app, mock and tests |
| `npm run lint` | ESLint with typescript-eslint and the React hooks rules, zero warnings |
| `npm test` | Vitest: rollup semantics, levels and open folders, "contra la corriente" box by box, density rules, libraries, real ELK layout direction, nesting and determinism in both drawing modes, controller races, SSE, search |
| `npm run build` | Typecheck, then the Vite build into `dist/` |

## Renderer: elkjs layered layout + SVG

- **Layout.** [elkjs](https://github.com/kieler/elkjs) `0.12.1` (pinned; EPL-2.0 OR GPL-3.0-or-later, while this package is MIT OR Apache-2.0; see `THIRD_PARTY_NOTICES.md`), algorithm `layered`, direction down, orthogonal routing, each open folder as a compound node. It runs in a Web Worker: `elk-worker.min.js` is imported with `?url`, so Vite copies it into `dist/assets` and the Rust binary serves it with the rest of `web/dist`. There is no CDN.
- **Two drawing modes.** Up to 40 cards the whole hierarchy is one ELK run (`INCLUDE_CHILDREN`) and edges go card to card across box borders. Above that, each box is laid out on its own (`SEPARATE_CHILDREN`) and an edge is drawn between the two children of the innermost box that holds both ends. Measured before choosing, with every folder open: card to card, a ~170-file repo came out 7.6k x 7.0k px, and a 1.3k-file repo was already 58k x 39k px (10 s) with only its 1200 heaviest edges, so the full run was not attempted; nested, 1.7k x 4.4k px (1 s) and about 8k x 16k px (5 s).
- **Drawing.** Plain SVG from React. The content of the camera group is memoized, so panning and tooltips only move one transform. The plan is fitted on load, never above 1:1 and never under 12/14 (plan text is 14 px, so it never renders under 12 px); a bigger plan is panned, with a minimap in the corner while it overflows. Zooming out by hand past that hides the plan's text and overlays the names of the biggest boxes.
- **Determinism.** ELK input is sorted by id with a fixed seed. A layout is reused while the plan's key (mode, boxes, card ids, oriented drawn edges, the card size) is unchanged, so a live re-index that only changes numbers doesn't move anything.

## The model (`src/model/`, pure and tested)

- `view.ts`: what the plan opens. A view is a level (1, 2, 3 or all) plus the folders opened below it or closed inside it, so any number of folders can be open at once. It goes to `/api/graph` as `depth` plus `expanded` / `collapsed`, so "Todo" is a short URL on any repo. Selecting something that isn't on the plan opens the folders above it; folders deleted by a re-index drop out. `initialView` is level 1 with the chain of folders that clearly hold their parent's code opened (half of its files and two thirds of its subfolders' files).
- `flow.ts`: cycles (Tarjan) and the weighted greedy feedback arc set (Eades, Lin and Smyth) run inside each cycle, so every magenta edge lies on one. `upstreamEdges` lifts each edge to the two children of the innermost box that holds both ends and runs the arc set box by box, because a node outside a box can't sit between two cards inside it.
- `plan.ts`: what a view draws. Upstream edges go to ELK reversed (so layers point down) and are colored magenta; the tests check both directions and the nesting on the real ELK output, in both modes, at every level of the mock repo. Light edges are hidden only in a dense box (more than 30 drawn edges, or 22 with more than 25 connected nodes), never an upstream one, always with a count and a toggle. Unconnected nodes are counted and drawn faded on request. With libraries on, the 4 most used packages per ecosystem plus an "otras N" card sit in the bottom layer. No rule looks at folder names.
- `deeplink.ts`: links into the map. `?view=plan&focus=<file or folder>` selects it on the plan with its parent folders open, `?view=flows&focus=sym:<path>#<name>` opens Flujos at that function (the MCP tool `open_map` builds them). `state/deeplink.ts` applies the page's link once the first plan is loaded (the fly waits for that plan to be drawn and never goes below the readable zoom; nothing re-applies it later) and on `popstate`, shows a notice for an id the map doesn't know, then keeps the URL in step with the selection (`replaceState`, other parameters kept).

## Layout of the code

```
mock/            Vite middleware: MockIndex (contract semantics), fixtures, SSE and simulated re-indexing
src/api/         Typed client, SSE client with backoff and stale detection, search adapter
src/model/       Hierarchy and frontier rules (mirror of /api/graph), view (levels and open folders), flow (cycles, box by box), plan + ELK input/output
src/state/       Store (useSyncExternalStore), controller (API, live events, camera requests), plan memo + ELK worker
src/ui/          Header (info popover), toolbar (levels, layers, legend popover), SVG plan + minimap, detail drawer, explorer drawer, ui-sdk palette, issues drawer, toasts
```

## Contract notes

- Built against `docs/api-f2.md` including "Aclaraciones (backend)": wrapped `{generation, ...}` bodies, `weight` counted in distinct file pairs, `repo:` as the root, `reindexing` in `/api/meta`.
- The view travels as `depth=<level>` plus one `expanded=` or `collapsed=` parameter per folder opened or closed by hand, so the URL grows with what the user changed, not with the size of the repo. `POST /api/graph` is not needed. "Todo" on a repo whose frontier would pass 2000 cards opens the deepest level that stays under it, and the toolbar says "hasta el nivel N".
- "Líneas de código" in the info popover is the sum of `meta.languages` without `other`, which is where the backend counts JSON, SQL, Markdown, lockfiles and other non-code files. Cards and the explorer show all lines ("líneas").
- Problem dots: `unresolved` from `/api/graph` per card, plus diagnostics and skipped files from `/api/issues`, each placed on the nearest card or box that holds its path (a skipped file is not in `/api/tree`).
