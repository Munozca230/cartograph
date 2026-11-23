//! Java receivers typed by their declaration ([`crate::symbols::Call::recv_type`]): a
//! parameter, a local or a field declared with one named type, and the elements of a
//! collection, a map's values or a held value (`Optional<T>`, `ObjectProvider<T>`) where a
//! lambda or a `for (var x : …)` reads them (`list.stream().filter(x -> …).forEach(x ->
//! x.m())`, `map.values().forEach(v -> v.m())`, `opt.ifPresent(x -> x.m())`), or where a
//! call takes one out (`list.get(0).m()`, `provider.getObject().m()`).
//!
//! A collection itself is never the receiver's type: `list.size()` is not a method of the
//! element type.

use super::{has_child_kind, text};
use std::collections::HashMap;
use tree_sitter::Node;

/// A declared type as receiver typing reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JTy {
    /// One named type: `T x`, `var x = new T()`.
    Plain(String),
    /// A collection or stream of `T` (`List<T>`, `Set<T>`, `Collection<T>`, `Stream<T>`…).
    Elems(String),
    /// A map whose values are `T` (`Map<K, T>`).
    Values(String),
    /// One `T` held for later: `Optional<T>`, `ObjectProvider<T>`, `Provider<T>`.
    Held(String),
    /// A lambda or `for (var x : …)` variable: what it reads of the variable named here.
    From(String, Access),
}

/// What a lambda or loop variable reads of a collection, a map or a holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// An element of a collection or stream, or the value of a holder.
    Element,
    /// A value of a map.
    Value,
}

/// Generic types whose type argument is what they hold, by how they hold it.
const ELEMS: &[&str] = &[
    "List",
    "ArrayList",
    "LinkedList",
    "CopyOnWriteArrayList",
    "Set",
    "HashSet",
    "LinkedHashSet",
    "TreeSet",
    "SortedSet",
    "NavigableSet",
    "Collection",
    "Iterable",
    "Queue",
    "Deque",
    "ArrayDeque",
    "Stream",
];
const VALUES: &[&str] = &[
    "Map",
    "HashMap",
    "LinkedHashMap",
    "TreeMap",
    "SortedMap",
    "NavigableMap",
    "ConcurrentMap",
    "ConcurrentHashMap",
];
const HELD: &[&str] = &[
    "Optional",
    "ObjectProvider",
    "ObjectFactory",
    "Provider",
    "Supplier",
];

/// Stream steps that keep the elements as they are.
const KEEPS: &[&str] = &[
    "filter",
    "sorted",
    "distinct",
    "peek",
    "limit",
    "skip",
    "takeWhile",
    "dropWhile",
    "unordered",
    "parallel",
    "sequential",
    "onClose",
];
/// Stream steps whose lambda takes one element.
const TAKES_ELEMENT: &[&str] = &[
    "forEach",
    "forEachOrdered",
    "filter",
    "map",
    "flatMap",
    "mapToInt",
    "mapToLong",
    "mapToDouble",
    "mapToObj",
    "anyMatch",
    "allMatch",
    "noneMatch",
    "peek",
    "takeWhile",
    "dropWhile",
];
/// Calls on a collection or holder variable itself whose lambda takes an element (or the
/// held value).
const DIRECT: &[&str] = &[
    "forEach",
    "removeIf",
    "ifPresent",
    "ifPresentOrElse",
    "ifAvailable",
    "ifUnique",
    "map",
    "filter",
    "flatMap",
];
/// Calls that take one element or value out.
const ELEMENT_OUT: &[&str] = &["get", "getFirst", "getLast"];
const VALUE_OUT: &[&str] = &["get", "getOrDefault"];
const HELD_OUT: &[&str] = &[
    "get",
    "getObject",
    "getIfAvailable",
    "getIfUnique",
    "orElse",
    "orElseGet",
    "orElseThrow",
];

