# cartograph

cartograph builds an architecture map of a code repository: folders, files, imports,
symbols, calls, entry points and git history, kept in a local SQLite index that follows the
code as it changes. Read the map in a local web UI, query it from the command line, enforce
architecture rules in CI, and serve it over MCP so that coding agents answer structural
questions ("who calls this?", "what breaks if I change this file?") in one call.

This package runs the prebuilt native binary for your platform: Windows x86_64, Linux
x86_64 (glibc), macOS arm64 and x86_64. npm installs only the one that matches, as an
optional dependency. Node.js 18 or newer.

## Run it

```sh
npx -y @munozca230/cartograph --help
npx -y @munozca230/cartograph serve --open     # web UI on http://127.0.0.1:7878
```

Or install it globally:

```sh
npm i -g @munozca230/cartograph
cartograph index
cartograph query overview
```

## MCP server

`cartograph mcp <repo>` is a stdio MCP server. After `npm i -g @munozca230/cartograph`,
register it in your MCP client; it then starts with no network access and no npx:

```json
{
  "mcpServers": {
    "cartograph": { "command": "cartograph", "args": ["mcp", "."] }
  }
}
```

For clients whose `mcp add` command takes `--`: `<client> mcp add cartograph -- cartograph mcp .`
(in PowerShell, quote it: `'--'`, or PowerShell drops it). Without a global install, use
`"command": "npx"` with `"args": ["-y", "@munozca230/cartograph", "mcp", "."]`.

`"."` is the directory the client starts the server in; put an absolute path to the
repository there otherwise. On native Windows `cartograph` and `npx` are `.cmd` scripts: a
client that does not start commands through a shell needs `"command": "cmd"` with
`"args": ["/c", "cartograph", "mcp", "."]` (or `["/c", "npx", "-y", "@munozca230/cartograph", "mcp", "."]`).

The index goes to `<repo>/.cartograph/` (add it to `.gitignore`), or elsewhere with
`--data-dir DIR`.

## More

Documentation, release archives and the source: <https://github.com/Munozca230/cartograph>.

Licensed under MIT OR Apache-2.0. The binary bundles third-party code under its own
licenses; the platform packages carry `THIRD_PARTY_NOTICES.md`.
