//! The files the MCP server keeps in a data dir are the ones the walker and the watcher
//! leave out when the data dir is the repo root (`cartograph_index::DATA_FILES`).

use cartograph_mcp::{annotations, engine, stats};

#[test]
fn data_files_are_known_to_the_walker() {
    for f in [
        stats::FILE,
        stats::OLD_FILE,
        stats::LOCK_FILE,
        engine::LOCK_FILE,
        annotations::FILE,
        cartograph_index::DB_FILE,
    ] {
        assert!(cartograph_index::DATA_FILES.contains(&f), "{f}");
    }
    // `annotate` writes its notes through this one.
    let tmp = format!("{}.tmp", annotations::FILE);
    assert!(
        cartograph_index::DATA_FILES.contains(&tmp.as_str()),
        "{tmp}"
    );
}
