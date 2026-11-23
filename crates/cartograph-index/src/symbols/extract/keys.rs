//! Raw string-key sites ([`RawKey`]) of TS/JS, Java and Python files: where a key such as
//! an action name (`"READ-FILE"`) or a step name is declared as an enum constant, run by a
//! switch case, bound to code by a registration, listed by a catalog, or used anywhere
//! else. String constants (`static final String`, TS `const`, `as const` objects, string
//! enums, Python module constants) are [`crate::symbols::Code::consts`]. Read after
//! resolution (`symbols::resolve::keys`): constants through imports, handlers like calls.
//!
//! What counts as a key: a string of 2 to 64 characters without spaces, quotes, brackets
//! or format holes, that is not a path (`/a`, `./a`) or a URL. A constant reference is a
//! dotted name whose last part is upper case (`Steps.OPEN`, `IPC.SAVE_DOC`).

use super::bridge::{dotted, function_ref, unwrap_expr};
use super::{Lang, end_line, handler_ref, line, named_args, string_value, text};
use crate::symbols::strexpr::{Part, StrExpr, key_like, looks_constant};
use crate::symbols::{Call, Form, KeyForm, RawKey};
use std::collections::HashSet;
use tree_sitter::Node;

/// Properties that name an element of a catalog array, in the order looked for.
const NAME_PROPS: &[&str] = &[
    "name", "id", "key", "action", "type", "value", "code", "command", "channel",
];

/// Calls that build a list or a set of their arguments.
const LIST_CALLS: &[&str] = &["of", "asList", "copyOf", "ofEntries"];

/// Receivers of [`LIST_CALLS`] that are collections (`List.of`, `ImmutableMap.of`,
/// `Arrays.asList`): `Path.of`, `Stream.of`, `EnumSet.of` and RxJS's `of` build none.
const COLLECTIONS: &[&str] = &[
    "List",
    "Set",
    "Map",
    "Arrays",
    "ImmutableList",
    "ImmutableSet",
    "ImmutableMap",
    "ImmutableSortedSet",
    "ImmutableSortedMap",
    "SortedSet",
    "SortedMap",
    "NavigableSet",
    "NavigableMap",
];

/// Annotations whose string argument names the class they are on.
const NAMING_ANNOTATIONS: &[&str] = &[
    "Component",
    "Service",
    "Repository",
    "Controller",
    "RestController",
    "Named",
    "Qualifier",
    "JsonTypeName",
];

/// Words that make a call or a decorator a registration when another argument is a
/// name (`register("K", handler)`); with a function written in place any call is one.
const REGISTER_WORDS: &[&str] = &[
    "register",
    "handle",
    "handler",
    "on",
    "bind",
    "provide",
    "define",
    "add",
    "action",
    "command",
    "subscribe",
    "listen",
];

/// Calls of test frameworks and of the DOM that take a name and a function: a test's
/// title, a mocked module, an event of the page are no keys a program registers.
const NOT_REGISTRATIONS: &[&str] = &[
    "describe",
    "it",
    "test",
    "suite",
    "context",
    "bench",
    "each",
    "only",
    "skip",
    "todo",
    "concurrent",
    "mock",
    "doMock",
    "unmock",
    "spyOn",
    "fn",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
    "addEventListener",
    "removeEventListener",
    "addInitScript",
    "step",
];

/// What an argument binds a key to: a handler to resolve like a call, or the lines of code
/// written in place.
type CodeArg = (Option<Call>, Option<(u32, u32)>);

/// Most elements of an array or entries of an object read for a catalog.
const MAX_ENTRIES: usize = 2000;

/// Whether a callee's name is a registration word (`register`, `registerStep`, `onSave`).
fn registers(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    REGISTER_WORDS.iter().any(|w| {
        lower == *w
            || (lower.starts_with(w)
                && name[w.len()..].starts_with(|c: char| c.is_ascii_uppercase() || c == '_'))
    })
}

/// Where a node sits: the class and function around, the declaration a literal belongs to.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// Innermost class (qualified by the outer ones) and its first line.
    class: Option<(String, u32)>,
    /// Innermost function or method.
    func: Option<String>,
    /// The declaration and property path of a literal (`ACTIONS[].items`), and the
    /// declaration's line.
    path: Option<(String, u32)>,
    /// The switch around a case, and its line.
    switch: Option<(String, u32)>,
}

impl Scope {
    /// The owner named in a container (`Runner.run`, `FileActionBindings`).
    fn owner(&self) -> String {
        match (&self.class, &self.func) {
            (Some((c, _)), Some(f)) => format!("{c}.{f}"),
            (Some((c, _)), None) => c.clone(),
            (None, Some(f)) => f.clone(),
            (None, None) => "module".to_string(),
        }
    }
}

struct Walk<'s> {
    lang: Lang,
    src: &'s [u8],
    out: Vec<RawKey>,
    /// Nodes already part of a site (never a use as well).
    claimed: HashSet<usize>,
}

/// The key sites of a file.
pub(super) fn keys(lang: Lang, root: Node<'_>, src: &[u8]) -> Vec<RawKey> {
    if !matches!(lang, Lang::Js | Lang::Java | Lang::Python) {
        return Vec::new();
    }
    let mut w = Walk {
        lang,
        src,
        out: Vec::new(),
        claimed: HashSet::new(),
    };
    w.visit(root, &Scope::default(), 0);
    w.out
}

/// How deep the walk goes (a guard against pathological nesting).
const MAX_WALK: u32 = 400;

fn site(form: KeyForm, value: StrExpr, line: u32, container: String, cline: u32) -> RawKey {
    RawKey {
        form,
        value,
        line,
        container,
        container_line: cline,
        name: None,
        body: None,
        handlers: Vec::new(),
    }
}

