//! SQLite store for the graph (`<repo>/.cartograph/graph.db`).
//!
//! The schema is versioned in `schema_version`; [`Store::open`] applies every
//! pending migration in order inside a transaction.

use crate::history::{
    AuthorShare, CoChange, FileHistory, GitAuthor, GitChange, GitCommit, GitRaw, GitStatus, History,
};
use crate::model::{
    CallStats, Diagnostic, Edge, EntryPoint, Graph, ImportSite, Node, SkippedFile, SuperType,
    Symbol, SymbolCall, Symbols, UnresolvedImport, merge_edges, merge_skipped,
};
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// How long a connection waits on a lock held by another connection before failing.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Ordered migrations. Index `i` upgrades the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    // v1: graph tables.
    r#"
    CREATE TABLE nodes (
        id    TEXT PRIMARY KEY,
        kind  TEXT NOT NULL,
        path  TEXT NOT NULL,
        name  TEXT NOT NULL,
        lang  TEXT,
        loc   INTEGER NOT NULL DEFAULT 0,
        bytes INTEGER NOT NULL DEFAULT 0,
        hash  TEXT,
        meta  TEXT NOT NULL DEFAULT 'null'
    );
    CREATE TABLE edges (
        src        TEXT NOT NULL,
        dst        TEXT NOT NULL,
        kind       TEXT NOT NULL,
        confidence TEXT NOT NULL,
        PRIMARY KEY (src, dst, kind)
    );
    CREATE TABLE skipped (
        path   TEXT PRIMARY KEY,
        reason TEXT NOT NULL,
        detail TEXT
    );
    "#,
    // v2: run metadata (timings, manifests…) and lookup indexes.
    r#"
    CREATE TABLE meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE INDEX idx_nodes_kind ON nodes(kind);
    CREATE INDEX idx_edges_dst ON edges(dst);
    "#,
    // v3: one path may be skipped for several reasons (e.g. read error and parse error).
    r#"
    CREATE TABLE skipped_v3 (
        path   TEXT NOT NULL,
        reason TEXT NOT NULL,
        detail TEXT,
        PRIMARY KEY (path, reason)
    );
    INSERT INTO skipped_v3 (path, reason, detail) SELECT path, reason, detail FROM skipped;
    DROP TABLE skipped;
    ALTER TABLE skipped_v3 RENAME TO skipped;
    "#,
    // v4: import graph. Edge weights, every resolved import site, every unresolved import.
    // Neither import table has a primary key: two identical imports on one line are two rows.
    r#"
    ALTER TABLE edges ADD COLUMN weight INTEGER NOT NULL DEFAULT 1;
    CREATE TABLE import_sites (
        file      TEXT NOT NULL,
        line      INTEGER NOT NULL,
        specifier TEXT NOT NULL,
        target    TEXT NOT NULL
    );
    CREATE INDEX idx_import_sites_file ON import_sites(file);
    CREATE INDEX idx_import_sites_target ON import_sites(target);
    CREATE TABLE unresolved (
        file      TEXT NOT NULL,
        line      INTEGER NOT NULL,
        specifier TEXT NOT NULL,
        reason    TEXT NOT NULL,
        detail    TEXT
    );
    CREATE INDEX idx_unresolved_file ON unresolved(file);
    CREATE INDEX idx_unresolved_reason ON unresolved(reason);
    "#,
    // v5: resolver config diagnostics, kept apart from skipped files (the file is indexed).
    r#"
    CREATE TABLE diagnostics (
        path   TEXT NOT NULL,
        detail TEXT NOT NULL
    );
    "#,
    // v6: extraction cache. The raw imports of a file, keyed by its content hash, language
    // and extractor version, so a re-index only parses changed files. Resolution is never
    // cached. `write_graph` leaves this table alone.
    r#"
    CREATE TABLE extract_cache (
        hash      TEXT NOT NULL,
        lang      TEXT NOT NULL,
        extractor TEXT NOT NULL,
        imports   TEXT NOT NULL,
        PRIMARY KEY (hash, lang, extractor)
    ) WITHOUT ROWID;
    "#,
    // v7: the symbol layer (N2): definitions, resolved calls between symbols, per-file call
    // counts (unresolved and ambiguous calls are counted, never stored) and flow entry
    // points. Apart from `edges`, so file-level dependencies never see call data. No
    // primary key on symbols or calls: ids are not unique (overloads) and two calls on one
    // line are two rows. No secondary indexes either: readers load the tables whole, and
    // keeping indexes current tripled the write time of a warm re-index.
    r#"
    CREATE TABLE symbols (
        file      TEXT NOT NULL,
        name      TEXT NOT NULL,
        container TEXT,
        kind      TEXT NOT NULL,
        line      INTEGER NOT NULL,
        end_line  INTEGER NOT NULL,
        exported  INTEGER NOT NULL
    );
    CREATE TABLE symbol_calls (
        file       TEXT NOT NULL,
        line       INTEGER NOT NULL,
        caller     TEXT,
        target     TEXT NOT NULL,
        confidence TEXT NOT NULL
    );
    CREATE TABLE call_stats (
        file       TEXT PRIMARY KEY,
        exact      INTEGER NOT NULL,
        resolved   INTEGER NOT NULL,
        heuristic  INTEGER NOT NULL,
        ambiguous  INTEGER NOT NULL,
        unresolved INTEGER NOT NULL
    );
    CREATE TABLE entry_points (
        file   TEXT NOT NULL,
        line   INTEGER NOT NULL,
        kind   TEXT NOT NULL,
        name   TEXT NOT NULL,
        target TEXT
    );
    "#,
    // v8: the git layer. Raw history (one row per commit, per author identity and per
    // path a non-merge commit changed) is the source of truth, appended as new commits
    // arrive; `git_files` and `git_pairs` are rebuilt from it. Authors keep a display name
    // and an email hash, never the email. `write_graph` leaves these tables alone.
    r#"
    CREATE TABLE git_commits (
        seq    INTEGER PRIMARY KEY,
        id     TEXT NOT NULL,
        time   INTEGER NOT NULL,
        author INTEGER NOT NULL,
        files  INTEGER NOT NULL,
        merge  INTEGER NOT NULL
    );
    CREATE TABLE git_authors (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL,
        email_hash TEXT NOT NULL
    );
    CREATE TABLE git_changes (
        seq      INTEGER NOT NULL,
        path     TEXT NOT NULL,
        old_path TEXT,
        kind     TEXT NOT NULL,
        added    INTEGER,
        removed  INTEGER
    );
    CREATE TABLE git_files (
        path       TEXT PRIMARY KEY,
        commits    INTEGER NOT NULL,
        added      INTEGER NOT NULL,
        removed    INTEGER NOT NULL,
        recent     INTEGER NOT NULL,
        first_time INTEGER NOT NULL,
        last_time  INTEGER NOT NULL,
        co_commits INTEGER NOT NULL,
        complexity INTEGER NOT NULL,
        authors    TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE git_pairs (
        a         TEXT NOT NULL,
        b         TEXT NOT NULL,
        together  INTEGER NOT NULL,
        last_time INTEGER NOT NULL,
        PRIMARY KEY (a, b)
    ) WITHOUT ROWID;
    "#,
    // v9: supertypes of the symbol layer (base classes, interfaces, implemented traits),
    // cleared and written with the other symbol tables.
    r#"
    CREATE TABLE symbol_supers (
        sub        TEXT NOT NULL,
        name       TEXT NOT NULL,
        target     TEXT,
        confidence TEXT,
        file       TEXT NOT NULL,
        line       INTEGER NOT NULL
    );
    "#,
    // v10: the precise layer (N3, SCIP). `scip_cache` holds the decoded form of each SCIP
    // file read, keyed by its path and checked against its size, mtime and hash, so an
    // unchanged index is never decoded twice; `write_graph` leaves it alone. `scip_files`
    // and `scip_symbols` belong to the symbol layer (covered files, symbols added from
    // SCIP) and are cleared with it. Self-contained: no other table changes.
    r#"
    CREATE TABLE scip_cache (
        path  TEXT PRIMARY KEY,
        size  INTEGER NOT NULL,
        mtime INTEGER NOT NULL,
        hash  TEXT NOT NULL,
        data  TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE scip_files (
        path TEXT PRIMARY KEY
    ) WITHOUT ROWID;
    CREATE TABLE scip_symbols (
        id   TEXT NOT NULL,
        line INTEGER NOT NULL
    );
    "#,
    // v11: workspaces. Each commit belongs to the member whose repository it comes from
    // (`''` for a single repo), so one member's history is rebuilt without the others'.
    r#"
    ALTER TABLE git_commits ADD COLUMN member TEXT NOT NULL DEFAULT '';
    "#,
    // v12: bridges (HTTP, IPC, packages; see `crate::bridge`): both sides of each one
    // found, and the ones matched. They belong to the symbol layer and are cleared with it.
    // Self-contained: no other table changes.
    r#"
    CREATE TABLE bridge_endpoints (
        kind       TEXT NOT NULL,
        role       TEXT NOT NULL,
        key        TEXT NOT NULL,
        label      TEXT NOT NULL,
        file       TEXT NOT NULL,
        line       INTEGER NOT NULL,
        symbol     TEXT,
        confidence TEXT NOT NULL,
        detail     TEXT
    );
    CREATE TABLE bridges (
        kind       TEXT NOT NULL,
        subtype    TEXT NOT NULL,
        src_file   TEXT NOT NULL,
        src_line   INTEGER NOT NULL,
        src        TEXT,
        dst_file   TEXT NOT NULL,
        dst_line   INTEGER NOT NULL,
        dst        TEXT,
        key        TEXT NOT NULL,
        confidence TEXT NOT NULL
    );
    "#,
    // v13: string keys (see `crate::key`): where each is defined, run, listed and used.
    // They belong to the symbol layer and are cleared with it. Self-contained.
    r#"
    CREATE TABLE key_sites (
        key            TEXT NOT NULL,
        role           TEXT NOT NULL,
        form           TEXT NOT NULL,
        name           TEXT,
        container      TEXT NOT NULL,
        container_file TEXT NOT NULL,
        container_line INTEGER NOT NULL,
        file           TEXT NOT NULL,
        line           INTEGER NOT NULL,
        symbol         TEXT,
        target         TEXT,
        confidence     TEXT NOT NULL
    );
    "#,
    // v14: Spring beans, injection points and data sinks (see `crate::spring`). They
    // belong to the symbol layer and are cleared with it. Self-contained.
    r#"
    CREATE TABLE spring_beans (
        ty         TEXT NOT NULL,
        name       TEXT NOT NULL,
        stereotype TEXT NOT NULL,
        file       TEXT NOT NULL,
        line       INTEGER NOT NULL,
        factory    TEXT,
        is_primary INTEGER NOT NULL,
        qualifiers TEXT NOT NULL,
        aliases    TEXT NOT NULL
    );
    CREATE TABLE spring_injections (
        owner     TEXT NOT NULL,
        how       TEXT NOT NULL,
        member    TEXT NOT NULL,
        wrapper   TEXT NOT NULL,
        ty        TEXT NOT NULL,
        qualifier TEXT,
        file      TEXT NOT NULL,
        line      INTEGER NOT NULL,
        uses      TEXT NOT NULL,
        resource  TEXT
    );
    CREATE TABLE data_sinks (
        store      TEXT NOT NULL,
        name       TEXT NOT NULL,
        entity     TEXT,
        symbol     TEXT,
        via        TEXT NOT NULL,
        file       TEXT NOT NULL,
        line       INTEGER NOT NULL,
        confidence TEXT NOT NULL
    );
    "#,
];

/// One row of the SCIP cache: the decoded form (`data`, opaque to the store) of the SCIP
/// file at `path`, with the size, mtime (nanoseconds since the epoch) and hash it had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipCacheRow {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub hash: String,
    pub data: String,
}

