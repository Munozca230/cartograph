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
        // superclass brings `projectId`.
        "PATCH /api/items/{}: ItemPayload {colour? kind? projectId? summary? tags? title?} -> UpdateItemDto {projectId summary tags title}",
        "POST /api/items: ItemPayload {colour? kind? projectId? summary? tags? title?} -> CreateItemDto {kind projectId! shade|color|colour summary technology title!}",
        // A list: its element's fields are never required.
        "POST /api/items/batch: ItemPayload[] {colour? kind? projectId? summary? tags? title?} -> CreateItemDto[] {kind projectId shade|color|colour summary technology title}",
        // A superclass outside the repo: unknown.
        "POST /api/items/external: ItemPayload {colour? kind? projectId? summary? tags? title?} -> ExternalDto (unknown)",
        // An object literal: no known type.
        "POST /api/items/raw: ? -> String (unknown)",
        // `import type` and `Pick` with literal keys; a record with `@JsonProperty`.
        "POST /api/items/tags: Pick<TagInput,'hex'> {hex?} -> TagDto {hex name!}",
        // Not validated: nothing required; a field without accessor is not read.
        "PUT /api/items/notes: NoteInput {extra text} -> NoteDto {text}",
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
            "create_item mcp/src/tools.ts:13 [projectId=Some(true) summary=Some(false) title=Some(true)]",
            "list_items mcp/src/tools.ts:16 [projectId=Some(true)]",
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
