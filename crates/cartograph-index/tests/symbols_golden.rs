//! N2 golden tests: symbols, resolved calls (with confidence), call counts and entry points
//! of one fixture per language, plus the precision rules each fixture is built to check.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test symbols_golden`
//! and review the diff.

use cartograph_core::{Confidence, Symbols};
use cartograph_index::{IndexOptions, index, index_to_store};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), &dst).unwrap();
        }
    }
}

/// Index `tests/fixtures/<name>` from a temp copy.
fn symbols_of(name: &str) -> Symbols {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
    index(&root, &IndexOptions::default()).unwrap().symbols
}

/// A compact, reviewable rendering.
fn render(s: &Symbols) -> Value {
    let symbols: Vec<String> = s
        .symbols
        .iter()
        .map(|x| {
            format!(
                "{} {} L{}-{}{}",
                x.kind.as_str(),
                x.id(),
                x.line,
                x.end_line,
                if x.exported { " exported" } else { "" }
            )
        })
        .collect();
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| {
            format!(
                "{}:{} {} -> {} [{}]",
                c.file,
                c.line,
                c.caller.as_deref().unwrap_or("(top level)"),
                c.target,
                c.confidence
            )
        })
        .collect();
    let stats: Vec<String> = s
        .stats
        .iter()
        .map(|c| {
            format!(
                "{}: exact {}, resolved {}, heuristic {}, ambiguous {}, unresolved {}",
                c.file, c.exact, c.resolved, c.heuristic, c.ambiguous, c.unresolved
            )
        })
        .collect();
    let entries: Vec<String> = s
        .entries
        .iter()
        .map(|e| {
            format!(
                "{} {:?} {}:{} -> {}",
                e.kind.as_str(),
                e.name,
                e.file,
                e.line,
                e.target.as_deref().unwrap_or("?")
            )
        })
        .collect();
    let supers: Vec<String> = s
        .supers
        .iter()
        .map(|x| {
            format!(
                "{} : {} -> {} [{}] {}:{}",
                x.sub,
                x.name,
                x.target.as_deref().unwrap_or("?"),
                x.confidence.map_or("-".to_string(), |c| c.to_string()),
                x.file,
                x.line
            )
        })
        .collect();
    json!({
        "symbols": symbols,
        "supers": supers,
        "calls": calls,
        "stats": stats,
        "entries": entries
    })
}

fn golden(name: &str, s: &Symbols) {
    let actual = serde_json::to_string_pretty(&render(s)).unwrap() + "\n";
    let path = repo_root()
        .join("tests/golden")
        .join(format!("{name}.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "missing golden {}; run with UPDATE_GOLDEN=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "golden {name} differs; rerun with UPDATE_GOLDEN=1 and review the diff"
    );
}

/// The call from `file:line` to `target`, if resolved.
fn call(s: &Symbols, at: &str, target: &str) -> Option<Confidence> {
    let (file, line) = at.rsplit_once(':').unwrap();
    let line: u32 = line.parse().unwrap();
    s.calls
        .iter()
        .find(|c| c.file == file && c.line == line && c.target == target)
        .map(|c| c.confidence)
}

/// No call resolves on this line.
fn none_at(s: &Symbols, at: &str) -> bool {
    let (file, line) = at.rsplit_once(':').unwrap();
    let line: u32 = line.parse().unwrap();
    !s.calls.iter().any(|c| c.file == file && c.line == line)
}

const FIXTURES: [&str; 19] = [
    "symbols-ts",
    "symbols-py",
    "symbols-rust",
    "symbols-go",
    "symbols-java",
    "symbols-cs",
    "symbols-c",
    "symbols-cpp",
    "symbols-gd",
    "inherit-ts",
    "inherit-py",
    "inherit-rust",
    "inherit-go",
    "inherit-java",
    "inherit-cs",
    "inherit-cpp",
    "inherit-gd",
    "electron-app",
    "spring-java",
];

#[test]
fn goldens() {
    for name in FIXTURES {
        golden(name, &symbols_of(name));
    }
}

#[test]
fn object_literal_members() {
    let s = symbols_of("electron-app");
    let has = |id: &str| s.symbols.iter().any(|x| x.id() == id);
    let caller_at = |at: &str| {
        let (file, line) = at.rsplit_once(':').unwrap();
        let line: u32 = line.parse().unwrap();
        s.calls
            .iter()
            .find(|c| c.file == file && c.line == line)
            .and_then(|c| c.caller.clone())
    };
    // Functions of a named object are its members: a module-level const (with a nested
    // object and a quoted key), an object handed to `exposeInMainWorld`, `export default`,
    // `module.exports`.
    for id in [
        "sym:src/electron/preload.ts#deskApi.ping",
        "sym:src/electron/preload.ts#deskApi.save",
        "sym:src/electron/preload.ts#deskApi.updater.check",
        "sym:src/electron/preload.ts#deskApi.on-status",
        "sym:src/electron/preload.ts#tools.format",
        "sym:src/electron/preload.ts#tools.reload",
        "sym:src/electron/api.ts#api.start",
        "sym:src/electron/api.ts#default.run",
        "sym:src/electron/legacy.js#module.exports.ping",
    ] {
        assert!(has(id), "{id}");
    }
    // A local object's functions are not symbols.
    assert!(!s.symbols.iter().any(|x| x.name == "skip"));
    // Exposed or exported objects export their members; a private const does not.
    let exported = |id: &str| s.symbols.iter().any(|x| x.id() == id && x.exported);
    assert!(exported("sym:src/electron/preload.ts#deskApi.save"));
    assert!(exported("sym:src/electron/preload.ts#tools.reload"));
    assert!(!exported("sym:src/electron/api.ts#helpers.twice"));
    // Calls inside them belong to them.
    assert_eq!(
        caller_at("src/electron/preload.ts:14").as_deref(),
        Some("sym:src/electron/preload.ts#deskApi.updater.check")
    );
    assert_eq!(
        caller_at("src/electron/preload.ts:26").as_deref(),
        Some("sym:src/electron/preload.ts#tools.format")
    );
    // `obj.m()` reaches the member, in the file and through an import.
    use Confidence::*;
    assert_eq!(
        call(
            &s,
            "src/electron/api.ts:18",
            "sym:src/electron/api.ts#helpers.twice"
        ),
        Some(Exact)
    );
    assert_eq!(
        call(&s, "src/ui/app.ts:4", "sym:src/electron/api.ts#api.stop"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "src/ui/app.ts:5", "sym:src/electron/api.ts#default.run"),
        Some(Resolved)
    );
}

#[test]
fn electron_entries() {
    let s = symbols_of("electron-app");
    let entry = |kind: &str, name: &str| {
        s.entries
            .iter()
            .find(|e| e.kind.as_str() == kind && e.name == name)
            .map(|e| (e.file.clone(), e.target.clone().unwrap_or_default()))
    };
    // `package.json` `main` names the build output (`dist/electron/main.js`, not in the
    // repo): the entry is the source it is built from.
    assert_eq!(
        entry("main", "main"),
        Some((
            "src/electron/main.ts".into(),
            "file:src/electron/main.ts".into()
        ))
    );
    // `ipcMain` handlers: a named handler, an inline one at module level (the file) and in a
    // method (the method); the channel as a literal or as the constant written.
    assert_eq!(
        entry("ipc", "doc:save").map(|e| e.1),
        Some("sym:src/electron/handlers.ts#saveDoc".into())
    );
    assert_eq!(
        entry("ipc", "CHANNELS.PING").map(|e| e.1),
        Some("file:src/electron/main.ts".into())
    );
    assert_eq!(
        entry("ipc", "update:now").map(|e| e.1),
        Some("sym:src/electron/updater.ts#Updater.register".into())
    );
    assert_eq!(
        entry("ipc", "legacy:ping").map(|e| e.1),
        Some("sym:src/electron/legacy.js#onPing".into())
    );
    // The renderer: the module script `index.html` loads.
    assert_eq!(
        entry("main", "index.html"),
        Some(("src/ui/main.ts".into(), "file:src/ui/main.ts".into()))
    );
    // A channel held in a parameter (`ipcMain.handle(channel, fn)`) names nothing.
    assert!(!s.entries.iter().any(|e| e.name == "channel"));
}

#[test]
fn typescript_rules() {
    let s = symbols_of("symbols-ts");
    use Confidence::*;
    // Barrel `export *` and a re-export alias are followed.
    assert_eq!(
        call(&s, "src/app.ts:19", "sym:src/util.ts#slugify"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "src/app.ts:9", "sym:src/store.ts#Store"),
        Some(Resolved)
    );
    // Default import, namespace import.
    assert_eq!(
        call(&s, "src/app.ts:12", "sym:src/store.ts#Store"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "src/app.ts:14", "sym:src/util.ts#helper"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "src/app.ts:15", "sym:src/dup-a.ts#dup"),
        Some(Resolved)
    );
    // `obj.m()`: unique method in an imported file → heuristic; `save` is too common.
    assert_eq!(
        call(
            &s,
            "src/app.ts:10",
            "sym:src/store.ts#Store.compactSegments"
        ),
        Some(Heuristic)
    );
    assert!(
        none_at(&s, "src/app.ts:11"),
        "store.save() stays unresolved"
    );
    // Two re-exported `dup`: ambiguous, not guessed. A library import never matches.
    assert!(none_at(&s, "src/app.ts:16"));
    assert!(none_at(&s, "src/app.ts:17"));
    assert!(none_at(&s, "src/app.ts:18"));
    let app = s.stats.iter().find(|c| c.file == "src/app.ts").unwrap();
    assert_eq!(app.ambiguous, 1);
    // Same file: exact; `this.m()`: exact.
    assert_eq!(
        call(&s, "src/util.ts:4", "sym:src/util.ts#normalize"),
        Some(Exact)
    );
    assert_eq!(
        call(&s, "src/store.ts:3", "sym:src/store.ts#Store.flushPending"),
        Some(Exact)
    );
    // JSX renders a component; a classic script calls another script's global.
    assert_eq!(
        call(&s, "src/view.tsx:6", "sym:src/panel.tsx#Panel"),
        Some(Resolved)
    );
    assert_eq!(
        call(
            &s,
            "scripts/page.js:3",
            "sym:scripts/legacy.js#legacyHelper"
        ),
        Some(Heuristic)
    );
    // Routes and commands only where the framework is imported.
    let names: Vec<&str> = s.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"GET /users/:id"), "{names:?}");
    assert!(names.contains(&"build"), "{names:?}");
    assert!(!s.entries.iter().any(|e| e.file == "src/client.ts"));
}

