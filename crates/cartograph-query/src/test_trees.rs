//! Test, fixture and example trees: files there do not count as the repo's own entry
//! points or flows. Shared by `overview` and `symbols`.

use crate::graph::{DirIx, FileIx, ROOT, RepoGraph};
use crate::kind::FileKind;

/// Folders whose contents are tests, test data or samples, wherever they are.
const TEST_TREES: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "__test__",
    "e2e",
    "__mocks__",
    "fixtures",
    "__fixtures__",
    "testdata",
    "test-data",
    "test_data",
    // Plural only: `com/example/` is the Java package of many real apps.
    "examples",
];

/// Whether `f` is a test, or lies under a folder of [`TEST_TREES`] below `scope` (`scope`
/// itself and the folders above it do not count: inside `tests/fixtures/app/`, its
/// `src/main.rs` is not a test). (A folder's dominant kind is not used: a crate or package
/// folder with more test files than source files is still where its entry points live.)
pub(crate) fn in_test_tree(g: &RepoGraph, f: FileIx, scope: DirIx) -> bool {
    let file = &g.files[f as usize];
    if file.kind == FileKind::Test {
        // A test by its name, or by a test folder below `scope`.
        let dir = &g.dirs[scope as usize].path;
        let below = match file.path.strip_prefix(dir.as_str()) {
            Some(rest) if !dir.is_empty() => rest.strip_prefix('/').unwrap_or(rest),
            _ => file.path.as_str(),
        };
        if crate::kind::is_test(below) {
            return true;
        }
    }
    let mut d = Some(file.dir);
    while let Some(x) = d {
        if x == scope || x == ROOT {
            break;
        }
        if TEST_TREES.contains(&g.dirs[x as usize].name.to_ascii_lowercase().as_str()) {
            return true;
        }
        d = g.dirs[x as usize].parent;
    }
    false
}
