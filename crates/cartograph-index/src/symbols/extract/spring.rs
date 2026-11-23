//! Spring (Java): beans, the places that receive them, and where data is stored.
//!
//! - Beans: classes annotated `@Component`, `@Service`, `@Repository`, `@Controller`,
//!   `@RestController` or `@Configuration`, and `@Bean` methods (their return type), with
//!   the name, `@Primary` and `@Qualifier`s Spring matches them by.
//! - Injection points: `@Autowired` / `@Inject` / `@Resource` fields, the constructor Spring
//!   calls (one marked `@Autowired` or `@Inject`, else the only one of a stereotype class,
//!   or the final fields of a Lombok `@RequiredArgsConstructor` / `@AllArgsConstructor`),
//!   `@Autowired` setters and methods, and `@Bean` method parameters; each with what holds
//!   the beans (`List<T>`, `Map<String, T>`, `ObjectProvider<T>`, `Optional<T>`…) and its
//!   qualifier.
//! - Data: `@Document` / `@Entity` / `@Table` classes, Spring Data repository interfaces
//!   and calls that name a collection or an entity class (`getCollection("x", E.class)`,
//!   `mongoTemplate.find(q, E.class)`, `entityManager.find(E.class, id)`).
//!
//! Matching (which beans an injection point gets) runs after resolution, at query time.

use super::bridge::{arg_nodes, str_expr};
use super::java_types::{generic_parts, holder};
use super::{CallCap, Lang, line, text};
use crate::symbols::strexpr::{Part, StrExpr};
use crate::symbols::{RawBean, RawEntity, RawInjection, RawRepo, RawSink};
use std::collections::HashMap;
use tree_sitter::Node;

/// Class annotations that make a bean of the class.
const STEREOTYPES: &[&str] = &[
    "Component",
    "Service",
    "Repository",
    "Controller",
    "RestController",
    "Configuration",
];
/// Annotations that make Spring inject a field, a constructor or a method.
const INJECTS: &[&str] = &["Autowired", "Inject", "Resource"];
/// Spring Data repository interfaces, with the store they imply (`""`: the entity's).
const REPOS: &[(&str, &str)] = &[
    ("MongoRepository", "mongo"),
    ("ReactiveMongoRepository", "mongo"),
    ("JpaRepository", "jpa"),
    ("CrudRepository", ""),
    ("ListCrudRepository", ""),
    ("PagingAndSortingRepository", ""),
    ("ListPagingAndSortingRepository", ""),
    ("ReactiveCrudRepository", ""),
    ("CassandraRepository", "cassandra"),
    ("ReactiveCassandraRepository", "cassandra"),
    ("MapIdCassandraRepository", "cassandra"),
    ("R2dbcRepository", "jdbc"),
    ("Repository", ""),
];
/// Receiver types whose calls name a Mongo collection or entity.
const MONGO_OPS: &[&str] = &[
    "MongoTemplate",
    "MongoOperations",
    "ReactiveMongoTemplate",
    "ReactiveMongoOperations",
];
/// Their calls that read or write.
const MONGO_CALLS: &[&str] = &[
    "find",
    "findOne",
    "findAll",
    "findById",
    "findAndModify",
    "findAndRemove",
    "findAndReplace",
    "findDistinct",
    "insert",
    "insertAll",
    "save",
    "upsert",
    "updateFirst",
    "updateMulti",
    "remove",
    "count",
    "exists",
    "aggregate",
    "stream",
    "bulkOps",
];
/// Where the overloads of those calls that name a collection have it: `(call, arguments,
/// position)`. With a class there instead, it is another overload.
const COLLECTION_AT: &[(&str, usize, usize)] = &[
    ("find", 3, 2),
    ("findOne", 3, 2),
    ("findAll", 2, 1),
    ("findById", 3, 2),
    ("findDistinct", 4, 2),
    ("findDistinct", 5, 2),
    ("findAndModify", 4, 3),
    ("findAndModify", 5, 4),
    ("findAndReplace", 3, 2),
    ("findAndReplace", 4, 3),
    ("findAndReplace", 5, 4),
    ("findAndReplace", 6, 4),
    ("findAndRemove", 3, 2),
    ("insert", 2, 1),
    ("save", 2, 1),
    ("upsert", 3, 2),
    ("upsert", 4, 3),
    ("updateFirst", 3, 2),
    ("updateFirst", 4, 3),
    ("updateMulti", 3, 2),
    ("updateMulti", 4, 3),
    ("remove", 2, 1),
    ("remove", 3, 2),
    ("count", 2, 1),
    ("count", 3, 2),
    ("exists", 2, 1),
    ("exists", 3, 2),
    ("aggregate", 3, 1),
    ("stream", 3, 2),
    ("bulkOps", 2, 1),
    ("bulkOps", 3, 2),
];
/// JPA `EntityManager` calls that name the entity class.
const JPA_CALLS: &[&str] = &["find", "getReference"];

/// What a Java file declares and calls for Spring and its stores.
#[derive(Debug, Default)]
pub(super) struct Spring {
    pub beans: Vec<RawBean>,
    pub injections: Vec<RawInjection>,
    pub entities: Vec<RawEntity>,
    pub repos: Vec<RawRepo>,
    pub sinks: Vec<RawSink>,
}

/// Where the names this extraction knows come from: an annotation or a Spring Data
/// interface counts only when it is Spring's (or the `javax.*` / `jakarta.*` standard it
/// follows, or Lombok's), written with its package or imported from it.
const ORIGINS: &[(&str, &[&str])] = &[
    ("Component", &["org.springframework."]),
    ("Service", &["org.springframework."]),
    ("Repository", &["org.springframework."]),
    ("Controller", &["org.springframework."]),
    ("RestController", &["org.springframework."]),
    ("Configuration", &["org.springframework."]),
    ("Bean", &["org.springframework."]),
    ("Primary", &["org.springframework."]),
    ("Autowired", &["org.springframework."]),
    ("Value", &["org.springframework."]),
    (
        "Qualifier",
        &["org.springframework.", "javax.inject.", "jakarta.inject."],
    ),
    ("Inject", &["javax.inject.", "jakarta.inject."]),
    ("Named", &["javax.inject.", "jakarta.inject."]),
    ("Resource", &["javax.annotation.", "jakarta.annotation."]),
    ("Document", &["org.springframework.data.mongodb."]),
    (
        "Table",
        &[
            "javax.persistence.",
            "jakarta.persistence.",
            "org.springframework.data.relational.",
            "org.springframework.data.cassandra.",
        ],
    ),
    ("Entity", &["javax.persistence.", "jakarta.persistence."]),
    ("Mapper", &["org.mapstruct."]),
    ("ConfigurationProperties", &["org.springframework.boot."]),
    ("RequiredArgsConstructor", &["lombok."]),
    ("AllArgsConstructor", &["lombok."]),
    ("NoRepositoryBean", &["org.springframework.data."]),
    ("MongoRepository", &["org.springframework.data."]),
    ("ReactiveMongoRepository", &["org.springframework.data."]),
    ("JpaRepository", &["org.springframework.data."]),
    ("CassandraRepository", &["org.springframework.data."]),
    (
        "ReactiveCassandraRepository",
        &["org.springframework.data."],
    ),
    ("MapIdCassandraRepository", &["org.springframework.data."]),
    ("R2dbcRepository", &["org.springframework.data."]),
    ("CrudRepository", &["org.springframework.data."]),
    ("ListCrudRepository", &["org.springframework.data."]),
    ("PagingAndSortingRepository", &["org.springframework.data."]),
    (
        "ListPagingAndSortingRepository",
        &["org.springframework.data."],
    ),
    ("ReactiveCrudRepository", &["org.springframework.data."]),
];

