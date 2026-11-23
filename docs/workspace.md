# Workspaces: several repositories as one map

A system often lives in several repositories: a shared library, an API that uses it, a
desktop app and a tool server that call the API. A workspace maps them as one: one index,
one map, one MCP server, with imports resolved across repositories.

## The file

Put a `cartograph-workspace.toml` in the folder that holds the repositories (or anywhere:
members may live outside it). Cartograph reads it only in the folder it is pointed at; it
never looks for one above a folder, so `cartograph index` inside a member still maps that
repository alone.

```toml
[workspace]
name = "system"            # name of the map; default: the folder's name

[[member]]
name = "lib"               # [a-z][a-z0-9-]+, at least 2 characters
path = "shared-lib"        # relative to this file, or absolute; ../x or a worktree is fine

[[member]]
name = "api"
path = "backend-api"

[[member]]
name = "desktop"
path = "desktop-app"

# Rules over the whole workspace, in the grammar of cartograph.toml: paths start with a
# member's name.
[[layers]]
name = "lib"
paths = ["lib"]
allow = []

[[layers]]
name = "api"
paths = ["api"]
allow = ["lib"]

[[layers]]
name = "clients"
paths = ["desktop"]
forbid = ["api"]

[rules]
no_cycles = true
```

`cartograph workspace init [PATH]` proposes the file: one member per child folder that is a
git repository with code. Worktree copies of a sibling (`<repo>-worktrees`), folders named
like output or data (`logs`, `backups`…) and folders without code are left out, each with
why in a comment. It prints the file; `--write` writes it (`--force` to overwrite).

Member names cannot be id kinds or ecosystems (`file`, `dir`, `sym`, `npm`, `maven`,
`go`…), so `name:` never reads as anything else. Two members cannot share or nest folders.
Tables for later features (`[bridges]`, `[member.http]`, `produces`) are accepted and
ignored for now.

## Ids and paths

Each member is a top-level folder of the map named after the member, whatever its folder is
called on disk: `src/main/java/App.java` of member `api` is `file:api/src/main/java/App.java`.
Everywhere a path is accepted (CLI, MCP tools, HTTP API, `open_map`), so is the alias
`api:src/main/java/App.java` (also `file:api:…`, `sym:api:…#name`). Outputs always print the
canonical form.

A plain path (`src/main.ts`) is first a path of the map; else it is looked for in the
default member (`--member api`), then in every member: in one it resolves, in several it
is an `ambiguous` error listing the candidates (`api:src/main.ts, desktop:src/main.ts`).
An absolute path maps through the member whose folder holds it.

## What is indexed

Only the members: each is walked from its own folder with its own `.gitignore`, so the
parent's other folders (logs, backups, documents, worktree copies) are never read. Imports
resolve across members wherever the resolvers would within one repository: a Java class
by its package; and a reference that leaves a member's folder lands in the member whose
folder holds that place, whatever it is named: TypeScript and C relative imports, tsconfig
`extends`, `baseUrl` and `paths` (`"@lib/*": ["../shared-lib/src/*"]`), Go `replace`
directives, Cargo path dependencies. Each member's root plays
the part of a repository root for the resolvers (its compilation database, its Go root
package). A member's own `index.scip` is imported for it.

Each member keeps its own git history: churn, owners and co-change come from its own
repository, its recent window ends at its own newest commit, and a new commit in one member
re-reads only that member. A member without git is simply left without history.

## Rules and findings

Two kinds of layer sets judge the dependencies:

- the workspace's (`[[layers]]` of `cartograph-workspace.toml`), over every edge;
- each member's own `cartograph.toml`, rebased: its layers are named `<member>.<layer>`
  and their paths start with the member's folder, so they judge the edges inside that
  member. A violation reads `layer_violation:api.web->api.db`.

A member's `ignore` and `no_cycles` are merged in, rebased; its `max_fan_out` applies to
its files. Its `[thresholds]` and `[findings]` are not read: the workspace's apply.

God modules and bottlenecks are judged against the files of their own member (one
population per member); cycles, fan-out and orphans run on the whole graph, so a library
file used only by another member is not an orphan. `overview` at the root lists the file
pairs from one member to another.

## Commands

```sh
cartograph workspace init ~/src/system --write
cartograph index ~/src/system                   # data dir: ~/src/system/.cartograph
cartograph serve ~/src/system --open            # members are the top-level cards
cartograph query -C ~/src/system impact lib:src/main/java/com/acme/lib/UserDto.java
cartograph query -C ~/src/system --member api explain pom.xml
cartograph check ~/src/system
cartograph diff -C ~/src/system                 # each member against its own merge-base
cartograph check ~/src/system --base main       # each member resolves main
cartograph check ~/src/system --base main,api=feat-x
cartograph mcp ~/src/system
```

`infer` on a workspace prints layers in the workspace's grammar (paths start with a
member) for `cartograph-workspace.toml`; it never rewrites that file (`--write` refuses).
Run `infer --write` on a member's folder for that member's own `cartograph.toml`.
`--config` takes a `cartograph-workspace.toml` too (members and rules), or a plain
`cartograph.toml` (rules only, over the workspace's paths).

`diff` and `check --base` take one base per member, from each member's own repository:
a revision for all (each member resolves it; the default is each member's own merge-base
with its default branch), `member=rev` for one member, or both (`main,api=feat-x`). A
revision named for one member must exist there; a member without git, or without a
revision given for all, is compared with its working tree, and the base's label says so.

## MCP

`cartograph mcp <parent>` serves the whole workspace. From inside a member, point it at
the file and name the member that plain paths try first:

```json
{ "command": "cartograph",
  "args": ["mcp", "--workspace", "../cartograph-workspace.toml", "--member", "api"] }
```

The watcher follows each member's folder (with its own ignore rules and git dirs); when
the workspace file changes it watches the members' folders again (new ones in, removed ones
out) and re-indexes everything. `diff` and `check` take
`base: "main"` or `"api=feat,lib=main"`.

## Web map and HTTP API

`/api/meta` has `workspace.members`: each member's `name`, `path` (as written), `root`,
`git` (state of its last git pass), `branch`, `head` and `detached`. The map draws each
member's card in its own hue with its branch. A single repository's answers are unchanged.

## Limits

- One combined index: a change in any member re-resolves the whole workspace (the
  extraction cache keeps unchanged files from being parsed again).
- Calls and dependencies between members over HTTP, IPC or published packages
  (a Maven or npm dependency on another member) are not linked yet.
