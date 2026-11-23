//! N2: symbol definitions, call sites, import bindings and flow entry points.
//!
//! Extraction runs on the tree the import extractor already parsed (one parse per file),
//! with one tree-sitter query per language (`queries/*.scm`, capture names documented at
//! the top of `javascript.scm`). The Rust side reads a few captures and applies the
//! language's rules for containers, visibility, receivers and import bindings
//! ([`extract`]). What comes out ([`Code`]) is cached with the raw imports.
//!
//! Resolution ([`resolve`]) never runs from the cache: it needs the whole repo (import
//! sites, every definition) and runs after import resolution on every index.

mod extract;
pub mod resolve;
pub mod strexpr;

pub use extract::extract;

use crate::grammars::Grammar;
use cartograph_core::{EntryKind, SymbolKind};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use strexpr::{ConstDef, StrExpr};
use tree_sitter::Query;

/// A Rust call's path (`krate::m` of `krate::m::f()`) as `(file, inline-module scope,
/// path)`, to resolve like a `use` of it.
pub type RustCallPath = (String, Vec<String>, Vec<String>);

/// The file each Rust call path leads to, as import resolution found it (see
/// [`crate::resolve::rust::Resolver::path_file`]); paths that lead to no repo file are left
/// out. An input of [`resolve::resolve`].
pub type RustCallPaths = std::collections::HashMap<RustCallPath, String>;

/// Everything N2 needs from one file, before resolution. Field names are short: this is
/// stored per file in the extraction cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code {
    #[serde(rename = "d", default, skip_serializing_if = "Vec::is_empty")]
    pub defs: Vec<Def>,
    #[serde(rename = "c", default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Call>,
    #[serde(rename = "b", default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<Binding>,
    #[serde(rename = "e", default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<RawEntry>,
    /// Name of the definition a JS/TS `export default` names.
    #[serde(rename = "x", default, skip_serializing_if = "Option::is_none")]
    pub default_export: Option<String>,
    /// The file is an ES module (has `import` or `export`): an unqualified name comes from
    /// the file itself or an import, never from another file's globals.
    #[serde(rename = "m", default, skip_serializing_if = "std::ops::Not::not")]
    pub module: bool,
    /// Go: the file's package is `main`.
    #[serde(rename = "p", default, skip_serializing_if = "std::ops::Not::not")]
    pub package_main: bool,
    /// Python: the names of an explicit literal `__all__` (sorted), the only names a star
    /// import of the module brings, imported ones included. `None` without one, or when it
    /// is built in a way the extractor cannot read.
    #[serde(rename = "a", default, skip_serializing_if = "Option::is_none")]
    pub all: Option<Vec<String>>,
    /// Declared types of fields (Rust struct fields, TS/Java/C# class fields), for
    /// `self.field.m()` calls.
    #[serde(rename = "t", default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldType>,
    /// Rust `impl Trait for Type` blocks: `Type` has `Trait` as a supertype. Kept per file
    /// because the type is often defined in another one.
    #[serde(rename = "i", default, skip_serializing_if = "Vec::is_empty")]
    pub impls: Vec<TraitImpl>,
    /// GDScript: what the script itself extends (`extends Node`, `extends "res://a.gd"`, the
    /// quotes kept). The file is the class: its functions have no container.
    #[serde(rename = "g", default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<ScriptExtends>,
    /// Module-level string constants (TS/JS, Java, Python), for bridge keys written as a
    /// constant.
    #[serde(rename = "k", default, skip_serializing_if = "Vec::is_empty")]
    pub consts: Vec<ConstDef>,
    /// Electron IPC channel sites (TS/JS).
    #[serde(rename = "ic", default, skip_serializing_if = "Vec::is_empty")]
    pub ipc: Vec<IpcSite>,
    /// Functions an Electron preload exposes to the renderer (`exposeInMainWorld`).
    #[serde(rename = "ex", default, skip_serializing_if = "Vec::is_empty")]
    pub exposes: Vec<Exposure>,
    /// Calls on a world a preload may expose (`window.api.getUser()`).
    #[serde(rename = "wu", default, skip_serializing_if = "Vec::is_empty")]
    pub worlds: Vec<WorldUse>,
    /// HTTP routes a server declares (Spring mappings, Express-style registrations).
    #[serde(rename = "hr", default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<HttpRoute>,
    /// HTTP client calls (`fetch`, axios, RestTemplate, WebClient, Feign, requests…).
    #[serde(rename = "hc", default, skip_serializing_if = "Vec::is_empty")]
    pub clients: Vec<HttpClient>,
    /// Express-style routers mounted under a prefix (`app.use("/v1", router)`).
    #[serde(rename = "hm", default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<HttpMount>,
    /// HTTP client instances with their base URL (`const api = axios.create({baseURL})`).
    #[serde(rename = "hi", default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<HttpInstance>,
    /// String-key sites (TS/JS, Java, Python): enum constants, switch cases,
    /// registrations, catalog entries and other uses of a key-like string or constant.
    /// String constants are in [`Code::consts`].
    #[serde(rename = "ky", default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<RawKey>,
    /// Spring beans a Java file declares: stereotype classes and `@Bean` methods.
    #[serde(rename = "sb", default, skip_serializing_if = "Vec::is_empty")]
    pub beans: Vec<RawBean>,
    /// Where Spring injects beans in a Java file: fields, constructor and setter
    /// parameters, `@Bean` method parameters.
    #[serde(rename = "si", default, skip_serializing_if = "Vec::is_empty")]
    pub injections: Vec<RawInjection>,
    /// Persistent classes a Java file declares (`@Document`, `@Entity`, `@Table`).
    #[serde(rename = "de", default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<RawEntity>,
    /// Spring Data repository interfaces of a Java file (`extends MongoRepository<E, Id>`).
    #[serde(rename = "dr", default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<RawRepo>,
    /// Calls that read or write a store by name or entity class (`getCollection("x")`,
    /// `mongoTemplate.find(q, E.class)`, `em.find(E.class, id)`).
    #[serde(rename = "ds", default, skip_serializing_if = "Vec::is_empty")]
    pub sinks: Vec<RawSink>,
}

