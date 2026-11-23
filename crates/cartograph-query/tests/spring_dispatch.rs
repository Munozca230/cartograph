//! What a call runs through Spring, the same for flows and for slices: every case of one
//! table is asked of both. A case is a few Java classes with a bean `Start` whose `go()` is
//! called by a controller; every method it names writes a collection called like itself,
//! so a slice that reaches the method lists its collection.

mod common;

use cartograph_index::{IndexOptions, index};
use cartograph_query::flow::{
    CallIndex, FlowOptions, MAX_DEPTH, MAX_NODES, SPRING_OTHER_VIA, flow,
};
use cartograph_query::{Config, Repo, path, symbols};
use std::collections::BTreeSet;
use std::fs;

const IMPORTS: &str = "package com.shop; import java.util.*; import com.mongodb.client.MongoDatabase; \
import org.springframework.stereotype.*; import org.springframework.beans.factory.annotation.*; \
import org.springframework.context.annotation.*; import org.springframework.web.bind.annotation.*;";

struct Case {
    name: &'static str,
    /// Java files, `Name: body`; `@@` in a body is a write of the method around it: see
    /// [`written`].
    java: &'static [(&'static str, &'static str)],
    reaches: &'static [&'static str],
    not: &'static [&'static str],
    /// Where Spring injects others: only after the rest, if at all.
    other: &'static [&'static str],
}

/// `«Card.save»` in a body: the write that marks `Card.save`.
/// Each statement on a line of its own (a call belongs to the method its line is in).
fn written(body: &str) -> String {
    body.replace('«', "db.getCollection(\"")
        .replace('»', "\");")
        .replace("; ", ";\n")
        .replace("{ ", "{\n")
        .replace("} ", "}\n")
}

fn repo(case: &Case) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let put = |p: &str, text: String| {
        let at = root.join(p);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(at, text).unwrap();
    };
    put("cartograph.toml", "[bridges]\nhttp = true\n".into());
    put(
        "ui/page.ts",
        "import { post } from './client';\nexport function page() { return post(); }\n".into(),
    );
    put(
        "ui/client.ts",
        "export function post() {\n  return fetch('/api/go', { method: 'POST' });\n}\n".into(),
    );
    put(
        "api/GoController.java",
        format!(
            "{IMPORTS}\n@RestController public class GoController {{\n    @Autowired private Start start;\n    @PostMapping(\"/api/go\") public void post() {{ start.go(); }}\n}}\n"
        ),
    );
    for (name, body) in case.java {
        put(
            &format!("api/{name}.java"),
            format!("{IMPORTS}\n{}\n", written(body)),
        );
    }
    let out = index(root, &IndexOptions::default()).unwrap();
    Repo::with_symbols(&out.graph, &out.symbols, Config::load(root).unwrap())
}

/// `Class.method` of every function a flow from the controller shows, and of those it gets
/// to only where Spring injects others.
fn by_flow(r: &Repo) -> (BTreeSet<String>, BTreeSet<String>) {
    let ix = CallIndex::build(r);
    let opts = FlowOptions {
        depth: MAX_DEPTH,
        max_nodes: MAX_NODES,
        expand: Vec::new(),
    };
    let g = flow(r, &ix, "sym:api/GoController.java#GoController.post", &opts).unwrap();
    let name = |id: &str| id.rsplit('#').next().unwrap_or("").to_string();
    let first: BTreeSet<String> = g
        .edges
        .iter()
        .filter(|e| e.via.as_deref() != Some(SPRING_OTHER_VIA))
        .map(|e| name(&e.dst))
        .collect();
    let all: BTreeSet<String> = g.nodes.iter().map(|n| name(&n.id)).collect();
    let other = all.difference(&first).cloned().collect();
    (all, other)
}

/// The collections a slice from the page names, and those but the one it ends in.
fn by_slice(r: &Repo) -> (BTreeSet<String>, BTreeSet<String>) {
    let t = path::path(r, "ui/page.ts", "ui/client.ts").unwrap().text;
    let mut out = BTreeSet::new();
    let mut others = BTreeSet::new();
    for line in t.lines() {
        if let Some(rest) = line.trim().strip_prefix("→ collection ") {
            out.insert(rest.split(' ').next().unwrap_or("").to_string());
        }
        if let Some(rest) = line.trim().strip_prefix("also reaches: ") {
            others.extend(
                rest.split(", ")
                    .filter_map(|x| x.split_once(" (Mongo"))
                    .map(|(name, _)| name)
                    .map(str::to_string),
            );
        }
    }
    out.extend(others.iter().cloned());
    (out, others)
}