#[test]
fn python_rules() {
    let s = symbols_of("symbols-py");
    use Confidence::*;
    // Through `pkg/__init__.py`'s re-export, a module alias, a star import.
    assert_eq!(
        call(&s, "app.py:19", "sym:pkg/core.py#compute"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "app.py:17", "sym:pkg/core.py#compute"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "star.py:5", "sym:pkg/core.py#compute"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "app.py:15", "sym:pkg/core.py#Engine.drain_queue"),
        Some(Heuristic)
    );
    assert!(none_at(&s, "app.py:16"), "engine.run() is too common");
    assert!(none_at(&s, "app.py:18"), "os.path.join is a library");
    assert_eq!(
        call(&s, "pkg/core.py:15", "sym:pkg/core.py#Engine.step_once"),
        Some(Exact)
    );
    let names: Vec<&str> = s.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"GET /items/<item_id>"), "{names:?}");
    assert!(names.contains(&"__main__"), "{names:?}");
    assert!(names.contains(&"hello"), "{names:?}");
}

#[test]
fn rust_rules() {
    let s = symbols_of("symbols-rust");
    use Confidence::*;
    assert_eq!(
        call(&s, "src/main.rs:15", "sym:src/model.rs#Store.new"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "src/model.rs:11", "sym:src/model.rs#Store.init_log"),
        Some(Exact)
    );
    assert_eq!(
        call(&s, "src/main.rs:18", "sym:src/util.rs#format_name"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "src/routes.rs:10", "sym:src/model.rs#shared"),
        Some(Resolved)
    );
    // `fmt` implements a trait: never matched by name.
    assert!(!s.calls.iter().any(|c| c.target.ends_with("#Store.fmt")));
    let names: Vec<&str> = s.entries.iter().map(|e| e.name.as_str()).collect();
    for n in ["main", "GET /health", "index", "serve-all"] {
        assert!(names.contains(&n), "{n}: {names:?}");
    }
}