/// A Spring bean: a class with a stereotype annotation (`@Component`, `@Service`,
/// `@Repository`, `@Controller`, `@RestController`, `@Configuration`) or a `@Bean` method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawBean {
    #[serde(rename = "l")]
    pub line: u32,
    /// The bean's type as written: the annotated class, or the `@Bean` method's return
    /// type (generic arguments removed).
    #[serde(rename = "t")]
    pub ty: String,
    /// `Component`, `Service`, … or `Bean`.
    #[serde(rename = "s")]
    pub stereotype: String,
    /// Spring's default name (the class name decapitalized, the `@Bean` method's name),
    /// when none is given.
    #[serde(rename = "n")]
    pub default_name: String,
    /// The names given (`@Service("orders")`, `@Bean({"orders", "alias"})`), literals or
    /// constants.
    #[serde(rename = "a", default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<StrExpr>,
    /// A `@Bean` method (qualified name) that makes it.
    #[serde(rename = "f", default, skip_serializing_if = "Option::is_none")]
    pub factory: Option<String>,
    #[serde(rename = "p", default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
    /// `@Qualifier("x")` values on the class or `@Bean` method.
    #[serde(rename = "q", default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<StrExpr>,
}

/// A place where Spring injects beans of a type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawInjection {
    #[serde(rename = "l")]
    pub line: u32,
    /// The class that receives it (its name, nested classes by their own name).
    #[serde(rename = "c")]
    pub class: String,
    /// `field`, `constructor`, `setter`, `method` (another `@Autowired` method) or `bean`
    /// (a parameter of a `@Bean` method).
    #[serde(rename = "h")]
    pub how: String,
    /// The field or parameter name.
    #[serde(rename = "m")]
    pub member: String,
    /// It is a parameter that keeps what it is given (not a field, as Lombok's are).
    #[serde(rename = "p", default, skip_serializing_if = "std::ops::Not::not")]
    pub param: bool,
    /// What holds the beans: `List`, `Set`, `Map`, `Collection`, `ObjectProvider`,
    /// `Optional`, `[]`…; empty for one bean.
    #[serde(rename = "w", default, skip_serializing_if = "String::is_empty")]
    pub wrapper: String,
    /// The bean type as written.
    #[serde(rename = "t")]
    pub ty: String,
    /// `@Qualifier("x")`, `@Named("x")`.
    #[serde(rename = "q", default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<StrExpr>,
    /// `@Resource`: the bean name it asks for, and whether it is the default one (the
    /// field's, the setter's property).
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<(StrExpr, bool)>,
    /// The fields that hold what is injected here, as `(name, line)`: the field itself, or
    /// those the parameter is assigned to.
    #[serde(rename = "f", default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, u32)>,
}

/// A persistent class: `@Document(collection = "x")` (Mongo), `@Entity` / `@Table(name =
/// "x")` (JPA).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEntity {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "c")]
    pub class: String,
    /// `mongo`, `jpa`, `jdbc` or `cassandra`.
    #[serde(rename = "k")]
    pub store: String,
    /// The collection or table as written; `None`: the store's default for the class.
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub name: Option<StrExpr>,
}

