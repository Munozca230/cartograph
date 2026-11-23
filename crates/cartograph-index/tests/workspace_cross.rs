//! References that leave a workspace member's folder (`../other-repo/...`) land in the
//! member whose folder holds that place, whatever the member is named: Go `replace`
//! directives, Cargo path dependencies, tsconfig `extends`, `baseUrl` and `paths`.
//! Every member here lives in a folder named otherwise, so a virtual `member/path` join
//! would miss.

use cartograph_core::{EdgeKind, Graph};
use cartograph_index::{IndexOptions, index};
use std::path::Path;

fn write(root: &Path, files: &[(&str, &str)]) {
    for (p, text) in files {
        let f = root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }
}

fn import(g: &Graph, src: &str, dst: &str) -> bool {
    g.edges.iter().any(|e| {
        e.kind == EdgeKind::Import
            && e.src == format!("file:{src}")
            && e.dst == format!("file:{dst}")
    })
}

#[test]
fn references_out_of_a_member_resolve_through_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        &[
            (
                "cartograph-workspace.toml",
                "[[member]]\nname = \"app\"\npath = \"go-app\"\n\n\
                 [[member]]\nname = \"shared\"\npath = \"go-shared\"\n\n\
                 [[member]]\nname = \"cli\"\npath = \"rust-cli\"\n\n\
                 [[member]]\nname = \"corelib\"\npath = \"rust-core\"\n\n\
                 [[member]]\nname = \"web\"\npath = \"web-app\"\n\n\
                 [[member]]\nname = \"tsshared\"\npath = \"ts-shared\"\n",
            ),
            (
                "go-app/go.mod",
                "module example.com/app\n\ngo 1.22\n\nrequire example.com/shared v0.0.0\n\n\
                 replace example.com/shared => ../go-shared\n",
            ),
            (
                "go-app/main.go",
                "package main\n\nimport \"example.com/shared\"\n\nfunc main() { shared.Hello() }\n",
            ),
            ("go-shared/go.mod", "module example.com/fork\n\ngo 1.22\n"),
            ("go-shared/shared.go", "package shared\n\nfunc Hello() {}\n"),
            (
                "rust-cli/Cargo.toml",
                "[package]\nname = \"cli\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\ncore-lib = { path = \"../rust-core\" }\n",
            ),
            (
                "rust-cli/src/main.rs",
                "use core_lib::hello;\n\nfn main() {\n    hello();\n}\n",
            ),
            (
                "rust-core/Cargo.toml",
                "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("rust-core/src/lib.rs", "pub fn hello() {}\n"),
            (
                "web-app/tsconfig.json",
                "{ \"extends\": \"../ts-shared/tsconfig.base.json\",\n  \"compilerOptions\": { \"paths\": { \"@lib/*\": [\"../ts-shared/src/*\"] } } }\n",
            ),
            (
                "web-app/src/main.ts",
                "import { util } from '@lib/util';\nexport const x = util();\n",
            ),
            (
                "ts-shared/tsconfig.base.json",
                "{ \"compilerOptions\": { \"strict\": true } }\n",
            ),
            (
                "ts-shared/src/util.ts",
                "export function util() { return 1; }\n",
            ),
        ],
    );
    let g = index(root, &IndexOptions::default()).unwrap().graph;
    assert!(
        import(&g, "app/main.go", "shared/shared.go"),
        "go replace: {:?} {:?}",
        g.unresolved,
        g.diagnostics
    );
    assert!(
        import(&g, "cli/src/main.rs", "corelib/src/lib.rs"),
        "cargo path dep: {:?}",
        g.unresolved
    );
    assert!(
        import(&g, "web/src/main.ts", "tsshared/src/util.ts"),
        "tsconfig paths: {:?}",
        g.unresolved
    );
    assert!(
        g.diagnostics.iter().all(|d| !d.detail.contains("extends")),
        "tsconfig extends: {:?}",
        g.diagnostics
    );
}
