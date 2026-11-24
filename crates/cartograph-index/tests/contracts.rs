//! Contracts (`tests/fixtures/workspace-contracts`): the bodies of HTTP bridges with the
//! shapes of their types, and MCP tools; the same whether the index resolves calls or only
//! its bridges (a git base for `diff`).

use cartograph_core::{BodyType, HttpBody, Symbols};
use cartograph_index::{IndexOptions, index, index_working_tree};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn show(t: &Option<BodyType>) -> String {
    let Some(t) = t else {
        return "?".into();
    };
    let list = if t.array { "[]" } else { "" };
    match &t.fields {
        None => format!("{}{list} (unknown)", t.label()),
        Some(fs) => {
            let fs: Vec<String> = fs
                .iter()
                .map(|f| {
                    let mut s = f.name.clone();
                    if f.required {
                        s.push('!');
                    }
                    if f.optional {
                        s.push('?');
                    }
                    for a in &f.aliases {
                        s.push_str(&format!("|{a}"));
                    }
                    s
                })
                .collect();
            format!("{}{list} {{{}}}", t.label(), fs.join(" "))
        }
    }
}

fn rows(s: &Symbols) -> Vec<String> {
    s.bodies
        .iter()
        .map(|b: &HttpBody| format!("{}: {} -> {}", b.key, show(&b.client), show(&b.server)))
        .collect()
}

#[test]
fn bodies_pair_client_and_server_types_with_their_shapes() {
    let out = index(&fixture("workspace-contracts"), &IndexOptions::default()).unwrap();
    let mut got = rows(&out.symbols);
    got.sort();
    let mut want = vec![
        // One client type for the update and the create: the alias reads `colour`,
        // `@JsonProperty` renames `type`, `@JsonIgnore` leaves `internal` out, the
        // superclass brings `ownerId`, a constraint of a group requires nothing.
        "PATCH /api/items/{}: ItemPayload {colour? kind? label? notes? ownerId? tags?} -> UpdateItemDto {label notes ownerId tags}",
        "POST /api/items: ItemPayload {colour? kind? label? notes? ownerId? tags?} -> CreateItemDto {category kind label! notes ownerId! shade|color|colour}",
        // A list: its element's fields are never required.
        "POST /api/items/batch: ItemPayload[] {colour? kind? label? notes? ownerId? tags?} -> CreateItemDto[] {category kind label notes ownerId shade|color|colour}",
        // A superclass outside the repo: unknown. (The call through a method known only
        // at run time is a heuristic bridge: no row.)
        "POST /api/items/external: ItemPayload {colour? kind? label? notes? ownerId? tags?} -> ExternalDto (unknown)",
        // An object literal: no known type.
        "POST /api/items/raw: ? -> String (unknown)",
        // Accessors that name the property: unknown.
        "POST /api/items/rename: RenameInput {ref} -> RenameDto (unknown)",
        // `import type` and `Pick` with literal keys; a record with `@JsonProperty`; a
        // primitive is never required.
        "POST /api/items/tags: Pick<TagInput,'hex'> {hex?} -> TagDto {hex name! weight}",
        // Declaration merging: unknown. (Not validated: nothing required; a field without
        // accessor is not read.)
        "PUT /api/items/notes: NoteInput (unknown) -> NoteDto {text}",
        // An object type written in place.
        "PUT /api/items/notes: {text:string} {text} -> NoteDto {text}",
        // A TS class: unknown.
        "PUT /api/items/notes: NoteDraft (unknown) -> NoteDto {text}",
    ];
    want.sort();
    assert_eq!(got, want);
    let tools: Vec<String> = out
        .symbols
        .tools
        .iter()
        .map(|t| {
            let ps: Vec<String> = t
                .params
                .iter()
                .flatten()
                .map(|p| format!("{}={:?}", p.name, p.required))
                .collect();
            format!("{} {}:{} [{}]", t.name, t.file, t.line, ps.join(" "))
        })
        .collect();
    assert_eq!(
        tools,
        [
            // A name known only at run time: kept without one.
            " mcp/src/tools.ts:18 [id=Some(true)]",
            "create_item mcp/src/tools.ts:14 [label=Some(true) notes=Some(false) ownerId=Some(true)]",
            "list_items mcp/src/tools.ts:19 [ownerId=Some(true)]",
        ]
    );
}

/// A base for `diff` is indexed without its symbol layer: its contracts must be the same,
/// or every row would look new.
#[test]
fn a_base_without_symbols_has_the_same_contracts() {
    let root = fixture("workspace-contracts");
    let full = index(&root, &IndexOptions::default()).unwrap();
    let data = tempfile::tempdir().unwrap();
    let opts = IndexOptions {
        data_dir: Some(data.path().to_path_buf()),
        ..IndexOptions::default()
    };
    let (base, _) = index_working_tree(&root, &opts).unwrap();
    assert!(base.symbols.symbols.is_empty());
    assert!(!full.symbols.bodies.is_empty());
    assert_eq!(base.symbols.bodies, full.symbols.bodies);
    assert_eq!(base.symbols.tools, full.symbols.tools);
}
