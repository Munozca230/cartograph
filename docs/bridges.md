# Bridges: links no import states

Some code reaches other code without importing it. An Electron renderer calls a function
the preload exposes, which invokes an IPC channel a main-process handler serves; a client
calls a route a server declares; a member of a workspace depends on the package another
member builds. Cartograph calls these
**bridges** and keeps them apart from imports: they show where a change lands on the
other side, never in cycles or size measures.

| Kind | Sides | Built |
|---|---|---|
| `ipc` | renderer `window.api.f()` → preload exposure → main-process handler, and back (`webContents.send` → listener) | single repos and workspaces |
| `package` | a dependency (`pom.xml`, Gradle, `package.json`) → the member that builds that package | workspaces |
| `http` | client call (or a wrapper's caller) → server route | workspaces; single repos with `http = true` |

## Settings

```toml
# cartograph.toml (a single repo) or cartograph-workspace.toml
[bridges]
ipc = true        # default: on (files that import `electron` only)
packages = true   # default: on in a workspace (nothing to link in a single repo)
http = true       # default: on in a workspace, off in a single repo
```

IPC is on by default in a single repository: its sites only count in files that import
`electron`, and sampled IPC bridges checked against the code held up (see the precision
notes below). HTTP is on in a workspace (clients and servers are usually separate
repositories) and opt-in in a single repo. Turn a kind off with `false`; the change takes
effect at the next index.

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

## HTTP

What is read:

- **routes**: Spring `@RestController` / `@Controller` classes: the class-level
  `@RequestMapping` × each `@GetMapping`, `@PostMapping`, `@PutMapping`, `@DeleteMapping`,
  `@PatchMapping` or `@RequestMapping(method = …)` of a method, every string of
  `value` / `path` arrays, paths written as constants (`static final String`, of this file
  or of another one, in another member too, through imports and concatenations);
  Express-style registrations (`app.get("/x", h)`, `router.post(…)`, `all` for any
  method) in files that import express, fastify, koa and the like, with routers mounted by
  `app.use("/prefix", router)` (the router imported or not, up to three mounts deep; a
  router mounted twice has its routes under both prefixes); and the HTTP entries of other
  frameworks (Flask, FastAPI, NestJS with `setGlobalPrefix`, Go, axum, ASP.NET) with their
  literal paths. A context path prefixes routes: `[member.http] context_path` for the
  whole member, else each Spring module's `server.servlet.context-path` (in that
  module's `src/main/resources/application.{properties,yml,yaml}`, `${NAME:/default}`
  read as its default) for the routes of that module.