/// A Spring Data repository interface: `interface R extends MongoRepository<E, Id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRepo {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "c")]
    pub class: String,
    /// The Spring Data interface (`MongoRepository`, `JpaRepository`, `CrudRepository`…).
    #[serde(rename = "b")]
    pub base: String,
    /// The entity type as written.
    #[serde(rename = "e")]
    pub entity: String,
    /// The store the interface implies; empty: the entity's.
    #[serde(rename = "k", default, skip_serializing_if = "String::is_empty")]
    pub store: String,
}

/// A call that reads or writes a store directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSink {
    #[serde(rename = "l")]
    pub line: u32,
    /// `mongo` or `jpa`.
    #[serde(rename = "k")]
    pub store: String,
    /// The call as people read it: `getCollection`, `mongoTemplate.save`, `em.find`.
    #[serde(rename = "w")]
    pub what: String,
    /// The collection or table named in the call.
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub name: Option<StrExpr>,
    /// The entity class named in the call (`Order.class`).
    #[serde(rename = "e", default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}

/// What a string-key site is (see [`RawKey`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyForm {
    /// An enum constant (Java, Python): the key is its string argument or value, else its
    /// name.
    #[serde(rename = "e")]
    Enum,
    /// A `case` label of a switch (or a Python `match`).
    #[serde(rename = "c")]
    Case,
    /// A call that binds the key to code: `register("K", handler)`,
    /// `new ActionBinding(K, Ops::run)`, `map.put("K", new Handler())`, a
    /// registering decorator.
    #[serde(rename = "r")]
    Register,
    /// An annotation that names a class (`@Component("K")`, `@Named("K")`).
    #[serde(rename = "a")]
    Annotation,
    /// An entry of a catalog: an element of an array of strings or of objects (by their
    /// `name`, `id`, `key`…), a key of an object or dictionary keyed by strings, an
    /// element of `List.of`/`Set.of`, a key of `Map.of`/`put`.
    #[serde(rename = "l")]
    Entry,
    /// Any other key-like string literal or constant reference.
    #[serde(rename = "u")]
    Use,
    /// A key-like word of a comment that is written as a key (`READ-FILE`, `users:get`,
    /// `READ_FILE`): the file knows the key, even where no code has it.
    #[serde(rename = "m")]
    Comment,
}

/// A string-key site found in a file, before constants are read (matched after
/// resolution, `symbols::resolve::keys`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawKey {
    #[serde(rename = "f")]
    pub form: KeyForm,
    /// The key as written: a literal, or a constant ([`Part::Ref`](strexpr::Part::Ref)
    /// `Steps.OPEN`, `READ_FILE`).
    #[serde(rename = "v")]
    pub value: StrExpr,
    #[serde(rename = "l")]
    pub line: u32,
    /// What holds it, for people and for grouping: the enum (`Action`), the switch
    /// (`switch in Runner.run`), the registering class and callee
    /// (`FileActionBindings: new ActionBinding`), the catalog's declaration and path
    /// (`ACTION_CATEGORIES[].actions[].name`), the annotation (`@Component`).
    #[serde(rename = "c", default, skip_serializing_if = "String::is_empty")]
    pub container: String,
    /// First line of the container (0: the container spans files, as an annotation's).
    #[serde(rename = "k", default, skip_serializing_if = "is_zero_u32")]
    pub container_line: u32,
    /// An enum constant's name (`Action.READ_FILE`).
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The lines of the code a case runs (or a registered lambda), whose calls are what
    /// the key runs.
    #[serde(rename = "b", default, skip_serializing_if = "Option::is_none")]
    pub body: Option<(u32, u32)>,
    /// What a registration or annotation binds the key to (`Ops::run`, `Foo.class`, the
    /// annotated class), resolved like calls.
    #[serde(rename = "h", default, skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<Call>,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// How an HTTP call gives its method.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MethodExpr {
    /// `GET`, `POST`… (upper case).
    #[serde(rename = "l")]
    Lit(String),
    /// The n-th parameter of the function around the call is the method.
    #[serde(rename = "p")]
    Param(u8),
    /// The `method` of the n-th parameter (an options object: `fetch(url, init)`).
    #[serde(rename = "o")]
    Opts(u8),
    /// Not known.
    #[serde(rename = "u")]
    Unknown,
}

