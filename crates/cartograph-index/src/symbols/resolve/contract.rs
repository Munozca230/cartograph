//! Contracts (see [`cartograph_core::contract`]): the shapes of the body types of HTTP
//! bridges, and the MCP tools a server declares.
//!
//! Shapes are read after resolution, only for the types a bridge's body names: the file
//! that declares one is parsed again from disk (the extraction cache keeps no fields), and
//! a type is looked up in the file where it is written, through its own definitions, its
//! imports, its package (Java) and its glob imports: never by its name elsewhere in the
//! repo.
//!
//! - **Java** (what Jackson reads into an object): the fields of a class or the components
//!   of a record, `static` ones left out, `@JsonProperty` naming one, `@JsonAlias` adding
//!   names, `@JsonIgnore` and `@JsonIgnoreProperties({…})` leaving them out, fields of the
//!   superclasses of the repo first. A field counts when Jackson can set it: Lombok's
//!   `@Data`, `@Getter`, `@Setter` or `@Value` (on the class or the field), a public field,
//!   `@JsonProperty`, or an accessor of the class. `@NotNull`, `@NotBlank` and `@NotEmpty`
//!   (Bean Validation) make it required, on a body Spring validates (`@Valid`).
//! - **TypeScript**: interfaces (with what they extend), type aliases of object types,
//!   intersections, `Partial`, `Required`, `Readonly`, and `Pick` / `Omit` with literal
//!   keys; `name?:` is optional.
//!
//! Anything else (a supertype outside the repo or not found, `@JsonTypeInfo`,
//! `@JsonNaming`, `@JsonUnwrapped`, `@JsonAnySetter`, `@JsonCreator`, an index signature, a
//! union, a class in TS, a computed key) leaves the shape unknown: it is never compared.

use super::{Bindings, Ctx, Out, Sid, bridge::Consts, via_binding};
use crate::grammars::{Grammar, parse};
use cartograph_core::{BodyType, ContractField, McpTool, SymbolKind};
use std::collections::HashMap;
use std::path::PathBuf;
use tree_sitter::{Node, Tree};

/// Supertypes and aliases followed.
const MAX_DEPTH: u8 = 6;

/// Java collections a body may be a list of.
const JAVA_LISTS: &[&str] = &[
    "List",
    "ArrayList",
    "Set",
    "HashSet",
    "Collection",
    "Iterable",
];

fn text(n: Node<'_>, src: &[u8]) -> String {
    String::from_utf8_lossy(src.get(n.byte_range()).unwrap_or(&[])).into_owned()
}

/// A string literal's value (Java or TS), without quotes.
fn literal(n: Node<'_>, src: &[u8]) -> Option<String> {
    let n = if n.kind() == "literal_type" {
        n.named_child(0)?
    } else {
        n
    };
    if !n.kind().contains("string") {
        return None;
    }
    let t = text(n, src);
    let t = t.trim();
    Some(
        t.trim_matches(|c| matches!(c, '"' | '\'' | '`'))
            .to_string(),
    )
}