#[test]
fn compiled_languages_rules() {
    use Confidence::*;
    let go = symbols_of("symbols-go");
    assert_eq!(
        call(&go, "main.go:11", "sym:store/store.go#New"),
        Some(Resolved)
    );
    assert_eq!(
        call(&go, "main.go:14", "sym:helpers.go#localHelper"),
        Some(Resolved)
    );
    assert_eq!(
        call(&go, "store/store.go:16", "sym:store/store.go#Store.write"),
        Some(Exact)
    );
    let java = symbols_of("symbols-java");
    let app = "src/main/java/com/acme/App.java";
    assert_eq!(
        call(
            &java,
            &format!("{app}:7"),
            "sym:src/main/java/com/acme/core/Repo.java#Repo"
        ),
        Some(Resolved)
    );
    assert_eq!(
        call(
            &java,
            &format!("{app}:9"),
            "sym:src/main/java/com/acme/Util.java#Util.format"
        ),
        Some(Resolved)
    );
    assert_eq!(
        call(
            &java,
            &format!("{app}:10"),
            &format!("sym:{app}#App.helper")
        ),
        Some(Exact)
    );
    assert!(java.entries.iter().any(|e| e.name == "GET /users/{id}"));
    let cs = symbols_of("symbols-cs");
    assert_eq!(
        call(
            &cs,
            "App/UsersController.cs:11",
            "sym:Core/Repo.cs#Repo.Describe"
        ),
        Some(Resolved)
    );
    assert!(cs.entries.iter().any(|e| e.name == "GET /api/Users/{id}"));
    let c = symbols_of("symbols-c");
    assert_eq!(
        call(&c, "src/main.c:13", "sym:src/util.c#util_add"),
        Some(Resolved)
    );
    assert_eq!(
        call(&c, "src/main.c:13", "sym:src/orphan.c#orphan_only"),
        Some(Heuristic)
    );
    assert!(
        none_at(&c, "src/main.c:14"),
        "static functions stay private"
    );
    let cpp = symbols_of("symbols-cpp");
    assert_eq!(
        call(&cpp, "src/main.cpp:6", "sym:src/shape.cpp#Shape.make"),
        Some(Resolved)
    );
    assert_eq!(
        call(&cpp, "src/shape.cpp:4", "sym:src/shape.hpp#Shape.sides"),
        Some(Resolved)
    );
}

#[test]
fn godot_rules() {
    use Confidence::*;
    let s = symbols_of("symbols-gd");
    assert_eq!(
        call(&s, "main.gd:4", "sym:player.gd#Player"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "main.gd:9", "sym:autoload/events.gd#emit_score"),
        Some(Resolved)
    );
    assert_eq!(
        call(&s, "player.gd:8", "sym:player.gd#move_by"),
        Some(Exact)
    );
    assert_eq!(
        call(&s, "player.gd:9", "sym:player.gd#move_by"),
        Some(Exact)
    );
    assert_eq!(
        call(&s, "player.gd:11", "sym:bullet.gd#fire_now"),
        Some(Heuristic)
    );
    let kinds: Vec<&str> = s.entries.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"godot_main_scene") && kinds.contains(&"godot_autoload"));
}

/// The supertype edge `sub : name -> target`, if recorded.
fn super_of(s: &Symbols, sub: &str, name: &str) -> Option<Option<String>> {
    s.supers
        .iter()
        .find(|x| x.sub == sub && x.name == name)
        .map(|x| x.target.clone())
}

/// Methods a type inherits, per language (the `inherit-*` fixtures): `this.m()` and
/// implicit `m()`, `super.m()`, a typed field, a static call through a subclass, the
/// lookup order, and the precision rule: a base outside the repo ends the walk.
#[test]
fn inheritance_rules() {
    use Confidence::*;
    let ts = symbols_of("inherit-ts");
    let describe = "sym:src/shape.ts#Shape.describe";
    for at in ["src/circle.ts:9", "src/circle.ts:11", "src/circle.ts:18"] {
        assert_eq!(call(&ts, at, describe), Some(Resolved), "{at}");
    }
    assert_eq!(
        call(&ts, "src/circle.ts:17", "sym:src/circle.ts#Circle.draw"),
        Some(Resolved),
        "super.draw() is the parent's override"
    );
    assert_eq!(call(&ts, "src/app.ts:9", describe), Some(Resolved));
    assert_eq!(
        call(&ts, "src/app.ts:10", "sym:src/root.ts#Root.rootHelper"),
        Some(Resolved)
    );
    assert_eq!(
        call(&ts, "src/app.ts:12", "sym:src/shape.ts#Shape.create"),
        Some(Resolved)
    );
    assert!(none_at(&ts, "src/app.ts:11"), "no supertype defines it");
    assert!(
        none_at(&ts, "src/widget.ts:5"),
        "a library base ends the walk"
    );
    assert!(none_at(&ts, "src/widget.ts:6"));
    assert_eq!(
        super_of(&ts, "sym:src/widget.ts#Widget", "Component"),
        Some(None)
    );

    let py = symbols_of("inherit-py");
    assert_eq!(
        call(&py, "pkg/models.py:8", "sym:pkg/base.py#Mixin.save"),
        Some(Resolved),
        "the MRO: the first base that defines it"
    );
    assert_eq!(
        call(&py, "pkg/models.py:11", "sym:pkg/base.py#Base.validate"),
        Some(Resolved),
        "super(Class, self)"
    );
    assert_eq!(
        call(&py, "pkg/diamond.py:17", "sym:pkg/diamond.py#Right.hook"),
        Some(Resolved),
        "a shared base comes after every class that has it as a base"
    );
    assert_eq!(
        call(&py, "pkg/models.py:17", "sym:pkg/base.py#Base.validate"),
        Some(Resolved)
    );
    assert!(
        none_at(&py, "pkg/models.py:23"),
        "a library base ends the walk"
    );
    assert_eq!(
        super_of(&py, "sym:pkg/models.py#Config", "Config"),
        Some(Some("sym:pkg/base.py#Config".into())),
        "a base named like the class is the imported one"
    );

    let rs = symbols_of("inherit-rust");
    let save_all = "sym:src/persist.rs#Persist.save_all";
    assert_eq!(call(&rs, "src/model.rs:26", save_all), Some(Resolved));
    assert_eq!(call(&rs, "src/service.rs:9", save_all), Some(Resolved));
    assert_eq!(call(&rs, "src/service.rs:11", save_all), Some(Resolved));
    assert_eq!(
        call(&rs, "src/persist.rs:13", "sym:src/persist.rs#Named.label"),
        Some(Resolved),
        "a supertrait's default method"
    );
    assert!(none_at(&rs, "src/service.rs:10"), "a library trait");

    let java = symbols_of("inherit-java");
    let j = "src/main/java/com/acme/";
    let at = |f: &str, l: u32| format!("{j}{f}:{l}");
    let audit = format!("sym:{j}core/BaseRepo.java#BaseRepo.audit");
    assert_eq!(
        call(&java, &at("core/Repo.java", 6), &audit),
        Some(Resolved)
    );
    assert_eq!(
        call(&java, &at("core/Repo.java", 7), &audit),
        Some(Resolved)
    );
    assert_eq!(
        call(
            &java,
            &at("core/Repo.java", 13),
            &format!("sym:{j}core/BaseRepo.java#BaseRepo.saveEverything")
        ),
        Some(Resolved)
    );
    assert_eq!(
        call(
            &java,
            &at("Service.java", 11),
            &format!("sym:{j}core/Store.java#Store.reload")
        ),
        Some(Resolved),
        "an interface's default method"
    );
    assert!(none_at(&java, &at("Service.java", 18)), "a library base");

    let cs = symbols_of("inherit-cs");
    let log = "sym:App/Shapes.cs#ShapeBase.Log";
    assert_eq!(call(&cs, "App/Shapes.cs:24", log), Some(Resolved));
    assert_eq!(call(&cs, "App/Shapes.cs:26", log), Some(Resolved));
    assert_eq!(
        call(
            &cs,
            "App/Program.cs:9",
            "sym:App/Shapes.cs#ShapeBase.Describe"
        ),
        Some(Resolved)
    );
    assert!(none_at(&cs, "App/Program.cs:17"), "a library base");

    let cpp = symbols_of("inherit-cpp");
    assert_eq!(
        call(&cpp, "src/derived.cpp:4", "sym:src/base.cpp#Base.helper"),
        Some(Resolved),
        "defined out of class in the base header's source file"
    );

    let go = symbols_of("inherit-go");
    assert_eq!(
        call(&go, "server.go:12", "sym:base.go#Base.Log"),
        Some(Resolved)
    );
    assert!(none_at(&go, "server.go:13"), "an embedded library type");

    let gd = symbols_of("inherit-gd");
    let move_by = "sym:actor.gd#move_by";
    assert_eq!(call(&gd, "enemy.gd:5", move_by), Some(Resolved));
    assert_eq!(call(&gd, "boss.gd:4", move_by), Some(Resolved));
    assert_eq!(call(&gd, "boss.gd:10", move_by), Some(Resolved));
    assert_eq!(
        call(&gd, "boss.gd:5", "sym:enemy.gd#hurt"),
        Some(Resolved),
        "the nearest script that defines it"
    );
    assert_eq!(call(&gd, "enemy.gd:9", "sym:actor.gd#hurt"), Some(Resolved));
    assert!(none_at(&gd, "boss.gd:6"), "an engine method");
}