/// An HTTP route a server declares: every method × path × prefix of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRoute {
    #[serde(rename = "l")]
    pub line: u32,
    /// `spring`, `express`.
    #[serde(rename = "f")]
    pub lib: String,
    /// The declaration as people read it: `@GetMapping`, `router.get`.
    #[serde(rename = "w")]
    pub what: String,
    /// Upper case; empty for any method.
    #[serde(rename = "m", default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    /// The paths of the handler (empty: the prefix alone).
    #[serde(rename = "p", default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<StrExpr>,
    /// Class-level prefixes (Spring `@RequestMapping` on the controller).
    #[serde(rename = "x", default, skip_serializing_if = "Vec::is_empty")]
    pub prefixes: Vec<StrExpr>,
    /// The receiver of an Express-style registration (`app`, `router`, `this.router`).
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub recv: Option<String>,
    /// The handler is a definition of this file (qualified name).
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    /// A handler passed by reference.
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<Call>,
}

/// An HTTP client call (or a Feign method): its URL and method as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpClient {
    #[serde(rename = "l")]
    pub line: u32,
    /// `fetch`, `axios`, `got`, `ky`, `resttemplate`, `webclient`, `httpclient`, `feign`,
    /// `requests`, `httpx`.
    #[serde(rename = "f")]
    pub lib: String,
    /// The call as people read it: `fetch`, `axios.get`, `restTemplate.exchange`.
    #[serde(rename = "w")]
    pub what: String,
    #[serde(rename = "m")]
    pub method: MethodExpr,
    #[serde(rename = "u")]
    pub url: StrExpr,
    /// The base URL of the instance or client the call goes through.
    #[serde(rename = "b", default, skip_serializing_if = "Option::is_none")]
    pub base: Option<StrExpr>,
    /// A receiver imported from another file: a client only if that file creates an
    /// instance of this name ([`HttpInstance`]).
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub recv: Option<String>,
    /// The definition the endpoint stands for (a Feign method), qualified.
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    /// With a parameter in the URL or the method: the last line of the function the
    /// parameters belong to (that function is a wrapper only when a definition ends there).
    #[serde(rename = "e", default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
}

/// `recv.use(prefix, target)`: the routes of `target` (a router) under `prefix`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpMount {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "r")]
    pub recv: String,
    #[serde(rename = "p")]
    pub prefix: StrExpr,
    #[serde(rename = "t")]
    pub target: String,
}

/// An HTTP client instance: `const api = axios.create({ baseURL })`, a WebClient built with
/// `.baseUrl(…)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpInstance {
    #[serde(rename = "l")]
    pub line: u32,
    /// As calls name it: `api`, `this.http` (Java: the field or variable).
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "f")]
    pub lib: String,
    #[serde(rename = "b", default, skip_serializing_if = "Option::is_none")]
    pub base: Option<StrExpr>,
}

/// A property of an object literal passed as an argument (`{ method: "POST" }`): `key` is
/// `method` or `url`, or `...` for a spread (`{ ...init }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptArg {
    #[serde(rename = "i")]
    pub arg: u8,
    #[serde(rename = "k")]
    pub key: String,
    #[serde(rename = "v")]
    pub value: StrExpr,
}

/// What an Electron IPC call does with its channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IpcOp {
    /// `ipcMain.handle` / `handleOnce`: serves `invoke`.
    #[serde(rename = "h")]
    Handle,
    /// `ipcMain.on` / `once`: serves `send`.
    #[serde(rename = "o")]
    On,
    /// `ipcRenderer.invoke`.
    #[serde(rename = "i")]
    Invoke,
    /// `ipcRenderer.send`.
    #[serde(rename = "s")]
    Send,
    /// `ipcRenderer.sendSync`.
    #[serde(rename = "y")]
    SendSync,
    /// `ipcRenderer.on` / `once`: listens to what the main process pushes.
    #[serde(rename = "l")]
    Listen,
    /// `webContents.send`: the main process pushes to a renderer.
    #[serde(rename = "p")]
    Push,
}

impl IpcOp {
    /// It serves a channel (else it uses one).
    pub fn provides(self) -> bool {
        matches!(self, IpcOp::Handle | IpcOp::On | IpcOp::Listen)
    }

    /// The method as written, for labels.
    pub fn label(self) -> &'static str {
        match self {
            IpcOp::Handle => "ipcMain.handle",
            IpcOp::On => "ipcMain.on",
            IpcOp::Invoke => "ipcRenderer.invoke",
            IpcOp::Send => "ipcRenderer.send",
            IpcOp::SendSync => "ipcRenderer.sendSync",
            IpcOp::Listen => "ipcRenderer.on",
            IpcOp::Push => "webContents.send",
        }
    }
}

