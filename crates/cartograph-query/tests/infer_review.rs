//! Regression tests for `infer`: escaped globs, the `infer` → `check` round trip, `ignore`.

mod common;

use cartograph_query::{Config, Repo, check, infer};
use common::index_dir;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Two groups of modules (`r/[id]/` + `p1/`, `r/{g}/` + `p2/`) joined by one import
/// `p1 → p2`, so `infer` proposes two layers; `r/` holds a folder of each, so the folder
/// names with glob characters end up in globs. `p2/tool.config.ts` (config, not code)
/// imports `p1/` against the proposed order.
fn two_groups(root: &Path) {
    for i in 1..=5 {
        write(
            root,
            &format!("r/[id]/a{i}.ts"),
            "import { A } from '../../p1/p1a';\nimport { B } from '../../p1/p1b';\nexport const x = A + B;\n",
        );
        write(
            root,
            &format!("r/{{g}}/b{i}.ts"),
            "import { Q } from '../../p2/q1';\nimport { R } from '../../p2/q2';\nexport const y = Q + R;\n",
        );
    }
    write(
        root,
        "p1/p1a.ts",
        "import { Q } from '../p2/q1';\nexport const A = Q;\n",
    );
    write(root, "p1/p1b.ts", "export const B = 2;\n");
    write(root, "p2/q1.ts", "export const Q = 1;\n");
    write(root, "p2/q2.ts", "export const R = 1;\n");
    write(
        root,
        "p2/tool.config.ts",
        "import { A } from '../p1/p1a';\nexport default { a: A };\n",
    );
}

#[test]
fn infer_escapes_glob_characters_in_folder_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("g");
    two_groups(&root);
    let repo = Repo::new(&index_dir(&root), Config::default());
    let inferred = infer::infer(&repo).data;
    assert_eq!(inferred.layers.len(), 2, "{}", inferred.toml);
    let cfg =
        Config::parse(&inferred.toml, "infer").unwrap_or_else(|e| panic!("{e}\n{}", inferred.toml));
    let layer = |p: &str| cfg.layer_of(p);
    assert!(layer("r/[id]/a1.ts").is_some(), "{}", inferred.toml);
    assert_eq!(
        layer("r/[id]/a1.ts"),
        layer("p1/p1a.ts"),
        "{}",
        inferred.toml
    );
    assert_eq!(layer("r/{g}/b1.ts"), layer("p2/q1.ts"), "{}", inferred.toml);
    assert_ne!(layer("r/[id]/a1.ts"), layer("r/{g}/b1.ts"));
    // Escaped, not matching look-alikes.
    assert_eq!(layer("r/i/a1.ts"), None);
    assert_eq!(layer("r/g/b1.ts"), None);
}

#[test]
fn infer_proposal_passes_check_with_non_code_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("g");
    two_groups(&root);
    let graph = index_dir(&root);
    let inferred = infer::infer(&Repo::new(&graph, Config::default())).data;
    let cfg = Config::parse(&inferred.toml, "infer").unwrap();
    let repo = Repo::new(&graph, cfg);
    let c = check::check(&repo);
    assert!(c.data.ok, "{}\n{}", inferred.toml, c.text);
    // The config file's import goes against the order and says so.
    assert!(
        inferred.toml.contains("against the order"),
        "{}",
        inferred.toml
    );
}

#[test]
fn load_ignore_blames_ignore_only_for_its_own_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let file = root.join(cartograph_query::config::CONFIG_FILE);
    // Cannot be read (not UTF-8): no key to blame.
    fs::write(&file, b"ignore = [\"a\"]\n\xff\xfe\n").unwrap();
    let e = Config::load_ignore(root).unwrap_err();
    assert_eq!(e.key, None, "{e}");
    assert!(e.message.contains("cannot read"), "{e}");
    // Not TOML: no key either.
    fs::write(&file, "ignore = [\n").unwrap();
    let e = Config::load_ignore(root).unwrap_err();
    assert_eq!(e.key, None, "{e}");
    // An `ignore` that is not a list of patterns is about `ignore`.
    fs::write(&file, "ignore = 3\n").unwrap();
    let e = Config::load_ignore(root).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("ignore"), "{e}");
}

#[test]
fn infer_honours_ignore_and_writes_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("g");
    two_groups(&root);
    let graph = index_dir(&root);
    let cfg = Config::parse("ignore = [\"p1/**\"]\n", "cartograph.toml").unwrap();
    let inferred = infer::infer(&Repo::new(&graph, cfg)).data;
    assert!(
        !inferred
            .layers
            .iter()
            .any(|l| l.paths.iter().any(|p| p.starts_with("p1"))),
        "{}",
        inferred.toml
    );
    assert_eq!(
        inferred.layers.iter().map(|l| l.files).sum::<u64>(),
        7,
        "p1/ is not counted: {}",
        inferred.toml
    );
    let back = Config::parse(&inferred.toml, "infer").unwrap();
    assert!(back.ignore.matches("p1/p1a.ts"), "{}", inferred.toml);
    let c = check::check(&Repo::new(&graph, back));
    assert!(c.data.ok, "{}\n{}", inferred.toml, c.text);
}