/// One row of the extraction cache: `imports` is opaque to the store (JSON written by the
/// indexer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub hash: String,
    pub lang: String,
    pub imports: String,
}

/// Latest schema version this build understands.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Handle to a graph database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) the database at `path` in WAL mode and migrate it to
    /// [`SCHEMA_VERSION`]. WAL lets readers (UI, MCP) keep reading while the indexer
    /// writes; [`BUSY_TIMEOUT`] absorbs short write-write contention.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enable_wal(&conn)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// Journal mode reported by SQLite (`wal` for file databases opened with [`Store::open`]).
    pub fn journal_mode(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))?)
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Store { conn };
        store.migrate_to(SCHEMA_VERSION)?;
        Ok(store)
    }

    /// Current schema version of the open database.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))?)
    }

    /// Apply migrations up to `target`. Public so tests can exercise an upgrade from an
    /// older version.
    ///
    /// The transaction is IMMEDIATE: it takes the write lock before reading the version,
    /// so two processes upgrading at once serialise on `busy_timeout` instead of failing
    /// with `SQLITE_BUSY_SNAPSHOT` (which a deferred read-then-write transaction gets in
    /// WAL mode and which the busy handler cannot retry).
    pub fn migrate_to(&mut self, target: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")?;
        let current: Option<i64> = tx
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()?;
        let current = match current {
            Some(v) => v,
            None => {
                tx.execute("INSERT INTO schema_version (version) VALUES (0)", [])?;
                0
            }
        };
        if current > SCHEMA_VERSION {
            return Err(Error::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let to = i as i64 + 1;
            if to > current && to <= target {
                tx.execute_batch(sql)?;
                tx.execute("UPDATE schema_version SET version = ?1", [to])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace the whole stored graph with `graph`, atomically.
    pub fn replace_graph(&mut self, graph: &Graph) -> Result<()> {
        let tx = self.begin_replace()?;
        tx.write_graph(graph)?;
        tx.commit()
    }

    /// Start a write transaction (IMMEDIATE). Write the graph and every run-metadata key
    /// through it, then [`WriteTx::commit`], so readers never see a new graph with old
    /// metadata. Dropping it without committing rolls everything back.
    pub fn begin_replace(&mut self) -> Result<WriteTx<'_>> {
        Ok(WriteTx {
            tx: self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?,
        })
    }

    /// Load the whole graph, sorted (see [`Graph::normalize`]), from one consistent
    /// snapshot.
    pub fn load_graph(&self) -> Result<Graph> {
        let tx = self.conn.unchecked_transaction()?;
        let graph = read_graph(&tx)?;
        tx.finish()?;
        Ok(graph)
    }

    /// Load the graph and all run metadata from one consistent snapshot.
    pub fn load_snapshot(&self) -> Result<(Graph, BTreeMap<String, String>)> {
        let tx = self.conn.unchecked_transaction()?;
        let graph = read_graph(&tx)?;
        let meta = {
            let mut stmt = tx.prepare("SELECT key, value FROM meta")?;
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<BTreeMap<_, _>>>()?
        };
        tx.finish()?;
        Ok((graph, meta))
    }

    /// Set a run-metadata value (outside any graph replacement).
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn, key, value)
    }

    /// Get a run-metadata value.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// Every extraction-cache row written by extractor version `extractor`. Rows of other
    /// versions are ignored (and removed by [`WriteTx::prune_extract_cache`]).
    pub fn load_extract_cache(&self, extractor: &str) -> Result<Vec<CacheEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT hash, lang, imports FROM extract_cache WHERE extractor = ?1")?;
        let rows = stmt.query_map([extractor], |r| {
            Ok(CacheEntry {
                hash: r.get(0)?,
                lang: r.get(1)?,
                imports: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The SCIP cache row of `path`, if any (none in a database without the table).
    pub fn load_scip_cache(&self, path: &str) -> Result<Option<ScipCacheRow>> {
        if !has_table(&self.conn, "scip_cache")? {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT path, size, mtime, hash, data FROM scip_cache WHERE path = ?1",
                [path],
                |r| {
                    Ok(ScipCacheRow {
                        path: r.get(0)?,
                        size: r.get(1)?,
                        mtime: r.get(2)?,
                        hash: r.get(3)?,
                        data: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Load the symbol layer (sorted, see [`Symbols::normalize`]). Empty for a graph
    /// written by [`Store::replace_graph`] alone.
    pub fn load_symbols(&self) -> Result<Symbols> {
        let tx = self.conn.unchecked_transaction()?;
        let symbols = read_symbols(&tx)?;
        tx.finish()?;
        Ok(symbols)
    }

    /// Load the graph and the symbol layer from one consistent snapshot.
    pub fn load_graph_and_symbols(&self) -> Result<(Graph, Symbols)> {
        let tx = self.conn.unchecked_transaction()?;
        let graph = read_graph(&tx)?;
        let symbols = read_symbols(&tx)?;
        tx.finish()?;
        Ok((graph, symbols))
    }

    /// The raw git history (empty when the git layer never ran), commits by `seq`.
    pub fn load_git_raw(&self) -> Result<GitRaw> {
        let tx = self.conn.unchecked_transaction()?;
        let raw = read_git_raw(&tx)?;
        tx.finish()?;
        Ok(raw)
    }

    /// The aggregated git history and the status of the last git pass, from one snapshot.
    pub fn load_history(&self) -> Result<History> {
        let tx = self.conn.unchecked_transaction()?;
        let history = read_history(&tx)?;
        tx.finish()?;
        Ok(history)
    }

    /// The members the stored git rows belong to (`""` for a single repo).
    pub fn git_members(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT member FROM git_commits ORDER BY member")?;
        let out = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(out)
    }

    /// Raw connection, for ad-hoc queries (reports, tests).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// Switch the database to WAL. Switching needs an exclusive lock and SQLite returns
/// `SQLITE_BUSY` for it without consulting the busy handler, so a concurrent first open
/// is retried here with backoff for up to [`BUSY_TIMEOUT`].
fn enable_wal(conn: &Connection) -> Result<()> {
    let current: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if current.eq_ignore_ascii_case("wal") {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    let mut wait = Duration::from_millis(2);
    loop {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(mode) => return Err(Error::NotWal(mode)),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Write side of [`Store::begin_replace`].
pub struct WriteTx<'a> {
    tx: Transaction<'a>,
}

impl WriteTx<'_> {
    /// Replace nodes, edges, skipped entries and import records with `graph`. The symbol
    /// layer is cleared too, since it describes the previous graph; write the new one with
    /// [`WriteTx::write_symbols`] after this.
    pub fn write_graph(&self, graph: &Graph) -> Result<()> {
        self.tx.execute_batch(
            "DELETE FROM edges; DELETE FROM nodes; DELETE FROM skipped;
             DELETE FROM import_sites; DELETE FROM unresolved; DELETE FROM diagnostics;",
        )?;
        clear_symbols(&self.tx)?;
        insert_graph(&self.tx, graph)
    }

    /// Replace the symbol layer with `symbols`.
    pub fn write_symbols(&self, symbols: &Symbols) -> Result<()> {
        clear_symbols(&self.tx)?;
        insert_symbols(&self.tx, symbols)
    }

    /// Set a run-metadata value.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.tx, key, value)
    }

    /// Remove a run-metadata value (nothing when it is not set).
    pub fn delete_meta(&self, key: &str) -> Result<()> {
        self.tx.execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Insert (or overwrite) extraction-cache rows for extractor version `extractor`.
    pub fn put_extract_cache(&self, extractor: &str, entries: &[CacheEntry]) -> Result<()> {
        let mut stmt = self.tx.prepare(
            "INSERT OR REPLACE INTO extract_cache (hash, lang, extractor, imports)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for e in entries {
            stmt.execute(params![e.hash, e.lang, extractor, e.imports])?;
        }
        Ok(())
    }

    /// Insert (or overwrite) a SCIP cache row.
    pub fn put_scip_cache(&self, row: &ScipCacheRow) -> Result<()> {
        self.tx.execute(
            "INSERT OR REPLACE INTO scip_cache (path, size, mtime, hash, data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![row.path, row.size, row.mtime, row.hash, row.data],
        )?;
        Ok(())
    }

    /// Keep only the SCIP cache rows of `paths` (the SCIP files of this run).
    pub fn retain_scip_cache(&self, paths: &[String]) -> Result<()> {
        let keep: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();
        let stored: Vec<String> = {
            let mut stmt = self.tx.prepare("SELECT path FROM scip_cache")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut del = self.tx.prepare("DELETE FROM scip_cache WHERE path = ?1")?;
        for p in stored.iter().filter(|p| !keep.contains(p.as_str())) {
            del.execute([p])?;
        }
        Ok(())
    }

    /// Delete the given `(hash, lang)` rows of version `extractor`, and every row of any
    /// other extractor version.
    pub fn prune_extract_cache(&self, extractor: &str, stale: &[(String, String)]) -> Result<()> {
        self.tx.execute(
            "DELETE FROM extract_cache WHERE extractor <> ?1",
            [extractor],
        )?;
        let mut stmt = self.tx.prepare(
            "DELETE FROM extract_cache WHERE hash = ?1 AND lang = ?2 AND extractor = ?3",
        )?;
        for (hash, lang) in stale {
            stmt.execute(params![hash, lang, extractor])?;
        }
        Ok(())
    }

    /// Delete the whole git layer (raw rows and aggregates), before a rebuild.
    pub fn clear_git(&self) -> Result<()> {
        self.tx.execute_batch(
            "DELETE FROM git_commits; DELETE FROM git_authors; DELETE FROM git_changes;
             DELETE FROM git_files; DELETE FROM git_pairs;",
        )?;
        Ok(())
    }

    /// Delete the git layer of one workspace member (`""`: the whole layer, as
    /// [`WriteTx::clear_git`]): its commits and their changes, the aggregates of its files,
    /// and the authors no commit names any more.
    pub fn clear_git_member(&self, member: &str) -> Result<()> {
        if member.is_empty() {
            return self.clear_git();
        }
        self.tx.execute(
            "DELETE FROM git_changes WHERE seq IN (SELECT seq FROM git_commits WHERE member = ?1)",
            [member],
        )?;
        self.tx
            .execute("DELETE FROM git_commits WHERE member = ?1", [member])?;
        self.tx.execute(
            "DELETE FROM git_authors WHERE id NOT IN (SELECT author FROM git_commits)",
            [],
        )?;
        delete_history_under(&self.tx, member)
    }

    /// Delete the git layer of every member not in `keep` (and that of a single repo, unless
    /// `keep` holds `""`): rows, aggregates and their meta keys (status, cursor, scope), so a
    /// member added again starts from scratch.
    pub fn retain_git_members(&self, keep: &[&str]) -> Result<()> {
        let mut stmt = self.tx.prepare(
            "SELECT key FROM meta WHERE key LIKE 'git:%' OR key LIKE 'git_cursor%'
               OR key LIKE 'git_scope%'",
        )?;
        let keys: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        for k in keys {
            let member = k.split_once(':').map_or("", |(_, m)| m);
            if !keep.contains(&member) {
                self.tx.execute("DELETE FROM meta WHERE key = ?1", [&k])?;
            }
        }
        let mut stmt = self.tx.prepare("SELECT DISTINCT member FROM git_commits")?;
        let found: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        for m in found.iter().filter(|m| !keep.contains(&m.as_str())) {
            if m.is_empty() {
                self.tx.execute(
                    "DELETE FROM git_changes WHERE seq IN
                       (SELECT seq FROM git_commits WHERE member = '')",
                    [],
                )?;
                self.tx
                    .execute("DELETE FROM git_commits WHERE member = ''", [])?;
            } else {
                self.clear_git_member(m)?;
            }
        }
        if keep.contains(&"") {
            return Ok(());
        }
        // Aggregates of paths outside every member (a single repo before).
        let mut stmt = self.tx.prepare("SELECT path FROM git_files")?;
        let paths: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        let outside = |p: &str| !keep.contains(&p.split('/').next().unwrap_or(p));
        for p in paths.iter().filter(|p| outside(p)) {
            self.tx
                .execute("DELETE FROM git_files WHERE path = ?1", [p])?;
            self.tx
                .execute("DELETE FROM git_pairs WHERE a = ?1 OR b = ?1", [p])?;
        }
        Ok(())
    }

    /// Append raw history rows. Authors are upserted by id; commits must have new `seq`s.
    pub fn append_git_raw(&self, raw: &GitRaw) -> Result<()> {
        let mut stmt = self.tx.prepare(
            "INSERT OR REPLACE INTO git_authors (id, name, email_hash) VALUES (?1, ?2, ?3)",
        )?;
        for a in &raw.authors {
            stmt.execute(params![a.id, a.name, a.email_hash])?;
        }
        let mut stmt = self.tx.prepare(
            "INSERT INTO git_commits (seq, id, time, author, files, merge, member)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for c in &raw.commits {
            stmt.execute(params![
                c.seq, c.id, c.time, c.author, c.files, c.merge, c.member
            ])?;
        }
        let mut stmt = self.tx.prepare(
            "INSERT INTO git_changes (seq, path, old_path, kind, added, removed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for c in &raw.changes {
            stmt.execute(params![
                c.seq,
                c.path,
                c.old_path,
                c.kind.as_str(),
                c.added,
                c.removed
            ])?;
        }
        Ok(())
    }

    /// Replace the aggregated history and store its status under the [`GIT_META`] key.
    pub fn write_history(&self, h: &History) -> Result<()> {
        self.write_member_history("", h)
    }

    /// Replace the aggregated history of one workspace member (the files under its folder;
    /// `""`: every file) and store its status under [`git_meta_key`].
    pub fn write_member_history(&self, member: &str, h: &History) -> Result<()> {
        delete_history_under(&self.tx, member)?;
        let mut stmt = self.tx.prepare(
            "INSERT INTO git_files (path, commits, added, removed, recent, first_time,
                last_time, co_commits, complexity, authors)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for f in &h.files {
            let authors: Vec<(&str, u32)> = f
                .authors
                .iter()
                .map(|a| (a.name.as_str(), a.commits))
                .collect();
            stmt.execute(params![
                f.path,
                f.commits,
                f.added as i64,
                f.removed as i64,
                f.recent,
                f.first_time,
                f.last_time,
                f.co_commits,
                f.complexity as i64,
                serde_json::to_string(&authors)?
            ])?;
        }
        let mut stmt = self
            .tx
            .prepare("INSERT INTO git_pairs (a, b, together, last_time) VALUES (?1, ?2, ?3, ?4)")?;
        for p in &h.pairs {
            stmt.execute(params![p.a, p.b, p.together, p.last_time])?;
        }
        if let Some(status) = &h.status {
            set_meta(
                &self.tx,
                &git_meta_key(member),
                &serde_json::to_string(status)?,
            )?;
        }
        Ok(())
    }

    /// Commit everything written through this transaction.
    pub fn commit(self) -> Result<()> {
        self.tx.commit()?;
        Ok(())
    }
}

/// Meta key holding the [`GitStatus`] of the last git pass (JSON). In a workspace it sums
/// the members up; each member's own is under [`git_meta_key`].
pub const GIT_META: &str = "git";

/// Meta key of the git status of a workspace member (`git:<member>`; [`GIT_META`] for
/// `""`, a single repo).
pub fn git_meta_key(member: &str) -> String {
    if member.is_empty() {
        GIT_META.to_string()
    } else {
        format!("{GIT_META}:{member}")
    }
}

/// Delete the aggregates of the files under `member`'s folder (`""`: all of them).
fn delete_history_under(conn: &Connection, member: &str) -> Result<()> {
    if member.is_empty() {
        conn.execute_batch("DELETE FROM git_files; DELETE FROM git_pairs;")?;
        return Ok(());
    }
    let like = format!("{member}/%");
    conn.execute(
        "DELETE FROM git_files WHERE path = ?1 OR path LIKE ?2",
        params![member, like],
    )?;
    conn.execute(
        "DELETE FROM git_pairs WHERE a = ?1 OR a LIKE ?2 OR b = ?1 OR b LIKE ?2",
        params![member, like],
    )?;
    Ok(())
}

fn read_git_raw(conn: &Connection) -> Result<GitRaw> {
    let mut raw = GitRaw::default();
    let mut stmt = conn.prepare(
        "SELECT seq, id, time, author, files, merge, member FROM git_commits ORDER BY seq",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(GitCommit {
            seq: r.get(0)?,
            id: r.get(1)?,
            time: r.get(2)?,
            author: r.get(3)?,
            files: r.get(4)?,
            merge: r.get(5)?,
            member: r.get(6)?,
        })
    })?;
    for row in rows {
        raw.commits.push(row?);
    }
    let mut stmt = conn.prepare("SELECT id, name, email_hash FROM git_authors ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(GitAuthor {
            id: r.get(0)?,
            name: r.get(1)?,
            email_hash: r.get(2)?,
        })
    })?;
    for row in rows {
        raw.authors.push(row?);
    }
    let mut stmt = conn.prepare(
        "SELECT seq, path, old_path, kind, added, removed FROM git_changes ORDER BY seq, rowid",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, u32>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<u32>>(4)?,
            r.get::<_, Option<u32>>(5)?,
        ))
    })?;
    for row in rows {
        let (seq, path, old_path, kind, added, removed) = row?;
        raw.changes.push(GitChange {
            seq,
            path,
            old_path,
            kind: kind.parse()?,
            added,
            removed,
        });
    }
    Ok(raw)
}

fn read_history(conn: &Connection) -> Result<History> {
    let status = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [GIT_META], |r| {
            r.get::<_, String>(0)
        })
        .optional()?;
    let mut h = History {
        status: match status {
            Some(s) => Some(serde_json::from_str::<GitStatus>(&s)?),
            None => None,
        },
        ..History::default()
    };
    let mut stmt = conn.prepare(
        "SELECT path, commits, added, removed, recent, first_time, last_time, co_commits,
            complexity, authors FROM git_files",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            FileHistory {
                path: r.get(0)?,
                commits: r.get(1)?,
                added: r.get::<_, i64>(2)? as u64,
                removed: r.get::<_, i64>(3)? as u64,
                recent: r.get(4)?,
                first_time: r.get(5)?,
                last_time: r.get(6)?,
                co_commits: r.get(7)?,
                complexity: r.get::<_, i64>(8)? as u64,
                authors: Vec::new(),
            },
            r.get::<_, String>(9)?,
        ))
    })?;
    for row in rows {
        let (mut f, authors) = row?;
        let list: Vec<(String, u32)> = serde_json::from_str(&authors)?;
        f.authors = list
            .into_iter()
            .map(|(name, commits)| AuthorShare { name, commits })
            .collect();
        h.files.push(f);
    }
    let mut stmt = conn.prepare("SELECT a, b, together, last_time FROM git_pairs")?;
    let rows = stmt.query_map([], |r| {
        Ok(CoChange {
            a: r.get(0)?,
            b: r.get(1)?,
            together: r.get(2)?,
            last_time: r.get(3)?,
        })
    })?;
    for row in rows {
        h.pairs.push(row?);
    }
    h.normalize();
    Ok(h)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn read_graph(conn: &Connection) -> Result<Graph> {
    let mut graph = Graph::default();
    let mut stmt =
        conn.prepare("SELECT id, kind, path, name, lang, loc, bytes, hash, meta FROM nodes")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, String>(8)?,
        ))
    })?;
    for row in rows {
        let (id, kind, path, name, lang, loc, bytes, hash, meta) = row?;
        graph.nodes.push(Node {
            id,
            kind: kind.parse()?,
            path,
            name,
            lang,
            loc: loc as u64,
            bytes: bytes as u64,
            hash,
            meta: serde_json::from_str(&meta)?,
        });
    }

    let mut stmt = conn.prepare("SELECT src, dst, kind, confidence, weight FROM edges")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (src, dst, kind, confidence, weight) = row?;
        graph.edges.push(Edge {
            src,
            dst,
            kind: kind.parse()?,
            confidence: confidence.parse()?,
            weight: weight as u64,
        });
    }

    let mut stmt = conn.prepare("SELECT file, line, specifier, target FROM import_sites")?;
    let rows = stmt.query_map([], |r| {
        Ok(ImportSite {
            file: r.get(0)?,
            line: r.get(1)?,
            specifier: r.get(2)?,
            target: r.get(3)?,
        })
    })?;
    for row in rows {
        graph.imports.push(row?);
    }

    let mut stmt = conn.prepare("SELECT file, line, specifier, reason, detail FROM unresolved")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (file, line, specifier, reason, detail) = row?;
        graph.unresolved.push(UnresolvedImport {
            file,
            line,
            specifier,
            reason: reason.parse()?,
            detail,
        });
    }

    let mut stmt = conn.prepare("SELECT path, reason, detail FROM skipped")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (path, reason, detail) = row?;
        graph.skipped.push(SkippedFile {
            path,
            reason: reason.parse()?,
            detail,
        });
    }

    let mut stmt = conn.prepare("SELECT path, detail FROM diagnostics")?;
    let rows = stmt.query_map([], |r| {
        Ok(Diagnostic {
            path: r.get(0)?,
            detail: r.get(1)?,
        })
    })?;
    for row in rows {
        graph.diagnostics.push(row?);
    }

    graph.normalize();
    Ok(graph)
}