/// An Electron IPC call on a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcSite {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "o")]
    pub op: IpcOp,
    #[serde(rename = "c")]
    pub channel: StrExpr,
    /// A handler or listener passed by reference (`getUser`, `this.onSave.bind(this)`);
    /// `None` for one written inline (the function around the call runs it).
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<Call>,
}

/// A function a preload exposes: `world.path` (`api.getUser`, `api.updater.check`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "w")]
    pub world: String,
    #[serde(rename = "p")]
    pub path: String,
    /// The definition of the file that runs it (qualified name), when there is one.
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
}

/// A renderer call on a world: `window.world.path(…)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldUse {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "w")]
    pub world: String,
    #[serde(rename = "p")]
    pub path: String,
    /// Reached through a local alias (`const api = window.api`).
    #[serde(rename = "a", default, skip_serializing_if = "std::ops::Not::not")]
    pub alias: bool,
    /// The string-like arguments (see [`Call::args`]).
    #[serde(rename = "g", default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Option<StrExpr>>,
}

/// A supertype of a class, struct, interface or trait, as written in its definition (generic
/// arguments removed): a base class, an implemented or extended interface, a supertrait, a
/// Go embedded type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Super {
    /// `Base`, `ns.Base`, `a::b::Trait`; anything that is not a name (`mixin(Base)`) is kept
    /// as written and never resolves.
    #[serde(rename = "n")]
    pub name: String,
    /// It cannot hide a method of a later supertype: an implemented interface, a trait, an
    /// embedded Go interface, or a base known to add no methods (Python `object`, `ABC`,
    /// `Generic`, `Protocol`). When it is not a type of the repo, the walk over supertypes
    /// goes past it; past any other supertype of no repo type it stops (the method may come
    /// from there).
    #[serde(rename = "i", default, skip_serializing_if = "std::ops::Not::not")]
    pub iface: bool,
}

/// Rust `impl Trait for Type` (both as written, generic arguments removed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitImpl {
    #[serde(rename = "t")]
    pub ty: String,
    #[serde(rename = "r")]
    pub tr: String,
    #[serde(rename = "l")]
    pub line: u32,
}

/// GDScript `extends` of the script itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptExtends {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "l")]
    pub line: u32,
}

/// A field whose declared type is one named type: `guard: Guard`, `guard: Arc<Guard>`,
/// `&'a Guard`, `private guard: Guard`. Wrappers a method call sees through (`&`, `Box`,
/// `Arc`, `Rc`) are removed; anything else (`Option<T>`, `Vec<T>`, a path, a union) is
/// left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldType {
    /// The struct or class that declares the field.
    #[serde(rename = "c")]
    pub container: String,
    #[serde(rename = "n")]
    pub name: String,
    /// The type's name as written (`Guard`).
    #[serde(rename = "t")]
    pub ty: String,
}

/// A definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Def {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "k")]
    pub kind: SymbolKind,
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Line of the name.
    #[serde(rename = "l")]
    pub line: u32,
    /// First line of the definition (decorators and annotations included).
    #[serde(rename = "s")]
    pub start: u32,
    #[serde(rename = "e")]
    pub end: u32,
    #[serde(rename = "x", default, skip_serializing_if = "std::ops::Not::not")]
    pub exported: bool,
    /// Go: the method's receiver variable (`s` in `func (s *Server) Run()`), a `self`.
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// The method implements or overrides an interface or trait method (Rust
    /// `impl Trait for`, Java `@Override`, C# `override`): library types share its name,
    /// so `obj.m()` is never matched to it by name alone.
    #[serde(rename = "o", default, skip_serializing_if = "std::ops::Not::not")]
    pub overrides: bool,
    /// A class that may declare methods it does not define (a TS `abstract` class, or an
    /// ambient one: `declare class`): a subclass defines them, as for an interface.
    #[serde(rename = "a", default, skip_serializing_if = "std::ops::Not::not")]
    pub declares: bool,
    /// Supertypes of a class, struct, interface or trait, in declared order (`extends`
    /// before `implements`). Rust trait impls and a GDScript script's own `extends` are on
    /// [`Code`].
    #[serde(rename = "u", default, skip_serializing_if = "Vec::is_empty")]
    pub supers: Vec<Super>,
    /// A GDScript `class_name` statement: the class is the whole script.
    #[serde(rename = "g", default, skip_serializing_if = "std::ops::Not::not")]
    pub class_name: bool,
    /// A member of a JS object literal (`api.start` of `const api = {…}`). Reached through
    /// the object (`api.start()`, `this.start()`, an import of `api`), never by its name
    /// alone: a preload bridge repeats the names of the services it wraps, and would make
    /// their `svc.m()` calls ambiguous.
    #[serde(rename = "j", default, skip_serializing_if = "std::ops::Not::not")]
    pub object: bool,
    /// A Java method's modifiers and number of parameters.
    #[serde(rename = "v", default, skip_serializing_if = "Option::is_none")]
    pub java: Option<JavaMods>,
}