/// The name a generic type is known by (`List` of `java.util.List<T>`), and its type
/// arguments as written.
fn generic_parts<'t>(ty: Node<'t>, src: &[u8]) -> Option<(String, Vec<Node<'t>>)> {
    if ty.kind() != "generic_type" {
        return None;
    }
    let mut c = ty.walk();
    let kids: Vec<Node<'t>> = ty.named_children(&mut c).collect();
    let base = kids.iter().find(|k| {
        matches!(
            k.kind(),
            "type_identifier" | "scoped_type_identifier" | "identifier"
        )
    })?;
    let name = text(*base, src);
    let name = name.rsplit('.').next().unwrap_or(&name).to_string();
    let args = kids.iter().find(|k| k.kind() == "type_arguments")?;
    let mut c = args.walk();
    let list = args.named_children(&mut c).collect();
    Some((name, list))
}

/// The one type name a type argument stands for: `T`, `T<…>` (its name), `? extends T`.
fn arg_name(n: Node<'_>, src: &[u8]) -> Option<String> {
    match n.kind() {
        "type_identifier" => Some(text(n, src)),
        "generic_type" => generic_parts(n, src).map(|(name, _)| name),
        "wildcard" => {
            let t = text(n, src);
            if !t.contains("extends") {
                return None;
            }
            let mut c = n.walk();
            let inner = n
                .named_children(&mut c)
                .filter(|k| matches!(k.kind(), "type_identifier" | "generic_type"))
                .last()?;
            arg_name(inner, src)
        }
        _ => None,
    }
}

/// A Java generic type that holds a type of the repo as people write it for injection or
/// iteration: `(container, held type)`, with the map's value type for a map (`Map<String,
/// T>` → `("Map", "T")`). `None` for other types.
pub(super) fn holder(ty: Node<'_>, src: &[u8]) -> Option<(String, String)> {
    let (name, args) = generic_parts(ty, src)?;
    let held = if VALUES.contains(&name.as_str()) {
        match args.as_slice() {
            [_, v] => arg_name(*v, src)?,
            _ => return None,
        }
    } else if ELEMS.contains(&name.as_str()) || HELD.contains(&name.as_str()) {
        match args.as_slice() {
            [one] => arg_name(*one, src)?,
            _ => return None,
        }
    } else {
        return None;
    };
    Some((name, held))
}

/// What a Java declaration stands for: `T` as written, `var x = new T(…)`, or a collection,
/// map or holder of a type.
fn declared(ty: Option<Node<'_>>, value: Option<Node<'_>>, src: &[u8]) -> Option<JTy> {
    let ty = ty?;
    if ty.kind() == "generic_type" {
        let (name, held) = holder(ty, src)?;
        return Some(if VALUES.contains(&name.as_str()) {
            JTy::Values(held)
        } else if HELD.contains(&name.as_str()) {
            JTy::Held(held)
        } else {
            JTy::Elems(held)
        });
    }
    if ty.kind() != "type_identifier" {
        return None;
    }
    let t = text(ty, src);
    if t != "var" {
        return Some(JTy::Plain(t));
    }
    let v = value.filter(|v| {
        v.kind() == "object_creation_expression" && !has_child_kind(*v, &["class_body"])
    })?;
    v.child_by_field_name("type")
        .filter(|t| t.kind() == "type_identifier")
        .map(|t| JTy::Plain(text(t, src)))
}

/// `x` (an identifier) or `x.values()`: the variable read, and how.
fn source_of(n: Node<'_>, src: &[u8]) -> Option<(String, Access)> {
    match n.kind() {
        "identifier" => Some((text(n, src), Access::Element)),
        "method_invocation" => {
            let name = n.child_by_field_name("name").map(|x| text(x, src))?;
            let obj = n.child_by_field_name("object")?;
            (name == "values" && obj.kind() == "identifier")
                .then(|| (text(obj, src), Access::Value))
        }
        _ => None,
    }
}