/// Whether the database has table `name` (one held at an older version by a test may not).
fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Empty the symbol tables (a database held at an older version by a test has none).
fn clear_symbols(conn: &Connection) -> Result<()> {
    if !has_table(conn, "symbols")? {
        return Ok(());
    }
    conn.execute_batch(
        "DELETE FROM symbols; DELETE FROM symbol_calls; DELETE FROM call_stats;
         DELETE FROM entry_points;",
    )?;
    if has_table(conn, "symbol_supers")? {
        conn.execute("DELETE FROM symbol_supers", [])?;
    }
    conn.execute("DELETE FROM meta WHERE key = ?1", [SYMBOLS_INDEXED])?;
    conn.execute("DELETE FROM meta WHERE key = ?1", [KEYS_INDEXED])?;
    if has_table(conn, "scip_files")? {
        conn.execute_batch("DELETE FROM scip_files; DELETE FROM scip_symbols;")?;
    }
    conn.execute("DELETE FROM meta WHERE key = ?1", [SCIP_STATS])?;
    if has_table(conn, "bridges")? {
        conn.execute_batch("DELETE FROM bridge_endpoints; DELETE FROM bridges;")?;
    }
    if has_table(conn, "key_sites")? {
        conn.execute("DELETE FROM key_sites", [])?;
    }
    if has_table(conn, "spring_beans")? {
        conn.execute_batch(
            "DELETE FROM spring_beans; DELETE FROM spring_injections; DELETE FROM data_sinks;",
        )?;
    }
    Ok(())
}