#[test]
fn a_warm_run_yields_the_same_symbols_as_an_uncached_one() {
    for name in FIXTURES {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&repo_root().join("tests/fixtures").join(name), &repo);
        let opts = IndexOptions {
            data_dir: Some(tmp.path().join("db")),
            ..IndexOptions::default()
        };
        let cold = index_to_store(&repo, &opts).unwrap();
        let warm = index_to_store(&repo, &opts).unwrap();
        assert_eq!(warm.timings.cache_misses, 0, "{name}");
        let plain = index(&repo, &opts).unwrap();
        assert_eq!(warm.symbols, plain.symbols, "{name}");
        assert_eq!(cold.symbols, plain.symbols, "{name}");
        let stored = cartograph_core::Store::open(tmp.path().join("db/graph.db"))
            .unwrap()
            .load_symbols()
            .unwrap();
        assert_eq!(stored, plain.symbols, "{name}");
        assert!(!plain.symbols.symbols.is_empty(), "{name}");
    }
}

/// Index `files` from a temp dir.
fn symbols_in(files: &[(&str, &str)]) -> Symbols {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    index(&root, &IndexOptions::default()).unwrap().symbols
}

/// Only what a module exports is reached through an import: `export *`, named and
/// namespace imports never land on a JS/TS definition the file keeps private, and a Python
/// star import skips `_names`. A Python named import may name a `_name`; Rust's
/// `use super::*` sees the parent's private items.
#[test]
fn imports_reach_only_exported_definitions() {
    use Confidence::*;
    let s = symbols_in(&[
        (
            "src/a.ts",
            "function hidden() { return 1; }\nexport function shown() { return hidden(); }\n",
        ),
        ("src/b.ts", "export function hidden() { return 2; }\n"),
        (
            "src/barrel.ts",
            "export * from './a';\nexport * from './b';\n",
        ),
        (
            "src/use.ts",
            "import { hidden, shown } from './barrel';\nimport { hidden as h2 } from './a';\n\
             import * as ns from './a';\nexport function go() {\n  hidden();\n  shown();\n  \
             h2();\n  ns.hidden();\n}\n",
        ),
        (
            "src/d.ts",
            "function foo() { return 1; }\nexport default foo;\n",
        ),
        (
            "src/use-d.ts",
            "import f from './d';\nexport function go() {\n  return f();\n}\n",
        ),
        ("pkg/__init__.py", ""),
        (
            "pkg/m.py",
            "def _private():\n    return 1\n\n\ndef public():\n    return 2\n",
        ),
        ("pkg/other.py", "def _private():\n    return 3\n"),
        (
            "pkg/star.py",
            "from .m import *\nfrom .other import *\n\n_private()\npublic()\n",
        ),
        ("pkg/named.py", "from .m import _private\n\n_private()\n"),
        (
            "pkg/listed.py",
            "__all__ = [\"_kept\"]\n\n\ndef _kept():\n    return 1\n\n\n\
             def dropped():\n    return 2\n",
        ),
        (
            "pkg/star_all.py",
            "from .listed import *\n\n_kept()\ndropped()\n",
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"r\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "mod a;\n"),
        ("src/a.rs", "fn helper() -> u32 {\n    1\n}\nmod b;\n"),
        (
            "src/a/b.rs",
            "use super::*;\npub fn x() -> u32 {\n    helper()\n}\n",
        ),
    ]);
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| format!("{}:{} -> {}", c.file, c.line, c.target))
        .collect();
    // `hidden` through the barrel is b.ts's: a.ts keeps its own private.
    assert_eq!(
        call(&s, "src/use.ts:5", "sym:src/b.ts#hidden"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert_eq!(
        call(&s, "src/use.ts:6", "sym:src/a.ts#shown"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert!(none_at(&s, "src/use.ts:7"), "{calls:#?}");
    assert!(none_at(&s, "src/use.ts:8"), "{calls:#?}");
    // `export default foo` of a non-exported `foo` still resolves.
    assert_eq!(
        call(&s, "src/use-d.ts:3", "sym:src/d.ts#foo"),
        Some(Resolved),
        "{calls:#?}"
    );
    // Python: a star import skips `_private`; a named import does not.
    assert!(none_at(&s, "pkg/star.py:4"), "{calls:#?}");
    assert_eq!(
        call(&s, "pkg/star.py:5", "sym:pkg/m.py#public"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/named.py:3", "sym:pkg/m.py#_private"),
        Some(Resolved),
        "{calls:#?}"
    );
    // `__all__` decides what a star import brings, `_names` included.
    assert_eq!(
        call(&s, "pkg/star_all.py:3", "sym:pkg/listed.py#_kept"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert!(none_at(&s, "pkg/star_all.py:4"), "{calls:#?}");
    // Rust: a child module's `use super::*` sees the parent's private items.
    assert_eq!(
        call(&s, "src/a/b.rs:3", "sym:src/a.rs#helper"),
        Some(Resolved),
        "{calls:#?}"
    );
}

/// Rust calls written through a path with no `use` for it: another workspace crate's
/// (`alpha::run()`, through a module, a `pub use` re-export or a type), the own crate's
/// (`crate::m::f()`), the parent module's (`super::f()`, relative to an inline `mod tests`
/// when inside one). Libraries, missing names and unknown types stay unresolved.
#[test]
fn rust_calls_through_crate_and_module_paths() {
    use Confidence::*;
    let s = symbols_in(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"alpha\", \"beta\"]\n",
        ),
        (
            "alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
        ),
        (
            "alpha/src/lib.rs",
            "pub mod util;\npub use util::deep;\n\npub fn run() {}\n\npub struct Engine;\n\n\
             impl Engine {\n    pub fn new() -> Engine {\n        Engine\n    }\n}\n",
        ),
        (
            "alpha/src/util.rs",
            "pub fn helper() {}\n\npub fn deep() {}\n",
        ),
        (
            "beta/Cargo.toml",
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
             alpha = { path = \"../alpha\" }\n",
        ),
        (
            "beta/src/lib.rs",
            "mod local;\n\npub fn g() {}\n\npub fn go() {\n    alpha::run();\n    \
             alpha::util::helper();\n    alpha::deep();\n    alpha::Engine::new();\n    \
             crate::local::f();\n    std::mem::drop(1);\n    alpha::missing();\n    \
             Unknown::new();\n}\n",
        ),
        (
            "beta/src/local.rs",
            "pub fn f() {\n    super::g();\n    self::h();\n}\n\nfn h() {}\n\n\
             mod tests {\n    fn h() {}\n\n    fn t() {\n        super::h();\n    }\n}\n",
        ),
    ]);
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| format!("{}:{} -> {} {:?}", c.file, c.line, c.target, c.confidence))
        .collect();
    for (at, target) in [
        ("beta/src/lib.rs:6", "sym:alpha/src/lib.rs#run"),
        ("beta/src/lib.rs:7", "sym:alpha/src/util.rs#helper"),
        ("beta/src/lib.rs:8", "sym:alpha/src/util.rs#deep"),
        ("beta/src/lib.rs:9", "sym:alpha/src/lib.rs#Engine.new"),
        ("beta/src/lib.rs:10", "sym:beta/src/local.rs#f"),
        ("beta/src/local.rs:2", "sym:beta/src/lib.rs#g"),
    ] {
        assert_eq!(call(&s, at, target), Some(Resolved), "{at}: {calls:#?}");
    }
    // `super` inside `mod tests` is the file's own module.
    assert!(
        s.calls.iter().any(|c| c.file == "beta/src/local.rs"
            && c.line == 12
            && c.target == "sym:beta/src/local.rs#h"),
        "{calls:#?}"
    );
    for at in [
        "beta/src/lib.rs:11",
        "beta/src/lib.rs:12",
        "beta/src/lib.rs:13",
    ] {
        assert!(none_at(&s, at), "{at}: {calls:#?}");
    }
}

/// An axum route handler named through a path (`get(api::handlers::list)`) resolves
/// through the file the path leads to, as a call written that way does.
#[test]
fn rust_route_handlers_through_module_paths() {
    let s = symbols_in(&[
        (
            "Cargo.toml",
            "[package]\nname = \"web\"\nversion = \"0.1.0\"\n\n[dependencies]\naxum = \"0.8\"\n",
        ),
        (
            "src/main.rs",
            "mod api;\n\nuse axum::routing::get;\nuse axum::Router;\n\nfn main() {\n    \
             let _r: Router = Router::new()\n        .route(\"/items\", get(api::handlers::list))\n        \
             .route(\"/one\", get(crate::api::handlers::one));\n}\n",
        ),
        ("src/api/mod.rs", "pub mod handlers;\n"),
        (
            "src/api/handlers.rs",
            "pub async fn list() {}\n\npub async fn one() {}\n",
        ),
    ]);
    let target = |name: &str| {
        s.entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no entry {name}: {:#?}", s.entries))
            .target
            .clone()
    };
    assert_eq!(
        target("GET /items").as_deref(),
        Some("sym:src/api/handlers.rs#list")
    );
    assert_eq!(
        target("GET /one").as_deref(),
        Some("sym:src/api/handlers.rs#one")
    );
}

/// `krate::Bar::new()` where the crate re-exports `pub use inner::Foo as Bar`: the method
/// is looked up on the type's own name, `Foo`.
#[test]
fn rust_path_calls_through_a_renamed_type() {
    let s = symbols_in(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"alpha\", \"beta\"]\n",
        ),
        (
            "alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
        ),
        (
            "alpha/src/lib.rs",
            "mod inner;\npub use inner::Foo as Bar;\n",
        ),
        (
            "alpha/src/inner.rs",
            "pub struct Foo;\n\nimpl Foo {\n    pub fn new() -> Foo {\n        Foo\n    }\n}\n",
        ),
        (
            "beta/Cargo.toml",
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
             alpha = { path = \"../alpha\" }\n",
        ),
        (
            "beta/src/lib.rs",
            "pub fn go() {\n    alpha::Bar::new();\n}\n",
        ),
    ]);
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| format!("{}:{} -> {}", c.file, c.line, c.target))
        .collect();
    assert_eq!(
        call(&s, "beta/src/lib.rs:2", "sym:alpha/src/inner.rs#Foo.new"),
        Some(Confidence::Resolved),
        "{calls:#?}"
    );
}

/// With an explicit `__all__`, a Python star import brings exactly the listed names: not
/// a name the module only imported and left out, but a listed one it imported (a
/// re-export, `_name` included) and one it defines. Without `__all__`, the names the
/// module imported come along as before (`_names` left out).
#[test]
fn a_python_star_import_brings_only_the_names_in_all() {
    use Confidence::*;
    let s = symbols_in(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/impl.py",
            "def helper():\n    return 1\n\n\ndef _inner():\n    return 2\n\n\n\
             def shared():\n    return 3\n",
        ),
        (
            "pkg/api.py",
            "from .impl import helper, _inner, shared\n\n__all__ = [\"run\", \"_inner\", \
             \"shared\"]\n\n\ndef run():\n    return helper()\n",
        ),
        (
            "pkg/open.py",
            "from .impl import helper, _inner\n\n\ndef run():\n    return helper()\n",
        ),
        (
            "pkg/use_api.py",
            "from .api import *\n\nrun()\nhelper()\n_inner()\nshared()\n",
        ),
        (
            "pkg/use_open.py",
            "from .open import *\n\nhelper()\n_inner()\n",
        ),
    ]);
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| format!("{}:{} -> {}", c.file, c.line, c.target))
        .collect();
    assert_eq!(
        call(&s, "pkg/use_api.py:3", "sym:pkg/api.py#run"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert!(
        none_at(&s, "pkg/use_api.py:4"),
        "not in __all__: {calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/use_api.py:5", "sym:pkg/impl.py#_inner"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/use_api.py:6", "sym:pkg/impl.py#shared"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/use_open.py:3", "sym:pkg/impl.py#helper"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert!(none_at(&s, "pkg/use_open.py:4"), "{calls:#?}");
}

/// `__all__` assigned or mutated anywhere but a plain module-level statement (inside an
/// `if`, a `try`, a function) is unknown: a star import falls back to the `_name` rule
/// instead of a partial list. Reading it elsewhere keeps the list.
#[test]
fn a_python_all_built_in_a_block_is_unknown() {
    use Confidence::*;
    let s = symbols_in(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/impl.py",
            "def helper():\n    return 1\n\n\ndef extra():\n    return 2\n\n\n\
             def other():\n    return 3\n",
        ),
        (
            "pkg/cond.py",
            "from .impl import helper\n\n__all__ = [\"run\"]\n\nif True:\n    \
             __all__ += [\"helper\"]\n\n\ndef run():\n    return 1\n",
        ),
        (
            "pkg/tried.py",
            "from .impl import extra\n\n__all__ = [\"run\"]\ntry:\n    \
             __all__.append(\"extra\")\nexcept Exception:\n    pass\n\n\n\
             def run():\n    return 1\n",
        ),
        (
            "pkg/reads.py",
            "from .impl import other\n\n__all__ = [\"run\"]\n\n\ndef __dir__():\n    \
             return __all__\n\n\ndef run():\n    return 1\n",
        ),
        ("pkg/use_cond.py", "from .cond import *\n\nhelper()\n"),
        ("pkg/use_tried.py", "from .tried import *\n\nextra()\n"),
        (
            "pkg/use_reads.py",
            "from .reads import *\n\nother()\nrun()\n",
        ),
    ]);
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| format!("{}:{} -> {}", c.file, c.line, c.target))
        .collect();
    assert_eq!(
        call(&s, "pkg/use_cond.py:3", "sym:pkg/impl.py#helper"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/use_tried.py:3", "sym:pkg/impl.py#extra"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert!(
        none_at(&s, "pkg/use_reads.py:3"),
        "not in __all__: {calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/use_reads.py:4", "sym:pkg/reads.py#run"),
        Some(Resolved),
        "{calls:#?}"
    );
}

/// An item assignment or `del` of `__all__[…]` inside a function mutates the module's list
/// (it binds no local): the list is unknown. Reading an item keeps it.
#[test]
fn a_python_all_item_written_in_a_function_is_unknown() {
    use Confidence::*;
    let s = symbols_in(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/impl.py",
            "def helper():\n    return 1\n\n\ndef extra():\n    return 2\n\n\n\
             def other():\n    return 3\n",
        ),
        (
            "pkg/item.py",
            "from .impl import helper\n\n__all__ = [\"run\"]\n\n\ndef patch():\n    \
             __all__[0] = \"helper\"\n\n\ndef run():\n    return 1\n",
        ),
        (
            "pkg/dele.py",
            "from .impl import extra\n\n__all__ = [\"run\"]\n\n\ndef patch():\n    \
             del __all__[0]\n\n\ndef run():\n    return 1\n",
        ),
        (
            "pkg/reads.py",
            "from .impl import other\n\n__all__ = [\"run\"]\n\n\ndef first():\n    \
             return __all__[0]\n\n\ndef run():\n    return 1\n",
        ),
        ("pkg/use_item.py", "from .item import *\n\nhelper()\n"),
        ("pkg/use_dele.py", "from .dele import *\n\nextra()\n"),
        ("pkg/use_reads.py", "from .reads import *\n\nother()\n"),
    ]);
    let calls: Vec<String> = s
        .calls
        .iter()
        .map(|c| format!("{}:{} -> {}", c.file, c.line, c.target))
        .collect();
    assert_eq!(
        call(&s, "pkg/use_item.py:3", "sym:pkg/impl.py#helper"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert_eq!(
        call(&s, "pkg/use_dele.py:3", "sym:pkg/impl.py#extra"),
        Some(Resolved),
        "{calls:#?}"
    );
    assert!(
        none_at(&s, "pkg/use_reads.py:3"),
        "not in __all__: {calls:#?}"
    );
}

/// `this.field.m()` whose field type is an interface (it declares `m`, an implementation
/// defines it) falls through to the other rules: `m`, the only method of that name, defined
/// in a file the caller imports.
#[test]
fn a_field_typed_as_an_interface_falls_back_to_the_member_rule() {
    let s = symbols_in(&[
        (
            "src/store.ts",
            "export interface Store {\n  persistAll(): void;\n}\n",
        ),
        (
            "src/db.ts",
            "import { Store } from './store';\n\nexport class DbStore implements Store {\n  \
             persistAll() {}\n}\n",
        ),
        (
            "src/app.ts",
            "import { Store } from './store';\nimport { DbStore } from './db';\n\n\
             export class App {\n  private store: Store = new DbStore();\n  run() {\n    \
             this.store.persistAll();\n  }\n}\n",
        ),
    ]);
    assert_eq!(
        call(&s, "src/app.ts:7", "sym:src/db.ts#DbStore.persistAll"),
        Some(Confidence::Heuristic),
        "{:#?}",
        s.calls
    );
}

/// A TS abstract class (`abstract m()`) or an ambient one (`declare class`) declares methods
/// a subclass defines: `this.field.m()` through it falls through as for an interface. Its
/// own defined methods still decide first.
#[test]
fn a_field_typed_as_an_abstract_or_ambient_class_falls_back_to_the_member_rule() {
    let s = symbols_in(&[
        (
            "src/store.ts",
            "export abstract class Store {\n  abstract persistAll(): void;\n  load() {}\n}\n",
        ),
        (
            "src/db.ts",
            "import { Store } from './store';\n\nexport class DbStore extends Store {\n  \
             persistAll() {}\n}\n",
        ),
        (
            "src/remote.ts",
            "export declare class Remote {\n  sendAll(): void;\n}\n",
        ),
        (
            "src/http.ts",
            "import { Remote } from './remote';\n\nexport class HttpRemote extends Remote {\n  \
             sendAll() {}\n}\n",
        ),
        (
            "src/app.ts",
            "import { Store } from './store';\nimport { DbStore } from './db';\n\
             import { Remote } from './remote';\nimport { HttpRemote } from './http';\n\n\
             export class App {\n  private store: Store = new DbStore();\n  \
             private remote: Remote = new HttpRemote();\n  run() {\n    \
             this.store.persistAll();\n    this.remote.sendAll();\n    this.store.load();\n  \
             }\n}\n",
        ),
    ]);
    assert_eq!(
        call(&s, "src/app.ts:10", "sym:src/db.ts#DbStore.persistAll"),
        Some(Confidence::Heuristic),
        "{:#?}",
        s.calls
    );
    assert_eq!(
        call(&s, "src/app.ts:11", "sym:src/http.ts#HttpRemote.sendAll"),
        Some(Confidence::Heuristic),
        "{:#?}",
        s.calls
    );
    assert_eq!(
        call(&s, "src/app.ts:12", "sym:src/store.ts#Store.load"),
        Some(Confidence::Resolved),
        "{:#?}",
        s.calls
    );
}

/// `this.field.m()` whose field type is a concrete class of the repo without `m` stops
/// there (a wrong caller is worse than a missing one): a same-named method elsewhere is
/// not linked. A method the class inherits from a base of the repo (Java's
/// `saveEverything` here) resolves to the base; with a base outside the repo the call stays
/// unresolved, since that base may define it.
#[test]
fn a_concrete_field_type_without_the_method_stops_the_call() {
    let s = symbols_in(&[
        ("src/cache.ts", "export class Cache {\n  get() {}\n}\n"),
        (
            "src/files.ts",
            "export class Files {\n  flushEverything() {}\n}\n",
        ),
        (
            "src/app.ts",
            "import { Cache } from './cache';\nimport { Files } from './files';\n\n\
             export class App {\n  private cache: Cache = new Cache();\n  run() {\n    \
             this.cache.flushEverything();\n    new Files();\n  }\n}\n",
        ),
        (
            "src/main/java/app/BaseRepo.java",
            "package app;\n\npublic class BaseRepo {\n    public void saveEverything() {}\n}\n",
        ),
        (
            "src/main/java/app/Repo.java",
            "package app;\n\npublic class Repo extends BaseRepo {\n    public void load() {}\n}\n",
        ),
        (
            "src/main/java/app/Files.java",
            "package app;\n\npublic class Files {\n    public void flushEverything() {}\n}\n",
        ),
        (
            "src/main/java/app/Names.java",
            "package app;\n\npublic class Names extends java.util.ArrayList<String> {}\n",
        ),
        (
            "src/main/java/app/Service.java",
            "package app;\n\npublic class Service {\n    private Repo repo;\n    \
             private Names names;\n\n    void run() {\n        \
             this.repo.saveEverything();\n        this.repo.load();\n        \
             this.names.flushEverything();\n    }\n}\n",
        ),
    ]);
    assert!(none_at(&s, "src/app.ts:7"), "{:#?}", s.calls);
    assert_eq!(
        call(
            &s,
            "src/main/java/app/Service.java:8",
            "sym:src/main/java/app/BaseRepo.java#BaseRepo.saveEverything"
        ),
        Some(Confidence::Resolved),
        "{:#?}",
        s.calls
    );
    // A base outside the repo: the repo's only `flushEverything` is not linked.
    assert!(
        none_at(&s, "src/main/java/app/Service.java:10"),
        "{:#?}",
        s.calls
    );
    // The field's own type still decides when it has the method.
    assert_eq!(
        call(
            &s,
            "src/main/java/app/Service.java:9",
            "sym:src/main/java/app/Repo.java#Repo.load"
        ),
        Some(Confidence::Resolved),
        "{:#?}",
        s.calls
    );
}

/// Rust: `self.field.m()` whose type has no `m` in its own file looks for an `impl` block
/// of it in the type's folder or below, then anywhere in the same crate (an `impl` may live
/// in any module of the crate). A same-named type of another crate is not a candidate.
#[test]
fn a_rust_field_call_finds_an_impl_anywhere_in_the_crate() {
    let s = symbols_in(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"alpha\", \"beta\"]\n",
        ),
        (
            "alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
        ),
        (
            "alpha/src/lib.rs",
            "pub mod app;\npub mod model;\npub mod persist;\n",
        ),
        ("alpha/src/model/mod.rs", "pub mod store;\n"),
        ("alpha/src/model/store.rs", "pub struct Store;\n"),
        (
            "alpha/src/persist.rs",
            "use crate::model::store::Store;\n\nimpl Store {\n    pub fn persist_all(&self) {}\n}\n",
        ),
        (
            "alpha/src/app.rs",
            "use crate::model::store::Store;\n\npub struct App {\n    store: Store,\n}\n\n\
             impl App {\n    pub fn run(&self) {\n        self.store.persist_all();\n    }\n}\n",
        ),
        (
            "beta/Cargo.toml",
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
        ),
        (
            "beta/src/lib.rs",
            "pub struct Store;\n\nimpl Store {\n    pub fn persist_all(&self) {}\n}\n",
        ),
    ]);
    assert_eq!(
        call(
            &s,
            "alpha/src/app.rs:9",
            "sym:alpha/src/persist.rs#Store.persist_all"
        ),
        Some(Confidence::Heuristic),
        "{:#?}",
        s.calls
    );
}