/// What dispatch needs of a Java method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaMods {
    /// Parameters; -1 with a variable number.
    #[serde(rename = "n")]
    pub arity: i32,
    /// No body.
    #[serde(rename = "a", default, skip_serializing_if = "std::ops::Not::not")]
    pub is_abstract: bool,
    #[serde(rename = "s", default, skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
    #[serde(rename = "p", default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

impl Def {
    /// `Container.name` or `name`.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) => format!("{c}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// How a call names what it calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Form {
    /// `f()`, `<Comp/>`.
    #[serde(rename = "p")]
    Plain,
    /// `this.f()`, `self.f()`, `Self::f()`.
    #[serde(rename = "s")]
    SelfCall,
    /// `x.f()`; `qualifier` is `x` when it is a plain identifier.
    #[serde(rename = "m")]
    Member,
    /// `Type::f()` / `module::f()` (Rust, C++).
    #[serde(rename = "q")]
    Path,
    /// `new T()`.
    #[serde(rename = "n")]
    New,
    /// `super.f()`, `super().f()`, `base.F()`: a method of the caller's supertypes.
    /// `qualifier` is the class Python's `super(Class, self)` names.
    #[serde(rename = "u")]
    Super,
}

/// A call site (or a reference resolved like one, such as a route handler).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "f")]
    pub form: Form,
    #[serde(rename = "q", default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
    /// Rust `a::b::f()`: the whole path before the name (`["a", "b"]`; `crate`, `super`,
    /// `self` and an empty first segment for a leading `::` kept), when every segment is a
    /// plain identifier. Resolved like a `use` of it ([`RustCallPaths`]).
    #[serde(rename = "p", default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// Rust: the inline modules around the call (`mod tests { … }`), outermost first: what
    /// `super` and `self` in `path` are relative to.
    #[serde(rename = "m", default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// `self.field.f()` / `this.field.f()` ([`Form::Member`]): the field, resolved through
    /// its declared type ([`Code::fields`]).
    #[serde(rename = "v", default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Java `x.f()` ([`Form::Member`], `qualifier` `x`): the one named type `x` is declared
    /// with where the call is: a parameter, a local variable (`var x = new T()` included)
    /// or a field of an enclosing class (`private final T x;`, the Spring injection idiom);
    /// for a lambda or `for (var x : …)` variable, the element type of the collection, the
    /// value type of the map or the held type it reads (`list.stream().map(x -> x.f())`,
    /// `opt.ifPresent(x -> x.f())`); for `x.get(i).f()` and the like (no `qualifier`), what
    /// `x` gives out. `None` when `x` is declared otherwise (a collection itself, an array,
    /// `var`, a lambda parameter of anything else) or not at all: the other rules decide.
    #[serde(rename = "y", default, skip_serializing_if = "Option::is_none")]
    pub recv_type: Option<String>,
    /// [`Call::recv_type`] is what a collection or holder gives out: where it leads to no
    /// method, the other rules decide.
    #[serde(rename = "z", default, skip_serializing_if = "std::ops::Not::not")]
    pub recv_elem: bool,
    /// Java `super.f()` and `Type.super.f()`.
    #[serde(rename = "w", default, skip_serializing_if = "std::ops::Not::not")]
    pub sup: bool,
    /// Java `x.f()`: the variable the receiver is, or comes from (the collection a loop or
    /// lambda variable reads, `x` of `x.get(0).f()`), as it is declared.
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub recv_decl: Option<RecvDecl>,
    /// TS/JS: the string-like arguments among the first ones (literals, templates,
    /// constant-looking names), `None` for the others; empty when none is. A bridge reads
    /// them when the called function passes one on as a channel or URL (a wrapper).
    #[serde(rename = "a", default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Option<StrExpr>>,
    /// With [`Call::args`], the last argument when it is a function reference (a handler
    /// handed to a wrapper).
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<Box<Call>>,
    /// Properties of object-literal arguments a bridge reads (`{ method: "POST" }`).
    #[serde(rename = "o", default, skip_serializing_if = "Vec::is_empty")]
    pub opts: Vec<OptArg>,
    /// With a parameter among [`Call::args`] or [`Call::opts`]: the last line of the
    /// function the parameters belong to (see [`HttpClient::frame`]).
    #[serde(rename = "e", default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    /// With [`Call::args`], how many arguments the call passes (the last ones that are not
    /// string-like are left out of `args`, not missing).
    #[serde(rename = "k", default, skip_serializing_if = "is_zero")]
    pub argc: u8,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// The declaration a Java receiver comes from (see [`Call::recv_decl`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecvDecl {
    #[serde(rename = "n")]
    pub name: String,
    /// Line of the declarator or parameter: tells a field from a parameter or local that
    /// shadows it.
    #[serde(rename = "l")]
    pub line: u32,
    /// Read element by element (a stream, `forEach`, a loop).
    #[serde(rename = "e", default, skip_serializing_if = "std::ops::Not::not")]
    pub each: bool,
}

