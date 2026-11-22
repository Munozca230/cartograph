//! A SCIP index of `tests/fixtures/scip-ts`, built with the `scip` crate's types (no
//! indexer is run). It reads as scip-typescript would write it for the `web/` package from
//! a CI checkout: `project_root` is a folder that does not exist here, and document paths
//! are relative to `web/`. Lines are 0-based. Both range encodings (typed and the legacy
//! `repeated int32`) appear.
//!
//! On purpose, beyond what the sources say: `util.ts` references `run` of `app.ts` (a
//! dependency no import shows, closing a cycle) and a lodash symbol (a library it does not
//! import); `view.ts`'s `cfg.loadAll()` is a local (N2 guesses `Store.loadAll`); a document
//! names a file that is not in the repo.

#![allow(dead_code)]

use protobuf::{EnumOrUnknown, MessageField};
use scip::types::{
    Document, Index, Metadata, MultiLineRange, Occurrence, SingleLineRange, SymbolInformation,
    ToolInfo, occurrence, symbol_information::Kind,
};
use std::path::Path;

pub const PROJECT_ROOT: &str = "file:///ci/work/checkout/web";

const DEF: i32 = 1;
const IMPORT: i32 = 2;

pub fn web(desc: &str) -> String {
    format!("scip-typescript npm web 1.0.0 {desc}")
}

fn lodash(desc: &str) -> String {
    format!("scip-typescript npm @types/lodash 4.14.0 {desc}")
}

fn lib(desc: &str) -> String {
    format!("scip-typescript npm typescript 5.4.0 lib/`lib.dom.d.ts`/{desc}")
}