/// Only a `class_name` statement makes a definition the script itself: an inner class on the
/// first line stays a class, and its methods are what its subclasses inherit.
#[test]
fn an_inner_gdscript_class_on_line_one_is_not_the_script() {
    let s = symbols_in(&[(
        "util.gd",
        "class Helper extends Node:\n\tfunc ping():\n\t\tpass\n\n\
         class Sub extends Helper:\n\tfunc tick():\n\t\tping()\n\n\
         func ping():\n\tpass\n",
    )]);
    assert_eq!(
        call(&s, "util.gd:7", "sym:util.gd#Helper.ping"),
        Some(Confidence::Resolved),
        "{:#?}",
        s.calls
    );
    assert_eq!(call(&s, "util.gd:7", "sym:util.gd#ping"), None);
}

#[test]
fn page_scripts_are_normalized() {
    use cartograph_index::symbols::resolve::page_entries;
    let pages = [(
        "web/index.html",
        "<script type=\"module\" src=\"./src/main.ts\"></script>\n<script type=\"module\" src=\"../shared/boot.ts\"></script>",
    )];
    let files = ["web/src/main.ts", "shared/boot.ts"];
    let got: Vec<String> = page_entries(&pages, |p| files.contains(&p))
        .into_iter()
        .map(|e| e.file)
        .collect();
    assert_eq!(got, vec!["web/src/main.ts", "shared/boot.ts"]);
}