/// What the parameters of a lambda read, by position (`None` for one that reads nothing
/// known): the elements of `x` in `x.forEach(e -> …)`, `x.stream().filter(…).map(e -> …)`,
/// `x.values().stream()…`, the value of `x.ifPresent(e -> …)`, the values of
/// `map.forEach((k, v) -> …)`.
fn lambda_reads(lambda: Node<'_>, params: usize, src: &[u8]) -> Vec<Option<JTy>> {
    let mut out = vec![None; params];
    let Some(call) = lambda
        .parent()
        .filter(|a| a.kind() == "argument_list")
        .and_then(|a| a.parent())
        .filter(|c| c.kind() == "method_invocation")
    else {
        return out;
    };
    let Some(op) = call.child_by_field_name("name").map(|x| text(x, src)) else {
        return out;
    };
    let Some(obj) = call.child_by_field_name("object") else {
        return out;
    };
    let found = if DIRECT.contains(&op.as_str()) && obj.kind() == "identifier" {
        Some((text(obj, src), Access::Element))
    } else if op == "forEach"
        && let Some((x, Access::Value)) = source_of(obj, src)
    {
        Some((x, Access::Value))
    } else if TAKES_ELEMENT.contains(&op.as_str()) {
        stream_source(obj, src)
    } else {
        None
    };
    let Some((x, how)) = found else {
        return out;
    };
    match (params, how, op.as_str()) {
        (1, _, _) => out[0] = Some(JTy::From(x, how)),
        // `map.forEach((k, v) -> …)`.
        (2, Access::Element, "forEach") => out[1] = Some(JTy::From(x, Access::Value)),
        _ => {}
    }
    out
}

/// The variable a stream comes from, through steps that keep its elements:
/// `x.stream().filter(…).sorted()` → `x`; `x.values().stream()` → the values of `x`.
fn stream_source(mut n: Node<'_>, src: &[u8]) -> Option<(String, Access)> {
    for _ in 0..16 {
        if n.kind() != "method_invocation" {
            return None;
        }
        let name = n.child_by_field_name("name").map(|x| text(x, src))?;
        let obj = n.child_by_field_name("object")?;
        if KEEPS.contains(&name.as_str()) {
            n = obj;
            continue;
        }
        if matches!(name.as_str(), "stream" | "parallelStream" | "orderedStream") {
            return source_of(obj, src);
        }
        return None;
    }
    None
}

/// Java `x.m()` calls whose receiver has one declared type where the call is (see
/// [`crate::symbols::Call::recv_type`]), by `method_invocation` node id. One walk of the
/// file, keeping the path from the root; inside out from each call:
/// - the member around it (a method, constructor, initializer or field): a declaration of
///   `x` (a parameter, a local, a lambda, catch, loop, resource or pattern variable) whose
///   scope (block, lambda, loop, `catch`, method…) holds the call;
/// - else the fields of the class body around it;
/// - else, when that class has a superclass (or is anonymous, or an enum constant's body),
///   nothing: an inherited field comes first and is not known here;
/// - else the same one level out (a local class sees the enclosing method's locals, an
///   inner class the outer class's fields).
///
/// A lambda or `for (var …)` variable that reads a collection, a map or a holder is looked
/// up the same way from where the lambda or loop is (see [`lambda_reads`]); so is `x` of
/// `x.get(i).m()`, `x.getObject().m()`. What each scope declares is read once.
pub(super) fn receiver_types(root: Node<'_>, src: &[u8]) -> HashMap<usize, String> {
    let mut scopes = Scopes::default();
    let mut out = HashMap::new();
    let mut path: Vec<Node<'_>> = Vec::new();
    let mut cursor = root.walk();
    loop {
        let n = cursor.node();
        if n.kind() == "method_invocation"
            && let Some(obj) = n.child_by_field_name("object")
        {
            let t = match obj.kind() {
                "identifier" => scopes.plain(&text(obj, src), n, &path, src),
                "method_invocation" => scopes.taken_out(obj, n, &path, src),
                _ => None,
            };
            out.extend(t.map(|t| (n.id(), t)));
        }
        if cursor.goto_first_child() {
            path.push(n);
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return out;
            }
            path.pop();
        }
    }
}

/// What each member and each class body declares, read once.
#[derive(Default)]
struct Scopes {
    members: HashMap<usize, JavaDecls>,
    bodies: HashMap<usize, HashMap<String, Option<JTy>>>,
}