/// An occurrence on one line, with the typed encoding.
fn at(symbol: &str, line: i32, roles: i32) -> Occurrence {
    Occurrence {
        symbol: symbol.to_string(),
        symbol_roles: roles,
        typed_range: Some(occurrence::Typed_range::SingleLineRange(SingleLineRange {
            line,
            start_character: 2,
            end_character: 8,
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// The same with the legacy `range` encoding.
fn at_legacy(symbol: &str, line: i32, roles: i32) -> Occurrence {
    Occurrence {
        symbol: symbol.to_string(),
        symbol_roles: roles,
        range: vec![line, 2, 8],
        ..Default::default()
    }
}

/// A reference to `symbol` at `line`, characters `start..end` (typed encoding).
fn refer(symbol: &str, line: i32, start: i32, end: i32) -> Occurrence {
    Occurrence {
        symbol: symbol.to_string(),
        typed_range: Some(occurrence::Typed_range::SingleLineRange(SingleLineRange {
            line,
            start_character: start,
            end_character: end,
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// The same with the legacy `range` encoding.
fn refer_legacy(symbol: &str, line: i32, start: i32, end: i32) -> Occurrence {
    Occurrence {
        symbol: symbol.to_string(),
        range: vec![line, start, end],
        ..Default::default()
    }
}

/// A definition spanning `line..=end` (typed enclosing range).
fn def(symbol: &str, line: i32, end: i32) -> Occurrence {
    Occurrence {
        typed_enclosing_range: Some(occurrence::Typed_enclosing_range::MultiLineEnclosingRange(
            MultiLineRange {
                start_line: line,
                start_character: 0,
                end_line: end,
                end_character: 1,
                ..Default::default()
            },
        )),
        ..at(symbol, line, DEF)
    }
}

/// The same with the legacy encodings.
fn def_legacy(symbol: &str, line: i32, end: i32) -> Occurrence {
    Occurrence {
        enclosing_range: vec![line, 0, end, 1],
        ..at_legacy(symbol, line, DEF)
    }
}

fn info(symbol: &str, kind: Kind) -> SymbolInformation {
    SymbolInformation {
        symbol: symbol.to_string(),
        kind: EnumOrUnknown::new(kind),
        ..Default::default()
    }
}

fn doc(path: &str, occurrences: Vec<Occurrence>, symbols: Vec<SymbolInformation>) -> Document {
    Document {
        language: "TypeScript".into(),
        relative_path: path.into(),
        occurrences,
        symbols,
        ..Default::default()
    }
}

pub fn index() -> Index {
    let store = |d: &str| web(&format!("src/`store.ts`/{d}"));
    let util = |d: &str| web(&format!("src/`util.ts`/{d}"));
    let app = |d: &str| web(&format!("src/`app.ts`/{d}"));
    let view = |d: &str| web(&format!("src/`view.ts`/{d}"));
    let legacy = |d: &str| web(&format!("src/`legacy.ts`/{d}"));
    let documents = vec![
        doc(
            "src/store.ts",
            vec![
                def(&store("Store#"), 0, 8),
                def(&store("Store#items."), 1, 1),
                def(&store("Store#save()."), 2, 4),
                refer(&store("Store#items."), 3, 9, 14),
                def_legacy(&store("Store#loadAll()."), 5, 7),
                refer(&store("Store#items."), 6, 16, 21),
                def(&store("makeStore()."), 10, 12),
                refer(&store("Store#"), 10, 29, 34),
                refer(&store("Store#"), 11, 13, 18),
                def(&store("api."), 14, 18),
                // A method of an object literal passed to a call (`Object.freeze({…})`): N2 has
                // no symbol for it.
                def(&store("api.fetch()."), 15, 17),
                refer(&store("makeStore()."), 16, 11, 20),
                refer_legacy(&store("Store#loadAll()."), 16, 23, 30),
                at("local 0", 3, 0),
            ],
            vec![
                info(&store("Store#"), Kind::Class),
                info(&store("Store#items."), Kind::Property),
                info(&store("Store#save()."), Kind::Method),
                info(&store("Store#loadAll()."), Kind::Method),
                info(&store("makeStore()."), Kind::Function),
                info(&store("api."), Kind::Variable),
                info(&store("api.fetch()."), Kind::Method),
            ],
        ),
        doc("src/index.ts", vec![], vec![]),
        doc(
            "src/util.ts",
            vec![
                def(&util("format()."), 0, 2),
                refer(&lodash("trim()."), 1, 11, 15),
                def(&util("save()."), 4, 6),
                refer(&app("run()."), 5, 9, 10),
            ],
            vec![],
        ),
        doc(
            "src/app.ts",
            vec![
                at(&store("makeStore()."), 0, IMPORT),
                at(&store("api."), 0, IMPORT),
                at(&util("format()."), 1, IMPORT),
                at(&lodash("debounce()."), 2, IMPORT),
                def(&app("run()."), 4, 10),
                refer(&store("makeStore()."), 5, 12, 21),
                refer(&store("Store#save()."), 6, 4, 8),
                refer(&util("format()."), 6, 9, 15),
                refer(&app("helper()."), 7, 2, 8),
                refer(&lodash("debounce()."), 8, 2, 10),
                // A function passed as a value: not a call.
                refer(&app("run()."), 8, 11, 14),
                refer(&store("api."), 9, 2, 5),
                refer(&store("api.fetch()."), 9, 6, 11),
                def_legacy(&app("helper()."), 12, 14),
                refer(&lib("console."), 13, 2, 9),
                def(&app("read()."), 16, 18),
                at("local 1", 17, 0),
            ],
            vec![],
        ),
        doc(
            "src/view.ts",
            vec![
                at(&store("Store#"), 0, IMPORT),
                def(&view("show()."), 2, 5),
                refer(&store("Store#"), 2, 55, 60),
                refer(&store("Store#save()."), 3, 5, 9),
                at("local 2", 4, 0),
            ],
            vec![],
        ),
        doc(
            "src/legacy.ts",
            vec![
                at(&util("format()."), 0, IMPORT),
                def(&legacy("old()."), 2, 4),
                refer(&util("format()."), 3, 9, 15),
            ],
            vec![],
        ),
        // A file of the CI checkout that is not in this repo.
        doc(
            "src/generated.ts",
            vec![def(&web("src/`generated.ts`/gen()."), 0, 1)],
            vec![],
        ),
    ];
    Index {
        metadata: MessageField::some(Metadata {
            project_root: PROJECT_ROOT.into(),
            tool_info: MessageField::some(ToolInfo {
                name: "scip-typescript".into(),
                version: "0.3.14".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        documents,
        ..Default::default()
    }
}

/// Write [`index`] (with `project_root` replaced when given) to `path` as protobuf.
pub fn write(path: &Path, project_root: Option<&str>) {
    let mut ix = index();
    if let Some(r) = project_root {
        ix.metadata.mut_or_insert_default().project_root = r.to_string();
    }
    scip::write_message_to_file(path, ix).unwrap();
}

/// Set a file's modification time to `secs` seconds from now.
pub fn touch(path: &Path, secs: i64) {
    let t = std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(secs.unsigned_abs()))
        .unwrap();
    let t = if secs < 0 {
        std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(secs.unsigned_abs()))
            .unwrap()
    } else {
        t
    };
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(t).unwrap();
}
