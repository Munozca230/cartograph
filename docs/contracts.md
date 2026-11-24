# Contracts: what both sides of a link expect

A client and the route it calls agree on a body; an MCP client and a server agree on the
tools and their parameters. Cartograph reads both sides from the source, compares them
where it can, and says what a change breaks. It never guesses: a shape it cannot read
whole is shown as unknown and never compared.

## HTTP bodies

For every HTTP bridge (see [bridges.md](bridges.md)) whose route takes a body or whose
client sends a typed one, the index keeps the client's type and the server's, with their
fields when they can be read.

**Server** (Spring): the `@RequestBody` parameter of the handler, with `@Valid` /
`@Validated` noted. A list (`List<T>`, `Set<T>`, `T[]`) is a list of `T`. Feign interfaces
are clients: their `@RequestBody` parameter is what they send.

**Client** (TypeScript): the type of what becomes the body, followed through the wrappers
the HTTP bridges already infer (`service → ApiClient.post → request → send → fetch`,
the body passed as an argument or in an options object, `{ body }`, `{ ...init }`,
`JSON.stringify(x)`, `x ? JSON.stringify(x) : undefined`). A type is known when the value
is:

- a parameter with a declared type (`create(dto: CreateUser)`);
- a local with a declared type (`const p: Payload = …`), or `x as T` / `x satisfies T`;
- a local made by a function or method of the same file with a declared return type
  (`const payload = this.toPayload(…)`, `Promise<T>` read as `T`), or `new T(…)`.

axios takes the body as its second argument or `data`, got and ky as `json`.

**Shapes.** Types are looked up where they are written: the file's own types, its imports
(`import type` too), its Java package and its glob imports; never by name elsewhere in the
repo. Only the types that take part in a bridge are read (their files are parsed again
after resolution), and only those are stored.

- Java (what Jackson reads into an object): the fields of a class or the components of a
  record, `static` ones left out; `@JsonProperty("x")` names one, `@JsonAlias` adds names,
  `@JsonIgnore` and `@JsonIgnoreProperties({…})` leave them out; the fields of the
  superclasses (of the repo) come first. A field counts when Jackson can set it: Lombok's
  `@Data`, `@Getter`, `@Setter` or `@Value` on the class or the field, a public field,
  `@JsonProperty`, or an accessor (`setX`, `getX`, `isX`) of the class. `@NotNull`,
  `@NotBlank` and `@NotEmpty` (Bean Validation, `javax` or `jakarta`) make a field
  required, only on a body Spring validates (`@Valid`), and never in a list (`@Valid
  List<T>` does not validate its elements unless the controller turns on method
  validation).
- TypeScript: interfaces (with what they extend), type aliases of object types,
  intersections, `Partial`, `Required`, `Readonly`, `Pick` and `Omit` with literal keys,
  a generic interface by its own fields (`Page<T>`); `name?:` is optional.

Unknown (never compared): a superclass or an extended type that is not a type of the repo
or cannot be found, `@JsonTypeInfo`, `@JsonSubTypes`, `@JsonNaming`, `@JsonDeserialize`,
`@JsonFormat` on the class, `@JsonUnwrapped`, `@JsonAnySetter`, `@JsonCreator`, an index
signature, a union, a computed key, a TS class, an object literal passed directly
(`post(url, { title })`), a value of no declared type, `String`, `Map`, `JsonNode` and
the like. A list is compared only with a list.

## Where contracts show up

`explain` of the file that declares a body type (the DTO, or the client type) lists, per
route, the client type and the server type with their files, the fields only the server
type has (marked required or optional) and the fields only the client type has:

```
contracts (HTTP bodies, client → server):
  POST /api/items: ItemPayload (desktop/src/api/ItemsApi.ts:5) → CreateItemDto (api/…/CreateItemDto.java:10)
    only the server type has: technology (optional)
    only the client type has: tags (UpdateItemDto of PATCH /api/items/{} reads it)
```

A field only the client sends is never an error: Jackson ignores unknown properties by
default. When one client type goes to several routes (a create and an update), the route
whose server type does read the field is named.

Finding:

| Finding | Severity | When |
|---|---|---|
| `contract_missing_field` | warning | a route's body type requires a field (validated, see above) that no client type sent to that route declares, every client shape of the route known whole |

## MCP tools

Tools declared in the source of a file that imports the MCP SDK
(`@modelcontextprotocol/…`):

- `server.tool(name, [description], [schema], [annotations], handler)` and
  `server.registerTool(name, { inputSchema }, handler)`, the schema a zod raw shape or a
  `z.object({…})` written as a literal, in the call or as a module constant of the file
  (`inputSchema.shape`); a name is a literal or a constant (also imported);
- tool lists written as `{ name: "x", inputSchema: { type: "object", properties: {…},
  required: [...] } }` (JSON schema literals);
- Rust (rmcp, in files that use it): methods marked `#[tool(…)]`, named by the method or
  the attribute's `name`, their parameters the fields of the `Parameters<T>` struct when
  it is in the same file (`Option<…>` or `#[serde(default)]` optional, `#[serde(rename)]`
  read; a struct that renames all its fields or flattens one is unknown).

A zod field is required unless its chain has `.optional()`, `.nullish()`, `.default()` or
`.catch()`, or is `z.optional(…)`; a field built by a helper (`labelField()`) has an
unknown requiredness. A schema that is not a literal (`config.input.shape`) leaves the
tool's parameters unknown.

## diff and check --base

Both sides are built alike (the base keeps its bodies and tools in its cached snapshot),
and `diff` lists **contract breaks**:

- a field the route's body type no longer reads (removed, or renamed: the one new field is
  named) that a client type sent to it still declares;
- an HTTP call that reached a route in the base and reaches none now (the route removed or
  renamed), when no new `http_unmatched_call` or `http_method_mismatch` finding already
  says so;
- an MCP tool removed, a parameter removed, a parameter newly required (optional in the
  base, or new and required).

An IPC channel that lost its handler is the `ipc_no_handler` finding, new in the diff; it
is not repeated. `check --base` lists the same breaks under "not failing": they never fail
the check.

## Limits

- Response types are not read (handlers usually answer with a generic envelope).
- Server bodies other than Spring's `@RequestBody` (Express, NestJS `@Body()`, Flask…)
  are not read; nor are Java clients' bodies other than Feign's (`RestTemplate`,
  `WebClient`) or Python's.
- A client type made by a function of another file is unknown (return types are read in
  the same file only).
- Jackson settings outside the class (a global naming strategy,
  `FAIL_ON_UNKNOWN_PROPERTIES`, mix-ins) and Kotlin are not read.
- Nested DTOs are compared by their field names only, not field by field inside.