/// Run-metadata key of the SCIP import counts ([`crate::ScipStats`], JSON).
const SCIP_STATS: &str = "scip_stats";

/// Run-metadata key present while the symbol tables hold a layer the indexer computed
/// (see [`Symbols::indexed`]), even an empty one.
const SYMBOLS_INDEXED: &str = "symbols_indexed";

/// Run-metadata key present while `key_sites` holds what the indexer found (see
/// [`Symbols::keys_indexed`]), even nothing.
const KEYS_INDEXED: &str = "keys_indexed";

fn insert_symbols(tx: &Transaction<'_>, s: &Symbols) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO symbols (file, name, container, kind, line, end_line, exported)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for x in &s.symbols {
        stmt.execute(params![
            x.file,
            x.name,
            x.container,
            x.kind.as_str(),
            x.line,
            x.end_line,
            x.exported
        ])?;
    }
    let mut stmt = tx.prepare(
        "INSERT INTO symbol_calls (file, line, caller, target, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for c in &s.calls {
        stmt.execute(params![
            c.file,
            c.line,
            c.caller,
            c.target,
            c.confidence.as_str()
        ])?;
    }
    // Two rows for one file add up: the primary key holds and nothing is lost.
    let mut stmt = tx.prepare(
        "INSERT INTO call_stats (file, exact, resolved, heuristic, ambiguous, unresolved)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(file) DO UPDATE SET exact = exact + excluded.exact,
            resolved = resolved + excluded.resolved,
            heuristic = heuristic + excluded.heuristic,
            ambiguous = ambiguous + excluded.ambiguous,
            unresolved = unresolved + excluded.unresolved",
    )?;
    for c in &s.stats {
        stmt.execute(params![
            c.file,
            c.exact,
            c.resolved,
            c.heuristic,
            c.ambiguous,
            c.unresolved
        ])?;
    }
    let mut stmt = tx.prepare(
        "INSERT INTO entry_points (file, line, kind, name, target) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for e in &s.entries {
        stmt.execute(params![e.file, e.line, e.kind.as_str(), e.name, e.target])?;
    }
    if !s.supers.is_empty() {
        let mut stmt = tx.prepare(
            "INSERT INTO symbol_supers (sub, name, target, confidence, file, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for x in &s.supers {
            stmt.execute(params![
                x.sub,
                x.name,
                x.target,
                x.confidence.map(|c| c.as_str()),
                x.file,
                x.line
            ])?;
        }
    }
    if s.indexed {
        set_meta(tx, SYMBOLS_INDEXED, "1")?;
    }
    if s.keys_indexed && has_table(tx, "key_sites")? {
        set_meta(tx, KEYS_INDEXED, "1")?;
    }
    if !s.scip.is_empty() {
        let mut stmt = tx.prepare("INSERT OR IGNORE INTO scip_files (path) VALUES (?1)")?;
        for f in &s.scip.files {
            stmt.execute([f])?;
        }
        let mut stmt = tx.prepare("INSERT INTO scip_symbols (id, line) VALUES (?1, ?2)")?;
        for x in &s.scip.added {
            stmt.execute(params![x.id, x.line])?;
        }
        if let Some(st) = &s.scip.stats {
            set_meta(tx, SCIP_STATS, &serde_json::to_string(st)?)?;
        }
    }
    // A database held at an older version by a test has no bridge tables.
    let bridges = (!s.endpoints.is_empty() || !s.bridges.is_empty()) && has_table(tx, "bridges")?;
    if bridges && !s.endpoints.is_empty() {
        let mut stmt = tx.prepare(
            "INSERT INTO bridge_endpoints
             (kind, role, key, label, file, line, symbol, confidence, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for e in &s.endpoints {
            stmt.execute(params![
                e.kind.as_str(),
                e.role.as_str(),
                e.key,
                e.label,
                e.file,
                e.line,
                e.symbol,
                e.confidence.as_str(),
                e.detail
            ])?;
        }
    }
    if !s.keys.is_empty() && has_table(tx, "key_sites")? {
        let mut stmt = tx.prepare(
            "INSERT INTO key_sites
             (key, role, form, name, container, container_file, container_line, file, line,
              symbol, target, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for k in &s.keys {
            stmt.execute(params![
                k.key,
                k.role.as_str(),
                k.form,
                k.name,
                k.container,
                k.container_file,
                k.container_line,
                k.file,
                k.line,
                k.symbol,
                k.target,
                k.confidence.as_str()
            ])?;
        }
    }
    if has_table(tx, "spring_beans")? {
        insert_spring(tx, s)?;
    }
    if bridges && !s.bridges.is_empty() {
        let mut stmt = tx.prepare(
            "INSERT INTO bridges
             (kind, subtype, src_file, src_line, src, dst_file, dst_line, dst, key, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for b in &s.bridges {
            stmt.execute(params![
                b.kind.as_str(),
                b.subtype,
                b.src_file,
                b.src_line,
                b.src,
                b.dst_file,
                b.dst_line,
                b.dst,
                b.key,
                b.confidence.as_str()
            ])?;
        }
    }
    Ok(())
}

fn read_symbols(conn: &Connection) -> Result<Symbols> {
    let mut out = Symbols::default();
    let mut stmt =
        conn.prepare("SELECT file, name, container, kind, line, end_line, exported FROM symbols")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, u32>(4)?,
            r.get::<_, u32>(5)?,
            r.get::<_, bool>(6)?,
        ))
    })?;
    for row in rows {
        let (file, name, container, kind, line, end_line, exported) = row?;
        out.symbols.push(Symbol {
            file,
            name,
            container,
            kind: kind.parse()?,
            line,
            end_line,
            exported,
        });
    }
    let mut stmt =
        conn.prepare("SELECT file, line, caller, target, confidence FROM symbol_calls")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (file, line, caller, target, confidence) = row?;
        out.calls.push(SymbolCall {
            file,
            line,
            caller,
            target,
            confidence: confidence.parse()?,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT file, exact, resolved, heuristic, ambiguous, unresolved FROM call_stats",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CallStats {
            file: r.get(0)?,
            exact: r.get(1)?,
            resolved: r.get(2)?,
            heuristic: r.get(3)?,
            ambiguous: r.get(4)?,
            unresolved: r.get(5)?,
        })
    })?;
    for row in rows {
        out.stats.push(row?);
    }
    let mut stmt = conn.prepare("SELECT file, line, kind, name, target FROM entry_points")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u32>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (file, line, kind, name, target) = row?;
        out.entries.push(EntryPoint {
            file,
            line,
            kind: kind.parse()?,
            name,
            target,
        });
    }
    if has_table(conn, "symbol_supers")? {
        let mut stmt = conn.prepare(
            "SELECT sub, name, target, confidence, file, line FROM symbol_supers ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?;
        for row in rows {
            let (sub, name, target, confidence, file, line) = row?;
            out.supers.push(SuperType {
                sub,
                name,
                target,
                confidence: confidence.map(|c| c.parse()).transpose()?,
                file,
                line,
            });
        }
    }
    out.indexed = conn
        .query_row(
            "SELECT 1 FROM meta WHERE key = ?1",
            [SYMBOLS_INDEXED],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if has_table(conn, "scip_files")? {
        let mut stmt = conn.prepare("SELECT path FROM scip_files")?;
        out.scip.files = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = conn.prepare("SELECT id, line FROM scip_symbols")?;
        out.scip.added = stmt
            .query_map([], |r| {
                Ok(crate::scip::ScipSymbol {
                    id: r.get(0)?,
                    line: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    let stats: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [SCIP_STATS], |r| {
            r.get(0)
        })
        .optional()?;
    out.scip.stats = stats.map(|v| serde_json::from_str(&v)).transpose()?;
    if has_table(conn, "bridges")? {
        read_bridges(conn, &mut out)?;
    }
    if has_table(conn, "key_sites")? {
        read_keys(conn, &mut out)?;
        out.keys_indexed = conn
            .query_row("SELECT 1 FROM meta WHERE key = ?1", [KEYS_INDEXED], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
    }
    if has_table(conn, "spring_beans")? {
        read_spring(conn, &mut out)?;
    }
    out.normalize();
    Ok(out)
}

/// Spring beans, injection points and data sinks (schema v14).
fn insert_spring(tx: &Transaction<'_>, s: &Symbols) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO spring_beans
         (ty, name, stereotype, file, line, factory, is_primary, qualifiers, aliases)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for b in &s.beans {
        stmt.execute(params![
            b.ty,
            b.name,
            b.stereotype,
            b.file,
            b.line,
            b.factory,
            b.primary,
            serde_json::to_string(&b.qualifiers)?,
            serde_json::to_string(&b.aliases)?
        ])?;
    }
    let mut stmt = tx.prepare(
        "INSERT INTO spring_injections
         (owner, how, member, wrapper, ty, qualifier, file, line, uses, resource)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for i in &s.injections {
        stmt.execute(params![
            i.owner,
            i.how,
            i.member,
            i.wrapper,
            i.ty,
            i.qualifier,
            i.file,
            i.line,
            serde_json::to_string(&i.uses)?,
            i.resource.as_ref().map(serde_json::to_string).transpose()?
        ])?;
    }
    let mut stmt = tx.prepare(
        "INSERT INTO data_sinks (store, name, entity, symbol, via, file, line, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for d in &s.sinks {
        stmt.execute(params![
            d.store,
            d.name,
            d.entity,
            d.symbol,
            d.via,
            d.file,
            d.line,
            d.confidence.as_str()
        ])?;
    }
    Ok(())
}

fn read_spring(conn: &Connection, out: &mut Symbols) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT ty, name, stereotype, file, line, factory, is_primary, qualifiers, aliases
         FROM spring_beans",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ),
            (
                r.get::<_, u32>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ),
        ))
    })?;
    for row in rows {
        let ((ty, name, stereotype, file), (line, factory, primary, qualifiers, aliases)) = row?;
        out.beans.push(crate::spring::SpringBean {
            ty,
            name,
            stereotype,
            file,
            line,
            factory,
            primary,
            qualifiers: serde_json::from_str(&qualifiers)?,
            aliases: serde_json::from_str(&aliases)?,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT owner, how, member, wrapper, ty, qualifier, file, line, uses, resource
         FROM spring_injections",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            crate::spring::SpringInjection {
                owner: r.get(0)?,
                how: r.get(1)?,
                member: r.get(2)?,
                wrapper: r.get(3)?,
                ty: r.get(4)?,
                qualifier: r.get(5)?,
                file: r.get(6)?,
                line: r.get(7)?,
                uses: Vec::new(),
                resource: None,
            },
            r.get::<_, String>(8)?,
            r.get::<_, Option<String>>(9)?,
        ))
    })?;
    for row in rows {
        let (mut i, uses, resource) = row?;
        i.resource = resource.map(|r| serde_json::from_str(&r)).transpose()?;
        i.uses = serde_json::from_str(&uses)?;
        out.injections.push(i);
    }
    let mut stmt = conn.prepare(
        "SELECT store, name, entity, symbol, via, file, line, confidence FROM data_sinks",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ),
            (
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, u32>(6)?,
                r.get::<_, String>(7)?,
            ),
        ))
    })?;
    for row in rows {
        let ((store, name, entity, symbol), (via, file, line, confidence)) = row?;
        out.sinks.push(crate::spring::DataSink {
            store,
            name,
            entity,
            symbol,
            via,
            file,
            line,
            confidence: confidence.parse()?,
        });
    }
    Ok(())
}

