# Bridges: links no import states

Some code reaches other code without importing it. An Electron renderer calls a function
the preload exposes, which invokes an IPC channel a main-process handler serves; a member
of a workspace depends on the package another member builds. Cartograph calls these
**bridges** and keeps them apart from imports: they show where a change lands on the
other side, never in cycles or size measures.

| Kind | Sides | Built |
|---|---|---|
| `ipc` | renderer `window.api.f()` → preload exposure → main-process handler, and back (`webContents.send` → listener) | single repos and workspaces |
| `package` | a dependency (`pom.xml`, Gradle, `package.json`) → the member that builds that package | workspaces |
| `http` | client call → server route | not yet |

## Settings

```toml
# cartograph.toml (a single repo) or cartograph-workspace.toml
[bridges]
ipc = true        # default: on (files that import `electron` only)
packages = true   # default: on in a workspace (nothing to link in a single repo)
http = false      # reserved
```

IPC is on by default in a single repository: its sites only count in files that import
`electron`, and sampled IPC bridges checked against the code held up (see the precision
notes below). Turn a kind off with `false`; the change takes effect at the next index.

## Electron IPC

What is read, per member (a single repo is one member):

- **main process**: `ipcMain.handle|handleOnce` (serves `invoke`), `ipcMain.on|once`
  (serves `send`), the same on `webContents.ipc`; `webContents.send(channel)` pushes to a
  renderer;
- **preload**: `ipcRenderer.invoke|send|sendSync` use a channel, `ipcRenderer.on|once`
  listens to a push; `contextBridge.exposeInMainWorld("api", obj)` exposes every function
  of `obj` (an object literal or a module-level `const`, nested objects included) as
  `api.f`, `api.updater.check`;
- **renderer**: `window.api.f(…)`, `globalThis.api.f(…)`, `(window as any).api.f(…)` and
  local aliases (`const api = window.api; api.f(…)`).

Channels are read through constants: a literal, a template, a concatenation, a constant of
the file or one an import brings (`IPC.USERS_GET` of `export const IPC = { … } as const`, a
TS string enum). A channel only known at run time (a variable, a call)
is counted as `dynamic` and never matched or reported.

Handlers registered through a wrapper are followed: a method that passes its parameter as
the channel (`protected handle(channel, h) { ipcMain.handle(channel, …) }` in a base class)
makes each call to it (`this.handle("docs:save", this.onSave.bind(this))`) a handler of that
channel, run by `onSave`. A preload function that passes its parameter on
(`on: (channel, cb) => ipcRenderer.on(channel, …)`) makes the renderer's argument the
channel.

Bridges: `expose` (renderer → exposed function), `invoke` / `send` (use → handler), `push`
(main → listener). Confidence: `exact` for literals on both sides, `resolved` through a
constant, a wrapper or an alias, `heuristic` for a channel with an unknown part (`job:*`).

Findings:

| Finding | Severity | When |
|---|---|---|
| `ipc_no_handler` | warning | a channel is invoked or sent and no handler serves it |
| `ipc_unexposed` | warning | the renderer calls a function of an exposed world the preload does not expose |
| `ipc_unused_handler` | info | a handler no `invoke` or `send` uses |

## Packages of a workspace

Coordinates come from every manifest of every member: Maven (`groupId:artifactId`, the
groupId and version inherited from `<parent>`, `${…}` properties, dependencies outside
`<dependencyManagement>`, `<build>` and `<profiles>`), Gradle (`group`, the project's
folder or `rootProject.name`, `group:artifact:version` strings and map notation) and npm
(`name`, `version`, every kind of dependency). A dependency on a coordinate another member
builds is a `package` bridge (`maven` or `npm`, exact), from the dependency's line to the
manifest that builds it.

Declared dependencies decide what an import across members may reach:

- **Java**: an import that names a class declared in several members resolves to the
  importer's own member, else to one it declares (transitively); a class only an
  undeclared member has is still imported, as a guess (`heuristic`), and the pair is an
  `undeclared_member_dependency` (warning). Members without manifests are not judged.
- **TypeScript**: a bare import of a package another member builds (`@acme/ui-kit`,
  `@acme/ui-kit/button`), declared in the importer's `package.json`, resolves to that
  member's source (`exports`, `module`, `main`, `src/index.*`, `index.*`).

A dependency at another version than the member builds is a `version_skew` (info). Maven
versions compare as written (a range is not judged); npm ranges as npm reads `^`, `~` and
exact versions (`workspace:`, `file:` and `*` are not judged).

## Where bridges show up

| Surface | What it adds |
|---|---|
| `bridges` (MCP, `cartograph query bridges`) | every bridge, or with `unmatched` what has no other side; `node`, `kind`, `member` narrow it |
| `callers` | `over IPC "save" (invoke) from src/preload.ts:12 api.save()`, then the renderer sites that reach that function |
| `deps`, `explain` | a block "over bridges (not imports)" with the other end and the key |
| `impact` | "reached over bridges": what reaches the change over one or two bridges, and what imports that |
| `path` | follows bridges (`bridges: false` keeps imports only); a step over one says so |
| `entries` | an IPC handler says how many sites reach it over IPC |
| `diff` | bridges the change adds and removes (both sides are indexed with the same settings) |
| web map | `GET /api/bridges`, a `bridges` array in `/api/graph` (apart from `edges`), a toolbar toggle that draws them dashed over the plan, the file panel, dashed edges in flows |

Bridges never count as imports: cycles, folder cycles, stable dependencies, hubs, god
modules, chains, orphans and fan-in leave them out. Layer rules ignore them unless
`[rules] layers_count_bridges = true`.

## Precision

Bridges are checked against the code by sampling each kind and confidence and reading both
ends, and recall against a grep of the call shapes (`ipcRenderer.invoke|send|sendSync`,
`ipcMain.handle|on`, handler wrappers, `webContents.send`) as the oracle. The fixture
`tests/fixtures/workspace-bridges` holds the traps (a constant channel next to a dynamic
one, a wrapper method, a call the preload does not expose, a class two members declare,
an undeclared member, a version skew) with golden bridge lists.

## Limits

- HTTP bridges are not built yet.
- A channel built at run time is counted, never matched.
- IPC through a framework that hides `ipcMain` / `ipcRenderer` behind its own API is not
  read unless it wraps them in a function that takes the channel as a parameter.
- Flows show the preload hop (renderer → exposed function → handler) instead of
  collapsing it.