/// A Java file's imports: simple name → qualified name, and the packages of `.*` imports.
#[derive(Debug, Default)]
struct Imports {
    named: HashMap<String, String>,
    globs: Vec<String>,
}

impl Imports {
    fn of(root: Node<'_>, src: &[u8]) -> Imports {
        let mut out = Imports::default();
        let mut c = root.walk();
        for n in root.named_children(&mut c) {
            if n.kind() != "import_declaration" {
                continue;
            }
            let t = text(n, src);
            if t.contains(" static ") {
                continue;
            }
            let path: String = t
                .trim_start_matches("import")
                .trim_end_matches(';')
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            match path.strip_suffix(".*") {
                Some(pkg) => out.globs.push(format!("{pkg}.")),
                None => {
                    let simple = path.rsplit('.').next().unwrap_or(&path).to_string();
                    out.named.insert(simple, path);
                }
            }
        }
        out
    }

    /// The file imports something of a package.
    fn any(&self, pkg: &str) -> bool {
        self.named
            .values()
            .chain(&self.globs)
            .any(|q| q.starts_with(pkg))
    }

    /// The simple name of `written` (`Inject`, `jakarta.inject.Inject`) when it is the
    /// annotation or interface this extraction knows by that name; other names pass as
    /// they are (nothing reads them).
    fn known(&self, written: &str) -> Option<String> {
        self.origin(written).map(|(simple, _)| simple)
    }

    /// [`Imports::known`], with the package it comes from (empty for a name this
    /// extraction does not know).
    fn origin(&self, written: &str) -> Option<(String, String)> {
        let simple = written.rsplit('.').next().unwrap_or(written).to_string();
        let Some((_, origins)) = ORIGINS.iter().find(|(n, _)| *n == simple) else {
            return Some((simple, String::new()));
        };
        let from = |q: &str| origins.iter().any(|o| q.starts_with(o));
        let q = if written.contains('.') {
            Some(written)
        } else if let Some(q) = self.named.get(&simple) {
            Some(q.as_str())
        } else {
            self.globs.iter().map(String::as_str).find(|g| from(g))
        };
        q.filter(|q| from(q)).map(|q| (simple, q.to_string()))
    }
}

/// An annotation: its simple name and its arguments (`None` key: `value`).
struct Annot<'t> {
    name: String,
    /// Where it comes from (see [`Imports::origin`]).
    origin: String,
    args: Vec<(Option<String>, Node<'t>)>,
}

impl<'t> Annot<'t> {
    /// The argument named one of `keys` (`"value"` also matches an unnamed one).
    fn arg(&self, keys: &[&str]) -> Option<Node<'t>> {
        self.args.iter().find_map(|(k, v)| {
            let k = k.as_deref().unwrap_or("value");
            keys.contains(&k).then_some(*v)
        })
    }
}

/// The annotations of a declaration (in its `modifiers`).
fn annotations<'t>(decl: Node<'t>, src: &[u8], imp: &Imports) -> Vec<Annot<'t>> {
    let mut out = Vec::new();
    let mut c = decl.walk();
    for m in decl.children(&mut c).filter(|m| m.kind() == "modifiers") {
        let mut c2 = m.walk();
        for a in m.named_children(&mut c2) {
            if !matches!(a.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            let Some(name) = a.child_by_field_name("name").map(|x| text(x, src)) else {
                continue;
            };
            let Some((name, origin)) = imp.origin(&name) else {
                continue;
            };
            let mut args = Vec::new();
            if let Some(list) = a.child_by_field_name("arguments") {
                let mut c3 = list.walk();
                for x in list.named_children(&mut c3) {
                    if x.kind() == "element_value_pair" {
                        if let (Some(k), Some(v)) =
                            (x.child_by_field_name("key"), x.child_by_field_name("value"))
                        {
                            args.push((Some(text(k, src)), v));
                        }
                    } else if !x.kind().contains("comment") {
                        args.push((None, x));
                    }
                }
            }
            out.push(Annot { name, origin, args });
        }
    }
    out
}

/// Whether a declaration's modifiers include the keyword `kw` (`static`, `final`).
fn has_modifier(decl: Node<'_>, kw: &str) -> bool {
    let mut c = decl.walk();
    decl.children(&mut c)
        .filter(|m| m.kind() == "modifiers")
        .any(|m| {
            let mut c2 = m.walk();
            m.children(&mut c2).any(|k| k.kind() == kw)
        })
}

/// The strings an annotation value holds (`"x"`, `{"x", Names.Y}`), as written.
fn strings(v: Node<'_>, src: &[u8]) -> Vec<StrExpr> {
    if v.kind() == "element_value_array_initializer" {
        let mut c = v.walk();
        return v
            .named_children(&mut c)
            .flat_map(|x| strings(x, src))
            .collect();
    }
    let e = str_expr(Lang::Java, v, src);
    match e.0.as_slice() {
        [Part::Lit(l)] if l.is_empty() => Vec::new(),
        [] => Vec::new(),
        _ => vec![e],
    }
}