fn read_keys(conn: &Connection, out: &mut Symbols) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT key, role, form, name, container, container_file, container_line, file, line,
                symbol, target, confidence
         FROM key_sites",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ),
            (
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, u32>(6)?,
            ),
            (
                r.get::<_, String>(7)?,
                r.get::<_, u32>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, String>(11)?,
            ),
        ))
    })?;
    for row in rows {
        let (
            (key, role, form, name),
            (container, container_file, container_line),
            (file, line, symbol, target, confidence),
        ) = row?;
        out.keys.push(crate::key::KeySite {
            key,
            role: role.parse()?,
            form,
            name,
            container,
            container_file,
            container_line,
            file,
            line,
            symbol,
            target,
            confidence: confidence.parse()?,
        });
    }
    Ok(())
}

fn read_bridges(conn: &Connection, out: &mut Symbols) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT kind, role, key, label, file, line, symbol, confidence, detail
         FROM bridge_endpoints",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, u32>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, Option<String>>(8)?,
        ))
    })?;
    for row in rows {
        let (kind, role, key, label, file, line, symbol, confidence, detail) = row?;
        out.endpoints.push(crate::bridge::BridgeEndpoint {
            kind: kind.parse()?,
            role: role.parse()?,
            key,
            label,
            file,
            line,
            symbol,
            confidence: confidence.parse()?,
            detail,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT kind, subtype, src_file, src_line, src, dst_file, dst_line, dst, key, confidence
         FROM bridges",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u32>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, u32>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
        ))
    })?;
    for row in rows {
        let (kind, subtype, src_file, src_line, src, dst_file, dst_line, dst, key, confidence) =
            row?;
        out.bridges.push(crate::bridge::Bridge {
            kind: kind.parse()?,
            subtype,
            src_file,
            src_line,
            src,
            dst_file,
            dst_line,
            dst,
            key,
            confidence: confidence.parse()?,
        });
    }
    Ok(())
}