const CASES: &[Case] = &[
    Case {
        name: "an inherited method of the injected class",
        java: &[
            (
                "Base",
                "public abstract class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            ("Card", "@Service public class Card extends Base { }"),
            (
                "Wire",
                "@Service public class Wire extends Base { @Override public void save() { «Wire.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Card card; void go() { card.save(); } }",
            ),
        ],
        reaches: &["Base.save"],
        not: &["Wire.save"],
        other: &[],
    },
    Case {
        name: "an inherited method of a local variable's class",
        java: &[
            (
                "Base",
                "public abstract class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            ("Card", "@Service public class Card extends Base { }"),
            (
                "Wire",
                "@Service public class Wire extends Base { @Override public void save() { «Wire.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { void go() { Card card = find(); card.save(); } Card find() { return null; } }",
            ),
        ],
        reaches: &["Base.save"],
        not: &["Wire.save"],
        other: &[],
    },
    Case {
        name: "a field typed below the declaring class, overridden there",
        java: &[
            (
                "Base",
                "public abstract class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            ("Cards", "public abstract class Cards extends Base { }"),
            (
                "Visa",
                "@Service public class Visa extends Cards { @Override public void save() { «Visa.save» } }",
            ),
            (
                "Wire",
                "@Service public class Wire extends Base { @Override public void save() { «Wire.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Cards cards; void go() { cards.save(); } }",
            ),
        ],
        reaches: &["Visa.save", "Base.save"],
        not: &["Wire.save"],
        other: &[],
    },
    Case {
        name: "private and static methods",
        java: &[
            (
                "Base",
                "public abstract class Base { MongoDatabase db; static MongoDatabase sdb;
    public void run() { check(); make(); }
    private void check() { «Base.check» }
    static void make() { sdb.getCollection(\"Base.make\"); }
}",
            ),
            (
                "Sub",
                "@Service public class Sub extends Base { MongoDatabase db;
    void check() { «Sub.check» }
    static void make() { sdb.getCollection(\"Sub.make\"); }
}",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Base base; void go() { base.run(); } }",
            ),
        ],
        reaches: &["Base.check", "Base.make"],
        not: &["Sub.check", "Sub.make"],
        other: &[],
    },
    Case {
        name: "an abstract method called by a class between",
        java: &[
            (
                "Base",
                "public abstract class Base { abstract void step(); }",
            ),
            (
                "Mid",
                "public abstract class Mid extends Base { public void run() { step(); } }",
            ),
            (
                "Leaf",
                "@Service public class Leaf extends Mid { MongoDatabase db; @Override void step() { «Leaf.step» } }",
            ),
            (
                "Other",
                "@Service public class Other extends Base { MongoDatabase db; @Override void step() { «Other.step» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Mid mid; void go() { mid.run(); } }",
            ),
        ],
        reaches: &["Leaf.step"],
        not: &["Other.step"],
        other: &[],
    },
    Case {
        name: "super runs the base class's own",
        java: &[
            (
                "Base",
                "public class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            (
                "Audited",
                "@Service public class Audited extends Base { @Override public void save() { super.save(); «Audited.save» } }",
            ),
            (
                "Other",
                "@Service public class Other extends Base { @Override public void save() { «Other.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Audited a; void go() { a.save(); } }",
            ),
        ],
        reaches: &["Audited.save", "Base.save"],
        not: &["Other.save"],
        other: &[],
    },
    Case {
        name: "super of an interface runs its default",
        java: &[
            (
                "Flow",
                "public interface Flow { MongoDatabase db = null; default void save() { «Flow.save» } }",
            ),
            (
                "Audited",
                "@Service public class Audited implements Flow { MongoDatabase db; @Override public void save() { «Audited.save» Flow.super.save(); } }",
            ),
            (
                "Other",
                "@Service public class Other implements Flow { @Override public void save() { «Other.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Audited a; void go() { a.save(); } }",
            ),
        ],
        reaches: &["Audited.save", "Flow.save"],
        not: &["Other.save"],
        other: &[],
    },
    Case {
        name: "super and another object on one line",
        java: &[
            (
                "Base",
                "public class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            (
                "Audited",
                "@Service public class Audited extends Base { void both(Base other) { } @Override public void save() { Base o = find(); super.save();o.save(); } Base find() { return null; } }",
            ),
            (
                "Other",
                "@Service public class Other extends Base { @Override public void save() { «Other.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Audited a; void go() { a.save(); } }",
            ),
        ],
        reaches: &["Base.save", "Other.save"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a decorator of an interface",
        java: &[
            ("Gateway", "public interface Gateway { void charge(); }"),
            (
                "Retry",
                "@Service @Primary public class Retry implements Gateway { MongoDatabase db; Gateway inner = find(); @Override public void charge() { «Retry.charge» inner.charge(); } Gateway find() { return null; } }",
            ),
            (
                "Wire",
                "@Service public class Wire implements Gateway { MongoDatabase db; @Override public void charge() { «Wire.charge» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Gateway g; void go() { g.charge(); } }",
            ),
        ],
        reaches: &["Retry.charge", "Wire.charge"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a decorator of a class",
        java: &[
            (
                "Gateway",
                "public abstract class Gateway { public abstract void charge(); }",
            ),
            (
                "Retry",
                "@Service @Primary public class Retry extends Gateway { MongoDatabase db; private final Gateway inner; Retry(@Qualifier(\"wire\") Gateway inner) { this.inner = inner; } @Override public void charge() { «Retry.charge» inner.charge(); } }",
            ),
            (
                "Wire",
                "@Service public class Wire extends Gateway { MongoDatabase db; @Override public void charge() { «Wire.charge» } }",
            ),
            (
                "Card",
                "@Service public class Card extends Gateway { MongoDatabase db; @Override public void charge() { «Card.charge» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Gateway g; void go() { g.charge(); } }",
            ),
        ],
        reaches: &["Retry.charge", "Wire.charge"],
        not: &[],
        other: &["Card.charge"],
    },
    Case {
        name: "a decorator of a class it was not injected with",
        java: &[
            (
                "Gateway",
                "public class Gateway { MongoDatabase db; public void charge() { «Gateway.charge» } }",
            ),
            (
                "Retry",
                "@Service @Primary public class Retry extends Gateway { MongoDatabase db; Gateway inner; void use(Gateway g) { inner = g; } @Override public void charge() { «Retry.charge» inner.charge(); } }",
            ),
            (
                "Wire",
                "@Service public class Wire extends Gateway { @Override public void charge() { «Wire.charge» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Gateway g; void go() { g.charge(); } }",
            ),
        ],
        reaches: &["Retry.charge", "Wire.charge"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a default method that calls what its bean has",
        java: &[
            (
                "Flow",
                "public interface Flow { void check(); default void save() { check(); } }",
            ),
            (
                "Plain",
                "@Service(\"plain\") public class Plain implements Flow { MongoDatabase db; @Override public void check() { «Plain.check» } }",
            ),
            (
                "Loud",
                "@Service(\"loud\") public class Loud implements Flow { MongoDatabase db; @Override public void check() { } @Override public void save() { «Loud.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired @Qualifier(\"plain\") private Flow f; void go() { f.save(); } }",
            ),
        ],
        reaches: &["Plain.check"],
        not: &[],
        other: &["Loud.save"],
    },
    Case {
        name: "a default of an interface below wins over the abstract one",
        java: &[
            ("Flow", "public interface Flow { void save(); }"),
            (
                "Kept",
                "public interface Kept extends Flow { MongoDatabase db = null; @Override default void save() { «Kept.save» } }",
            ),
            (
                "Plain",
                "@Service public class Plain implements Flow, Kept { }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Flow f; void go() { f.save(); } }",
            ),
        ],
        reaches: &["Kept.save"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a bean in a nested class",
        java: &[
            ("Gateway", "public interface Gateway { void charge(); }"),
            (
                "Outer",
                "public class Outer { MongoDatabase out; void charge() { out.getCollection(\"Outer.charge\"); } @Service static class Inner implements Gateway { MongoDatabase db; @Override public void charge() { «Inner.charge» } } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Gateway g; void go() { g.charge(); } }",
            ),
        ],
        reaches: &["Inner.charge"],
        not: &["Outer.charge"],
        other: &[],
    },
    Case {
        name: "two nested classes of one name",
        java: &[
            ("Gateway", "public interface Gateway { void charge(); }"),
            (
                "A",
                "public class A { @Service(\"a\") static class Impl implements Gateway { MongoDatabase db; @Override public void charge() { «Impl.charge» } } }",
            ),
            (
                "B",
                "public class B { @Service(\"b\") static class Impl implements Gateway { MongoDatabase db; @Override public void charge() { «B.charge» } } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired @Qualifier(\"a\") private Gateway g; void go() { g.charge(); } }",
            ),
        ],
        reaches: &["Impl.charge"],
        not: &[],
        other: &["B.charge"],
    },
    Case {
        name: "variable parameters and generic overloads",
        java: &[
            (
                "Base",
                "public abstract class Base<T> { MongoDatabase db; public abstract void put(T a); public void put(T a, T b) { «Base.put» } }",
            ),
            (
                "Sub",
                "@Service public class Sub extends Base<String> { @Override public void put(String a) { «Sub.put» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Base<String> base; @Autowired private Sub sub; void go() { sub.put(\"a\"); } }",
            ),
        ],
        reaches: &["Sub.put"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a default method two interfaces declare",
        java: &[
            ("Left", "public interface Left { void save(); }"),
            (
                "Right",
                "public interface Right { MongoDatabase db = null; default void save() { «Right.save» } }",
            ),
            (
                "Both",
                "@Service public class Both extends Base implements Left, Right { }",
            ),
            (
                "Base",
                "public abstract class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Left f; void go() { f.save(); } }",
            ),
        ],
        reaches: &["Base.save"],
        not: &["Right.save"],
        other: &[],
    },
    Case {
        name: "a lambda over a list typed below",
        java: &[
            (
                "Base",
                "public abstract class Base { MongoDatabase db; public void save() { «Base.save» } }",
            ),
            ("Card", "@Service public class Card extends Base { }"),
            (
                "Wire",
                "@Service public class Wire extends Base { @Override public void save() { «Wire.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private List<Card> cards; void go() { for (Card c : cards) { c.save(); } } }",
            ),
        ],
        reaches: &["Base.save"],
        not: &["Wire.save"],
        other: &[],
    },
    Case {
        name: "a method that calls itself on this and on another",
        java: &[
            (
                "Base",
                "public abstract class Base { public void run() { this.step(); } protected void step() { } }",
            ),
            (
                "Sub",
                "@Service public class Sub extends Base { MongoDatabase db; @Override protected void step() { «Sub.step» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Sub sub; void go() { sub.run(); } }",
            ),
        ],
        reaches: &["Sub.step"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a bean made of an interface in @Bean",
        java: &[
            ("Gateway", "public interface Gateway { void charge(); }"),
            (
                "Wire",
                "public class Wire implements Gateway { MongoDatabase db; @Override public void charge() { «Wire.charge» } }",
            ),
            (
                "Config",
                "@Configuration public class Config { @Bean Wire wire() { return new Wire(); } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Gateway g; void go() { g.charge(); } }",
            ),
        ],
        reaches: &["Wire.charge"],
        not: &[],
        other: &[],
    },
    Case {
        name: "an overload is not an override",
        java: &[
            (
                "Base",
                "public class Base { MongoDatabase db; public void save(String a) { «Base.save» } }",
            ),
            (
                "Sub",
                "@Service public class Sub extends Base { public void save(String a, String b) { «Sub.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Base base; void go() { base.save(\"a\"); } }",
            ),
        ],
        reaches: &["Base.save"],
        not: &["Sub.save"],
        other: &[],
    },
    Case {
        name: "one overload of several is overridden",
        java: &[
            (
                "Base",
                "public abstract class Base { MongoDatabase db; public void save(String a) { save(a, a); } public abstract void save(String a, String b); }",
            ),
            (
                "Sub",
                "@Service public class Sub extends Base { @Override public void save(String a, String b) { «Sub.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Base base; void go() { base.save(\"a\"); } }",
            ),
        ],
        reaches: &["Sub.save"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a generic interface",
        java: &[
            (
                "Handler",
                "public interface Handler<T> { void handle(T t); }",
            ),
            (
                "Orders",
                "@Service public class Orders implements Handler<String> { MongoDatabase db; @Override public void handle(String t) { «Orders.handle» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private List<Handler<String>> all; void go() { all.forEach(h -> h.handle(\"a\")); } }",
            ),
        ],
        reaches: &["Orders.handle"],
        not: &[],
        other: &[],
    },
    Case {
        name: "a bean of a generic base class",
        java: &[
            (
                "Base",
                "public abstract class Base<T> { public void run(T t) { handle(t); } protected abstract void handle(T t); }",
            ),
            (
                "Orders",
                "@Service public class Orders extends Base<String> { MongoDatabase db; @Override protected void handle(String t) { «Orders.handle» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired private Orders orders; void go() { orders.run(\"a\"); } }",
            ),
        ],
        reaches: &["Orders.handle"],
        not: &[],
        other: &[],
    },
    Case {
        name: "no bean of the qualifier in the source",
        java: &[
            (
                "Flow",
                "public class Flow { MongoDatabase db; public void save() { «Flow.save» } }",
            ),
            (
                "Other",
                "@Service public class Other extends Flow { @Override public void save() { «Other.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired @Qualifier(\"jar\") private Flow f; void go() { f.save(); } }",
            ),
        ],
        reaches: &["Flow.save"],
        not: &[],
        other: &["Other.save"],
    },
    Case {
        name: "an interface's bean not in the source",
        java: &[
            ("Flow", "public interface Flow { void save(); }"),
            (
                "Other",
                "@Service public class Other implements Flow { MongoDatabase db; @Override public void save() { «Other.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired @Qualifier(\"jar\") private Flow f; void go() { f.save(); } }",
            ),
        ],
        reaches: &[],
        not: &[],
        other: &["Other.save"],
    },
    Case {
        name: "two beans that inherit one method",
        java: &[
            ("Flow", "public interface Flow { void save(); }"),
            (
                "Base",
                "public abstract class Base implements Flow { MongoDatabase db; @Override public void save() { «Base.save» } }",
            ),
            ("One", "@Service(\"one\") public class One extends Base { }"),
            ("Two", "@Service(\"two\") public class Two extends Base { }"),
            (
                "Loud",
                "@Service(\"loud\") public class Loud implements Flow { MongoDatabase db; @Override public void save() { «Loud.save» } }",
            ),
            (
                "Start",
                "@Service public class Start { @Autowired @Qualifier(\"two\") private Flow f; void go() { f.save(); } }",
            ),
        ],
        reaches: &["Base.save"],
        not: &[],
        other: &["Loud.save"],
    },
];

#[test]
fn flows_and_slices_reach_the_same() {
    let mut failures = Vec::new();
    for case in CASES {
        let r = repo(case);
        for (how, (got, later)) in [("flow", by_flow(&r)), ("slice", by_slice(&r))] {
            for want in case.reaches {
                if !got.contains(*want) {
                    failures.push(format!("{} [{how}]: no {want}: {got:?}", case.name));
                }
            }
            for x in case.other {
                // A slice that reaches no other data has it alone.
                if got.contains(*x) && !later.contains(*x) && !case.reaches.is_empty() {
                    failures.push(format!("{} [{how}]: {x} before the rest", case.name));
                }
            }
            for bad in case.not {
                if got.contains(*bad) {
                    failures.push(format!("{} [{how}]: {bad}: {got:?}", case.name));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn callers_follow_the_same_rules() {
    let of = |name: &str, sym: &str| {
        let case = CASES.iter().find(|c| c.name == name).unwrap();
        symbols::callers(&repo(case), sym).unwrap().text
    };
    // Listed as in the code: what Spring injects is a note.
    let t = of(
        "an inherited method of a local variable's class",
        "Wire.save",
    );
    assert!(t.contains("Start.go()"), "{t}");
    let t = of("an inherited method of the injected class", "Wire.save");
    assert!(t.contains("(Spring injects Card here)"), "{t}");
    // `super.save()` runs the base class's own.
    let t = of("super runs the base class's own", "Other.save");
    assert!(!t.contains("Audited.save()"), "{t}");
    // A decorator calls another implementation.
    let t = of("a decorator of an interface", "Wire.charge");
    assert!(t.contains("Retry.charge()"), "{t}");
}

#[test]
fn data_only_other_beans_reach_is_marked() {
    let case = CASES
        .iter()
        .find(|c| c.name == "a decorator of a class")
        .unwrap();
    let t = path::path(&repo(case), "ui/page.ts", "ui/client.ts")
        .unwrap()
        .text;
    assert!(t.contains("Card.charge (Mongo, another bean)"), "{t}");
    assert!(!t.contains("Wire.charge (Mongo, another bean)"), "{t}");
}

#[test]
fn a_flow_from_an_interface_method_shows_its_beans() {
    let case = CASES
        .iter()
        .find(|c| c.name == "a decorator of an interface")
        .unwrap();
    let r = repo(case);
    let ix = CallIndex::build(&r);
    let g = flow(
        &r,
        &ix,
        "sym:api/Gateway.java#Gateway.charge",
        &FlowOptions::default(),
    )
    .unwrap();
    assert!(
        g.nodes.iter().any(|n| n.id.ends_with("#Wire.charge")),
        "{:?}",
        g.edges
    );
}