/// Annotations on a Java declaration: `(name, arguments)`.
fn annotations<'t>(decl: Node<'t>, src: &[u8]) -> Vec<(String, Option<Node<'t>>)> {
    let mut out = Vec::new();
    let mut c = decl.walk();
    for ch in decl.children(&mut c) {
        if ch.kind() != "modifiers" {
            continue;
        }
        let mut c2 = ch.walk();
        for a in ch.named_children(&mut c2) {
            if !matches!(a.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            let name = a
                .child_by_field_name("name")
                .map(|x| text(x, src))
                .unwrap_or_default();
            let name = name.rsplit('.').next().unwrap_or(&name).to_string();
            out.push((name, a.child_by_field_name("arguments")));
        }
    }
    out
}

/// The strings of an annotation's `value` (`@JsonAlias({"a", "b"})`, `@JsonProperty("x")`,
/// `@JsonProperty(value = "x")`).
fn annotation_strings(args: Option<Node<'_>>, src: &[u8]) -> Vec<String> {
    let Some(args) = args else { return Vec::new() };
    let mut out = Vec::new();
    let mut c = args.walk();
    for ch in args.named_children(&mut c) {
        let v = if ch.kind() == "element_value_pair" {
            if ch
                .child_by_field_name("key")
                .is_none_or(|k| text(k, src) != "value")
            {
                continue;
            }
            match ch.child_by_field_name("value") {
                Some(v) => v,
                None => continue,
            }
        } else {
            ch
        };
        if v.kind() == "element_value_array_initializer" {
            let mut c2 = v.walk();
            out.extend(v.named_children(&mut c2).filter_map(|x| literal(x, src)));
        } else if let Some(s) = literal(v, src) {
            out.push(s);
        }
    }
    out
}

fn has(annots: &[(String, Option<Node<'_>>)], names: &[&str]) -> bool {
    annots.iter().any(|(a, _)| names.contains(&a.as_str()))
}

/// Fields by name, `add` replacing what `base` has under its names.
fn merge(base: Vec<ContractField>, add: Vec<ContractField>) -> Vec<ContractField> {
    let mut out: Vec<ContractField> = base
        .into_iter()
        .filter(|f| !add.iter().any(|a| a.name == f.name))
        .collect();
    out.extend(add);
    out.sort();
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// A parsed file kept for the shapes read from it.
struct Parsed {
    tree: Tree,
    src: Vec<u8>,
}

/// Reads the shapes of body types (see the module docs), memoized per type.
pub(super) struct Shapes<'c, 'a> {
    ctx: &'c Ctx<'a>,
    binds: &'c [Bindings<'a>],
    /// Where each mount's files are on disk: `(prefix, folder)`.
    sources: &'c [(String, PathBuf)],
    parsed: HashMap<usize, Option<std::rc::Rc<Parsed>>>,
    memo: HashMap<Sid, Option<Vec<ContractField>>>,
}

impl<'c, 'a> Shapes<'c, 'a> {
    pub(super) fn new(
        ctx: &'c Ctx<'a>,
        binds: &'c [Bindings<'a>],
        sources: &'c [(String, PathBuf)],
    ) -> Self {
        Shapes {
            ctx,
            binds,
            sources,
            parsed: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    /// The type written `written` in file `fi`, with its shape. `validated`: Bean
    /// Validation runs on it (else no field is required).
    pub(super) fn body(&mut self, fi: usize, written: &str, validated: bool) -> BodyType {
        let java = self.ctx.files[fi].fam == super::Fam::Java;
        let (array, inner) = strip_array(written, java);
        let mut out = BodyType {
            written: written.to_string(),
            name: None,
            file: None,
            line: 0,
            array,
            fields: None,
        };
        let plain = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
        };
        // Java: a generic class of the repo is its class (`Page<T>` reads `Page`).
        let head = if java {
            inner.split('<').next().unwrap_or(inner)
        } else {
            inner
        };
        if plain(head) {
            if let Some(sid) = self.find_type(fi, head) {
                let sym = &self.ctx.symbols[sid as usize];
                out.name = Some(sym.name.clone());
                out.file = Some(sym.file.clone());
                out.line = sym.line;
                out.fields = self.shape_of(sid, 0);
            }
        } else if !java {
            out.fields = self.ts_written(fi, inner);
        }
        if !validated && let Some(fs) = out.fields.as_mut() {
            for f in fs {
                f.required = false;
            }
        }
        out
    }

    /// A type named `name` as file `fi` sees it: its own, one it imports, one of its
    /// package (Java) or of a glob import.
    fn find_type(&self, fi: usize, name: &str) -> Option<Sid> {
        let ctx = self.ctx;
        let typeish = |s: Sid| {
            matches!(
                ctx.symbols[s as usize].kind,
                SymbolKind::Class
                    | SymbolKind::Interface
                    | SymbolKind::Type
                    | SymbolKind::Struct
                    | SymbolKind::Enum
            )
        };
        let unique = |v: Vec<Sid>| -> Option<Sid> {
            let first = *v.first()?;
            v.iter()
                .all(|&s| ctx.ids[s as usize] == ctx.ids[first as usize])
                .then_some(first)
        };
        let own: Vec<Sid> = ctx
            .top(fi, name)
            .iter()
            .copied()
            .filter(|&s| typeish(s))
            .collect();
        if !own.is_empty() {
            return unique(own);
        }
        // A nested type of the file (`static class Item` used inside `Order`).
        let nested = ctx.top_types(fi, name);
        if !nested.is_empty() {
            return unique(nested);
        }
        let b = &self.binds[fi];
        if let Some(bd) = b.by_name.get(name) {
            return match via_binding(ctx, self.binds, bd, name) {
                Out::Hit(s, _) if typeish(s) => Some(s),
                _ => None,
            };
        }
        let scoped: Vec<Sid> = b
            .scope
            .iter()
            .chain(&b.globs)
            .flat_map(|&t| ctx.top(t, name).iter().copied())
            .filter(|&s| typeish(s))
            .collect();
        unique(scoped)
    }

    fn parsed(&mut self, fi: usize) -> Option<std::rc::Rc<Parsed>> {
        if let Some(p) = self.parsed.get(&fi) {
            return p.clone();
        }
        let path = self.ctx.files[fi].path;
        let p = (|| {
            let (prefix, root) = self
                .sources
                .iter()
                .filter(|(p, _)| {
                    p.is_empty()
                        || path
                            .strip_prefix(p.as_str())
                            .is_some_and(|r| r.starts_with('/'))
                })
                .max_by_key(|(p, _)| p.len())?;
            let rel = if prefix.is_empty() {
                path
            } else {
                &path[prefix.len() + 1..]
            };
            let src = std::fs::read(root.join(rel)).ok()?;
            let g = Grammar::for_lang(crate::lang::detect(path)?)?;
            let tree = parse(g, &src).ok()?;
            Some(std::rc::Rc::new(Parsed { tree, src }))
        })();
        self.parsed.insert(fi, p.clone());
        p
    }

    /// The fields of type `sid`; `None` when the shape is not known whole.
    fn shape_of(&mut self, sid: Sid, depth: u8) -> Option<Vec<ContractField>> {
        if let Some(m) = self.memo.get(&sid) {
            return m.clone();
        }
        if depth > MAX_DEPTH {
            return None;
        }
        // In progress: a type that reaches itself has no known shape.
        self.memo.insert(sid, None);
        let fi = self.ctx.owner[sid as usize];
        let sym = self.ctx.symbols[sid as usize].clone();
        let out = (|| {
            let p = self.parsed(fi)?;
            let decl = find_decl(p.tree.root_node(), &sym.name, sym.line, &p.src)?;
            match decl.kind() {
                "class_declaration" | "record_declaration" => {
                    self.java_fields(fi, decl, &p.src, depth)
                }
                "interface_declaration" if self.ctx.files[fi].fam == super::Fam::Js => {
                    self.ts_interface(fi, decl, &p.src, depth)
                }
                "type_alias_declaration" => {
                    self.ts_type(fi, decl.child_by_field_name("value")?, &p.src, depth)
                }
                _ => None,
            }
        })();
        self.memo.insert(sid, out.clone());
        out
    }

    fn java_fields(
        &mut self,
        fi: usize,
        decl: Node<'_>,
        src: &[u8],
        depth: u8,
    ) -> Option<Vec<ContractField>> {
        const OPAQUE: &[&str] = &[
            "JsonTypeInfo",
            "JsonSubTypes",
            "JsonNaming",
            "JsonDeserialize",
            "JsonFormat",
        ];
        let annots = annotations(decl, src);
        if has(&annots, OPAQUE) {
            return None;
        }
        let lombok = has(&annots, &["Data", "Getter", "Setter", "Value"]);
        let ignored: Vec<String> = annots
            .iter()
            .filter(|(a, _)| a == "JsonIgnoreProperties")
            .flat_map(|(_, args)| annotation_strings(*args, src))
            .collect();
        let validation = String::from_utf8_lossy(src).contains("validation.constraints");
        let field = |name: String, fa: &[(String, Option<Node<'_>>)]| -> ContractField {
            let json = fa
                .iter()
                .find(|(a, _)| a == "JsonProperty")
                .and_then(|(_, args)| annotation_strings(*args, src).into_iter().next())
                .filter(|s| !s.is_empty())
                .unwrap_or(name);
            ContractField {
                name: json,
                aliases: fa
                    .iter()
                    .filter(|(a, _)| a == "JsonAlias")
                    .flat_map(|(_, args)| annotation_strings(*args, src))
                    .collect(),
                required: validation && has(fa, &["NotNull", "NotBlank", "NotEmpty"]),
                optional: false,
            }
        };
        let mut own = Vec::new();
        if decl.kind() == "record_declaration" {
            let ps = decl.child_by_field_name("parameters")?;
            let mut c = ps.walk();
            for p in ps.named_children(&mut c) {
                if p.kind() != "formal_parameter" {
                    continue;
                }
                let fa = annotations(p, src);
                if has(&fa, &["JsonIgnore"]) {
                    continue;
                }
                let name = text(p.child_by_field_name("name")?, src);
                own.push(field(name, &fa));
            }
        }
        let body = decl.child_by_field_name("body")?;
        // Accessors, and what makes the shape open or built otherwise.
        let mut methods: Vec<String> = Vec::new();
        let mut c = body.walk();
        for m in body.named_children(&mut c) {
            if !matches!(m.kind(), "method_declaration" | "constructor_declaration") {
                continue;
            }
            let ma = annotations(m, src);
            if has(&ma, &["JsonAnySetter", "JsonCreator"]) {
                return None;
            }
            if let Some(n) = m.child_by_field_name("name") {
                methods.push(text(n, src));
            }
        }
        let mut c = body.walk();
        for f in body.named_children(&mut c) {
            if f.kind() != "field_declaration" {
                continue;
            }
            let mods = f
                .children(&mut f.walk())
                .find(|x| x.kind() == "modifiers")
                .map(|m| text(m, src))
                .unwrap_or_default();
            if mods.split_whitespace().any(|w| w == "static") {
                continue;
            }
            let fa = annotations(f, src);
            if has(&fa, &["JsonUnwrapped", "JsonAnySetter"]) {
                return None;
            }
            if has(&fa, &["JsonIgnore"]) {
                continue;
            }
            let public = mods.split_whitespace().any(|w| w == "public");
            let mut dc = f.walk();
            for d in f.children_by_field_name("declarator", &mut dc) {
                let Some(name) = d.child_by_field_name("name").map(|n| text(n, src)) else {
                    continue;
                };
                let cap: String = name
                    .chars()
                    .take(1)
                    .flat_map(char::to_uppercase)
                    .chain(name.chars().skip(1))
                    .collect();
                let accessor = ["set", "get", "is"]
                    .iter()
                    .any(|p| methods.contains(&format!("{p}{cap}")));
                let visible =
                    lombok || public || accessor || has(&fa, &["Getter", "Setter", "JsonProperty"]);
                if visible {
                    own.push(field(name, &fa));
                }
            }
        }
        // Fields of the superclass first (a class of the repo, else the shape is unknown).
        let mut all = Vec::new();
        if let Some(sup) = decl.child_by_field_name("superclass") {
            let t = sup.named_child(0)?;
            let name = text(t, src);
            let name = name.split('<').next().unwrap_or(&name).trim().to_string();
            let sid = self.find_type(fi, &name)?;
            all = self.shape_of(sid, depth + 1)?;
        }
        let mut out = merge(all, own);
        out.retain(|f| !ignored.contains(&f.name));
        Some(out)
    }

    fn ts_interface(
        &mut self,
        fi: usize,
        decl: Node<'_>,
        src: &[u8],
        depth: u8,
    ) -> Option<Vec<ContractField>> {
        let mut all = Vec::new();
        let mut c = decl.walk();
        for ch in decl.named_children(&mut c) {
            if ch.kind() != "extends_type_clause" {
                continue;
            }
            let mut c2 = ch.walk();
            for t in ch.named_children(&mut c2) {
                let fs = self.ts_type(fi, t, src, depth + 1)?;
                all = merge(all, fs);
            }
        }
        let own = members(decl.child_by_field_name("body")?, src)?;
        Some(merge(all, own))
    }

    /// The fields of a TS type expression written in file `fi`.
    fn ts_type(
        &mut self,
        fi: usize,
        t: Node<'_>,
        src: &[u8],
        depth: u8,
    ) -> Option<Vec<ContractField>> {
        if depth > MAX_DEPTH {
            return None;
        }
        match t.kind() {
            "object_type" | "interface_body" => members(t, src),
            "type_identifier" | "identifier" => {
                let sid = self.find_type(fi, &text(t, src))?;
                self.shape_of(sid, depth + 1)
            }
            "parenthesized_type" | "readonly_type" => {
                self.ts_type(fi, t.named_child(0)?, src, depth)
            }
            "intersection_type" => {
                let mut all = Vec::new();
                let mut c = t.walk();
                for x in t.named_children(&mut c).collect::<Vec<_>>() {
                    let fs = self.ts_type(fi, x, src, depth + 1)?;
                    all = merge(all, fs);
                }
                Some(all)
            }
            "generic_type" => {
                let name = text(t.child_by_field_name("name")?, src);
                let args: Vec<Node<'_>> = t
                    .child_by_field_name("type_arguments")
                    .map(|a| {
                        let mut c = a.walk();
                        a.named_children(&mut c).collect()
                    })
                    .unwrap_or_default();
                match (name.as_str(), args.as_slice()) {
                    ("Partial" | "Required" | "Readonly", [x]) => {
                        let mut fs = self.ts_type(fi, *x, src, depth + 1)?;
                        for f in &mut fs {
                            match name.as_str() {
                                "Partial" => f.optional = true,
                                "Required" => f.optional = false,
                                _ => {}
                            }
                        }
                        Some(fs)
                    }
                    ("Pick" | "Omit", [x, keys]) => {
                        let keys = literal_keys(*keys, src)?;
                        let mut fs = self.ts_type(fi, *x, src, depth + 1)?;
                        fs.retain(|f| keys.contains(&f.name) == (name == "Pick"));
                        Some(fs)
                    }
                    // A generic interface or alias of the repo (`Page<T>`): its fields.
                    _ => {
                        let sid = self.find_type(fi, &name)?;
                        self.shape_of(sid, depth + 1)
                    }
                }
            }
            _ => None,
        }
    }

    /// A type written as text in file `fi` (`Partial<User>`, `A & B`), read by parsing it.
    fn ts_written(&mut self, fi: usize, written: &str) -> Option<Vec<ContractField>> {
        let src = format!("type __Body = {written};").into_bytes();
        let tree = parse(Grammar::TypeScript, &src).ok()?;
        let root = tree.root_node();
        let alias = root.named_child(0)?;
        if alias.kind() != "type_alias_declaration" {
            return None;
        }
        self.ts_type(fi, alias.child_by_field_name("value")?, &src, 0)
    }
}

/// `'a' | 'b'` as keys; `None` for anything else.
fn literal_keys(n: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    match n.kind() {
        "literal_type" => Some(vec![literal(n, src)?]),
        "union_type" => {
            let mut out = Vec::new();
            let mut c = n.walk();
            for x in n.named_children(&mut c).collect::<Vec<_>>() {
                out.extend(literal_keys(x, src)?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// The properties of a TS object type or interface body; `None` with an index signature or
/// a key that is not a name or a string.
fn members(obj: Node<'_>, src: &[u8]) -> Option<Vec<ContractField>> {
    let mut out = Vec::new();
    let mut c = obj.walk();
    for m in obj.named_children(&mut c) {
        match m.kind() {
            "property_signature" => {
                let n = m.child_by_field_name("name")?;
                let name = match n.kind() {
                    "property_identifier" => text(n, src),
                    k if k.contains("string") => literal(n, src)?,
                    _ => return None,
                };
                let optional = m.children(&mut m.walk()).any(|x| x.kind() == "?");
                out.push(ContractField {
                    name,
                    aliases: Vec::new(),
                    required: false,
                    optional,
                });
            }
            "method_signature" | "call_signature" | "construct_signature" | "comment" => {}
            _ => return None,
        }
    }
    out.sort();
    out.dedup_by(|a, b| a.name == b.name);
    Some(out)
}

/// The declaration of type `name` whose name is on `line`.
fn find_decl<'t>(root: Node<'t>, name: &str, line: u32, src: &[u8]) -> Option<Node<'t>> {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "class_declaration"
                | "record_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "abstract_class_declaration"
        ) && let Some(x) = n.child_by_field_name("name")
            && x.start_position().row as u32 + 1 == line
            && text(x, src) == name
        {
            return Some(n);
        }
        // Only what may hold the line.
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            let (a, b) = (
                ch.start_position().row as u32 + 1,
                ch.end_position().row as u32 + 1,
            );
            if a <= line && line <= b {
                stack.push(ch);
            }
        }
    }
    None
}

/// `List<T>` / `T[]` (Java), `T[]` / `Array<T>` (TS): a list, and what it holds.
fn strip_array(written: &str, java: bool) -> (bool, &str) {
    if let Some(inner) = written.strip_suffix("[]") {
        return (true, inner);
    }
    if let Some((head, rest)) = written.split_once('<')
        && let Some(inner) = rest.strip_suffix('>')
    {
        let list = if java {
            JAVA_LISTS.contains(&head)
        } else {
            matches!(head, "Array" | "ReadonlyArray")
        };
        if list {
            return (true, inner);
        }
    }
    (false, written)
}

/// The MCP tools of the repo, their names read through constants.
pub(super) fn tools(ctx: &Ctx<'_>, binds: &[Bindings<'_>]) -> Vec<McpTool> {
    if ctx.files.iter().all(|f| f.code.tools.is_empty()) {
        return Vec::new();
    }
    let consts = Consts::new(ctx, binds);
    let mut out = Vec::new();
    for (fi, f) in ctx.files.iter().enumerate() {
        for t in &f.code.tools {
            let v = consts.eval(fi, &t.name, 0);
            if !v.known() || v.key.contains('*') {
                continue;
            }
            out.push(McpTool {
                name: v.key,
                file: f.path.to_string(),
                line: t.line,
                params: t.params.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_array;

    #[test]
    fn lists_of_bodies() {
        assert_eq!(strip_array("List<ItemDto>", true), (true, "ItemDto"));
        assert_eq!(strip_array("ItemDto[]", true), (true, "ItemDto"));
        assert_eq!(strip_array("Page<ItemDto>", true), (false, "Page<ItemDto>"));
        assert_eq!(strip_array("Array<Item>", false), (true, "Item"));
        assert_eq!(
            strip_array("Partial<Item>", false),
            (false, "Partial<Item>")
        );
    }
}