/// What an import binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BindKind {
    /// `import { a as b }`, `from m import a as b`, `use m::a as b`, `import a.b.C;`.
    #[serde(rename = "n")]
    Named,
    /// `import X from` (the target's default export).
    #[serde(rename = "d")]
    Default,
    /// `import * as ns`, `import m as ns`, a Go package, `mod m;`, `const X = preload(…)`.
    #[serde(rename = "s")]
    Namespace,
    /// Every name of the target: `export * from`, `from m import *`, `use m::*`,
    /// `import a.b.*;`, `using N;`, a Go dot import.
    #[serde(rename = "g")]
    Glob,
}

/// One name an import brings into the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Line of the import statement (its import sites share it).
    #[serde(rename = "l")]
    pub line: u32,
    /// Local name (empty for a glob).
    #[serde(rename = "n")]
    pub local: String,
    /// Name in the target, when it differs or matters (`a` in `import { a as b }`).
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<String>,
    #[serde(rename = "k")]
    pub kind: BindKind,
    /// The specifier as the import resolver records it, to pick the right site when a line
    /// has several.
    #[serde(rename = "s", default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Java static import: the class that holds `imported`.
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

/// A flow entry point found in a file, before resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEntry {
    #[serde(rename = "l")]
    pub line: u32,
    #[serde(rename = "k")]
    pub kind: EntryKind,
    #[serde(rename = "n")]
    pub name: String,
    /// The handler, named like a call (`getUser`, `ctrl.getUser`).
    #[serde(rename = "h", default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<Call>,
    /// The handler is a definition of this file (qualified name).
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    /// Kept only when the file imports a library of this family (see
    /// [`resolve::gate_matches`]); `None` for patterns that identify themselves.
    #[serde(rename = "g", default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

const JS_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/javascript-only.scm"),
    "\n",
    include_str!("queries/jsx.scm")
);
const TS_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/typescript.scm")
);
const TSX_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/typescript.scm"),
    "\n",
    include_str!("queries/jsx.scm")
);
const PY_QUERY: &str = include_str!("queries/python.scm");
const RUST_QUERY: &str = include_str!("queries/rust.scm");
const GO_QUERY: &str = include_str!("queries/go.scm");
const JAVA_QUERY: &str = include_str!("queries/java.scm");
const CS_QUERY: &str = include_str!("queries/csharp.scm");
const C_QUERY: &str = include_str!("queries/c.scm");
const CPP_QUERY: &str = concat!(
    include_str!("queries/c.scm"),
    "\n",
    include_str!("queries/cpp.scm")
);
const GD_QUERY: &str = include_str!("queries/gdscript.scm");

/// Every symbol query, for the extraction-cache key.
pub const QUERIES: [&str; 11] = [
    JS_QUERY, TS_QUERY, TSX_QUERY, PY_QUERY, RUST_QUERY, GO_QUERY, JAVA_QUERY, CS_QUERY, C_QUERY,
    CPP_QUERY, GD_QUERY,
];

/// Key of a File node's `meta` for the `uses` edges to types of its own Java package that
/// it names without an import: `{"package": "a.b", "files": ["file:…"]}` (written by import
/// resolution). Call resolution takes those files into the file's scope; findings treat a
/// cycle made only of them as one inside a package (the package is the unit).
pub const SAME_PACKAGE_META: &str = "same_package_uses";

