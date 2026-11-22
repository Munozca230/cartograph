# cartograph · art direction of the layered plan

The UI text is in Spanish; quoted labels below ("Nivel", "Librerías", "Encuadrar"…) are the
words on screen.

## The idea
The repository reads like a technical drawing: every folder is a card of the same size and
dependencies point down. Entries (what nobody imports) sit at the top and the base (what
everything uses) at the bottom. Position gives direction, so one glance shows what depends
on what.

What does not fit that rule is the interesting part: a dependency that has to point up for
the drawing to close forms a cycle. Those are magenta and are called "contra la corriente"
(against the current).

The plan replaced an earlier Voronoi treemap ("Atlas"), which proved tangled, crowded and
had too many different sizes. The screen holds only the plan; everything else is one
button away.

## Rules
- **Only the plan.** The plan fills the window under a thin header and a toolbar. The
  explorer, the legend, the repository figures, the detail, the libraries and what has no
  connections sit behind a button. Nothing appears on the right until a card is chosen.
- **The drawing says almost everything.** Each card carries its name, a size bar (lines,
  on a log scale relative to the largest card of the view) and a magenta dot if it has
  problems (unresolved imports, skipped files, diagnostics) or is in a cycle. A closed
  folder is drawn as a stack of two cards: there is more inside. The rest (lines, files,
  how many it depends on, how many use it) goes in the tooltip.