#[test]
fn commonjs_and_listed_object_members() {
    let s = symbols_of("electron-app");
    // `const legacy = require("./legacy"); legacy.ping()` reaches `module.exports.ping`.
    assert_eq!(
        call(
            &s,
            "src/electron/uses-legacy.js:3",
            "sym:src/electron/legacy.js#module.exports.ping"
        ),
        Some(Confidence::Resolved)
    );
    // `export { panel }` exports the members of `const panel = {…}`.
    assert!(
        s.symbols
            .iter()
            .any(|x| x.id() == "sym:src/electron/listed.ts#panel.open" && x.exported)
    );
}

/// A library's `package.json` `main` (no Electron, no `bin`, no script running it) is not
/// where a program starts.
#[test]
fn a_library_main_is_not_a_program_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write(
        "package.json",
        r#"{"name":"lib","main":"dist/index.js","types":"dist/index.d.ts","scripts":{"build":"tsc"}}"#,
    );
    write("src/index.ts", "export function f() {}\n");
    let s = index(&root, &IndexOptions::default()).unwrap().symbols;
    assert!(s.entries.is_empty(), "{:?}", s.entries);
    // A script that runs it makes it a program.
    write(
        "package.json",
        r#"{"name":"lib","main":"dist/index.js","scripts":{"start":"node dist/index.js"}}"#,
    );
    let s = index(&root, &IndexOptions::default()).unwrap().symbols;
    assert_eq!(s.entries.len(), 1, "{:?}", s.entries);
}