/// Bump when extraction returns something else for the same input (part of the
/// extraction-cache key with the queries themselves). 8: field types and `self.field.f()`;
/// 9: a function's or class's own `__all__` leaves the module's known; 10: `__all__[i] = …`
/// and `del __all__[i]` in a function mutate the module's list; 11: `self.field.m()` on a
/// concrete field type without `m` is unresolved again (a resolution rule, bumped so every
/// stored index is rebuilt with it); 12: a Rust `self.field.m()` finds an `impl` anywhere
/// in the type's crate, else the other rules decide; 13: [`Def::declares`] (TS abstract and
/// ambient classes); 14: supertypes ([`Def::supers`], [`Code::impls`], [`Code::extends`]),
/// `super(Class, self)`; 15: [`Def::class_name`] (an inner GDScript class on line 1 is not
/// the script); 16: methods and function properties of named JS object literals
/// (module-level `const x = {…}`, `export default {…}`, `module.exports = {…}`,
/// `exposeInMainWorld("api", {…})`) as `x.prop`; 17: Electron `ipcMain.handle` / `on`
/// entries ([`cartograph_core::EntryKind::Ipc`]); 18: [`Call::recv_type`] (Java receivers
/// typed by their declaration); 19: a receiver's declaration counts only in its block, and
/// an inherited field may hide an outer one; 20: bridges: string constants
/// ([`Code::consts`]), IPC sites, exposures and world uses, [`Call::args`]; 21: an exposed
/// property built elsewhere (a call, a member, a bind) is an exposure; `ipc` is a receiver
/// only as `webContents.ipc`; 22: `await x.f<T>()` is a call of `x.f` (the grammar reads
/// `await x.f` as the function of a call with type arguments); 23: HTTP routes, clients,
/// mounts and instances, [`Call::opts`], call arguments of Java and Python, parameters of
/// the definition around (locals read through); 24: [`Call::argc`], options of a local
/// object, `${base}/${path}` wrappers, Flask variables, `String.format` placeholders, the
/// parameters of an object's functions inside a callback; 25: string-key sites
/// ([`Code::keys`]); 26: Java receivers typed through a collection's elements (stream
/// lambdas, `for (var …)`, `list.get(i)`), Spring beans and injection points
/// ([`Code::beans`], [`Code::injections`]), entities, repositories and store calls
/// ([`Code::entities`], [`Code::repos`], [`Code::sinks`]); 27: the declaration a Java
/// receiver comes from ([`Call::recv_decl`]), bean names and qualifiers as expressions,
/// fields an injected parameter is assigned to, raw generic injections, Spring Data
/// repositories as beans, entities by the package of their annotation; 28: the name a
/// `@Resource` asks for, the bean names of nested classes and generated mappers; 29:
/// parameters given another value, names of repository interfaces, Cassandra and R2DBC
/// repositories; 30: modifiers and parameters of Java methods, `super`
/// calls; 31: `Outer.Inner x` is not typed again;
/// 32: [`Call::recv_elem`].
pub const REVISION: u32 = 32;

/// A class name as a Java bean or a Mongo collection takes it by default (`OrderService` →
/// `orderService`; `URLMaker`, two capitals first, stays as it is).
pub fn decapitalize(name: &str) -> String {
    let mut cs = name.chars();
    match (cs.next(), cs.next()) {
        (Some(a), Some(b)) if a.is_uppercase() && b.is_uppercase() => name.to_string(),
        (Some(a), _) => a.to_lowercase().chain(name.chars().skip(1)).collect(),
        _ => String::new(),
    }
}

/// Query source for a grammar.
pub fn query_source(grammar: Grammar) -> &'static str {
    match grammar {
        Grammar::TypeScript => TS_QUERY,
        Grammar::Tsx => TSX_QUERY,
        Grammar::JavaScript => JS_QUERY,
        Grammar::Python => PY_QUERY,
        Grammar::Rust => RUST_QUERY,
        Grammar::Go => GO_QUERY,
        Grammar::Java => JAVA_QUERY,
        Grammar::CSharp => CS_QUERY,
        Grammar::C => C_QUERY,
        Grammar::Cpp => CPP_QUERY,
        Grammar::GdScript => GD_QUERY,
    }
}

/// Compiled symbol query for a grammar (once, shared by every thread).
pub fn query(grammar: Grammar) -> Result<&'static Query, String> {
    static CELLS: [OnceLock<Result<Query, String>>; 11] = [const { OnceLock::new() }; 11];
    let i = Grammar::ALL.iter().position(|g| *g == grammar).unwrap_or(0);
    CELLS[i]
        .get_or_init(|| {
            Query::new(&grammar.language(), query_source(grammar))
                .map_err(|e| format!("symbol query for {}: {e}", grammar.name()))
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_symbol_query_compiles() {
        for g in Grammar::ALL {
            query(g).unwrap();
        }
    }
}