impl Walk<'_> {
    fn text(&self, n: Node<'_>) -> String {
        text(n, self.src)
    }

    /// A key written here: a key-like literal, or a constant reference.
    fn key_value(&self, n: Node<'_>) -> Option<StrExpr> {
        let n = if self.lang == Lang::Js {
            unwrap_expr(n)
        } else {
            n
        };
        let k = n.kind();
        if k.contains("string") || k == "template_string" {
            if k == "template_string" && n.named_child_count() > 0 {
                // Substitutions: not one key.
                let mut c = n.walk();
                if n.named_children(&mut c)
                    .any(|x| x.kind() == "template_substitution")
                {
                    return None;
                }
            }
            if self.lang == Lang::Python && n.kind() == "concatenated_string" {
                return None;
            }
            let v = string_value(n, self.src)?;
            if self.lang == Lang::Python && self.text(n).starts_with(['f', 'F']) {
                return None;
            }
            return key_like(&v).then(|| StrExpr::lit(v));
        }
        self.const_ref(n)
    }

    /// A constant reference: a dotted name whose last part looks constant, or (Java, in a
    /// case) a bare upper-case name.
    fn const_ref(&self, n: Node<'_>) -> Option<StrExpr> {
        match n.kind() {
            "field_access" | "member_expression" | "attribute" => {
                let parts = dotted(n, self.src)?;
                let joined = parts.join(".");
                (parts.len() >= 2
                    && parts[0] != "this"
                    && parts[0] != "self"
                    && looks_constant(&joined)
                    && parts.last().is_some_and(|l| l.chars().count() >= 2))
                .then(|| StrExpr::one(Part::Ref(joined)))
            }
            _ => None,
        }
    }

    fn claim_all(&mut self, n: Node<'_>) {
        self.claimed.insert(n.id());
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            self.claim_all(ch);
        }
    }

    fn push_use(&mut self, n: Node<'_>, scope: &Scope) {
        if self.claimed.contains(&n.id()) {
            return;
        }
        if let Some(v) = self.key_value(n) {
            self.claimed.insert(n.id());
            self.out
                .push(site(KeyForm::Use, v, line(n), scope.owner(), 0));
        }
    }

    fn visit(&mut self, n: Node<'_>, scope: &Scope, depth: u32) {
        if depth > MAX_WALK {
            return;
        }
        let k = n.kind();
        if matches!(k, "comment" | "line_comment" | "block_comment") {
            self.comment(n);
            return;
        }
        // Never keys: imports, packages, types.
        if matches!(
            k,
            "import_declaration"
                | "package_declaration"
                | "import_statement"
                | "import_from_statement"
                | "future_import_statement"
                | "comment"
                | "line_comment"
                | "block_comment"
                | "type_annotation"
                | "literal_type"
                | "type_alias_declaration"
                | "interface_declaration"
                | "jsx_attribute"
        ) && !(self.lang == Lang::Java && k == "interface_declaration")
        {
            return;
        }
        let mut inner = scope.clone();
        match (self.lang, k) {
            (Lang::Js, "export_statement") => {
                // `export … from "m"`: the source is not a key.
                if let Some(s) = n.child_by_field_name("source") {
                    self.claim_all(s);
                }
            }
            (Lang::Java, "class_declaration" | "interface_declaration" | "record_declaration")
            | (Lang::Java, "enum_declaration")
            | (Lang::Js, "class_declaration" | "class")
            | (Lang::Python, "class_definition") => {
                if let Some(name) = n.child_by_field_name("name") {
                    let name = self.text(name);
                    let q = match &scope.class {
                        Some((c, _)) => format!("{c}.{name}"),
                        None => name,
                    };
                    inner.class = Some((q, line(n)));
                    inner.func = None;
                }
                if k == "enum_declaration" {
                    self.java_enum(n, &inner);
                }
                if k == "class_definition" {
                    self.python_enum(n, &inner);
                }
                if self.lang == Lang::Java {
                    self.class_annotations(n, &inner);
                }
            }
            (Lang::Java, "method_declaration" | "constructor_declaration")
            | (
                Lang::Js,
                "function_declaration" | "method_definition" | "generator_function_declaration",
            )
            | (Lang::Python, "function_definition") => {
                if let Some(name) = n.child_by_field_name("name") {
                    inner.func = Some(self.text(name));
                }
                inner.path = None;
            }
            (Lang::Js, "enum_declaration") => {
                // String members are constants ([`Code::consts`]); members are not uses.
                self.claim_all(n);
                return;
            }
            (Lang::Java, "annotation" | "marker_annotation") | (Lang::Python, "decorator") => {
                if k == "decorator" {
                    self.decorator(n, scope);
                }
                self.claim_all(n);
                return;
            }
            (Lang::Js, "variable_declarator") | (Lang::Python, "assignment") => {
                let name = if k == "assignment" {
                    n.child_by_field_name("left")
                } else {
                    n.child_by_field_name("name")
                };
                if let Some(name) = name.filter(|x| x.kind() == "identifier") {
                    inner.path = Some((self.text(name), line(n)));
                }
            }
            (Lang::Java, "variable_declarator") => {
                if let Some(name) = n.child_by_field_name("name") {
                    inner.path = Some((self.text(name), line(n)));
                }
            }
            (Lang::Js, "pair") | (Lang::Python, "pair") => {
                if let Some(key) = n.child_by_field_name("key")
                    && let Some((p, l)) = &scope.path
                {
                    let name = match key.kind() {
                        "property_identifier" | "identifier" => Some(self.text(key)),
                        _ => string_value(key, self.src),
                    };
                    if let Some(name) = name {
                        inner.path = Some((format!("{p}.{name}"), *l));
                    }
                }
            }
            (Lang::Java, "switch_expression" | "switch_statement")
            | (Lang::Js, "switch_statement")
            | (Lang::Python, "match_statement") => {
                inner.switch = Some((format!("switch in {}", scope.owner()), line(n)));
            }
            (Lang::Java, "switch_block_statement_group" | "switch_rule") => {
                self.java_case(n, scope);
            }
            (Lang::Js, "switch_case") => self.js_case(n, scope),
            (Lang::Python, "case_clause") => self.python_case(n, scope),
            (Lang::Java, "method_invocation" | "object_creation_expression")
            | (Lang::Js, "call_expression" | "new_expression")
            | (Lang::Python, "call") => self.call(n, scope),
            (Lang::Js, "array") | (Lang::Python, "list" | "set" | "tuple") => {
                self.array(n, scope);
                if let Some((p, l)) = &scope.path {
                    inner.path = Some((format!("{p}[]"), *l));
                }
            }
            (Lang::Java, "array_initializer") => self.array(n, scope),
            (Lang::Js, "object") | (Lang::Python, "dictionary") => self.object(n, scope),
            (Lang::Java, "string_literal")
            | (Lang::Js, "string" | "template_string")
            | (Lang::Python, "string") => {
                self.push_use(n, scope);
                return;
            }
            (Lang::Java, "field_access")
            | (Lang::Js, "member_expression")
            | (Lang::Python, "attribute")
                if !self.claimed.contains(&n.id()) && self.const_ref(n).is_some() =>
            {
                self.push_use(n, scope);
                return;
            }
            _ => {}
        }
        let mut c = n.walk();
        let children: Vec<Node<'_>> = n.children(&mut c).collect();
        for ch in children {
            self.visit(ch, &inner, depth + 1);
        }
    }

    /// The words of a comment written like a key: with a separator (`READ-FILE`,
    /// `users:get`, `READ_FILE`), or in upper case (`ARCHIVE`); trailing punctuation
    /// dropped. Each once per comment.
    fn comment(&mut self, n: Node<'_>) {
        let text = self.text(n);
        let mut seen: HashSet<&str> = HashSet::new();
        for raw in text.split(|c: char| {
            !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '@' | '+'))
        }) {
            let w = raw.trim_end_matches(['.', ':', '/', '-', '+']);
            let w = w.trim_start_matches(['.', ':', '/', '-', '+']);
            let upper = w.chars().count() >= 3
                && w.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && w.chars().any(|c| c.is_ascii_uppercase());
            let shaped = (w.contains(['-', '_', ':']) || upper)
                && w.chars().any(|c| c.is_alphabetic())
                && !w.contains("//");
            if !shaped || !key_like(w) || !seen.insert(w) {
                continue;
            }
            self.out.push(site(
                KeyForm::Comment,
                StrExpr::lit(w),
                line(n),
                String::new(),
                0,
            ));
        }
    }

    /// Java `enum E { A("a"), B }`: each constant defines its string argument, else its name.
    fn java_enum(&mut self, n: Node<'_>, scope: &Scope) {
        let Some((enum_name, eline)) = scope.class.clone() else {
            return;
        };
        let Some(body) = n.child_by_field_name("body") else {
            return;
        };
        let mut c = body.walk();
        for k in body.named_children(&mut c) {
            if k.kind() != "enum_constant" {
                continue;
            }
            let Some(name) = k.child_by_field_name("name") else {
                continue;
            };
            let cname = self.text(name);
            let arg = k
                .child_by_field_name("arguments")
                .and_then(|a| named_args(a).into_iter().next())
                .and_then(|a| string_value(a, self.src).filter(|_| a.kind().contains("string")));
            if let Some(a) = k.child_by_field_name("arguments") {
                self.claim_all(a);
            }
            let key = arg.filter(|v| key_like(v)).unwrap_or(cname.clone());
            let mut s = site(
                KeyForm::Enum,
                StrExpr::lit(key),
                line(k),
                enum_name.clone(),
                eline,
            );
            s.name = Some(format!("{enum_name}.{cname}"));
            self.out.push(s);
        }
    }

    /// Python `class Action(Enum): READ = "read"`.
    fn python_enum(&mut self, n: Node<'_>, scope: &Scope) {
        let Some((enum_name, eline)) = scope.class.clone() else {
            return;
        };
        let is_enum = n.child_by_field_name("superclasses").is_some_and(|s| {
            let t = self.text(s);
            ["Enum", "StrEnum", "IntEnum", "Flag"]
                .iter()
                .any(|e| t.split(|c: char| !c.is_alphanumeric()).any(|w| w == *e))
        });
        let Some(body) = n.child_by_field_name("body").filter(|_| is_enum) else {
            return;
        };
        let mut c = body.walk();
        for st in body.named_children(&mut c) {
            let Some(a) = st.named_child(0).filter(|a| a.kind() == "assignment") else {
                continue;
            };
            let (Some(l), Some(r)) = (
                a.child_by_field_name("left"),
                a.child_by_field_name("right"),
            ) else {
                continue;
            };
            if l.kind() != "identifier" {
                continue;
            }
            let cname = self.text(l);
            let key = string_value(r, self.src)
                .filter(|v| r.kind() == "string" && key_like(v))
                .unwrap_or(cname.clone());
            self.claim_all(r);
            let mut s = site(
                KeyForm::Enum,
                StrExpr::lit(key),
                line(a),
                enum_name.clone(),
                eline,
            );
            s.name = Some(format!("{enum_name}.{cname}"));
            self.out.push(s);
        }
    }

    /// `@Component("K") class X`: the class is what the key names.
    fn class_annotations(&mut self, n: Node<'_>, scope: &Scope) {
        let Some((class, _)) = &scope.class else {
            return;
        };
        let simple = class.rsplit('.').next().unwrap_or(class).to_string();
        let mut c = n.walk();
        let Some(mods) = n.children(&mut c).find(|x| x.kind() == "modifiers") else {
            return;
        };
        let mut mc = mods.walk();
        for a in mods.named_children(&mut mc) {
            if a.kind() != "annotation" {
                continue;
            }
            let Some(name) = a.child_by_field_name("name") else {
                continue;
            };
            let aname = self.text(name);
            let aname = aname.rsplit('.').next().unwrap_or(&aname).to_string();
            if !NAMING_ANNOTATIONS.contains(&aname.as_str()) {
                continue;
            }
            let Some(args) = a.child_by_field_name("arguments") else {
                continue;
            };
            let mut ac = args.walk();
            let value = args.named_children(&mut ac).find_map(|x| match x.kind() {
                "element_value_pair" => x
                    .child_by_field_name("key")
                    .filter(|k| matches!(self.text(*k).as_str(), "value" | "name"))
                    .and_then(|_| x.child_by_field_name("value")),
                _ => Some(x),
            });
            let Some(v) = value.and_then(|v| self.key_value(v)) else {
                continue;
            };
            let mut s = site(KeyForm::Annotation, v, line(a), format!("@{aname}"), 0);
            s.handlers.push(plain_call(&simple, line(n), Form::New));
            self.out.push(s);
        }
    }

    /// Python `@registry.register("K")` on a function or class.
    fn decorator(&mut self, n: Node<'_>, scope: &Scope) {
        let Some(call) = n.named_child(0).filter(|c| c.kind() == "call") else {
            return;
        };
        let Some(f) = call.child_by_field_name("function") else {
            return;
        };
        let callee = match f.kind() {
            "identifier" => self.text(f),
            "attribute" => f
                .child_by_field_name("attribute")
                .map(|a| self.text(a))
                .unwrap_or_default(),
            _ => return,
        };
        if !registers(&callee) {
            return;
        }
        let Some(first) = call
            .child_by_field_name("arguments")
            .and_then(|a| named_args(a).into_iter().next())
        else {
            return;
        };
        let Some(v) = self.key_value(first) else {
            return;
        };
        let def = n
            .parent()
            .and_then(|p| p.child_by_field_name("definition"))
            .and_then(|d| d.child_by_field_name("name"));
        let mut s = site(
            KeyForm::Register,
            v,
            line(first),
            format!("{}: @{callee}", scope.owner()),
            scope.class.as_ref().map_or(0, |c| c.1),
        );
        if let Some(d) = def {
            s.handlers
                .push(plain_call(&self.text(d), line(d), Form::Plain));
        }
        self.out.push(s);
    }

    /// The switch around a case, as a container.
    fn switch_of(scope: &Scope) -> (String, u32) {
        scope
            .switch
            .clone()
            .unwrap_or_else(|| (format!("switch in {}", scope.owner()), 0))
    }

    fn java_case(&mut self, n: Node<'_>, scope: &Scope) {
        let mut c = n.walk();
        let children: Vec<Node<'_>> = n.named_children(&mut c).collect();
        let labels: Vec<Node<'_>> = children
            .iter()
            .copied()
            .filter(|x| x.kind() == "switch_label")
            .collect();
        let body: Vec<Node<'_>> = children
            .iter()
            .copied()
            .filter(|x| x.kind() != "switch_label" && !x.kind().contains("comment"))
            .collect();
        let mut range = match (body.first(), body.last()) {
            (Some(a), Some(b)) => Some((line(*a), end_line(*b))),
            _ => None,
        };
        // Stacked labels in groups of their own share the next group's statements.
        let mut next = n.next_named_sibling();
        while range.is_none()
            && let Some(x) = next
        {
            let mut c = x.walk();
            let stmts: Vec<Node<'_>> = x
                .named_children(&mut c)
                .filter(|y| y.kind() != "switch_label" && !y.kind().contains("comment"))
                .collect();
            if let (Some(a), Some(b)) = (stmts.first(), stmts.last()) {
                range = Some((line(*a), end_line(*b)));
            }
            next = x.next_named_sibling();
        }
        let (container, cline) = Self::switch_of(scope);
        for l in labels {
            let mut lc = l.walk();
            let values: Vec<Node<'_>> = l.named_children(&mut lc).collect();
            for v in values {
                let value = self.key_value(v).or_else(|| {
                    (v.kind() == "identifier" && looks_constant(&self.text(v)))
                        .then(|| StrExpr::one(Part::Ref(self.text(v))))
                });
                if let Some(value) = value {
                    self.claim_all(v);
                    let mut s = site(KeyForm::Case, value, line(v), container.clone(), cline);
                    s.body = range;
                    self.out.push(s);
                }
            }
        }
    }

    fn js_case(&mut self, n: Node<'_>, scope: &Scope) {
        let Some(v) = n.child_by_field_name("value") else {
            return;
        };
        let Some(value) = self.key_value(v) else {
            return;
        };
        self.claim_all(v);
        // Stacked cases (`case A: case B: …`) share the body of the next case that has one.
        let mut range = None;
        let mut cur = Some(n);
        while let Some(x) = cur {
            let mut c = x.walk();
            let body: Vec<Node<'_>> = x
                .children_by_field_name("body", &mut c)
                .filter(|b| !b.kind().contains("comment"))
                .collect();
            if let (Some(a), Some(b)) = (body.first(), body.last()) {
                range = Some((line(*a), end_line(*b)));
                break;
            }
            cur = x.next_named_sibling();
        }
        let (container, cline) = Self::switch_of(scope);
        let mut s = site(KeyForm::Case, value, line(v), container, cline);
        s.body = range;
        self.out.push(s);
    }

    fn python_case(&mut self, n: Node<'_>, scope: &Scope) {
        let mut c = n.walk();
        let patterns: Vec<Node<'_>> = n
            .named_children(&mut c)
            .filter(|x| x.kind() == "case_pattern")
            .collect();
        let range = n
            .child_by_field_name("consequence")
            .map(|b| (line(b), end_line(b)));
        let (container, cline) = Self::switch_of(scope);
        for p in patterns {
            let Some(v) = p.named_child(0) else { continue };
            let value = self.key_value(v).or_else(|| {
                (v.kind() == "dotted_name" && looks_constant(&self.text(v)))
                    .then(|| StrExpr::one(Part::Ref(self.text(v))))
            });
            if let Some(value) = value {
                self.claim_all(v);
                let mut s = site(KeyForm::Case, value, line(v), container.clone(), cline);
                s.body = range;
                self.out.push(s);
            }
        }
    }

    /// The callee's last name and arguments of a call.
    fn callee<'t>(&self, n: Node<'t>) -> Option<(String, Vec<Node<'t>>)> {
        let (name, args) = match (self.lang, n.kind()) {
            (Lang::Java, "method_invocation") => (
                self.text(n.child_by_field_name("name")?),
                n.child_by_field_name("arguments"),
            ),
            (Lang::Java, "object_creation_expression") => {
                let t = self.text(n.child_by_field_name("type")?);
                let t = t.split('<').next().unwrap_or(&t).to_string();
                (format!("new {t}"), n.child_by_field_name("arguments"))
            }
            (Lang::Js, "call_expression") => {
                let f = unwrap_expr(n.child_by_field_name("function")?);
                let name = match f.kind() {
                    "identifier" => self.text(f),
                    "member_expression" => self.text(f.child_by_field_name("property")?),
                    "import" => "import".to_string(),
                    _ => return None,
                };
                (name, n.child_by_field_name("arguments"))
            }
            (Lang::Js, "new_expression") => {
                let c = n.child_by_field_name("constructor")?;
                (
                    format!("new {}", self.text(c)),
                    n.child_by_field_name("arguments"),
                )
            }
            (Lang::Python, "call") => {
                let f = n.child_by_field_name("function")?;
                let name = match f.kind() {
                    "identifier" => self.text(f),
                    "attribute" => self.text(f.child_by_field_name("attribute")?),
                    _ => return None,
                };
                (name, n.child_by_field_name("arguments"))
            }
            _ => return None,
        };
        let args: Vec<Node<'t>> = args
            .map(named_args)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !matches!(a.kind(), "keyword_argument" | "spread_element"))
            .collect();
        Some((name, args))
    }

    /// Code an argument binds a key to: a function written in place, a method reference, a
    /// new object, a class literal; with a registering callee, a name too.
    fn code_arg(&self, a: Node<'_>, registering: bool) -> Option<CodeArg> {
        let a = if self.lang == Lang::Js {
            unwrap_expr(a)
        } else {
            a
        };
        let range = Some((line(a), end_line(a)));
        match (self.lang, a.kind()) {
            (Lang::Java, "method_reference") => {
                let mut c = a.walk();
                let parts: Vec<Node<'_>> = a.named_children(&mut c).collect();
                let (recv, name) = match parts.as_slice() {
                    [r, m] => (*r, *m),
                    // `Foo::new`: `new` is a keyword, not a named child.
                    [r] if self.text(a).trim_end().ends_with("new") => {
                        return Some((Some(plain_call(&self.text(*r), line(a), Form::New)), None));
                    }
                    _ => return Some((None, None)),
                };
                let name = self.text(name);
                if name == "new" {
                    return Some((Some(plain_call(&self.text(recv), line(a), Form::New)), None));
                }
                let call = if recv.kind() == "this" {
                    let mut c = plain_call(&name, line(a), Form::SelfCall);
                    c.qualifier = None;
                    c
                } else {
                    let mut c = plain_call(&name, line(a), Form::Member);
                    c.qualifier = Some(self.text(recv));
                    c
                };
                Some((Some(call), None))
            }
            // A class or a new object binds a key only for a registering callee
            // (`register("K", Foo.class)`, `put("K", new Handler())`): elsewhere it is a
            // type token or a value (`migrate("K", Dto.class)`, `append("K", new Date())`).
            (Lang::Java, "class_literal") if registering => {
                let t = a.named_child(0).map(|t| self.text(t))?;
                Some((Some(plain_call(&t, line(a), Form::New)), None))
            }
            (Lang::Java, "object_creation_expression") if registering => {
                let t = self.text(a.child_by_field_name("type")?);
                let t = t.split('<').next().unwrap_or(&t).to_string();
                Some((Some(plain_call(&t, line(a), Form::New)), None))
            }
            (Lang::Java, "lambda_expression")
            | (Lang::Js, "arrow_function" | "function_expression" | "function")
            | (Lang::Python, "lambda") => Some((None, range)),
            (Lang::Js, "class") => Some((None, range)),
            (Lang::Js, "new_expression") if registering => {
                let c = a.child_by_field_name("constructor")?;
                Some((Some(plain_call(&self.text(c), line(a), Form::New)), None))
            }
            (Lang::Js, "identifier" | "member_expression") if registering => {
                Some((function_ref(Lang::Js, a, self.src), None))
            }
            (Lang::Python, "identifier" | "attribute") if registering => {
                Some((handler_ref(Lang::Python, a, self.src), None))
            }
            (Lang::Java, "identifier") if registering => {
                Some((Some(plain_call(&self.text(a), line(a), Form::Plain)), None))
            }
            _ => None,
        }
    }

    fn container_of(&self, scope: &Scope, callee: &str) -> (String, u32) {
        let line = scope
            .class
            .as_ref()
            .map(|c| c.1)
            .or(scope.path.as_ref().map(|p| p.1))
            .unwrap_or(0);
        let owner = match (&scope.class, &scope.path) {
            (Some((c, _)), _) => c.clone(),
            (None, Some((p, _))) => p.split(['.', '[']).next().unwrap_or(p).to_string(),
            (None, None) => scope.func.clone().unwrap_or_else(|| "module".into()),
        };
        (format!("{owner}: {callee}"), line)
    }

    fn call(&mut self, n: Node<'_>, scope: &Scope) {
        let Some((callee, args)) = self.callee(n) else {
            return;
        };
        if matches!(callee.as_str(), "require" | "import") && self.lang == Lang::Js {
            for a in &args {
                self.claim_all(*a);
            }
            return;
        }
        // A test's title or a mocked module: neither a key nor a use of one.
        if self.lang != Lang::Java && NOT_REGISTRATIONS.contains(&callee.as_str()) {
            if let Some(first) = args.first() {
                self.claim_all(*first);
            }
            return;
        }
        let keyed: Vec<(usize, StrExpr)> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| !self.claimed.contains(&a.id()))
            .filter_map(|(i, a)| self.key_value(*a).map(|v| (i, v)))
            .collect();
        let Some((first, _)) = keyed.first().cloned() else {
            return;
        };
        let short = callee.rsplit(' ').next().unwrap_or(&callee);
        let registering = registers(short) || matches!(short, "put" | "putIfAbsent");
        // A registration: the key early, code among the other arguments.
        if first <= 1 {
            let mut handlers = Vec::new();
            let mut body = None;
            let mut any = false;
            // Code after the key (`register("K", handler)`), never before it.
            for (i, a) in args.iter().enumerate() {
                if i <= first {
                    continue;
                }
                if let Some((h, r)) = self.code_arg(*a, registering) {
                    any = true;
                    handlers.extend(h);
                    body = body.or(r);
                }
            }
            if any {
                let (container, cline) = self.container_of(scope, &callee);
                let mut s = site(
                    KeyForm::Register,
                    keyed[0].1.clone(),
                    line(args[first]),
                    container,
                    cline,
                );
                s.handlers = handlers;
                s.body = body;
                self.claim_all(args[first]);
                self.out.push(s);
                return;
            }
        }
        // A list built of its arguments, a map of keys to values.
        let entries: Vec<usize> = match short {
            s if LIST_CALLS.contains(&s) && self.on_collection(n) => {
                if short == "of" && args.len() >= 2 && self.is_map_of(n) {
                    (0..args.len()).step_by(2).collect()
                } else {
                    (0..args.len()).collect()
                }
            }
            "put" | "putIfAbsent" | "entry" | "set" | "add" => vec![0],
            _ => Vec::new(),
        };
        let entries: Vec<(usize, StrExpr)> = keyed
            .iter()
            .filter(|(i, _)| entries.contains(i))
            .cloned()
            .collect();
        if !entries.is_empty() {
            let (container, cline) = self.container_of(scope, &callee);
            for (i, v) in entries {
                self.claim_all(args[i]);
                self.out.push(site(
                    KeyForm::Entry,
                    v,
                    line(args[i]),
                    container.clone(),
                    cline,
                ));
            }
        }
    }

    /// `Map.of(k, v, …)` (not `List.of`): the receiver is named `Map`.
    /// The receiver of a call is a collection type (see [`COLLECTIONS`]).
    fn on_collection(&self, n: Node<'_>) -> bool {
        let recv = match n.kind() {
            "call_expression" => n
                .child_by_field_name("function")
                .and_then(|f| f.child_by_field_name("object")),
            _ => n.child_by_field_name("object"),
        };
        recv.is_some_and(|o| {
            let t = self.text(o);
            COLLECTIONS.contains(&t.rsplit('.').next().unwrap_or(&t))
        })
    }

    fn is_map_of(&self, n: Node<'_>) -> bool {
        n.child_by_field_name("object")
            .is_some_and(|o| self.text(o).ends_with("Map"))
    }

    fn container_path(&self, scope: &Scope, n: Node<'_>) -> (String, u32) {
        match &scope.path {
            Some((p, l)) => (p.clone(), *l),
            None => (format!("{} (line {})", scope.owner(), line(n)), line(n)),
        }
    }

    /// An array of keys, or of objects named by a key property.
    fn array(&mut self, n: Node<'_>, scope: &Scope) {
        let mut c = n.walk();
        let elems: Vec<Node<'_>> = n
            .named_children(&mut c)
            .filter(|x| !x.kind().contains("comment"))
            .take(MAX_ENTRIES)
            .collect();
        // An array inside a catalog's array (a category's items) joins the catalog: its
        // entries count with the others'.
        let nested = scope.path.as_ref().is_some_and(|p| p.0.contains("[]"));
        let min = if nested { 1 } else { 3 };
        if elems.len() < min {
            return;
        }
        let (path, cline) = self.container_path(scope, n);
        // Strings.
        let strings: Vec<(Node<'_>, StrExpr)> = elems
            .iter()
            .filter_map(|e| {
                let v = self.key_value(*e)?;
                matches!(v.0.as_slice(), [Part::Lit(_)] | [Part::Ref(_)]).then_some((*e, v))
            })
            .collect();
        if strings.len() >= min && strings.len() * 2 >= elems.len() {
            for (e, v) in strings {
                self.claim_all(e);
                self.out
                    .push(site(KeyForm::Entry, v, line(e), format!("{path}[]"), cline));
            }
            return;
        }
        // Objects named by a property.
        let objs: Vec<Node<'_>> = elems
            .iter()
            .copied()
            .map(|e| {
                if self.lang == Lang::Js {
                    unwrap_expr(e)
                } else {
                    e
                }
            })
            .filter(|e| matches!(e.kind(), "object" | "dictionary"))
            .collect();
        if objs.len() < min || objs.len() * 2 < elems.len() {
            return;
        }
        // The property most elements are named by.
        let mut by_prop: Vec<(String, Vec<(Node<'_>, StrExpr)>)> = Vec::new();
        for o in &objs {
            if let Some((prop, v)) = self.named_by(*o)
                && let Some(val) = self.key_value(v)
            {
                match by_prop.iter_mut().find(|x| x.0 == prop) {
                    Some(x) => x.1.push((v, val)),
                    None => by_prop.push((prop, vec![(v, val)])),
                }
            }
        }
        let Some((prop, hits)) = by_prop.into_iter().max_by_key(|x| x.1.len()) else {
            return;
        };
        if hits.len() < min || hits.len() * 2 < objs.len() {
            return;
        }
        for (v, val) in hits {
            self.claim_all(v);
            self.out.push(site(
                KeyForm::Entry,
                val,
                line(v),
                format!("{path}[].{prop}"),
                cline,
            ));
        }
    }

    /// The property among [`NAME_PROPS`] an object of a catalog is named by, and its value.
    fn named_by<'t>(&self, o: Node<'t>) -> Option<(String, Node<'t>)> {
        let mut c = o.walk();
        let pairs: Vec<Node<'t>> = o
            .named_children(&mut c)
            .filter(|p| p.kind() == "pair")
            .collect();
        for prop in NAME_PROPS {
            for p in &pairs {
                let Some(key) = p.child_by_field_name("key") else {
                    continue;
                };
                let kname = match key.kind() {
                    "property_identifier" | "identifier" => self.text(key),
                    _ => string_value(key, self.src).unwrap_or_default(),
                };
                if kname == *prop
                    && let Some(v) = p.child_by_field_name("value")
                {
                    return Some((kname, v));
                }
            }
        }
        None
    }

    /// An object (or dictionary) keyed by strings: each key is an entry.
    fn object(&mut self, n: Node<'_>, scope: &Scope) {
        let mut c = n.walk();
        let pairs: Vec<Node<'_>> = n
            .named_children(&mut c)
            .filter(|p| p.kind() == "pair")
            .take(MAX_ENTRIES)
            .collect();
        if pairs.len() < 3 {
            return;
        }
        let keys: Vec<Node<'_>> = pairs
            .iter()
            .filter_map(|p| p.child_by_field_name("key"))
            .collect();
        let quoted = keys.iter().filter(|k| k.kind() == "string").count();
        if quoted == 0 {
            return;
        }
        // Python: every key a string. JS: some quoted (a name that is not an identifier),
        // the others plain names of the same map.
        if self.lang == Lang::Python && quoted < keys.len() {
            return;
        }
        let (path, cline) = self.container_path(scope, n);
        let mut found: Vec<(Node<'_>, StrExpr)> = Vec::new();
        for k in &keys {
            let v = match k.kind() {
                "string" => string_value(*k, self.src).filter(|v| key_like(v)),
                "property_identifier" => Some(self.text(*k)),
                _ => None,
            };
            if let Some(v) = v {
                found.push((*k, StrExpr::lit(v)));
            }
        }
        if found.len() < 3 {
            return;
        }
        for (k, v) in found {
            self.claim_all(k);
            self.out
                .push(site(KeyForm::Entry, v, line(k), path.clone(), cline));
        }
    }
}

fn plain_call(name: &str, line: u32, form: Form) -> Call {
    Call {
        line,
        name: name.rsplit('.').next().unwrap_or(name).to_string(),
        form,
        qualifier: None,
        path: Vec::new(),
        scope: Vec::new(),
        field: None,
        recv_type: None,
        recv_decl: None,
        sup: false,
        recv_elem: false,
        args: Vec::new(),
        handler: None,
        opts: Vec::new(),
        frame: None,
        argc: 0,
    }
}

#[cfg(test)]
mod tests {
    use crate::grammars::{Grammar, parse};
    use crate::symbols::strexpr::Part;
    use crate::symbols::strexpr::key_like;
    use crate::symbols::{KeyForm, extract};

    fn sites(lang: &str, src: &str) -> Vec<String> {
        let g = Grammar::for_lang(lang).unwrap();
        let tree = parse(g, src.as_bytes()).unwrap();
        let code = extract(g, &tree, src.as_bytes()).unwrap();
        code.keys
            .iter()
            .map(|k| {
                let v = match k.value.0.as_slice() {
                    [Part::Lit(s)] => format!("{s:?}"),
                    [Part::Ref(r)] => r.clone(),
                    _ => "?".into(),
                };
                let mut s = format!("{:?} {v} @{} [{}]", k.form, k.line, k.container);
                if let Some(n) = &k.name {
                    s.push_str(&format!(" name={n}"));
                }
                if let Some((a, b)) = k.body {
                    s.push_str(&format!(" body={a}-{b}"));
                }
                for h in &k.handlers {
                    s.push_str(&format!(
                        " -> {}{}",
                        h.qualifier
                            .as_deref()
                            .map_or(String::new(), |q| format!("{q}.")),
                        h.name
                    ));
                }
                s
            })
            .collect()
    }

    #[test]
    fn what_is_key_like() {
        for s in [
            "READ-FILE",
            "users:get",
            "a_b",
            "READ_FILE",
            "x.y",
            "@scope/pkg",
            "v2",
        ] {
            assert!(key_like(s), "{s}");
        }
        for s in [
            "a", "", "a b", "/users", "./a", "http://x", "{id}", "a,b", "100", "%s", "${x}",
        ] {
            assert!(!key_like(s), "{s}");
        }
    }

    #[test]
    fn java_enums_switches_registrations_and_uses() {
        let src = r#"package p;
import a.b.C;
@Component("files")
class Registry {
    enum Action { READ("READ-FILE"), WRITE }
    List<Reg> all() {
        return List.of(
            new Reg(Action.READ, Steps::read),
            new Reg(Keys.WRITE_FILE, x -> run(x)),
            new Reg("COPY-FILE", Copy::new),
            new Reg("MOVE-FILE", Move.class));
    }
    void run(String s) {
        switch (s) {
            case Keys.OPEN:
            case "CLOSE":
                close(s);
                break;
            case READ: other(); break;
            default: break;
        }
        if ("PING".equals(s)) { log.info("no key here"); }
        Set<String> names = Set.of("A1", "B2", "C3");
    }
}
"#;
        let got = sites("java", src);
        let want = [
            "Enum \"READ-FILE\" @5 [Registry.Action] name=Registry.Action.READ",
            "Enum \"WRITE\" @5 [Registry.Action] name=Registry.Action.WRITE",
            "Annotation \"files\" @3 [@Component] -> Registry",
            "Register Action.READ @8 [Registry: new Reg] -> Steps.read",
            "Register Keys.WRITE_FILE @9 [Registry: new Reg] body=9-9",
            "Register \"COPY-FILE\" @10 [Registry: new Reg] -> Copy",
            "Use \"MOVE-FILE\" @11 [Registry.all]",
            "Case Keys.OPEN @15 [switch in Registry.run] body=17-18",
            "Case \"CLOSE\" @16 [switch in Registry.run] body=17-18",
            "Case READ @19 [switch in Registry.run] body=19-19",
            "Use \"PING\" @22 [Registry.run]",
            "Entry \"A1\" @23 [Registry: of]",
            "Entry \"B2\" @23 [Registry: of]",
            "Entry \"C3\" @23 [Registry: of]",
        ];
        let mut got_sorted = got.clone();
        got_sorted.sort();
        let mut want_sorted: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        want_sorted.sort();
        assert_eq!(got_sorted, want_sorted, "{got:#?}");
    }

    /// `of` builds a catalog only on a collection: `List.of`, `Set.of`, `Map.of`,
    /// `ImmutableList.of`, `Arrays.asList`; never `Path.of`, `Stream.of`, `EnumSet.of` or
    /// RxJS's `of`.
    #[test]
    fn only_collections_build_catalogs() {
        let src = r#"class C {
    void f() {
        var p = Path.of("a-b", "c-d", "e-f");
        var s = Stream.of("A1", "B2", "C3");
        var e = EnumSet.of("K1", "K2", "K3");
        var l = List.of("L1", "L2", "L3");
        var g = ImmutableSet.of("G1", "G2", "G3");
        var a = Arrays.asList("R1", "R2", "R3");
    }
}
"#;
        let got = sites("java", src);
        for k in ["a-b", "A1", "K1"] {
            assert!(
                got.iter()
                    .filter(|g| g.contains(&format!("\"{k}\"")))
                    .all(|g| g.starts_with("Use")),
                "{k}: {got:#?}"
            );
        }
        for k in ["L1", "G1", "R1"] {
            assert!(
                got.iter()
                    .any(|g| g.starts_with("Entry") && g.contains(&format!("\"{k}\""))),
                "{k}: {got:#?}"
            );
        }
        let got = sites(
            "typescript",
            "import { of } from 'rxjs';\nexport const s = of('X1', 'X2', 'X3');\n",
        );
        assert!(!got.iter().any(|g| g.starts_with("Entry")), "{got:#?}");
    }

    #[test]
    fn ts_catalogs_maps_switches_and_uses() {
        let src = r#"import { x } from "./mod";
export const CATS = [
  { id: "files", items: [
    { name: "READ-FILE", label: "Read" },
    { name: "WRITE-FILE", label: "Write" },
    { name: "COPY-FILE", label: "Copy" },
  ] },
  { id: "dirs", items: [{ name: "LIST-DIR" }, { name: "ZIP-DIR" }] },
  { id: "misc", items: [] },
];
export const ICONS: Record<string, string> = { "READ-FILE": "file", "WRITE-FILE": "edit", COPY: "copy" };
const small = { "A-B": 1, "C-D": 2 };
export function run(a: string) {
  switch (a) {
    case IPC.SAVE_DOC:
    case "close":
      doClose();
      break;
  }
  register("on-save", () => save());
  ipc.handle("users:get", getUser);
  return find("READ-FILE") ?? <div className="flex-row" />;
}
"#;
        let got = sites("tsx", src);
        for want in [
            "Entry \"files\" @3 [CATS[].id]",
            "Entry \"READ-FILE\" @4 [CATS[].items[].name]",
            "Entry \"ZIP-DIR\" @8 [CATS[].items[].name]",
            "Entry \"READ-FILE\" @11 [ICONS]",
            "Entry \"COPY\" @11 [ICONS]",
            "Case IPC.SAVE_DOC @15 [switch in run] body=17-18",
            "Case \"close\" @16 [switch in run] body=17-18",
            "Register \"on-save\" @20 [run: register] body=20-20",
            "Register \"users:get\" @21 [run: handle] -> getUser",
            "Use \"READ-FILE\" @22 [run]",
        ] {
            assert!(got.iter().any(|g| g == want), "{want}\n{got:#?}");
        }
        // Two entries are no catalog (only uses); an import source and a JSX attribute
        // are no keys.
        assert!(
            got.iter()
                .all(|g| !g.contains("A-B") || g.starts_with("Use")),
            "{got:#?}"
        );
        assert!(!got.iter().any(|g| g.contains("./mod")), "{got:#?}");
        assert!(!got.iter().any(|g| g.contains("flex-row")), "{got:#?}");
        assert!(
            got.iter()
                .filter(|g| g.starts_with("Entry") && g.contains("[CATS[].items[].name]"))
                .count()
                == 5,
            "{got:#?}"
        );
        let _ = KeyForm::Use;
    }

    #[test]
    fn python_enums_dicts_decorators_and_match() {
        let src = r#"from enum import Enum
class Action(Enum):
    READ = "read-file"
    WRITE = "write-file"

HANDLERS = {"open": on_open, "close": on_close, "save": on_save}

@registry.register("print")
def print_doc(x):
    match x:
        case "a-1":
            go()
        case Keys.B_2:
            pass
    return call("open")
"#;
        let got = sites("python", src);
        for want in [
            "Enum \"read-file\" @3 [Action] name=Action.READ",
            "Entry \"open\" @6 [HANDLERS]",
            "Register \"print\" @8 [module: @register] -> print_doc",
            "Case \"a-1\" @11 [switch in print_doc] body=12-12",
            "Case Keys.B_2 @13 [switch in print_doc] body=14-14",
            "Use \"open\" @15 [print_doc]",
        ] {
            assert!(got.iter().any(|g| g == want), "{want}\n{got:#?}");
        }
    }
}