- **What each thing is.** Each card shows its kind with a colored rule on its left edge, an
  icon before the name and the color of its size bar. There are five kinds, decided by the
  path alone and without any repository's own names (`src/model/kind.ts`): **code** (ts,
  py, rs, go, java, gd…), **tests** (folders `test`, `tests`, `__tests__`, `spec`, `e2e`;
  files `*.test.*`, `*.spec.*`, `test_*.py`, `*_test.go`, `*Test.java`), **scripts**
  (`scripts/` folders; `bin/` and `tools/` only at the root; sh, ps1, bat, Makefile, loose
  `.mjs`/`.cjs` at the root), **styles** (css, scss, less) and **config, data and docs**
  (everything else: json, yaml, md, `*.config.ts`, images). A folder keeps its folder shape
  (tab at the top left, folder icon, stack when closed) and takes the color of what shows
  when it is opened: each child votes with its kind, and a tie is broken by the number of
  files of each kind. So `src` with `ui`, `shared` and a huge `test` stays code, and
  `src/test` is tests. Its bar is split by the mix of kinds of its files. The name of an
  open folder is drawn in the color of its kind. The tooltip names the kind ("Carpeta de
  pruebas", "Scripts") and, for a mixed folder, how many files there are of each.
- **Levels, not a path.** "Nivel 1 · 2 · 3 · Todo" opens everything above the level: 1 is
  the first level only (close all) and Todo reaches the files (open all). Double-clicking a
  card opens that folder without closing the others; double-clicking a box's name closes
  it. What was opened or closed by hand is marked with a dot on the level, and pressing the
  level forgets it. Todo never closes anything: in a repository too large to open whole
  (over 2000 cards) it opens down to the deepest level that fits, keeps the folders that
  were already open, and the plan says so ("Todo · hasta el nivel N"). There is no
  breadcrumb: with several folders open at once there is no single path to show, and each
  box already carries its name on the plan.
- **Nested boxes.** Each open folder is a box with its children inside, and boxes nest.
  Connections are summed at the visible level, as `/api/graph` does.
- **Two drawing modes by size.** Up to 40 cards, each connection goes card to card even
  across boxes. Beyond that, each box is laid out on its own and a connection is drawn
  between the two children of the box that holds both ends (a card or a box); choosing a
  card still marks its real neighbors even inside another box. So "Todo" on a repository of
  a thousand files takes a few screens, not dozens.
- **Against the current, box by box.** Each box orders its children with the lightest
  feedback arc set of the connections lifted to that level, and a connection is magenta
  when its lifted pair is. The rule is the same in both modes, so what is magenta is
  exactly what points up.
- **Width by weight, capped.** Width says how many file pairs make up the connection
  (`1 + 0.9·log2(pairs)`, at most 6). The heaviest connection carries a label with its
  number.
- **What is hidden is counted and can be shown.** In a dense box (over 30 connections, or
  22 if it has over 25 connected items) the lightest connections are not drawn, never a
  magenta one, and the toolbar says how many with a button to draw them. What has no
  connections is counted in the "N sin conexiones" button, which draws it dimmed. No rule
  looks at folder names.
- **Libraries, on request.** The "Librerías" button draws external packages as dotted
  pills in the bottom layer: the 4 most used of each ecosystem and an "otras N" pill with
  the rest, each with its ecosystem on the right. Arrows start at what uses them (the card,
  or its first-level box in nested mode).
- **Always legible, always named.** Plan text is 14 px; below a 12/14 scale it would read
  under 12 px, so there the plan hides its own text and names come back as 12 px labels
  fixed on screen, anchored to their card or box (`src/ui/planLabels.ts`). Every card always
  shows its name: the zoom stops at K_MIN = 0.3, where a card is 48 px on screen and its
  name gets that width plus the gap to its neighbor (about 48 px: four or five letters and
  the ellipsis). Neither the wheel, nor −, nor pinch, nor the minimap go below that; a small
  plan stops earlier, close to fitting whole. A name is cut with the ellipsis in the
  middle, because the end tells siblings apart ("Order…Panel"); a file loses its extension
  first, unless a sibling (file or folder) would read the same without it, like `index.ts`
  and `index.js` or `utils.ts` next to the `utils` folder; a folder never loses its dots.
  The names of a row sit side by side and never touch. Fit ("Encuadrar", and the first
  view) shows the whole plan if it fits at K_MIN or more; otherwise it stays at K_MIN with
  the top-left corner in view and the minimap shows where you are. Box titles go where card
  names leave room: in their top strip, further right, on the border, above or lower, so
  titles of nested boxes that fall in the same corner stack ("src", "test", "unit"); a
  small box alone in its folder's corner carries its title after the folder's, like a path,
  or below it. If a title finds no room, its whole level goes, the deepest first, never
  part of a level. Labels that appear fade in briefly. Zoomed out, the icon, tab, rule and
  bar would be specks: the whole card takes a tint of its kind. Placement depends only on
  the framing (not on dragging) and is computed in one animation frame; dragging moves the
  whole label layer and only labels near the screen are drawn.
- **Health scoped to the view.** "N contra la corriente" appears in the toolbar only if
  there are cycles in what is shown. The full detection is in the findings.

## Palette (tokens in `src/styles/tokens.css`)
One family of ink blues, no grain, no contour lines and no meridians.

| Token | Dark | Light | Meaning |
|---|---|---|---|
| `--ground` | `#0C171E` | `#EEF1EF` | background of the plan and the toolbar |
| `--panel` | `#0F1C25` | `#F7F8F5` | header, drawers and popovers |
| `--rule` | `#1D2F3B` | `#D3DBDA` | rules, box borders, size bar track |
| `--card` / `--card-line` | `#132330` / `#294150` | `#FFFFFF` / `#B7C5C7` | cards |
| `--ink` / `--muted` / `--faint` | `#E6E1D3` / `#93A2A8` / `#5F727A` | `#16232C` / `#4B5D64` / `#6A7B81` | text |
| `--route` | `#7EA4B4` | `#4D7C8F` | **every** dependency |
| `--brass` | `#CDAE72` | `#86601C` | **the only accent**: selection, its connections, the active level, buttons that are on, and the weight label |
| `--alert` | `#E3709F` | `#B0336C` | **problems only**: against the current, the problem dot |
| `--live` | `#7FC3A6` | `#2F8A63` | "live" dot next to the project name |

The light theme comes from the same roles with inverted luminance; brass and magenta are
darkened to keep contrast on a light background.

Kinds (rule, icon and bar of each card; name of an open folder). Muted and far from brass,
magenta and the route blue; contrast of at least 5.1:1 on `--card` and 4.5:1 on `--ground`
in both themes, and a minimum OKLab ΔE of 12 between them (5.7 simulating deuteranopia; the
icon disambiguates). As text on nested boxes, whose background darkens with each level, the
light theme moves the color 35 % toward the ink (`--k-text`): at least 4.8:1 with six nested
boxes.

| Token | Dark | Light | Kind |
|---|---|---|---|
| `--kind-code` | `#6C9EE0` | `#22599C` | code |
| `--kind-test` | `#7FC596` | `#2F7A48` | tests |
| `--kind-script` | `#E58B6B` | `#AB4C28` | scripts |
| `--kind-style` | `#C9A9EF` | `#8556BD` | styles |
| `--kind-data` | `#9B988E` | `#57544B` | config, data and docs |

Languages (explorer dot): typescript `#7FA6D6`, tsx `#78BFC4`, javascript `#D6C07E`, python
`#9CC28A`, rust `#D49A80`, go `#79CFD8`, java `#C39AD6`, c/c++ `#A9A1E0`, c# `#8FB79C`,
gdscript `#B9D07A`, other `#6E7B80`.

## Typography
- **Cormorant Garamond** (variable, self-hosted): project name, detail title, titles of
  drawers and popovers.
- **Hanken Grotesk** (variable, self-hosted): everything else, figures with
  `tabular-nums`.
- **Sizes**: 20 px (serif titles), 13 px (text), 11 px (secondary) outside the plan; 14 px
  for all plan text, which with the scale floor reads between 12 and 14 px; 12 px fixed for
  the labels of the zoomed-out plan.

## Layout
```
┌──────────────────────────────────────────────────────────────────────────┐
│ [▤] ◎ cartograph  my-app ●                   [● 2 problemas] [Buscar Ctrl K] [ⓘ] │  48 px
│ Nivel [1] 2 3 Todo │ [Librerías] [25 sin conexiones] [● 3 contra la corriente]  [− + ⤢ Encuadrar] [?] │  44 px
├──────────────────────────────────────────────────────────────┬───────────┤
│                                                              │ (detail,  │
│                     layered plan (SVG)                       │  300 px,  │
│                                                ┌──────────┐  │  only with│
│                                                │ minimap  │  │  something│
│                                                └──────────┘  │  chosen)  │
└──────────────────────────────────────────────────────────────┴───────────┘
```
- **Header**: explorer button, mark, project and a live dot (it says "Indexando 40 %" or
  "Reconectando" only in those states; a click re-indexes), problems only when there are
  any, search and ⓘ. The ⓘ popover has the figures (files, lines of code, imports,
  libraries) and the theme.
- **Toolbar**: level, layer buttons (Librerías, sin conexiones, hidden connections; each
  appears only when it has something to show), the cycle notice, the camera and the legend
  ("?").
- **Detail**: a 300 px drawer on the right edge of the plan. Name, one plain-language
  sentence, three figures (files, lines, dependencies; or lines, imported by, imports for a
  file), "Ver más" for the lists (Depende de, La usan, Librerías) and "Abrir carpeta" /
  "Cerrar carpeta". It closes with × or Esc. Opening it does not move the plan under the
  pointer (so a double click still lands on the same card); after half a second (a
  Windows double click) the plan re-fits into the part the drawer leaves free if nobody
  moved it, and if the drawer covers the chosen card, the plan slides to show it.
- **Explorer**: a drawer on the left, above the plan, closed at start. Its open folders are
  the plan's. Esc closes it.
- Esc closes the topmost thing: the palette or the problems drawer, popovers, the explorer
  and finally the selection.

## Motion
- Only the first view of the project and the Encuadrar button show the whole plan. When a
  folder opens or closes, the level changes, or a layer is shown or hidden (libraries,
  unconnected, hidden connections), the zoom does not change and one card stays at the same
  point on screen: the folder that was opened or closed, else the chosen one if it was in
  view, else the one nearest the center; if it disappeared, the folder that now holds it
  (`src/ui/camera.ts`). If only numbers change (a re-index with no change of shape), the
  drawing does not move: the same layout is reused.
- After a re-index, the cards that changed flash brass once.
- The detail slides in briefly. With `prefers-reduced-motion: reduce` there are no
  animations or transitions.

## Interaction
- Click a card: selection (brass), its real neighbors with a brass border and the rest
  dimmed; the detail opens. Double click or Enter on a selected card: open the folder.
  Double click a box's name: close it.
- Wheel: zoom around the pointer. Drag: pan. −, + and Encuadrar buttons in the toolbar;
  keys `+`, `-`, `0` and arrows on the plan. Click or drag on the minimap: go to that part.
- Explorer and palette: choosing something that is not on the plan opens the folders that
  hold it (the others stay open) and brings the camera there.
