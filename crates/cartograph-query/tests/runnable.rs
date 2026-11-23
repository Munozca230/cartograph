//! Runnable tests in `impact` (`tests/fixtures/workspace-keys`, `spring-java`,
//! `query-app`): ranked by closeness, slow ones marked, a ready command per build.

mod common;

use cartograph_query::impact::impact;

fn tests_of(text: &str) -> Vec<&str> {
    text.lines()
        .skip_while(|l| !l.starts_with("tests to run"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .map(str::trim)
        .collect()
}

#[test]
fn maven_tests_closest_first_with_slow_ones_apart() {
    let r = common::fixture("workspace-keys");
    let a = impact(
        &r,
        &["server:app/src/main/java/com/acme/server/files/FileServiceImpl.java".into()],
    )
    .unwrap();
    let t = &a.text;
    assert_eq!(
        tests_of(t),
        [
            // Named after the class, then the direct ones, then through a helper.
            "server/app/src/test/java/com/acme/server/files/FileServiceImplTest.java 1 · named after it",
            "server/app/src/test/java/com/acme/server/files/FileControllerIT.java 1 · slow: @SpringBootTest",
            "server/app/src/test/java/com/acme/server/actions/FileStoreTest.java 2 · slow: @DataMongoTest (via MongoSliceTest)",
            "server/app/src/test/java/com/acme/server/support/TestFiles.java 1 · no test in it",
        ],
        "{t}"
    );
    assert!(
        t.contains(
            "same package, no import reaches it: \
             server/app/src/test/java/com/acme/server/files/FilePathsTest.java\n"
        ),
        "{t}"
    );
    // From the member's root, where the wrapper is, into the module; the helper is in no
    // command.
    assert!(
        t.contains("run (maven, member server): ./mvnw test -pl app -Dtest=FileServiceImplTest\n"),
        "{t}"
    );
    assert!(
        t.contains(
            "run the slow ones (maven, member server): \
             ./mvnw test -pl app -Dtest=FileControllerIT,FileStoreTest\n"
        ),
        "{t}"
    );
    assert!(
        !t.contains("TestFiles,") && !t.contains(",TestFiles"),
        "{t}"
    );
    let cmds = &a.data.commands;
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].member.as_deref(), Some("server"));
    assert_eq!((cmds[0].tests, cmds[0].slow), (1, false));
    assert_eq!((cmds[1].tests, cmds[1].slow), (2, true));
}

#[test]
fn js_tests_run_with_the_runner_their_package_uses() {
    let r = common::fixture("workspace-keys");
    let a = impact(&r, &["ui:src/actions/catalog.ts".into()]).unwrap();
    let t = &a.text;
    assert!(
        t.contains("ui/e2e/catalog.spec.ts 1 · named after it · slow: Playwright\n"),
        "{t}"
    );
    assert!(
        t.contains("run (vitest, member ui): npx vitest run src/actions/catalog.test.ts\n"),
        "{t}"
    );
    assert!(
        t.contains(
            "run the slow ones (playwright, member ui): npx playwright test e2e/catalog.spec.ts\n"
        ),
        "{t}"
    );
}

#[test]
fn a_single_repo_without_a_wrapper_runs_mvn_from_its_root() {
    let r = common::fixture("spring-java");
    let a = impact(
        &r,
        &["src/main/java/com/acme/shop/order/service/impl/OrderServiceImpl.java".into()],
    )
    .unwrap();
    let t = &a.text;
    assert!(
        t.contains("src/test/java/com/acme/shop/order/service/impl/OrderServiceImplTest.java 1 · named after it\n"),
        "{t}"
    );
    // The fixtures class of the package holds no test: never in the command.
    assert!(t.contains("run (maven): mvn test -Dtest="), "{t}");
    assert!(
        !t.contains("OrderFixtures,") && !t.contains(",OrderFixtures"),
        "{t}"
    );
    assert!(!t.contains("UnrelatedTest"), "{t}");
}

#[test]
fn gradle_tests_run_per_project_with_the_wrapper() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("g");
    let files = [
        ("settings.gradle", "include 'app'\n"),
        ("gradlew", "#!/bin/sh\n"),
        ("app/build.gradle", "plugins { id 'java' }\n"),
        (
            "app/src/main/java/com/acme/Clock.java",
            "package com.acme;\npublic class Clock {}\n",
        ),
        (
            "app/src/test/java/com/acme/ClockTest.java",
            "package com.acme;\nimport org.junit.jupiter.api.Test;\nclass ClockTest {\n    @Test void ticks() { new Clock(); }\n}\n",
        ),
    ];
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let out = cartograph_index::index(&root, &cartograph_index::IndexOptions::default()).unwrap();
    let r = cartograph_query::Repo::with_symbols(
        &out.graph,
        &out.symbols,
        cartograph_query::Config::default(),
    );
    let a = impact(&r, &["app/src/main/java/com/acme/Clock.java".into()]).unwrap();
    assert!(
        a.text
            .contains("run (gradle): ./gradlew :app:test --tests ClockTest\n"),
        "{}",
        a.text
    );
}

#[test]
fn no_command_without_a_runner_or_a_test_script() {
    let r = common::fixture("query-app");
    let a = impact(&r, &["src/core/model.ts".into()]).unwrap();
    assert!(a.data.commands.is_empty(), "{}", a.text);
    assert!(
        a.text.contains("tests/e2e.test.ts 3 · slow: e2e (name)"),
        "{}",
        a.text
    );
}