#[test]
fn java_calls_through_injected_fields_parameters_and_locals() {
    let s = symbols_of("spring-java");
    use Confidence::*;
    let main = "src/main/java/com/acme/shop/order";
    let ctl = format!("{main}/controller/OrderController.java");
    let admin = format!("{main}/controller/AdminController.java");
    let svc = |m: &str| format!("sym:{main}/service/OrderService.java#OrderService.{m}");
    let report = format!("sym:{main}/controller/OrderReport.java#OrderReport.print");
    // A `private final` field (Lombok constructor) typed with the interface: its method.
    assert_eq!(
        call(&s, &format!("{ctl}:18"), &svc("create")),
        Some(Resolved)
    );
    // A local typed with the interface.
    assert_eq!(call(&s, &format!("{ctl}:24"), &svc("find")), Some(Resolved));
    // An untyped lambda parameter shadows the field: never the field's type.
    assert_eq!(call(&s, &format!("{ctl}:30"), &svc("cancel")), None);
    // A parameter shadows the field with its own type.
    assert_eq!(call(&s, &format!("{ctl}:36"), &report), Some(Resolved));
    // A constructor parameter; an `@Autowired` field of a class of the same package.
    assert_eq!(
        call(&s, &format!("{admin}:18"), &svc("warmUp")),
        Some(Resolved)
    );
    assert_eq!(call(&s, &format!("{admin}:23"), &report), Some(Resolved));
    assert_eq!(
        call(&s, &format!("{admin}:24"), &svc("purge")),
        Some(Resolved)
    );
    // A package-private class of the same package; a repository method inherited from a
    // library interface stays unresolved.
    let imp = format!("{main}/service/impl/OrderServiceImpl.java");
    assert_eq!(
        call(
            &s,
            &format!("{imp}:19"),
            &format!("sym:{main}/service/impl/OrderValidator.java#OrderValidator.check")
        ),
        Some(Resolved)
    );
    assert!(none_at(&s, &format!("{imp}:31")));
    // A test of the same package under `src/test/`: its classes resolve like the folder's.
    let t = "src/test/java/com/acme/shop/order/service/impl/OrderServiceImplTest.java";
    assert_eq!(
        call(
            &s,
            &format!("{t}:14"),
            &format!("sym:{imp}#OrderServiceImpl.create")
        ),
        Some(Resolved)
    );
    assert_eq!(
        call(
            &s,
            &format!("{t}:13"),
            &format!("sym:{main}/service/impl/OrderValidator.java#OrderValidator.lenient")
        ),
        Some(Resolved)
    );
}