- **clients**: `fetch(url, { method })`; axios, got and ky (`axios.get(url)`,
  `axios({ url, method })`, and instances made with `axios.create({ baseURL })`, used in
  the file or imported from another one); RestTemplate (`getForObject`, `postForEntity`,
  `exchange(url, HttpMethod.X, …)`…); WebClient / RestClient (`x.get().uri(url)`, a base
  from `.baseUrl(…)`); `java.net.http` (`HttpRequest.newBuilder().uri(…).POST(…)`); Feign
  interfaces (`@FeignClient(name, url, path)` with mapping annotations; a `name` equal to a
  member's targets that member); Python `requests` / `httpx`.
- **wrappers**: a function or method that passes its parameter as the URL or path of a
  client call (with a base before it or not: `${this.baseUrl}${endpoint}`), or as its
  method, is a wrapper; every call to it is a client site with its arguments in place, up
  to four wrappers deep (`service → ApiClient.post → ApiClient.request → ApiClient.send →
  fetch`). Parameters are those of the definition around the call, through the arrows
  inside it; locals are followed; a helper named for a URL (`this.buildUrl(path)`) is an
  unknown base followed by its argument; `process.env.X ?? "http://…"` is its literal. A
  parameter after a literal ending in `/` (`/users/${id}`) is a path variable, not a
  wrapper. A call no resolver ties to its wrapper (`deps.client.post("/x")` through an
  untyped object) is tied by name when one wrapper of the member has that name and the
  URL argument is a literal path (`heuristic`).

Matching:

- the URL is cut at `?` and `#`; `{id}`, `{id:re}`, `:id`, `<int:id>`, `${…}` and unknown parts are
  path variables (`:id?` gives the route with and without it, a last `*` / `**` takes the
  rest); a literal segment beats a variable (`/users/me` before `/users/{id}`); methods are
  equal, or one side takes any; more than three routes tied for a call is `ambiguous`;
- a literal host: a member's `hosts` targets that member; `localhost`, `127.0.0.1` and the
  like target every member; any other host is `external`, never matched;
- a base only known at run time (`this.baseUrl`, a constructor parameter) is an unknown
  base: a route may have leading literal segments the call lacks. Once two calls through
  one base (the same text in the same file) reach routes of one member under one prefix,
  the base is mapped there: its other calls only match that member and prefix.

Confidence: `exact` for a literal call to a literal route with its method; `resolved`
through constants, a class prefix, a mount, a context path, a wrapper, an instance or a
mapped base; `heuristic` for an unknown base not mapped, a method not known or any, two
or three routes, a pattern, or a wrapper tied by name.

What is left without the other side (`bridges` with `unmatched`): `unmatched` (a call whose
server is known matches no route), `method_mismatch` (the same path with another method),
`no_server` (the call's base reaches no member's routes from that file: a service outside
the workspace), `external`, `ambiguous`, `dynamic` (a URL only known at run time), and for
routes `unused` (no member calls it, while other routes of its member are called) and
`no_clients` (no member calls the member at all, or it is `public`).

Findings:

| Finding | Severity | When |
|---|---|---|
| `http_unmatched_call` | warning | a call whose server is known (a member it targets, a declared host, a base whose other calls from that file reach routes) matches no route |
| `http_method_mismatch` | warning | the route exists with another method |
| `http_unused_route` | info | a route no member calls, in a member other members call (never for `public = true`) |

`no_server`, `external`, `ambiguous` and `dynamic` are counted, never reported: they are
calls to services outside the workspace or URLs built at run time.

```toml
# cartograph-workspace.toml
[[member]]
name = "api"
path = "backend-api"
[member.http]
hosts = ["localhost:8080", "api.internal"]  # calls to these hosts match only this member
context_path = "/api"                       # else server.servlet.context-path
public = false                              # true: clients outside call it
```

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
| `callers` | `over IPC "save" (invoke) from src/preload.ts:12 api.save()`, then the renderer sites that reach that function; `over HTTP "POST /api/users" (wrapper) from web/src/users.ts:14 createUser()` for a route handler |
| `deps`, `explain` | a block "over bridges (not imports)" with the other end and the key; `explain` lists first the file's calls left without the other side (a route no server has, the wrong method) |
| `impact` | "reached over bridges": what reaches the change over one or two bridges, and what imports that |
| `path` | follows bridges (`bridges: false` keeps imports only); a step over one says so; to a client that calls out over HTTP or IPC, the calls go on past it down to the data (see [spring.md](spring.md) and [mcp.md](mcp.md#slices)) |
| `entries` | an IPC handler says how many sites reach it over IPC, a route how many call it over HTTP |
| `diff` | bridges the change adds and removes (both sides are indexed with the same settings), and the contracts it breaks (see [contracts.md](contracts.md)) |
| `explain` of a body type | the bodies of its routes, client type against server type, field by field (see [contracts.md](contracts.md)) |
| web map | `GET /api/bridges`, a `bridges` array in `/api/graph` (apart from `edges`), a toolbar toggle that draws them dashed over the plan, the file panel, dashed edges in flows (and from an interface method to the Spring beans that implement it) |

Bridges never count as imports: cycles, folder cycles, stable dependencies, hubs, god
modules, chains, orphans and fan-in leave them out. Layer rules ignore them unless
`[rules] layers_count_bridges = true`.

## Precision

Bridges are checked against the code by sampling each kind and confidence and reading both
ends, and recall against a grep of the call shapes (`ipcRenderer.invoke|send|sendSync`,
`ipcMain.handle|on`, handler wrappers, `webContents.send`; for HTTP the client calls of the
wrappers and the mapping annotations) as the oracle. The fixture
`tests/fixtures/workspace-bridges` holds the traps (a constant channel next to a dynamic
one, a wrapper method, a call the preload does not expose, a class two members declare,
an undeclared member, a version skew; a Spring prefix written as a constant of another
member, a fetch wrapper three deep with a base known at run time, a call awaited with
type arguments, an imported axios instance and a wrapper over it, `/users/me` next to
`/users/{id}`, a router mounted twice, the wrong method, a route no member serves, a
foreign host, a URL built at run time) with golden bridge lists.

## Limits

- A channel or URL built at run time is counted, never matched.
- HTTP: Spring routes declared on an interface a controller implements, Spring functional
  routes, Kotlin controllers and `[[member.http.clients]]` overrides are not read; a
  TypeScript receiver typed only by `new T()` in a local or by a parameter type is not
  followed by the call resolver, so its calls reach their wrapper by name (`heuristic`).
- Two instances of one client class that point at different servers share their base: a
  call to a server outside the workspace through it is `no_server` only when its file
  reaches no route through that base.
- IPC through a framework that hides `ipcMain` / `ipcRenderer` behind its own API is not
  read unless it wraps them in a function that takes the channel as a parameter.
- Flows show the preload hop (renderer → exposed function → handler) instead of
  collapsing it.