impl Scopes {
    /// The declaration of `name` seen from `start` (whose ancestors are `path`, root
    /// first), with the id of the scope that declares it (0 for a field).
    fn lookup(
        &mut self,
        name: &str,
        start: Node<'_>,
        path: &[Node<'_>],
        src: &[u8],
    ) -> Option<(JTy, usize)> {
        let on_path = |id: usize| path.iter().any(|a| a.id() == id);
        let mut cur = start;
        for i in (0..path.len()).rev() {
            let p = path[i];
            if matches!(
                p.kind(),
                "class_body" | "interface_body" | "enum_body_declarations"
            ) {
                if !JAVA_TYPE_DECLS.contains(&cur.kind()) {
                    let member = self
                        .members
                        .entry(cur.id())
                        .or_insert_with(|| java_decls(cur, src));
                    let seen: Vec<&(usize, Option<JTy>)> = member
                        .get(name)
                        .into_iter()
                        .flatten()
                        .filter(|(scope, _)| on_path(*scope))
                        .collect();
                    if let Some(first) = seen.first() {
                        if seen.iter().all(|t| t.1 == first.1) {
                            return first.1.clone().map(|t| (t, first.0));
                        }
                        return None;
                    }
                }
                let fields = self
                    .bodies
                    .entry(p.id())
                    .or_insert_with(|| java_fields(p, src));
                if let Some(t) = fields.get(name) {
                    return t.clone().map(|t| (t, 0));
                }
                let owner = i.checked_sub(1).map(|o| path[o]);
                let inherits = owner.is_some_and(|o| {
                    matches!(o.kind(), "object_creation_expression" | "enum_constant")
                        || o.child_by_field_name("superclass").is_some()
                });
                if inherits {
                    return None;
                }
            }
            cur = p;
        }
        None
    }

    /// The type a receiver named `name` has at `start`: its declared type, or the element,
    /// value or held type a lambda or loop variable reads.
    fn plain(
        &mut self,
        name: &str,
        start: Node<'_>,
        path: &[Node<'_>],
        src: &[u8],
    ) -> Option<String> {
        match self.lookup(name, start, path, src)? {
            (JTy::Plain(t), _) => Some(t),
            (JTy::From(var, how), scope) => {
                // The variable is read where the lambda or loop that declares it is.
                let j = path.iter().position(|p| p.id() == scope)?;
                match (self.lookup(&var, path[j], &path[..j], src)?.0, how) {
                    (JTy::Elems(t) | JTy::Held(t), Access::Element) => Some(t),
                    (JTy::Values(t), Access::Value) => Some(t),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `x.get(i)`, `x.getObject()`, `x.orElseThrow()`: the element, value or held type of
    /// `x`, for a call on what it returns.
    fn taken_out(
        &mut self,
        inner: Node<'_>,
        start: Node<'_>,
        path: &[Node<'_>],
        src: &[u8],
    ) -> Option<String> {
        let name = inner.child_by_field_name("name").map(|x| text(x, src))?;
        let obj = inner
            .child_by_field_name("object")
            .filter(|o| o.kind() == "identifier")?;
        let name = name.as_str();
        match self.lookup(&text(obj, src), start, path, src)?.0 {
            JTy::Elems(t) if ELEMENT_OUT.contains(&name) => Some(t),
            JTy::Values(t) if VALUE_OUT.contains(&name) => Some(t),
            JTy::Held(t) if HELD_OUT.contains(&name) => Some(t),
            _ => None,
        }
    }
}

/// What a Java member declares: name → `(scope node id, what it stands for)` for each
/// declaration (see [`java_decls`]).
type JavaDecls = HashMap<String, Vec<(usize, Option<JTy>)>>;

/// Nodes whose declarations are visible only inside them.
const JAVA_SCOPES: &[&str] = &[
    "block",
    "constructor_body",
    "method_declaration",
    "constructor_declaration",
    "compact_constructor_declaration",
    "lambda_expression",
    "for_statement",
    "enhanced_for_statement",
    "catch_clause",
    "try_with_resources_statement",
    "switch_block_statement_group",
    "switch_rule",
    "class_body",
];

const JAVA_TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

/// Record a declaration of `name` standing for `ty` (two different ones: unknown).
fn declare(
    out: &mut HashMap<String, Option<JTy>>,
    name: Option<Node<'_>>,
    ty: Option<JTy>,
    src: &[u8],
) {
    let Some(n) = name.filter(|n| n.kind() == "identifier") else {
        return;
    };
    match out.entry(text(n, src)) {
        std::collections::hash_map::Entry::Vacant(e) => {
            e.insert(ty);
        }
        std::collections::hash_map::Entry::Occupied(mut e) => {
            if e.get() != &ty {
                e.insert(None);
            }
        }
    }
}

/// Variables of a `field_declaration` or `local_variable_declaration`.
fn java_declarators(n: Node<'_>, src: &[u8], out: &mut HashMap<String, Option<JTy>>) {
    let ty = n.child_by_field_name("type");
    let mut c = n.walk();
    for d in n.children_by_field_name("declarator", &mut c) {
        let t = if d.child_by_field_name("dimensions").is_some() {
            None
        } else {
            declared(ty, d.child_by_field_name("value"), src)
        };
        declare(out, d.child_by_field_name("name"), t, src);
    }
}

/// Every declaration in a Java member, with the scope it is visible in: parameters,
/// locals, resources, loop, catch, lambda and pattern variables (nested classes included).
/// A pattern variable is given the block around it (its flow scope can reach past the
/// `if`).
fn java_decls(member: Node<'_>, src: &[u8]) -> JavaDecls {
    let mut out: JavaDecls = HashMap::new();
    let mut stack = vec![(member, member.id())];
    while let Some((n, outer)) = stack.pop() {
        let scope = if JAVA_SCOPES.contains(&n.kind()) {
            n.id()
        } else {
            outer
        };
        // Declarations of this node, in the scope of `at`.
        let mut found: HashMap<String, Option<JTy>> = HashMap::new();
        let mut at = outer;
        match n.kind() {
            "local_variable_declaration" | "field_declaration" => {
                java_declarators(n, src, &mut found)
            }
            "formal_parameter" | "resource" => java_param(n, src, &mut found),
            "enhanced_for_statement" => {
                at = scope;
                let ty = n.child_by_field_name("type");
                if ty.is_some_and(|t| text(t, src) == "var") {
                    // `for (var x : list)`, `for (var x : map.values())`.
                    let t = n
                        .child_by_field_name("value")
                        .and_then(|v| source_of(v, src))
                        .map(|(x, how)| JTy::From(x, how));
                    declare(&mut found, n.child_by_field_name("name"), t, src);
                } else {
                    java_param(n, src, &mut found);
                }
            }
            "catch_formal_parameter" => {
                let mut c = n.walk();
                let t = n
                    .named_children(&mut c)
                    .find(|x| x.kind() == "catch_type")
                    .filter(|ct| ct.named_child_count() == 1)
                    .and_then(|ct| declared(ct.named_child(0), None, src))
                    .filter(|t| matches!(t, JTy::Plain(_)));
                declare(&mut found, n.child_by_field_name("name"), t, src);
            }
            "spread_parameter" => {
                let mut c = n.walk();
                let d = n
                    .named_children(&mut c)
                    .find(|x| x.kind() == "variable_declarator");
                declare(
                    &mut found,
                    d.and_then(|d| d.child_by_field_name("name")),
                    None,
                    src,
                );
            }
            "lambda_expression" => {
                at = scope;
                let ids: Vec<Node<'_>> = match n.child_by_field_name("parameters") {
                    Some(p) if p.kind() == "identifier" => vec![p],
                    Some(p) if p.kind() == "inferred_parameters" => {
                        let mut c = p.walk();
                        p.named_children(&mut c).collect()
                    }
                    _ => Vec::new(),
                };
                let reads = lambda_reads(n, ids.len(), src);
                for (id, t) in ids.into_iter().zip(reads) {
                    declare(&mut found, Some(id), t, src);
                }
            }
            "instanceof_expression" => {
                let t = declared(n.child_by_field_name("right"), None, src);
                declare(&mut found, n.child_by_field_name("name"), t, src);
            }
            "type_pattern" | "record_pattern_component" => {
                let mut c = n.walk();
                let kids: Vec<Node<'_>> = n.named_children(&mut c).collect();
                if let [ty, name] = kids.as_slice() {
                    declare(&mut found, Some(*name), declared(Some(*ty), None, src), src);
                } else if let Some(name) = kids.last() {
                    declare(&mut found, Some(*name), None, src);
                }
            }
            _ => {}
        }
        for (name, t) in found {
            out.entry(name).or_default().push((at, t));
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c).map(|k| (k, scope)));
    }
    out
}

/// The fields a Java class, interface, enum or record body declares itself (and a record's
/// components).
fn java_fields(body: Node<'_>, src: &[u8]) -> HashMap<String, Option<JTy>> {
    let mut out = HashMap::new();
    let mut c = body.walk();
    for m in body.named_children(&mut c) {
        if m.kind() == "field_declaration" || m.kind() == "constant_declaration" {
            java_declarators(m, src, &mut out);
        }
    }
    if let Some(rec) = body.parent().filter(|r| r.kind() == "record_declaration")
        && let Some(params) = rec.child_by_field_name("parameters")
    {
        let mut c = params.walk();
        for p in params.named_children(&mut c) {
            if p.kind() == "formal_parameter" {
                java_param(p, src, &mut out);
            }
        }
    }
    out
}

/// A parameter-like declaration with `name`, `type` and maybe `dimensions` fields.
fn java_param(n: Node<'_>, src: &[u8], out: &mut HashMap<String, Option<JTy>>) {
    let t = if n.child_by_field_name("dimensions").is_some() {
        None
    } else {
        declared(n.child_by_field_name("type"), None, src)
    };
    declare(out, n.child_by_field_name("name"), t, src);
}

#[cfg(test)]
mod tests {
    use crate::grammars::{Grammar, parse};
    use crate::symbols::extract;

    /// `line name → recv_type` of every call with one.
    fn typed(src: &str) -> Vec<String> {
        let g = Grammar::for_lang("java").unwrap();
        let tree = parse(g, src.as_bytes()).unwrap();
        let code = extract(g, &tree, src.as_bytes()).unwrap();
        code.calls
            .iter()
            .filter_map(|c| {
                c.recv_type
                    .as_ref()
                    .map(|t| format!("{} {} {t}", c.line, c.name))
            })
            .collect()
    }

    #[test]
    fn lambda_and_loop_variables_take_the_element_type() {
        let src = r#"
class Fleet {
    private final List<Engine> engines;
    private final Map<String, Engine> byName;
    Fleet(List<Engine> engines, Map<String, Engine> byName, Optional<Engine> spare,
          ObjectProvider<Engine> provider) {
        this.engines = engines;
        engines.stream().filter(e -> e.ready()).forEach(e -> e.start());
        engines.stream().flatMap(e -> e.parts().stream()).count();
        engines.forEach(e -> e.stop());
        byName.values().forEach(v -> v.stop());
        byName.forEach((k, v) -> v.start());
        byName.values().stream().map(v -> v.power()).count();
        spare.ifPresent(s -> s.start());
        provider.getObject().start();
        engines.get(0).stop();
        byName.get("a").stop();
        spare.orElseThrow().stop();
        for (var e : engines) { e.start(); }
        for (var v : byName.values()) { v.stop(); }
    }
    void after() {
        engines.stream().map(e -> e.part()).forEach(p -> p.power());
        engines.size();
        engines.stream().sorted((a, b) -> a.power()).count();
        byName.entrySet().stream().forEach(x -> x.getValue());
    }
}
"#;
        let t = typed(src);
        for want in [
            "8 ready Engine",
            "8 start Engine",
            "9 parts Engine",
            "10 stop Engine",
            "11 stop Engine",
            "12 start Engine",
            "13 power Engine",
            "14 start Engine",
            "15 start Engine",
            "16 stop Engine",
            "17 stop Engine",
            "18 stop Engine",
            "19 start Engine",
            "20 stop Engine",
        ] {
            assert!(t.contains(&want.to_string()), "{want}: {t:?}");
        }
        // After `map` the element is something else; a collection is never its element;
        // a comparator's two parameters and map entries are not typed.
        assert!(t.iter().all(|x| !x.starts_with("23 power")), "{t:?}");
        assert!(t.iter().all(|x| !x.starts_with("24 ")), "{t:?}");
        assert!(t.iter().all(|x| !x.starts_with("25 power")), "{t:?}");
        assert!(t.iter().all(|x| !x.starts_with("26 getValue")), "{t:?}");
        assert!(t.contains(&"23 part Engine".to_string()), "{t:?}");
    }

    #[test]
    fn a_lambda_parameter_hides_nothing_it_does_not_declare() {
        let src = r#"
class A {
    private Motor e;
    void run(List<Engine> engines) {
        e.spin();
        engines.forEach(e -> e.start());
        e.spin();
    }
}
"#;
        let t = typed(src);
        assert_eq!(
            t,
            ["5 spin Motor", "6 start Engine", "7 spin Motor"],
            "{t:?}"
        );
    }
}