#[test]
fn java_receiver_types_follow_scopes_and_never_guess() {
    let s = symbols_of("java-receivers");
    use Confidence::*;
    let scope = "src/main/java/com/acme/scope";
    let c = format!("{scope}/Caller.java");
    let at = |l: u32| format!("{c}:{l}");
    let svc = |m: &str| format!("sym:{scope}/Service.java#Service.{m}");
    let rep = |m: &str| format!("sym:{scope}/Report.java#Report.{m}");
    // A local of an `if` block does not reach past the block: the field.
    assert_eq!(call(&s, &at(12), &rep("print")), Some(Resolved));
    assert_eq!(call(&s, &at(15), &svc("create")), Some(Resolved));
    assert_eq!(call(&s, &at(15), &rep("create")), None);
    // An anonymous subclass sees its base's field before the enclosing class's.
    assert_eq!(call(&s, &at(22), &svc("print")), None);
    // A JDK type of a wildcard import and a nested type are never a same-named repo type.
    assert!(none_at(&s, &at(29)), "{:?}", s.calls);
    assert_eq!(
        call(
            &s,
            &at(38),
            "sym:src/main/java/com/acme/other/Builder.java#Builder.build"
        ),
        None
    );
    assert_eq!(
        call(&s, &at(38), &format!("sym:{c}#Builder.build")),
        Some(Resolved)
    );
}