fn insert_graph(tx: &Transaction<'_>, graph: &Graph) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO nodes (id, kind, path, name, lang, loc, bytes, hash, meta)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for n in &graph.nodes {
        stmt.execute(params![
            n.id,
            n.kind.as_str(),
            n.path,
            n.name,
            n.lang,
            n.loc as i64,
            n.bytes as i64,
            n.hash,
            serde_json::to_string(&n.meta)?,
        ])?;
    }
    // Duplicate edges are merged (weights added) rather than ignored, so nothing is lost.
    let mut edges = graph.edges.clone();
    merge_edges(&mut edges);
    let mut stmt = tx.prepare(
        "INSERT INTO edges (src, dst, kind, confidence, weight) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for e in &edges {
        stmt.execute(params![
            e.src,
            e.dst,
            e.kind.as_str(),
            e.confidence.as_str(),
            e.weight as i64
        ])?;
    }
    let mut stmt = tx.prepare(
        "INSERT INTO import_sites (file, line, specifier, target) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for s in &graph.imports {
        stmt.execute(params![s.file, s.line, s.specifier, s.target])?;
    }
    let mut stmt = tx.prepare(
        "INSERT INTO unresolved (file, line, specifier, reason, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for u in &graph.unresolved {
        stmt.execute(params![
            u.file,
            u.line,
            u.specifier,
            u.reason.as_str(),
            u.detail
        ])?;
    }
    let mut stmt = tx.prepare("INSERT INTO diagnostics (path, detail) VALUES (?1, ?2)")?;
    for d in &graph.diagnostics {
        stmt.execute(params![d.path, d.detail])?;
    }
    let mut stmt = tx.prepare("INSERT INTO skipped (path, reason, detail) VALUES (?1, ?2, ?3)")?;
    let mut skipped = graph.skipped.clone();
    merge_skipped(&mut skipped);
    for s in &skipped {
        stmt.execute(params![s.path, s.reason.as_str(), s.detail])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn sample() -> Graph {
        let mut g = Graph {
            nodes: vec![
                Node {
                    id: node_id(NodeKind::Repo, ""),
                    kind: NodeKind::Repo,
                    path: String::new(),
                    name: "demo".into(),
                    lang: None,
                    loc: 3,
                    bytes: 20,
                    hash: None,
                    meta: serde_json::Value::Null,
                },
                Node {
                    id: node_id(NodeKind::File, "src/módulo/a b.py"),
                    kind: NodeKind::File,
                    path: "src/módulo/a b.py".into(),
                    name: "a b.py".into(),
                    lang: Some("python".into()),
                    loc: 3,
                    bytes: 20,
                    hash: Some("abc".into()),
                    meta: serde_json::json!({"x": 1}),
                },
            ],
            edges: vec![
                Edge::new(
                    "repo:",
                    "file:src/módulo/a b.py",
                    EdgeKind::Contains,
                    Confidence::Exact,
                ),
                Edge {
                    weight: 3,
                    ..Edge::new(
                        "file:src/módulo/a b.py",
                        "external:pypi:requests",
                        EdgeKind::ImportExternal,
                        Confidence::Resolved,
                    )
                },
            ],
            skipped: vec![SkippedFile {
                path: "img.png".into(),
                reason: SkipReason::Binary,
                detail: None,
            }],
            imports: vec![ImportSite {
                file: "src/módulo/a b.py".into(),
                line: 1,
                specifier: "requests".into(),
                target: "external:pypi:requests".into(),
            }],
            // Two identical rows: both must survive the round trip.
            unresolved: vec![
                UnresolvedImport {
                    file: "src/módulo/a b.py".into(),
                    line: 2,
                    specifier: "importlib.import_module(name)".into(),
                    reason: UnresolvedReason::Dynamic,
                    detail: None,
                },
                UnresolvedImport {
                    file: "src/módulo/a b.py".into(),
                    line: 2,
                    specifier: "importlib.import_module(name)".into(),
                    reason: UnresolvedReason::Dynamic,
                    detail: None,
                },
            ],
            diagnostics: vec![Diagnostic {
                path: "tsconfig.json".into(),
                detail: "tsconfig extends \"x\" not found".into(),
            }],
        };
        g.normalize();
        g
    }

    #[test]
    fn graph_round_trips() {
        let mut store = Store::open_in_memory().unwrap();
        let g = sample();
        store.replace_graph(&g).unwrap();
        assert_eq!(store.load_graph().unwrap(), g);
        // Replacing is idempotent.
        store.replace_graph(&g).unwrap();
        assert_eq!(store.load_graph().unwrap(), g);
    }

    #[test]
    fn migrates_from_v1_to_latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(1).unwrap();
            assert_eq!(store.schema_version().unwrap(), 1);
            assert!(
                store.get_meta("x").is_err(),
                "meta table must not exist in v1"
            );
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        store.set_meta("x", "1").unwrap();
        store.set_meta("x", "2").unwrap();
        assert_eq!(store.get_meta("x").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn migrates_a_populated_v3_db_to_v4() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(3).unwrap();
            store
                .conn
                .execute_batch(
                    "INSERT INTO nodes (id, kind, path, name) VALUES ('repo:', 'repo', '', 'r');
                     INSERT INTO nodes (id, kind, path, name) VALUES ('file:a', 'file', 'a', 'a');
                     INSERT INTO edges (src, dst, kind, confidence)
                        VALUES ('repo:', 'file:a', 'contains', 'exact');
                     INSERT INTO skipped (path, reason) VALUES ('b.bin', 'binary');
                     INSERT INTO meta (key, value) VALUES ('repo_name', 'r');",
                )
                .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let g = store.load_graph().unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(
            g.edges,
            vec![Edge::new(
                "repo:",
                "file:a",
                EdgeKind::Contains,
                Confidence::Exact
            )]
        );
        assert_eq!(g.skipped.len(), 1);
        assert!(g.imports.is_empty() && g.unresolved.is_empty());
        assert_eq!(store.get_meta("repo_name").unwrap().as_deref(), Some("r"));
        // The migrated database accepts a full v4 graph.
        store.replace_graph(&sample()).unwrap();
        assert_eq!(store.load_graph().unwrap(), sample());
    }

    #[test]
    fn migrates_a_populated_v5_db_to_v6_with_an_empty_extract_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(5).unwrap();
            store.replace_graph(&sample()).unwrap();
            assert!(
                store.load_extract_cache("x").is_err(),
                "no cache table in v5"
            );
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load_graph().unwrap(), sample());
        assert!(store.load_extract_cache("v1").unwrap().is_empty());

        let entry = |h: &str, l: &str| CacheEntry {
            hash: h.into(),
            lang: l.into(),
            imports: format!("[{h}]"),
        };
        let tx = store.begin_replace().unwrap();
        tx.put_extract_cache("v1", &[entry("a", "typescript"), entry("b", "python")])
            .unwrap();
        tx.put_extract_cache("v0", &[entry("c", "typescript")])
            .unwrap();
        tx.commit().unwrap();
        // Replacing the graph keeps the cache.
        store.replace_graph(&sample()).unwrap();
        assert_eq!(store.load_extract_cache("v1").unwrap().len(), 2);
        assert_eq!(store.load_extract_cache("v0").unwrap().len(), 1);

        let tx = store.begin_replace().unwrap();
        tx.prune_extract_cache("v1", &[("b".into(), "python".into())])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            store.load_extract_cache("v1").unwrap(),
            vec![entry("a", "typescript")]
        );
        assert!(store.load_extract_cache("v0").unwrap().is_empty());
    }

    #[test]
    fn readers_proceed_while_a_writer_holds_a_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut writer = Store::open(&path).unwrap();
        assert_eq!(writer.journal_mode().unwrap(), "wal");
        writer.replace_graph(&sample()).unwrap();

        let r1 = Store::open(&path).unwrap();
        let r2 = Store::open(&path).unwrap();

        // Open a write transaction and leave it uncommitted while both readers read.
        let tx = writer.conn.transaction().unwrap();
        tx.execute("DELETE FROM nodes", []).unwrap();
        let g1 = r1.load_graph().unwrap();
        let g2 = r2.load_graph().unwrap();
        assert_eq!(g1, sample(), "reader must see the last committed snapshot");
        assert_eq!(g2, sample());
        tx.commit().unwrap();

        // Concurrent readers on other threads during a second write.
        let p = path.clone();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let s = Store::open(&p).unwrap();
                    for _ in 0..50 {
                        s.load_graph().unwrap();
                    }
                })
            })
            .collect();
        for _ in 0..20 {
            writer.replace_graph(&sample()).unwrap();
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(r1.load_graph().unwrap(), sample());
    }

    /// A graph distinct from `sample()` in nodes, edges and skipped entries.
    fn other() -> Graph {
        let mut g = sample();
        for n in &mut g.nodes {
            n.loc += 100;
            n.name.push_str("-b");
        }
        g.nodes.push(Node {
            id: node_id(NodeKind::Dir, "src"),
            kind: NodeKind::Dir,
            path: "src".into(),
            name: "src".into(),
            lang: None,
            loc: 0,
            bytes: 0,
            hash: None,
            meta: serde_json::Value::Null,
        });
        g.edges.push(Edge::new(
            "repo:",
            "dir:src",
            EdgeKind::Contains,
            Confidence::Exact,
        ));
        g.unresolved.pop();
        g.skipped.push(SkippedFile {
            path: "big.bin".into(),
            reason: SkipReason::TooLarge,
            detail: Some("2000000 bytes".into()),
        });
        g.normalize();
        g
    }

    #[test]
    fn load_graph_reads_one_consistent_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut writer = Store::open(&path).unwrap();
        let (a, b) = (sample(), other());
        writer.replace_graph(&a).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (p, a, b, stop) = (path.clone(), a.clone(), b.clone(), stop.clone());
            std::thread::spawn(move || {
                let s = Store::open(&p).unwrap();
                let mut reads = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) || reads < 200 {
                    let g = s.load_graph().unwrap();
                    assert!(g == a || g == b, "mixed snapshot: {g:?}");
                    reads += 1;
                }
            })
        };
        for i in 0..400 {
            writer
                .replace_graph(if i % 2 == 0 { &b } else { &a })
                .unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
    }

    #[test]
    fn snapshot_pairs_graph_with_its_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut writer = Store::open(&path).unwrap();
        let (a, b) = (sample(), other());
        let write = |w: &mut Store, g: &Graph, tag: &str| {
            let tx = w.begin_replace().unwrap();
            tx.write_graph(g).unwrap();
            tx.set_meta("tag", tag).unwrap();
            tx.commit().unwrap();
        };
        write(&mut writer, &a, "a");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (p, a, stop) = (path.clone(), a.clone(), stop.clone());
            std::thread::spawn(move || {
                let s = Store::open(&p).unwrap();
                let mut reads = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) || reads < 200 {
                    let (g, meta) = s.load_snapshot().unwrap();
                    let tag = meta.get("tag").map(String::as_str);
                    assert_eq!(tag == Some("a"), g == a, "graph and meta disagree");
                    reads += 1;
                }
            })
        };
        for i in 0..300 {
            if i % 2 == 0 {
                write(&mut writer, &b, "b");
            } else {
                write(&mut writer, &a, "a");
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
    }

    #[test]
    fn duplicate_skipped_paths_are_kept_not_fatal() {
        let mut store = Store::open_in_memory().unwrap();
        let mut g = sample();
        g.skipped = vec![
            SkippedFile {
                path: String::new(),
                reason: SkipReason::WalkError,
                detail: Some("first".into()),
            },
            SkippedFile {
                path: String::new(),
                reason: SkipReason::WalkError,
                detail: Some("second".into()),
            },
            SkippedFile {
                path: "x.py".into(),
                reason: SkipReason::ReadError,
                detail: None,
            },
            SkippedFile {
                path: "x.py".into(),
                reason: SkipReason::ParseError,
                detail: None,
            },
        ];
        store.replace_graph(&g).unwrap();
        let back = store.load_graph().unwrap();
        let got: Vec<(&str, SkipReason, Option<&str>)> = back
            .skipped
            .iter()
            .map(|s| (s.path.as_str(), s.reason, s.detail.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("", SkipReason::WalkError, Some("first; second")),
                ("x.py", SkipReason::ReadError, None),
                ("x.py", SkipReason::ParseError, None),
            ]
        );
    }

    fn open_concurrently(path: &Path, threads: usize) {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (p, barrier) = (path.to_path_buf(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let s = Store::open(&p).map_err(|e| e.to_string())?;
                    s.schema_version().map_err(|e| e.to_string())
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), Ok(SCHEMA_VERSION));
        }
    }

    #[test]
    fn concurrent_open_of_a_fresh_db_migrates_once() {
        for _ in 0..5 {
            let dir = tempfile::tempdir().unwrap();
            open_concurrently(&dir.path().join("graph.db"), 8);
        }
    }

    #[test]
    fn concurrent_open_of_an_old_db_upgrades_once() {
        for _ in 0..5 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("graph.db");
            {
                let conn = Connection::open(&path).unwrap();
                conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
                    .unwrap();
                let mut store = Store { conn };
                store.migrate_to(1).unwrap();
            }
            open_concurrently(&path, 8);
        }
    }

    fn sample_symbols() -> Symbols {
        let sym = |name: &str, container: Option<&str>, kind, line| Symbol {
            file: "src/a.ts".into(),
            name: name.into(),
            container: container.map(str::to_string),
            kind,
            line,
            end_line: line + 2,
            exported: container.is_none(),
        };
        let mut s = Symbols {
            symbols: vec![
                sym("A", None, SymbolKind::Class, 1),
                sym("run", Some("A"), SymbolKind::Method, 2),
                // An overload: same id, two rows, both kept.
                sym("run", Some("A"), SymbolKind::Method, 5),
                sym("f", None, SymbolKind::Function, 9),
            ],
            calls: vec![
                SymbolCall {
                    file: "src/b.ts".into(),
                    line: 4,
                    caller: None,
                    target: "sym:src/a.ts#f".into(),
                    confidence: Confidence::Resolved,
                },
                SymbolCall {
                    file: "src/b.ts".into(),
                    line: 4,
                    caller: None,
                    target: "sym:src/a.ts#f".into(),
                    confidence: Confidence::Resolved,
                },
                SymbolCall {
                    file: "src/a.ts".into(),
                    line: 10,
                    caller: Some("sym:src/a.ts#f".into()),
                    target: "sym:src/a.ts#A.run".into(),
                    confidence: Confidence::Heuristic,
                },
            ],
            stats: vec![CallStats {
                file: "src/b.ts".into(),
                resolved: 2,
                unresolved: 7,
                ..CallStats::default()
            }],
            entries: vec![EntryPoint {
                file: "src/a.ts".into(),
                line: 12,
                kind: EntryKind::Http,
                name: "GET /a".into(),
                target: Some("sym:src/a.ts#f".into()),
            }],
            supers: vec![
                SuperType {
                    sub: "sym:src/a.ts#A".into(),
                    name: "Base".into(),
                    target: Some("sym:src/base.ts#Base".into()),
                    confidence: Some(Confidence::Resolved),
                    file: "src/a.ts".into(),
                    line: 1,
                },
                SuperType {
                    sub: "sym:src/a.ts#A".into(),
                    name: "Serializable".into(),
                    target: None,
                    confidence: None,
                    file: "src/a.ts".into(),
                    line: 1,
                },
            ],
            indexed: true,
            scip: Default::default(),
            endpoints: Vec::new(),
            bridges: Vec::new(),
            keys: Vec::new(),
            keys_indexed: false,
            beans: Vec::new(),
            injections: Vec::new(),
            sinks: Vec::new(),
        };
        s.normalize();
        s
    }

    /// [`sample_symbols`] with key sites (schema v13).
    fn sample_keyed() -> Symbols {
        let mut s = Symbols {
            keys: vec![
                crate::KeySite {
                    key: "READ-FILE".into(),
                    role: crate::KeyRole::Define,
                    form: "enum".into(),
                    name: Some("Action.READ_FILE".into()),
                    container: "Action".into(),
                    container_file: "src/Action.java".into(),
                    container_line: 3,
                    file: "src/Action.java".into(),
                    line: 4,
                    symbol: None,
                    target: None,
                    confidence: Confidence::Exact,
                },
                crate::KeySite {
                    key: "READ-FILE".into(),
                    role: crate::KeyRole::Provide,
                    form: "case".into(),
                    name: None,
                    container: "switch in Runner.run".into(),
                    container_file: "src/Runner.java".into(),
                    container_line: 7,
                    file: "src/Runner.java".into(),
                    line: 8,
                    symbol: Some("sym:src/Runner.java#Runner.run".into()),
                    target: Some("sym:src/Files.java#Files.read".into()),
                    confidence: Confidence::Resolved,
                },
            ],
            keys_indexed: true,
            ..sample_symbols()
        };
        s.normalize();
        s
    }

    #[test]
    fn migrates_a_v12_db_to_v13_with_no_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(12).unwrap();
            let tx = store.begin_replace().unwrap();
            tx.write_graph(&sample()).unwrap();
            tx.write_symbols(&sample_keyed()).unwrap();
            tx.commit().unwrap();
            // Written before the table existed: no keys kept, and none read as indexed.
            assert_eq!(store.load_symbols().unwrap(), sample_symbols());
            assert!(!store.load_symbols().unwrap().keys_indexed);
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let tx = store.begin_replace().unwrap();
        tx.write_symbols(&sample_keyed()).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_symbols().unwrap(), sample_keyed());
        store.replace_graph(&other()).unwrap();
        assert_eq!(store.load_symbols().unwrap(), Symbols::default());
    }

    /// [`sample_symbols`] with Spring beans, injection points and data sinks (schema v14).
    fn sample_spring() -> Symbols {
        let mut s = Symbols {
            beans: vec![crate::SpringBean {
                ty: "sym:src/CardPayments.java#CardPayments".into(),
                name: "cardPayments".into(),
                aliases: vec!["card".into()],
                stereotype: "Service".into(),
                file: "src/CardPayments.java".into(),
                line: 3,
                factory: None,
                primary: true,
                qualifiers: vec!["card".into()],
            }],
            injections: vec![crate::SpringInjection {
                owner: "sym:src/Checkout.java#Checkout".into(),
                how: "constructor".into(),
                member: "payments".into(),
                wrapper: "List".into(),
                ty: "sym:src/Payments.java#Payments".into(),
                qualifier: None,
                resource: Some(crate::ResourceName {
                    name: "payments".into(),
                    default: true,
                }),
                file: "src/Checkout.java".into(),
                line: 9,
                uses: vec![crate::InjectedUse {
                    line: 12,
                    each: true,
                }],
            }],
            sinks: vec![crate::DataSink {
                store: "mongo".into(),
                name: "orders".into(),
                entity: Some("sym:src/Order.java#Order".into()),
                symbol: Some("sym:src/Orders.java#Orders.save".into()),
                via: "getCollection".into(),
                file: "src/Orders.java".into(),
                line: 12,
                confidence: Confidence::Resolved,
            }],
            ..sample_symbols()
        };
        s.normalize();
        s
    }

    #[test]
    fn migrates_a_v13_db_to_v14_with_no_beans() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(13).unwrap();
            let tx = store.begin_replace().unwrap();
            tx.write_graph(&sample()).unwrap();
            tx.write_symbols(&sample_spring()).unwrap();
            tx.commit().unwrap();
            // Written before the tables existed: nothing of them kept.
            assert_eq!(store.load_symbols().unwrap(), sample_symbols());
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let tx = store.begin_replace().unwrap();
        tx.write_symbols(&sample_spring()).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_symbols().unwrap(), sample_spring());
        store.replace_graph(&other()).unwrap();
        assert_eq!(store.load_symbols().unwrap(), Symbols::default());
    }

    /// [`sample_symbols`] with bridges (schema v12).
    fn sample_bridged() -> Symbols {
        let mut s = Symbols {
            endpoints: vec![
                crate::BridgeEndpoint {
                    kind: crate::BridgeKind::Ipc,
                    role: crate::BridgeRole::Provide,
                    key: "users:get".into(),
                    label: "ipcMain.handle".into(),
                    file: "src/main.ts".into(),
                    line: 3,
                    symbol: Some("sym:src/main.ts#getUser".into()),
                    confidence: Confidence::Resolved,
                    detail: None,
                },
                crate::BridgeEndpoint {
                    kind: crate::BridgeKind::Ipc,
                    role: crate::BridgeRole::Consume,
                    key: String::new(),
                    label: "invoke".into(),
                    file: "src/preload.ts".into(),
                    line: 9,
                    symbol: None,
                    confidence: Confidence::Heuristic,
                    detail: Some("dynamic".into()),
                },
            ],
            bridges: vec![crate::Bridge {
                kind: crate::BridgeKind::Ipc,
                subtype: "invoke".into(),
                src_file: "src/preload.ts".into(),
                src_line: 4,
                src: Some("sym:src/preload.ts#api.getUser".into()),
                dst_file: "src/main.ts".into(),
                dst_line: 3,
                dst: None,
                key: "users:get".into(),
                confidence: Confidence::Exact,
            }],
            ..sample_symbols()
        };
        s.normalize();
        s
    }

    #[test]
    fn migrates_a_v11_db_to_v12_with_no_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(11).unwrap();
            let tx = store.begin_replace().unwrap();
            tx.write_graph(&sample()).unwrap();
            tx.write_symbols(&sample_symbols()).unwrap();
            tx.commit().unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load_symbols().unwrap(), sample_symbols());
        let tx = store.begin_replace().unwrap();
        tx.write_symbols(&sample_bridged()).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_symbols().unwrap(), sample_bridged());
        // A new graph clears them with the rest of the layer.
        store.replace_graph(&other()).unwrap();
        assert_eq!(store.load_symbols().unwrap(), Symbols::default());
    }

    #[test]
    fn an_empty_symbol_layer_stays_marked_indexed() {
        let mut store = Store::open_in_memory().unwrap();
        let tx = store.begin_replace().unwrap();
        tx.write_graph(&sample()).unwrap();
        let empty = Symbols {
            indexed: true,
            ..Symbols::default()
        };
        tx.write_symbols(&empty).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_symbols().unwrap(), empty);
        // A graph written alone has no symbol layer.
        store.replace_graph(&other()).unwrap();
        assert!(!store.load_symbols().unwrap().indexed);
        // Nor does a layer written by a caller that did not compute one.
        let tx = store.begin_replace().unwrap();
        tx.write_symbols(&Symbols::default()).unwrap();
        tx.commit().unwrap();
        assert!(!store.load_symbols().unwrap().indexed);
    }

    #[test]
    fn symbols_round_trip_and_a_new_graph_clears_them() {
        let mut store = Store::open_in_memory().unwrap();
        let tx = store.begin_replace().unwrap();
        tx.write_graph(&sample()).unwrap();
        tx.write_symbols(&sample_symbols()).unwrap();
        tx.commit().unwrap();
        let (g, s) = store.load_graph_and_symbols().unwrap();
        assert_eq!(g, sample());
        assert_eq!(s, sample_symbols());
        assert_eq!(s.symbols.len(), 4, "duplicate ids are kept");
        assert_eq!(s.calls.len(), 3, "two calls on one line are two rows");
        // A graph written without symbols leaves none behind.
        store.replace_graph(&other()).unwrap();
        assert_eq!(store.load_symbols().unwrap(), Symbols::default());
    }

    #[test]
    fn migrates_a_populated_v6_db_to_v7_with_empty_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(6).unwrap();
            store.replace_graph(&sample()).unwrap();
            assert!(store.load_symbols().is_err(), "no symbol tables in v6");
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load_graph().unwrap(), sample());
        assert_eq!(store.load_symbols().unwrap(), Symbols::default());
        let tx = store.begin_replace().unwrap();
        tx.write_symbols(&sample_symbols()).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_symbols().unwrap(), sample_symbols());
    }

    fn sample_git() -> (GitRaw, History) {
        use crate::history::*;
        let raw = GitRaw {
            commits: vec![
                GitCommit {
                    seq: 0,
                    id: "a".repeat(40),
                    time: 100,
                    author: 0,
                    files: 2,
                    merge: false,
                    member: String::new(),
                },
                GitCommit {
                    seq: 1,
                    id: "b".repeat(40),
                    time: 200,
                    author: 1,
                    files: 0,
                    merge: true,
                    member: String::new(),
                },
            ],
            authors: vec![
                GitAuthor {
                    id: 0,
                    name: "Ada".into(),
                    email_hash: "h0".into(),
                },
                GitAuthor {
                    id: 1,
                    name: "Grace".into(),
                    email_hash: "h1".into(),
                },
            ],
            changes: vec![
                GitChange {
                    seq: 0,
                    path: "src/b.ts".into(),
                    old_path: Some("src/a.ts".into()),
                    kind: ChangeKind::Renamed,
                    added: Some(3),
                    removed: Some(1),
                },
                GitChange {
                    seq: 0,
                    path: "img.png".into(),
                    old_path: None,
                    kind: ChangeKind::Added,
                    added: None,
                    removed: None,
                },
            ],
        };
        let mut history = History {
            status: Some(GitStatus {
                state: GitState::Ok,
                head: Some("b".repeat(40)),
                commits: 2,
                merges: 1,
                ..GitStatus::default()
            }),
            files: vec![FileHistory {
                path: "src/b.ts".into(),
                commits: 1,
                added: 3,
                removed: 1,
                recent: 1,
                first_time: 100,
                last_time: 100,
                co_commits: 1,
                complexity: 7,
                authors: vec![AuthorShare {
                    name: "Ada".into(),
                    commits: 1,
                }],
            }],
            pairs: vec![CoChange {
                a: "img.png".into(),
                b: "src/b.ts".into(),
                together: 1,
                last_time: 100,
            }],
        };
        history.normalize();
        (raw, history)
    }

    #[test]
    fn git_layer_round_trips_and_survives_a_new_graph() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.load_history().unwrap(), History::default());
        let (raw, history) = sample_git();
        let tx = store.begin_replace().unwrap();
        tx.append_git_raw(&raw).unwrap();
        tx.write_history(&history).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_git_raw().unwrap(), raw);
        assert_eq!(store.load_history().unwrap(), history);
        // A new graph leaves the git layer alone.
        store.replace_graph(&sample()).unwrap();
        assert_eq!(store.load_history().unwrap(), history);
        // Clearing removes rows and aggregates (the status stays until rewritten).
        let tx = store.begin_replace().unwrap();
        tx.clear_git().unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_git_raw().unwrap(), GitRaw::default());
        let h = store.load_history().unwrap();
        assert!(h.files.is_empty() && h.pairs.is_empty());
    }

    #[test]
    fn migrates_a_populated_v7_db_to_v8_with_an_empty_git_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(7).unwrap();
            store.replace_graph(&sample()).unwrap();
            assert!(store.load_git_raw().is_err(), "no git tables in v7");
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load_graph().unwrap(), sample());
        assert_eq!(store.load_history().unwrap(), History::default());
        assert_eq!(store.load_git_raw().unwrap(), GitRaw::default());
    }

    fn sample_scip() -> Symbols {
        let mut s = sample_symbols();
        s.scip = crate::ScipCoverage {
            files: vec!["src/b.ts".into(), "src/a.ts".into()],
            added: vec![crate::ScipSymbol {
                id: "sym:src/a.ts#g".into(),
                line: 20,
            }],
            stats: Some(crate::ScipStats {
                files_covered: 2,
                calls_upgraded: 1,
                indexes: vec![crate::ScipIndexStats {
                    path: "index.scip".into(),
                    tool: "scip-typescript 0.3.0".into(),
                    documents: 3,
                    matched: 2,
                    unmatched: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        };
        s.normalize();
        s
    }

    #[test]
    fn scip_coverage_round_trips_and_a_new_graph_clears_it() {
        let mut store = Store::open_in_memory().unwrap();
        let tx = store.begin_replace().unwrap();
        tx.write_graph(&sample()).unwrap();
        tx.write_symbols(&sample_scip()).unwrap();
        tx.commit().unwrap();
        let s = store.load_symbols().unwrap();
        assert_eq!(s, sample_scip());
        assert_eq!(s.scip.files, ["src/a.ts", "src/b.ts"]);
        // A layer without SCIP clears the coverage and its counts.
        let tx = store.begin_replace().unwrap();
        tx.write_graph(&sample()).unwrap();
        tx.write_symbols(&sample_symbols()).unwrap();
        tx.commit().unwrap();
        assert!(store.load_symbols().unwrap().scip.is_empty());
    }

    #[test]
    fn the_scip_cache_survives_a_new_graph_and_keeps_only_current_files() {
        let mut store = Store::open_in_memory().unwrap();
        let row = |p: &str| ScipCacheRow {
            path: p.into(),
            size: 10,
            mtime: 42,
            hash: "scip1:abc".into(),
            data: "{}".into(),
        };
        let tx = store.begin_replace().unwrap();
        tx.put_scip_cache(&row("/r/index.scip")).unwrap();
        tx.put_scip_cache(&row("/r/old.scip")).unwrap();
        tx.commit().unwrap();
        store.replace_graph(&sample()).unwrap();
        assert_eq!(
            store.load_scip_cache("/r/index.scip").unwrap(),
            Some(row("/r/index.scip"))
        );
        let tx = store.begin_replace().unwrap();
        tx.retain_scip_cache(&["/r/index.scip".to_string()])
            .unwrap();
        tx.commit().unwrap();
        assert!(store.load_scip_cache("/r/old.scip").unwrap().is_none());
        assert!(store.load_scip_cache("/r/index.scip").unwrap().is_some());
    }

    #[test]
    fn migrates_a_populated_v9_db_to_v10_with_an_empty_scip_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(9).unwrap();
            let tx = store.begin_replace().unwrap();
            tx.write_graph(&sample()).unwrap();
            tx.write_symbols(&sample_symbols()).unwrap();
            tx.commit().unwrap();
            assert!(
                store.load_scip_cache("x").unwrap().is_none(),
                "no table yet"
            );
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load_graph().unwrap(), sample());
        assert_eq!(store.load_symbols().unwrap(), sample_symbols());
        assert!(store.load_scip_cache("x").unwrap().is_none());
    }

    #[test]
    fn migrates_a_v10_db_to_v11_with_commits_of_no_member() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(10).unwrap();
            store
                .connection()
                .execute_batch(
                    "INSERT INTO git_authors (id, name, email_hash) VALUES (0, 'Ada', 'h');
                     INSERT INTO git_commits (seq, id, time, author, files, merge)
                     VALUES (0, 'c0', 100, 0, 1, 0);",
                )
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let raw = store.load_git_raw().unwrap();
        assert_eq!(raw.commits.len(), 1);
        assert_eq!(raw.commits[0].member, "");
    }

    #[test]
    fn a_member_git_layer_is_rebuilt_without_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("graph.db")).unwrap();
        let commit = |seq: u32, member: &str| GitCommit {
            seq,
            id: format!("{seq:040}"),
            time: 100,
            author: seq,
            files: 1,
            merge: false,
            member: member.to_string(),
        };
        let change = |seq: u32, path: &str| GitChange {
            seq,
            path: path.to_string(),
            old_path: None,
            kind: crate::history::ChangeKind::Modified,
            added: Some(1),
            removed: Some(0),
        };
        let raw = GitRaw {
            commits: vec![commit(0, "api"), commit(1, "lib")],
            authors: vec![
                GitAuthor {
                    id: 0,
                    name: "Ada".into(),
                    email_hash: "a".into(),
                },
                GitAuthor {
                    id: 1,
                    name: "Grace".into(),
                    email_hash: "g".into(),
                },
            ],
            changes: vec![change(0, "api/a.ts"), change(1, "lib/b.java")],
        };
        let file = |path: &str| FileHistory {
            path: path.to_string(),
            commits: 1,
            added: 1,
            removed: 0,
            recent: 1,
            first_time: 100,
            last_time: 100,
            co_commits: 0,
            complexity: 0,
            authors: Vec::new(),
        };
        let history = |path: &str| History {
            status: Some(GitStatus::default()),
            files: vec![file(path)],
            pairs: Vec::new(),
        };
        let tx = store.begin_replace().unwrap();
        tx.append_git_raw(&raw).unwrap();
        tx.write_member_history("api", &history("api/a.ts"))
            .unwrap();
        tx.write_member_history("lib", &history("lib/b.java"))
            .unwrap();
        tx.commit().unwrap();
        let paths = |s: &Store| -> Vec<String> {
            s.load_history()
                .unwrap()
                .files
                .into_iter()
                .map(|f| f.path)
                .collect()
        };
        assert_eq!(paths(&store), vec!["api/a.ts", "lib/b.java"]);
        assert!(store.get_meta("git:api").unwrap().is_some());
        let tx = store.begin_replace().unwrap();
        tx.clear_git_member("api").unwrap();
        tx.commit().unwrap();
        let left = store.load_git_raw().unwrap();
        assert_eq!(left.commits, vec![commit(1, "lib")]);
        assert_eq!(left.changes, vec![change(1, "lib/b.java")]);
        assert_eq!(left.authors.len(), 1, "Ada authored only api's commit");
        assert_eq!(paths(&store), vec!["lib/b.java"]);
        let tx = store.begin_replace().unwrap();
        tx.retain_git_members(&["api"]).unwrap();
        tx.commit().unwrap();
        assert!(store.load_git_raw().unwrap().commits.is_empty());
        assert!(paths(&store).is_empty());
    }

    #[test]
    fn migrates_a_v8_db_with_symbols_to_v9_without_supertypes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut without = sample_symbols();
        without.supers.clear();
        {
            let conn = Connection::open(&path).unwrap();
            let mut store = Store { conn };
            store.migrate_to(8).unwrap();
            let tx = store.begin_replace().unwrap();
            tx.write_graph(&sample()).unwrap();
            tx.write_symbols(&without).unwrap();
            tx.commit().unwrap();
            assert_eq!(store.load_symbols().unwrap(), without);
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.load_symbols().unwrap(), without);
        let tx = store.begin_replace().unwrap();
        tx.write_symbols(&sample_symbols()).unwrap();
        tx.commit().unwrap();
        assert_eq!(store.load_symbols().unwrap(), sample_symbols());
        // A new graph clears the supertypes with the rest of the layer.
        store.replace_graph(&sample()).unwrap();
        assert!(store.load_symbols().unwrap().supers.is_empty());
    }

    #[test]
    fn rejects_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE schema_version SET version = 999", [])
            .unwrap();
        assert!(matches!(
            Store::open(&path),
            Err(Error::SchemaTooNew { found: 999, .. })
        ));
    }
}