/// `@Qualifier` and `@Named` values on a bean.
fn qualifiers(annots: &[Annot<'_>], src: &[u8]) -> Vec<StrExpr> {
    annots
        .iter()
        .filter(|a| a.name == "Qualifier" || a.name == "Named")
        .filter_map(|a| a.arg(&["value"]))
        .flat_map(|v| strings(v, src))
        .collect()
}

/// An annotation value as a string expression: a literal, a constant, a concatenation.
fn value_expr(v: Node<'_>, src: &[u8]) -> Option<StrExpr> {
    let v = if v.kind() == "element_value_array_initializer" {
        let mut c = v.walk();
        v.named_children(&mut c).next()?
    } else {
        v
    };
    let e = str_expr(Lang::Java, v, src);
    let known =
        e.0.first()
            .is_some_and(|p| matches!(p, Part::Lit(_) | Part::Ref(_)));
    known.then_some(e)
}

/// The bean name a `@Resource` field or setter asks for, and whether it is the default
/// one (`member`): `@Resource(name = "x")`, a JNDI lookup (not known).
fn resource(annots: &[Annot<'_>], member: &str, src: &[u8]) -> Option<(StrExpr, bool)> {
    let a = annots.iter().find(|a| a.name == "Resource")?;
    if let Some(v) = a.arg(&["lookup"]) {
        return Some((StrExpr::hole(&text(v, src)), false));
    }
    let given = a
        .arg(&["name"])
        .and_then(|v| strings(v, src).into_iter().next());
    Some(match given {
        Some(e) => (e, false),
        None => (StrExpr::lit(member), true),
    })
}

/// The name a `@Resource` asks for where what is injected holds beans: a collection of
/// that name is a bean whose elements are not known; a default name is the holder's.
fn in_wrapper(r: Option<(StrExpr, bool)>, wrapper: &str) -> Option<(StrExpr, bool)> {
    let (name, default) = r?;
    let many = wrapper == "[]"
        || cartograph_core::spring::COLLECTIONS.contains(&wrapper)
        || cartograph_core::spring::MAPS.contains(&wrapper);
    match (wrapper.is_empty(), default) {
        (true, _) => Some((name, default)),
        (false, true) => None,
        (false, false) if many => Some((StrExpr::hole("elements"), false)),
        (false, false) => Some((name, false)),
    }
}

/// The qualifier of a field or parameter: `@Qualifier("x")`, `@Named("x")`.
fn qualifier(annots: &[Annot<'_>], src: &[u8]) -> Option<StrExpr> {
    annots
        .iter()
        .filter(|a| a.name == "Qualifier" || a.name == "Named")
        .find_map(|a| a.arg(&["value"]))
        .and_then(|v| strings(v, src).into_iter().next())
}

/// `(wrapper, type)` of a declared type: `("", "T")`, `("List", "T")`, `("[]", "T")`.
fn injected_type(ty: Node<'_>, src: &[u8]) -> Option<(String, String)> {
    match ty.kind() {
        "type_identifier" => Some((String::new(), text(ty, src))),
        "scoped_type_identifier" => {
            let t = text(ty, src);
            Some((
                String::new(),
                t.rsplit('.').next().unwrap_or(&t).to_string(),
            ))
        }
        // Any other generic type is matched by its raw type.
        "generic_type" => holder(ty, src)
            .or_else(|| generic_parts(ty, src).map(|(name, _)| (String::new(), name))),
        "array_type" => {
            let (w, t) = injected_type(ty.child_by_field_name("element")?, src)?;
            w.is_empty().then(|| ("[]".to_string(), t))
        }
        _ => None,
    }
}

/// Types never injected as beans of the repo.
fn plain_value(ty: &str) -> bool {
    matches!(
        ty,
        "String"
            | "Integer"
            | "Long"
            | "Boolean"
            | "Double"
            | "Float"
            | "Short"
            | "Byte"
            | "Character"
            | "Object"
            | "var"
    )
}

/// The fields of a class body (name → its line and what its type injects), and the
/// assignments to them.
#[derive(Default)]
struct Fields {
    decls: HashMap<String, (u32, Option<(String, String)>)>,
    /// `(field, id of the method or constructor, in a method, the parameter assigned)`.
    sets: Vec<(String, usize, bool, Option<String>)>,
    /// The constructors' parameters, by node id.
    ctors: HashMap<usize, Vec<String>>,
    /// Parameters given another value: `(method or constructor, name)`.
    changed: Vec<(usize, String)>,
    /// `this(a, b)`: the constructor that delegates, and the variables it passes.
    delegates: Vec<(usize, Vec<Option<String>>)>,
}

/// Calls that give back what they are given, or a copy of its elements.
const PASSES: &[&str] = &[
    "requireNonNull",
    "copyOf",
    "unmodifiableList",
    "unmodifiableSet",
    "unmodifiableCollection",
    "unmodifiableMap",
];

impl Fields {
    fn of(members: &[Node<'_>], src: &[u8]) -> Fields {
        let mut out = Fields::default();
        for m in members.iter().filter(|m| m.kind() == "field_declaration") {
            let ty = m
                .child_by_field_name("type")
                .and_then(|ty| injected_type(ty, src));
            let mut c = m.walk();
            for d in m.children_by_field_name("declarator", &mut c) {
                if let Some(n) = d.child_by_field_name("name") {
                    out.decls.insert(text(n, src), (line(d), ty.clone()));
                }
            }
        }
        for m in members {
            let method = m.kind() == "method_declaration";
            if !method && m.kind() != "constructor_declaration" {
                continue;
            }
            // Parameters hide the fields of their name.
            let params: Vec<String> = m
                .child_by_field_name("parameters")
                .map(|ps| {
                    let mut c = ps.walk();
                    ps.named_children(&mut c)
                        .filter_map(|p| p.child_by_field_name("name"))
                        .map(|n| text(n, src))
                        .collect()
                })
                .unwrap_or_default();
            if !method {
                out.ctors.insert(m.id(), params.clone());
            }
            let mut stack = vec![*m];
            while let Some(n) = stack.pop() {
                let mut c = n.walk();
                stack.extend(n.named_children(&mut c));
                if n.kind() == "explicit_constructor_invocation"
                    && n.child_by_field_name("constructor")
                        .is_some_and(|t| t.kind() == "this")
                {
                    let args = arg_nodes(n.child_by_field_name("arguments"))
                        .into_iter()
                        .map(|a| passed(a, src).filter(|x| params.contains(x)))
                        .collect();
                    out.delegates.push((m.id(), args));
                }
                if n.kind() != "assignment_expression" {
                    continue;
                }
                let right = n.child_by_field_name("right").and_then(|r| passed(r, src));
                if let Some(l) = n
                    .child_by_field_name("left")
                    .filter(|l| l.kind() == "identifier")
                    .map(|l| text(l, src))
                    .filter(|l| params.contains(l) && right.as_ref() != Some(l))
                {
                    out.changed.push((m.id(), l));
                    continue;
                }
                let field = n.child_by_field_name("left").and_then(|l| match l.kind() {
                    "identifier" => Some(text(l, src)).filter(|x| !params.contains(x)),
                    "field_access" => l
                        .child_by_field_name("object")
                        .filter(|o| o.kind() == "this")
                        .and_then(|_| l.child_by_field_name("field"))
                        .map(|f| text(f, src)),
                    _ => None,
                });
                let Some(field) = field.filter(|f| out.decls.contains_key(f)) else {
                    continue;
                };
                let given = right.filter(|x| params.contains(x));
                out.sets.push((field, m.id(), method, given));
            }
        }
        out
    }

    /// The fields that hold what `param` of constructor or method `m` receives: assigned
    /// from it there, or in the constructor `m` hands it to (`this(param, …)`), of its
    /// type, and assigned in no other method.
    fn holding(&self, m: usize, param: &str, ty: &str) -> Vec<(String, u32)> {
        if self.changes(m, param) {
            return Vec::new();
        }
        for (_, args) in self.delegates.iter().filter(|(from, _)| *from == m) {
            let to: Vec<(&usize, &Vec<String>)> = self
                .ctors
                .iter()
                .filter(|(id, ps)| **id != m && ps.len() == args.len())
                .collect();
            let at = args.iter().position(|a| a.as_deref() == Some(param));
            if let ([(id, ps)], Some(at)) = (to.as_slice(), at)
                && !self.delegates.iter().any(|(from, _)| from == *id)
            {
                // What `m` itself sets from something else is no longer that.
                return self
                    .holding(**id, &ps[at], ty)
                    .into_iter()
                    .filter(|(f, _)| {
                        !self.sets.iter().any(|(x, at, _, given)| {
                            x == f && *at == m && given.as_deref() != Some(param)
                        })
                    })
                    .collect();
            }
        }
        let mut out: Vec<(String, u32)> = self
            .sets
            .iter()
            .filter(|(_, at, _, given)| *at == m && given.as_deref() == Some(param))
            .filter_map(|(f, ..)| {
                let (l, t) = self.decls.get(f)?;
                // Set from something else there too: what it ends with is not known.
                let again = self
                    .sets
                    .iter()
                    .any(|(x, at, _, g)| x == f && *at == m && g.as_deref() != Some(param));
                let same = !again && t.as_ref().is_some_and(|(_, t)| t == ty);
                (same && !self.set_elsewhere(f, m)).then(|| (f.clone(), *l))
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// `param` of `m` is given another value there.
    fn changes(&self, m: usize, param: &str) -> bool {
        self.changed.iter().any(|(at, p)| *at == m && p == param)
    }

    /// Some method (not a constructor) other than `m` assigns field `f`.
    fn set_elsewhere(&self, f: &str, m: usize) -> bool {
        self.sets
            .iter()
            .any(|(x, at, method, _)| x == f && *method && *at != m)
    }
}

/// The variable an expression passes on: `p`, `Objects.requireNonNull(p)`,
/// `List.copyOf(p)`, `new ArrayList<>(p)`.
fn passed(n: Node<'_>, src: &[u8]) -> Option<String> {
    let args = match n.kind() {
        "identifier" => return Some(text(n, src)),
        "method_invocation" => {
            let name = n.child_by_field_name("name").map(|x| text(x, src))?;
            PASSES.contains(&name.as_str()).then_some(())?;
            n.child_by_field_name("arguments")
        }
        "object_creation_expression" => {
            let ty = n.child_by_field_name("type")?;
            let (name, _) = generic_parts(ty, src)?;
            let copies = cartograph_core::spring::COLLECTIONS.contains(&name.as_str())
                || cartograph_core::spring::MAPS.contains(&name.as_str());
            copies.then_some(())?;
            n.child_by_field_name("arguments")
        }
        _ => return None,
    };
    let first = arg_nodes(args).into_iter().next()?;
    (first.kind() == "identifier").then(|| text(first, src))
}

/// The parameters of a constructor or method as injection points.
fn params_of(
    m: Node<'_>,
    class: &str,
    how: &str,
    src: &[u8],
    imp: &Imports,
    fields: &Fields,
    out: &mut Vec<RawInjection>,
) {
    let Some(ps) = m.child_by_field_name("parameters") else {
        return;
    };
    let mut c = ps.walk();
    for p in ps.named_children(&mut c) {
        if p.kind() != "formal_parameter" {
            continue;
        }
        let annots = annotations(p, src, imp);
        if annots.iter().any(|a| a.name == "Value") {
            continue;
        }
        let (Some(ty), Some(name)) = (p.child_by_field_name("type"), p.child_by_field_name("name"))
        else {
            continue;
        };
        let Some((wrapper, t)) = injected_type(ty, src) else {
            continue;
        };
        if plain_value(&t) {
            continue;
        }
        out.push(RawInjection {
            line: line(p),
            class: class.to_string(),
            how: how.to_string(),
            fields: fields.holding(m.id(), &text(name, src), &t),
            param: !fields.changes(m.id(), &text(name, src)),
            member: text(name, src),
            wrapper,
            ty: t,
            qualifier: qualifier(&annots, src),
            resource: None,
        });
    }
}

/// Beans, injection points, entities and repositories of the classes of a Java file.
fn classes(root: Node<'_>, src: &[u8], imp: &Imports, out: &mut Spring) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
        match n.kind() {
            "class_declaration" | "record_declaration" => class(n, src, imp, out),
            "interface_declaration" => {
                repo(n, src, imp, out);
                if let Some(name) = n.child_by_field_name("name").map(|x| text(x, src)) {
                    generated_bean(n, &name, &annotations(n, src, imp), src, out);
                }
            }
            _ => {}
        }
    }
}

/// A bean whose class a tool writes or Spring Boot registers from the type: a MapStruct
/// mapper of the `spring` component model (its generated implementation is the bean), a
/// `@ConfigurationProperties` class.
fn generated_bean(n: Node<'_>, name: &str, annots: &[Annot<'_>], src: &[u8], out: &mut Spring) {
    let made = annots.iter().find(|a| match a.name.as_str() {
        "Mapper" => a
            .arg(&["componentModel"])
            .is_some_and(|v| text(v, src).to_ascii_lowercase().contains("spring")),
        "ConfigurationProperties" => true,
        _ => false,
    });
    if let Some(a) = made {
        // MapStruct writes the bean's class (`OrderMapperImpl`).
        let class = if a.name == "Mapper" {
            format!("{name}Impl")
        } else {
            name.to_string()
        };
        out.beans.push(RawBean {
            line: line(n),
            ty: name.to_string(),
            stereotype: a.name.clone(),
            default_name: crate::symbols::decapitalize(&class),
            names: Vec::new(),
            factory: None,
            primary: annots.iter().any(|a| a.name == "Primary"),
            qualifiers: Vec::new(),
        });
    }
}

/// A Spring Data repository interface.
fn repo(n: Node<'_>, src: &[u8], imp: &Imports, out: &mut Spring) {
    let Some(name) = n.child_by_field_name("name").map(|x| text(x, src)) else {
        return;
    };
    let mut c = n.walk();
    let Some(ext) = n
        .named_children(&mut c)
        .find(|x| x.kind() == "extends_interfaces")
    else {
        return;
    };
    let mut stack = vec![ext];
    while let Some(x) = stack.pop() {
        if x.kind() == "generic_type"
            && let Some(base) = x.named_child(0).and_then(|b| imp.known(&text(b, src)))
            && REPOS.iter().any(|(r, _)| *r == base)
            && let Some(args) = x.named_child(1).filter(|a| a.kind() == "type_arguments")
            && let Some(entity) = args
                .named_child(0)
                .filter(|e| e.kind() == "type_identifier")
        {
            let annots = annotations(n, src, imp);
            // An abstract base of repositories (`@NoRepositoryBean`, or of any entity).
            let any = n.child_by_field_name("type_parameters").is_some();
            if any || annots.iter().any(|a| a.name == "NoRepositoryBean") {
                return;
            }
            // Spring Data writes its implementation: the bean.
            out.beans.push(RawBean {
                line: line(n),
                ty: name.clone(),
                stereotype: base.clone(),
                default_name: crate::symbols::decapitalize(&name),
                names: annots
                    .iter()
                    .find(|a| STEREOTYPES.contains(&a.name.as_str()))
                    .and_then(|a| a.arg(&["value"]))
                    .map(|v| strings(v, src))
                    .unwrap_or_default(),
                factory: None,
                primary: annots.iter().any(|a| a.name == "Primary"),
                qualifiers: qualifiers(&annots, src),
            });
            let store = REPOS
                .iter()
                .find(|(r, _)| *r == base)
                .map_or("", |(_, store)| *store);
            out.repos.push(RawRepo {
                line: line(n),
                class: name.clone(),
                store: store.to_string(),
                base,
                entity: text(entity, src),
            });
            return;
        }
        let mut c = x.walk();
        stack.extend(x.named_children(&mut c));
    }
}

/// A class with the classes around it (`Outer.Inner`), as Spring names a nested bean.
fn short_name(n: Node<'_>, name: &str, src: &[u8]) -> String {
    let mut out = name.to_string();
    let mut cur = n.parent();
    while let Some(p) = cur {
        if matches!(
            p.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
        ) && let Some(outer) = p.child_by_field_name("name")
        {
            out = format!("{}.{out}", text(outer, src));
        }
        cur = p.parent();
    }
    out
}

fn class(n: Node<'_>, src: &[u8], imp: &Imports, out: &mut Spring) {
    let Some(name) = n.child_by_field_name("name").map(|x| text(x, src)) else {
        return;
    };
    let annots = annotations(n, src, imp);
    let stereotype = annots
        .iter()
        .find(|a| STEREOTYPES.contains(&a.name.as_str()));
    if stereotype.is_none() {
        generated_bean(n, &name, &annots, src, out);
    }
    if let Some(st) = stereotype {
        out.beans.push(RawBean {
            line: line(n),
            ty: name.clone(),
            stereotype: st.name.clone(),
            default_name: crate::symbols::decapitalize(&short_name(n, &name, src)),
            names: st
                .arg(&["value"])
                .map(|v| strings(v, src))
                .unwrap_or_default(),
            factory: None,
            primary: annots.iter().any(|a| a.name == "Primary"),
            qualifiers: qualifiers(&annots, src),
        });
    }
    for a in &annots {
        let (store, keys): (&str, &[&str]) = match a.name.as_str() {
            "Document" => ("mongo", &["collection", "value"]),
            "Table" if a.origin.contains(".data.relational.") => ("jdbc", &["name", "value"]),
            "Table" if a.origin.contains(".data.cassandra.") => ("cassandra", &["value"]),
            "Table" => ("jpa", &["name"]),
            "Entity" if !annots.iter().any(|b| b.name == "Table") => ("jpa", &["name"]),
            _ => continue,
        };
        out.entities.push(RawEntity {
            line: line(n),
            class: name.clone(),
            store: store.to_string(),
            name: a.arg(keys).and_then(|v| value_expr(v, src)),
        });
    }
    let Some(body) = n.child_by_field_name("body") else {
        return;
    };
    let mut c = body.walk();
    let members: Vec<Node<'_>> = body.named_children(&mut c).collect();
    let fields = Fields::of(&members, src);
    let ctors: Vec<Node<'_>> = members
        .iter()
        .copied()
        .filter(|m| m.kind() == "constructor_declaration")
        .collect();
    let marked: Vec<Node<'_>> = ctors
        .iter()
        .copied()
        .filter(|m| {
            annotations(*m, src, imp)
                .iter()
                .any(|a| a.name == "Autowired" || a.name == "Inject")
        })
        .collect();
    // The constructor Spring calls: the one marked, else the only one of a bean class.
    match (marked.as_slice(), ctors.as_slice()) {
        ([one], _) => params_of(
            *one,
            &name,
            "constructor",
            src,
            imp,
            &fields,
            &mut out.injections,
        ),
        ([], [one]) if stereotype.is_some() => params_of(
            *one,
            &name,
            "constructor",
            src,
            imp,
            &fields,
            &mut out.injections,
        ),
        _ => {}
    }
    let lombok = annots.iter().find_map(|a| match a.name.as_str() {
        "RequiredArgsConstructor" => Some(true),
        "AllArgsConstructor" => Some(false),
        _ => None,
    });
    for m in &members {
        match m.kind() {
            "field_declaration" => {
                let fa = annotations(*m, src, imp);
                if fa.iter().any(|a| a.name == "Value") || has_modifier(*m, "static") {
                    continue;
                }
                let injected = fa.iter().any(|a| INJECTS.contains(&a.name.as_str()));
                // Lombok writes the constructor of a bean class without one.
                let by_lombok = stereotype.is_some()
                    && ctors.is_empty()
                    && match lombok {
                        Some(true) => has_modifier(*m, "final"),
                        Some(false) => true,
                        None => false,
                    };
                if !injected && !by_lombok {
                    continue;
                }
                let Some((wrapper, t)) = m
                    .child_by_field_name("type")
                    .and_then(|ty| injected_type(ty, src))
                else {
                    continue;
                };
                if plain_value(&t) {
                    continue;
                }
                let mut c = m.walk();
                for d in m.children_by_field_name("declarator", &mut c) {
                    // A final field with a value is not set by the constructor.
                    if !injected && d.child_by_field_name("value").is_some() {
                        continue;
                    }
                    let Some(field) = d.child_by_field_name("name").map(|x| text(x, src)) else {
                        continue;
                    };
                    let name_of_field = field.clone();
                    let holds = if fields.set_elsewhere(&field, 0) {
                        Vec::new()
                    } else {
                        vec![(field.clone(), line(d))]
                    };
                    out.injections.push(RawInjection {
                        fields: holds,
                        param: false,
                        line: line(d),
                        class: name.clone(),
                        how: if injected { "field" } else { "constructor" }.to_string(),
                        member: name_of_field.clone(),
                        wrapper: wrapper.clone(),
                        ty: t.clone(),
                        qualifier: qualifier(&fa, src),
                        resource: in_wrapper(resource(&fa, &name_of_field, src), &wrapper),
                    });
                }
            }
            "method_declaration" => {
                let ma = annotations(*m, src, imp);
                let mname = m
                    .child_by_field_name("name")
                    .map(|x| text(x, src))
                    .unwrap_or_default();
                if let Some(bean) = ma.iter().find(|a| a.name == "Bean") {
                    if let Some((_, t)) =
                        m.child_by_field_name("type")
                            .and_then(|ty| match ty.kind() {
                                "generic_type" => {
                                    ty.named_child(0).map(|b| (String::new(), text(b, src)))
                                }
                                _ => injected_type(ty, src),
                            })
                    {
                        out.beans.push(RawBean {
                            line: line(*m),
                            ty: t,
                            stereotype: "Bean".to_string(),
                            default_name: mname.clone(),
                            names: bean
                                .arg(&["name", "value"])
                                .map(|v| strings(v, src))
                                .unwrap_or_default(),
                            factory: Some(format!("{name}.{mname}")),
                            primary: ma.iter().any(|a| a.name == "Primary"),
                            qualifiers: qualifiers(&ma, src),
                        });
                    }
                    params_of(*m, &name, "bean", src, imp, &fields, &mut out.injections);
                } else if ma.iter().any(|a| INJECTS.contains(&a.name.as_str())) {
                    let how = if mname.starts_with("set") {
                        "setter"
                    } else {
                        "method"
                    };
                    let before = out.injections.len();
                    params_of(*m, &name, how, src, imp, &fields, &mut out.injections);
                    // A qualifier or a `@Resource` on a setter is its one parameter's; the
                    // property names the bean (`setWire` → `wire`).
                    let property = mname
                        .strip_prefix("set")
                        .filter(|p| !p.is_empty())
                        .map_or(mname.clone(), crate::symbols::decapitalize);
                    let q = qualifier(&ma, src);
                    let r = resource(&ma, &property, src);
                    for i in &mut out.injections[before..] {
                        if i.qualifier.is_none() {
                            i.qualifier = q.clone();
                        }
                        i.resource = in_wrapper(r.clone(), &i.wrapper);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The class a `X.class` argument names.
fn class_literal(n: Node<'_>, src: &[u8]) -> Option<String> {
    if n.kind() != "class_literal" {
        return None;
    }
    let t = text(n, src);
    let t = t.strip_suffix(".class")?.trim();
    Some(t.rsplit('.').next().unwrap_or(t).to_string())
}

/// Calls that name a collection or an entity class.
fn sinks(
    calls: &[CallCap<'_>],
    recv_types: &HashMap<usize, String>,
    src: &[u8],
    imp: &Imports,
) -> Vec<RawSink> {
    let mongo = imp.any("com.mongodb.") || imp.any("org.springframework.data.mongodb.");
    let mut out = Vec::new();
    for c in calls {
        if c.node.kind() != "method_invocation" {
            continue;
        }
        let name = text(c.name, src);
        let args = arg_nodes(c.args.or_else(|| c.node.child_by_field_name("arguments")));
        let recv = c.recv.map(|r| text(r, src)).unwrap_or_default();
        let recv_ty = recv_types.get(&c.node.id()).map(String::as_str);
        let entity = args.iter().find_map(|a| class_literal(*a, src));
        // A collection named by a literal or a constant (not a parameter or a call).
        let named = |a: &Node<'_>| -> Option<StrExpr> {
            if !matches!(a.kind(), "string_literal" | "identifier" | "field_access") {
                return None;
            }
            let e = str_expr(Lang::Java, *a, src);
            matches!(e.0.as_slice(), [Part::Lit(_)] | [Part::Ref(_)]).then_some(e)
        };
        let field = recv
            .rsplit('.')
            .next()
            .unwrap_or(&recv)
            .to_ascii_lowercase();
        let mongo_recv = recv_ty.is_some_and(|t| MONGO_OPS.contains(&t))
            || field.ends_with("mongotemplate")
            || field.ends_with("mongooperations");
        let (store, what, coll) = if name == "getCollection" && mongo {
            // `db.getCollection("x")`, `db.getCollection(X, E.class)`.
            let Some(first) = args.first().and_then(named) else {
                continue;
            };
            ("mongo", name.clone(), Some(first))
        } else if mongo_recv && MONGO_CALLS.contains(&name.as_str()) {
            let coll = COLLECTION_AT
                .iter()
                .filter(|(call, n, _)| *call == name && *n == args.len())
                .filter_map(|(.., at)| args.get(*at))
                .find(|a| {
                    a.kind() == "string_literal" || {
                        let t = text(**a, src);
                        let last = t.rsplit('.').next().unwrap_or(&t);
                        crate::symbols::strexpr::looks_constant(last)
                    }
                })
                .and_then(named);
            if coll.is_none() && entity.is_none() {
                continue;
            }
            ("mongo", format!("{}.{name}", short_recv(&recv)), coll)
        } else if recv_ty == Some("EntityManager") && JPA_CALLS.contains(&name.as_str()) {
            if entity.is_none() {
                continue;
            }
            ("jpa", format!("{}.{name}", short_recv(&recv)), None)
        } else {
            continue;
        };
        out.push(RawSink {
            line: line(c.node),
            store: store.to_string(),
            what,
            name: coll,
            entity: entity.clone(),
        });
    }
    out
}

/// `this.mongoTemplate` → `mongoTemplate`.
fn short_recv(recv: &str) -> &str {
    recv.strip_prefix("this.").unwrap_or(recv)
}

/// Everything above for one Java file.
pub(super) fn java(
    root: Node<'_>,
    src: &[u8],
    calls: &[CallCap<'_>],
    recv_types: &HashMap<usize, String>,
) -> Spring {
    let mut out = Spring::default();
    let imp = Imports::of(root, src);
    classes(root, src, &imp, &mut out);
    out.sinks = sinks(calls, recv_types, src, &imp);
    out.beans.sort_by_key(|b| b.line);
    out.injections.sort_by_key(|i| i.line);
    out.entities.sort_by_key(|e| e.line);
    out.repos.sort_by_key(|r| r.line);
    out
}

#[cfg(test)]
mod tests {
    use crate::grammars::{Grammar, parse};
    use crate::symbols::strexpr::Part;
    use crate::symbols::{Code, extract};

    fn code(src: &str) -> Code {
        let g = Grammar::for_lang("java").unwrap();
        let tree = parse(g, src.as_bytes()).unwrap();
        extract(g, &tree, src.as_bytes()).unwrap()
    }

    fn shown(e: &crate::symbols::strexpr::StrExpr) -> String {
        match e.0.as_slice() {
            [Part::Lit(l)] => l.clone(),
            [Part::Ref(r)] => format!("@{r}"),
            _ => "?".into(),
        }
    }

    /// A bean's names as given, else its default.
    fn named(b: &crate::symbols::RawBean) -> String {
        if b.names.is_empty() {
            return b.default_name.clone();
        }
        b.names.iter().map(shown).collect::<Vec<_>>().join("|")
    }

    fn injections(c: &Code) -> Vec<String> {
        c.injections
            .iter()
            .map(|i| {
                let asked = i.resource.as_ref().map_or(String::new(), |(e, default)| {
                    format!(" #{}{}", shown(e), if *default { "?" } else { "" })
                });
                format!(
                    "{} {} {} {}<{}>{}{asked}",
                    i.class,
                    i.how,
                    i.member,
                    i.wrapper,
                    i.ty,
                    i.qualifier
                        .as_ref()
                        .map_or(String::new(), |q| format!(" @{}", shown(q)))
                )
            })
            .collect()
    }

    #[test]
    fn beans_with_names_primary_and_qualifiers() {
        let c = code(
            r#"import org.springframework.stereotype.*; import org.springframework.context.annotation.*; import org.springframework.beans.factory.annotation.Qualifier;
@Service
public class CardPayments implements Payments {}
@Component("wire") @Primary
class WirePayments implements Payments {}
@Repository @Qualifier("fast")
class URLStore {}
@Configuration
class Config {
    @Bean @Primary
    Payments cash(Ledger ledger) { return new CashPayments(ledger); }
    @Bean(name = "slow")
    Clock clock() { return null; }
}
class Plain {}
"#,
        );
        let beans: Vec<String> = c
            .beans
            .iter()
            .map(|b| {
                format!(
                    "{} {} {}{}{}{}",
                    b.stereotype,
                    b.ty,
                    named(b),
                    if b.primary { " primary" } else { "" },
                    b.factory
                        .as_deref()
                        .map_or(String::new(), |f| format!(" by {f}")),
                    b.qualifiers
                        .iter()
                        .map(|q| format!(" @{}", shown(q)))
                        .collect::<String>()
                )
            })
            .collect();
        assert_eq!(
            beans,
            [
                "Service CardPayments cardPayments",
                "Component WirePayments wire primary",
                "Repository URLStore URLStore @fast",
                "Configuration Config config",
                "Bean Payments cash primary by Config.cash",
                "Bean Clock slow by Config.clock",
            ]
        );
        assert_eq!(injections(&c), ["Config bean ledger <Ledger>"]);
    }

    #[test]
    fn the_injection_points_spring_uses() {
        let c = code(
            r#"import org.springframework.stereotype.*; import org.springframework.beans.factory.annotation.*; import jakarta.annotation.Resource; import lombok.RequiredArgsConstructor;
@Service
class Checkout {
    @Autowired private Payments payments;
    @Autowired @Qualifier("wire") private Payments wire;
    @Resource(name = "cash") private Payments cash;
    @Value("${x}") private Payments notInjected;
    private Payments notAnnotated;
    private final List<Payments> all;
    private Ledger ledger;

    Checkout(Ledger unused) {}

    @Autowired
    Checkout(List<Payments> all, Map<String, Payments> byName, ObjectProvider<Audit> audit,
             Optional<Clock> clock, @Qualifier("fast") Store store, String label) {
        this.all = all;
    }

    @Autowired
    void setLedger(Ledger ledger) { this.ledger = ledger; }
}

@Component
class Single {
    Single(Set<Payments> set) {}
}

class NotABean {
    NotABean(Payments p) {}
}

@Service
@RequiredArgsConstructor
class WithLombok {
    private final Collection<Payments> payments;
    private final Ledger ledger;
    private final int retries = 3;
    private static final Ledger SHARED = null;
    private Clock clock;
}
"#,
        );
        assert_eq!(
            injections(&c),
            [
                "Checkout field payments <Payments>",
                "Checkout field wire <Payments> @wire",
                "Checkout field cash <Payments> #cash",
                "Checkout constructor all List<Payments>",
                "Checkout constructor byName Map<Payments>",
                "Checkout constructor audit ObjectProvider<Audit>",
                "Checkout constructor clock Optional<Clock>",
                "Checkout constructor store <Store> @fast",
                "Checkout setter ledger <Ledger>",
                "Single constructor set Set<Payments>",
                "WithLombok constructor payments Collection<Payments>",
                "WithLombok constructor ledger <Ledger>",
            ]
        );
    }

    /// `line what name entity` of every sink.
    fn sinks(c: &Code) -> Vec<String> {
        c.sinks
            .iter()
            .map(|s| {
                let n = s
                    .name
                    .as_ref()
                    .map_or("-".to_string(), |n| match n.0.as_slice() {
                        [Part::Lit(l)] => l.clone(),
                        [Part::Ref(r)] => format!("@{r}"),
                        _ => "?".into(),
                    });
                format!(
                    "{} {} {n} {}",
                    s.line,
                    s.what,
                    s.entity.as_deref().unwrap_or("-")
                )
            })
            .collect()
    }

    #[test]
    fn a_template_call_names_its_collection_only_where_the_overload_has_it() {
        let c = code(
            r#"import org.springframework.data.mongodb.core.MongoTemplate;
class Store {
    private MongoTemplate mongoTemplate;
    void a() {
        mongoTemplate.findById(ID, Order.class);
        mongoTemplate.findById(ID, Order.class, COLL);
        mongoTemplate.findDistinct(q, "city", Order.class, String.class);
        mongoTemplate.findDistinct(q, "city", "orders", Order.class, String.class);
        mongoTemplate.find(q, Order.class, "archive");
        mongoTemplate.save(o, "orders");
        mongoTemplate.remove(q, Order.class, COLL);
        mongoTemplate.upsert(q, u, Order.class);
        mongoTemplate.updateFirst(q, u, "orders");
        mongoTemplate.aggregate(agg, "orders", Total.class);
        mongoTemplate.aggregate(agg, Order.class, Total.class);
        mongoTemplate.findAll(Order.class, "orders");
        mongoTemplate.findAndModify(q, u, opts, Order.class, "orders");
        mongoTemplate.count(q, "orders");
    }
}
"#,
        );
        assert_eq!(
            sinks(&c),
            [
                "5 mongoTemplate.findById - Order",
                "6 mongoTemplate.findById @COLL Order",
                "7 mongoTemplate.findDistinct - Order",
                "8 mongoTemplate.findDistinct orders Order",
                "9 mongoTemplate.find archive Order",
                "10 mongoTemplate.save orders -",
                "11 mongoTemplate.remove @COLL Order",
                "12 mongoTemplate.upsert - Order",
                "13 mongoTemplate.updateFirst orders -",
                "14 mongoTemplate.aggregate orders Total",
                "15 mongoTemplate.aggregate - Order",
                "16 mongoTemplate.findAll orders Order",
                "17 mongoTemplate.findAndModify orders Order",
                "18 mongoTemplate.count orders -",
            ]
        );
    }

    #[test]
    fn generic_types_are_injected_by_their_raw_type() {
        let c = code(
            r#"import org.springframework.stereotype.Service;
@Service
class Desk {
    Desk(Handler<Order> one, Handler<Order>[] array, List<Handler<Order>> list, com.shop.Handler<Order> scoped) {}
}
"#,
        );
        assert_eq!(
            injections(&c),
            [
                "Desk constructor one <Handler>",
                "Desk constructor array []<Handler>",
                "Desk constructor list List<Handler>",
                "Desk constructor scoped <Handler>",
            ]
        );
    }

    #[test]
    fn documents_and_tables_by_the_package_of_their_annotation() {
        let ents = |src: &str| -> Vec<String> {
            code(src)
                .entities
                .iter()
                .map(|e| {
                    let n = e
                        .name
                        .as_ref()
                        .map_or("-".to_string(), |n| match n.0.as_slice() {
                            [Part::Lit(l)] => l.clone(),
                            _ => "?".into(),
                        });
                    format!("{} {} {n}", e.class, e.store)
                })
                .collect()
        };
        assert_eq!(
            ents(
                "import org.springframework.data.mongodb.core.mapping.Document; @Document(\"orders\") class O {}"
            ),
            ["O mongo orders"]
        );
        assert!(
            ents("import org.springframework.data.elasticsearch.annotations.Document; @Document(indexName = \"o\") class O {}")
                .is_empty()
        );
        assert_eq!(
            ents(
                "import org.springframework.data.relational.core.mapping.Table; @Table(\"orders\") class O {}"
            ),
            ["O jdbc orders"]
        );
        assert_eq!(
            ents(
                "import org.springframework.data.cassandra.core.mapping.*; @Table(value = \"orders\") class O {}"
            ),
            ["O cassandra orders"]
        );
        assert_eq!(
            ents("import jakarta.persistence.*; @Entity @Table(name = \"orders\") class O {}"),
            ["O jpa orders"]
        );
    }

    #[test]
    fn repositories_of_every_documented_store() {
        let c = code(
            r#"import org.springframework.data.cassandra.repository.*; import org.springframework.data.r2dbc.repository.R2dbcRepository;
interface A extends CassandraRepository<Order, String> {}
interface B extends ReactiveCassandraRepository<Order, String> {}
interface C extends MapIdCassandraRepository<Order> {}
interface D extends R2dbcRepository<Order, Long> {}
"#,
        );
        let repos: Vec<String> = c
            .repos
            .iter()
            .map(|r| format!("{} {} {}", r.class, r.base, r.entity))
            .collect();
        assert_eq!(
            repos,
            [
                "A CassandraRepository Order",
                "B ReactiveCassandraRepository Order",
                "C MapIdCassandraRepository Order",
                "D R2dbcRepository Order",
            ]
        );
        // A namesake of another package is none.
        let c = code(
            "import com.other.CassandraRepository; interface A extends CassandraRepository<Order, String> {}",
        );
        assert!(c.repos.is_empty());
    }

    #[test]
    fn get_collection_outside_the_mongo_driver_is_no_sink() {
        let c = code(r#"class Shelf { void a(Library l) { l.getCollection("novels"); } }"#);
        assert!(c.sinks.is_empty(), "{:?}", c.sinks);
    }

    #[test]
    fn post_construct_methods_call_what_is_injected() {
        for pkg in ["javax", "jakarta"] {
            let c = code(&format!(
                r#"import {pkg}.annotation.PostConstruct; import {pkg}.inject.Inject;
class Boot {{
    @Inject private List<Rule> rules;
    @PostConstruct void init() {{ rules.forEach(r -> r.load()); }}
}}
"#
            ));
            assert_eq!(injections(&c), ["Boot field rules List<Rule>"], "{pkg}");
            let load = c.calls.iter().find(|x| x.name == "load").unwrap();
            assert_eq!(load.recv_type.as_deref(), Some("Rule"), "{pkg}");
        }
    }

    #[test]
    fn generated_beans_mapstruct_and_configuration_properties() {
        let c = code(
            r#"import org.mapstruct.Mapper; import org.springframework.boot.context.properties.ConfigurationProperties;
@Mapper(componentModel = "spring")
public interface OrderMapper {}
@Mapper
interface PlainMapper {}
@ConfigurationProperties(prefix = "shop")
class ShopProperties {}
"#,
        );
        let beans: Vec<String> = c
            .beans
            .iter()
            .map(|b| format!("{} {} {}", b.stereotype, b.ty, named(b)))
            .collect();
        assert_eq!(
            beans,
            [
                "Mapper OrderMapper orderMapperImpl",
                "ConfigurationProperties ShopProperties shopProperties"
            ]
        );
    }

    #[test]
    fn javax_and_jakarta_inject_alike_and_foreign_namesakes_not_at_all() {
        let jakarta = code(
            r#"import jakarta.inject.Inject; import jakarta.inject.Named; import org.springframework.stereotype.Component;
@Component
class A {
    @Inject @Named("fast") private Store store;
}
"#,
        );
        assert_eq!(injections(&jakarta), ["A field store <Store> @fast"]);
        let javax = code(
            r#"import javax.inject.*; import org.springframework.stereotype.Component;
@Component
class A {
    @Inject private Store store;
    @Inject
    A(Clock clock) {}
}
"#,
        );
        assert_eq!(
            injections(&javax),
            ["A field store <Store>", "A constructor clock <Clock>"]
        );
        // Written with their packages, no import needed.
        let qualified = code(
            r#"
@org.springframework.stereotype.Service
class A {
    @jakarta.inject.Inject private Store store;
}
"#,
        );
        assert_eq!(qualified.beans.len(), 1);
        assert_eq!(injections(&qualified), ["A field store <Store>"]);
        // Another framework's `@Component` and `@Inject`: not Spring's.
        let foreign = code(
            r#"import com.other.di.Component; import com.other.di.Inject;
@Component
class A {
    @Inject private Store store;
    A(Clock clock) {}
}
"#,
        );
        assert!(foreign.beans.is_empty(), "{:?}", foreign.beans);
        assert!(foreign.injections.is_empty(), "{:?}", foreign.injections);
    }

    #[test]
    fn entities_repositories_and_store_calls() {
        let c = code(
            r#"import org.springframework.data.mongodb.core.mapping.Document; import jakarta.persistence.*; import org.springframework.data.mongodb.repository.MongoRepository; import org.springframework.data.jpa.repository.JpaRepository;
@Document(collection = "orders")
class Order {}
@Document(Names.CARTS)
class Cart {}
@Document
class Invoice {}
@Entity @Table(name = "payments")
class Payment {}
@Entity
class Refund {}
interface Orders extends MongoRepository<Order, String> {}
interface Payments extends JpaRepository<Payment, Long> {}
interface Other extends Comparable<Order> {}
class Store {
    private static final String COLL = "receipts";
    private MongoTemplate mongoTemplate;
    private EntityManager em;
    void save(Receipt r, String dynamic) {
        db().getCollection(COLL, Receipt.class).insertOne(r);
        db().getCollection("audit");
        db().getCollection(dynamic);
        mongoTemplate.save(r, "receipts");
        mongoTemplate.find(query, Order.class);
        mongoTemplate.indexOps("x");
        em.find(Refund.class, 1L);
        other.save("not a sink");
    }
}
"#,
        );
        let ents: Vec<String> = c
            .entities
            .iter()
            .map(|e| {
                let n = e
                    .name
                    .as_ref()
                    .map_or("-".to_string(), |n| match n.0.as_slice() {
                        [Part::Lit(l)] => l.clone(),
                        [Part::Ref(r)] => format!("@{r}"),
                        _ => "?".into(),
                    });
                format!("{} {} {n}", e.class, e.store)
            })
            .collect();
        assert_eq!(
            ents,
            [
                "Order mongo orders",
                "Cart mongo @Names.CARTS",
                "Invoice mongo -",
                "Payment jpa payments",
                "Refund jpa -",
            ]
        );
        let repos: Vec<String> = c
            .repos
            .iter()
            .map(|r| format!("{} {} {}", r.class, r.base, r.entity))
            .collect();
        assert_eq!(
            repos,
            [
                "Orders MongoRepository Order",
                "Payments JpaRepository Payment"
            ]
        );
        let sinks: Vec<String> = c
            .sinks
            .iter()
            .map(|s| {
                let n = s
                    .name
                    .as_ref()
                    .map_or("-".to_string(), |n| match n.0.as_slice() {
                        [Part::Lit(l)] => l.clone(),
                        [Part::Ref(r)] => format!("@{r}"),
                        _ => "?".into(),
                    });
                format!(
                    "{} {} {} {n} {}",
                    s.line,
                    s.store,
                    s.what,
                    s.entity.as_deref().unwrap_or("-")
                )
            })
            .collect();
        assert_eq!(
            sinks,
            [
                "20 mongo getCollection @COLL Receipt",
                "21 mongo getCollection audit -",
                "23 mongo mongoTemplate.save receipts -",
                "24 mongo mongoTemplate.find - Order",
                "26 jpa em.find - Refund",
            ]
        );
    }
}
