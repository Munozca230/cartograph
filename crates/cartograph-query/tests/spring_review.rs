//! Spring and slices on small repos written by each test: handlers passed by reference,
//! which sink a request writes, calls that run the declared method itself, receivers Spring
//! did not inject, providers and less usual collections, qualifiers and bean names written
//! as constants, Spring Data repositories as beans.

mod common;

use cartograph_index::{IndexOptions, index};
use cartograph_query::{Config, Repo, path, symbols};
use std::fs;

const IMPORTS: &str = "import java.util.*; import java.util.concurrent.*;
import org.springframework.stereotype.*; import org.springframework.beans.factory.*;
import org.springframework.beans.factory.annotation.*;
import org.springframework.context.annotation.*;
import org.springframework.web.bind.annotation.*;
import org.springframework.data.mongodb.core.*;
import org.springframework.data.mongodb.core.mapping.*;
import org.springframework.data.mongodb.repository.*;
";

/// A repo of these files (Java ones get the imports on their first line, so line numbers
/// are those of the text plus one), HTTP bridges on.
fn repo(files: &[(&str, &str)]) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(root.join("cartograph.toml"), "[bridges]\nhttp = true\n").unwrap();
    for (p, text) in files {
        let at = root.join(p);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        let text = if p.ends_with(".java") {
            format!("package com.shop; {}\n{text}", IMPORTS.replace('\n', " "))
        } else {
            text.to_string()
        };
        fs::write(at, text).unwrap();
    }
    let out = index(root, &IndexOptions::default()).unwrap();
    let config = Config::load(root).unwrap();
    Repo::with_symbols(&out.graph, &out.symbols, config)
}

fn callers(r: &Repo, sym: &str) -> String {
    symbols::callers(r, sym).unwrap().text
}

/// The slice from a page to the client that posts to `/api/orders`, over a controller that
/// calls `flow.save(x)` on the field this declares.
fn slice(field: &str, java: &[(&str, &str)]) -> String {
    let controller = format!(
        "@RestController @RequestMapping(\"/api/orders\")
public class OrderController {{
    {field}
    @PostMapping
    public Order create(@RequestBody Order x) {{ return flow.save(x); }}
}}
"
    );
    let mut files = vec![
        (
            "ui/page.ts",
            "import { post } from './client';\nexport function page() { return post({}); }\n",
        ),
        (
            "ui/client.ts",
            "export function post(x: unknown) {\n  return fetch('/api/orders', { method: 'POST', body: JSON.stringify(x) });\n}\n",
        ),
        ("api/OrderController.java", controller.as_str()),
        (
            "api/Order.java",
            "@Document(\"orders\") public class Order { }",
        ),
        (
            "api/Orders.java",
            "public interface Orders extends MongoRepository<Order, String> { }",
        ),
    ];
    files.extend_from_slice(java);
    let r = repo(&files);
    path::path(&r, "ui/page.ts", "ui/client.ts").unwrap().text
}

// 1 ----------------------------------------------------------------------------------

#[test]
fn a_handler_passed_by_reference_is_the_whole_function() {
    let r = repo(&[
        (
            "package.json",
            "{\"name\":\"d\",\"dependencies\":{\"electron\":\"30\"}}",
        ),
        (
            "src/view.tsx",
            "export function View() {\n  return window.api.load(1);\n}\n",
        ),
        (
            "src/preload.ts",
            "import { contextBridge, ipcRenderer } from 'electron';\ncontextBridge.exposeInMainWorld('api', {\n  load: (id: number) => ipcRenderer.invoke('order:load', id),\n});\n",
        ),
        (
            "src/client.ts",
            "export function fetchOrder(id: number) {\n  return fetch(`/api/orders/${id}`);\n}\n",
        ),
        (
            "src/main.ts",
            "import { ipcMain } from 'electron';\nimport { fetchOrder } from './client';\n\nfunction onLoad(_e: unknown, id: number) {\n  return fetchOrder(id);\n}\n\nexport function register() {\n  ipcMain.handle('order:load', onLoad);\n}\n",
        ),
        (
            "api/OrderController.java",
            "@RestController public class OrderController {\n    @GetMapping(\"/api/orders/{id}\")\n    public String get(@PathVariable String id) { return id; }\n}\n",
        ),
    ]);
    let t = path::path(&r, "src/view.tsx", "src/client.ts")
        .unwrap()
        .text;
    assert!(t.contains("src/main.ts:5 onLoad"), "{t}");
    assert!(t.contains("OrderController.get"), "{t}");
}

// 2 ----------------------------------------------------------------------------------

#[test]
fn the_call_says_whether_it_writes_not_the_method_around_it() {
    let t = slice(
        "@Autowired private Flow flow;",
        &[
            (
                "api/Customer.java",
                "@Document(\"customers\") public class Customer { }",
            ),
            (
                "api/Customers.java",
                "public interface Customers extends MongoRepository<Customer, String> { }",
            ),
            (
                "api/Flow.java",
                "@Service public class Flow {
    @Autowired private Customers customers;
    @Autowired private Orders orders;
    public Order save(Order x) {
        customers.findById(\"1\");
        return orders.save(x);
    }
}
",
            ),
        ],
    );
    assert!(t.contains("→ collection orders (Mongo, via Order)"), "{t}");
    assert!(t.contains("also reaches: customers (Mongo)"), "{t}");
}

#[test]
fn find_and_modify_writes() {
    let t = slice(
        "@Autowired private Flow flow;",
        &[
            (
                "api/Rate.java",
                "@Document(\"a_rates\") public class Rate { }",
            ),
            (
                "api/Flow.java",
                "@Service public class Flow {
    @Autowired private MongoTemplate mongoTemplate;
    public Order save(Order x) { return next(x); }
    private Order next(Order x) {
        mongoTemplate.findOne(q, Rate.class);
        return mongoTemplate.findAndModify(q, u, Order.class);
    }
}
",
            ),
        ],
    );
    assert!(t.contains("→ collection orders (Mongo, via Order)"), "{t}");
}

// 3 ----------------------------------------------------------------------------------

#[test]
fn super_calls_run_the_base_method() {
    let t = slice(
        "@Autowired private OrderFlow flow;",
        &[
            (
                "api/OrderFlow.java",
                "public interface OrderFlow { Order save(Order x); }",
            ),
            (
                "api/BaseFlow.java",
                "public abstract class BaseFlow implements OrderFlow {
    @Autowired private Orders orders;
    @Override public Order save(Order x) { return orders.save(x); }
}
",
            ),
            (
                "api/AuditedFlow.java",
                "@Service public class AuditedFlow extends BaseFlow {
    @Override public Order save(Order x) { return super.save(x); }
}
",
            ),
        ],
    );
    assert!(t.contains("AuditedFlow.save"), "{t}");
    assert!(t.contains("BaseFlow.save"), "{t}");
    assert!(t.contains("→ collection orders"), "{t}");
}

#[test]
fn a_bean_that_keeps_a_default_method_runs_it() {
    let t = slice(
        "@Autowired @Qualifier(\"plain\") private OrderFlow flow;",
        &[
            (
                "api/OrderFlow.java",
                "public interface OrderFlow {
    default Order save(Order x) { return Store.put(x); }
}
",
            ),
            (
                "api/Store.java",
                "public class Store {
    static MongoTemplate mongoTemplate;
    static Order put(Order x) { return mongoTemplate.save(x, \"orders\"); }
}
",
            ),
            (
                "api/LoudFlow.java",
                "@Service(\"loud\") public class LoudFlow implements OrderFlow {
    @Override public Order save(Order x) { return x; }
}
",
            ),
            (
                "api/PlainFlow.java",
                "@Service(\"plain\") public class PlainFlow implements OrderFlow { }",
            ),
        ],
    );
    assert!(t.contains("→ collection orders"), "{t}");
    assert!(!t.contains("LoudFlow"), "{t}");
}

#[test]
fn two_beans_that_inherit_one_method_both_run_it() {
    let t = slice(
        "@Autowired @Qualifier(\"second\") private OrderFlow flow;",
        &[
            (
                "api/OrderFlow.java",
                "public interface OrderFlow { Order save(Order x); }",
            ),
            (
                "api/BaseFlow.java",
                "public abstract class BaseFlow implements OrderFlow {
    @Autowired private Orders orders;
    @Override public Order save(Order x) { return orders.save(x); }
}
",
            ),
            (
                "api/AFlow.java",
                "@Service(\"first\") public class AFlow extends BaseFlow { }",
            ),
            (
                "api/BFlow.java",
                "@Service(\"second\") public class BFlow extends BaseFlow { }",
            ),
        ],
    );
    assert!(t.contains("BaseFlow.save"), "{t}");
    assert!(t.contains("→ collection orders"), "{t}");
}

#[test]
fn a_call_no_bean_serves_keeps_its_own_edge() {
    let t = slice(
        "@Autowired @Qualifier(\"from-a-jar\") private OrderFlow flow;",
        &[
            (
                "api/OrderFlow.java",
                "public class OrderFlow {
    @Autowired private Orders orders;
    public Order save(Order x) { return orders.save(x); }
}
",
            ),
            (
                "api/OtherFlow.java",
                "@Service public class OtherFlow extends OrderFlow {
    @Override public Order save(Order x) { return x; }
}
",
            ),
        ],
    );
    assert!(t.contains("OrderFlow.save"), "{t}");
    assert!(t.contains("→ collection orders"), "{t}");
}

#[test]
fn of_two_sinks_alike_the_first_line_wins() {
    let t = slice(
        "@Autowired private Flow flow;",
        &[(
            "api/Flow.java",
            "@Service public class Flow {
    private com.mongodb.client.MongoDatabase db;
    public Order save(Order x) {
        db.getCollection(\"zeta\");
        db.getCollection(\"alpha\");
        return x;
    }
}
",
        )],
    );
    // The file names the driver only by its package: it imports Spring Data Mongo.
    assert!(t.contains("→ collection zeta (Mongo)"), "{t}");
}

#[test]
fn a_decorator_calls_the_other_beans() {
    let t = slice(
        "@Autowired @Qualifier(\"outer\") private OrderFlow flow;",
        &[
            (
                "api/OrderFlow.java",
                "public interface OrderFlow { Order save(Order x); }",
            ),
            (
                "api/Outer.java",
                "@Service(\"outer\") public class Outer implements OrderFlow {
    @Override public Order save(Order x) { OrderFlow next = find(); return next.save(x); }
}
",
            ),
            (
                "api/Inner.java",
                "@Service(\"inner\") public class Inner implements OrderFlow {
    @Autowired private Orders orders;
    @Override public Order save(Order x) { return orders.save(x); }
}
",
            ),
        ],
    );
    assert!(t.contains("Inner.save"), "{t}");
    assert!(t.contains("→ collection orders"), "{t}");
}

#[test]
fn flows_do_not_go_from_a_base_method_back_to_its_override() {
    use cartograph_query::flow::{CallIndex, FlowOptions, flow};
    let r = repo(&[
        (
            "api/BaseFlow.java",
            "public class BaseFlow { public void save() { done(); } void done() { } }",
        ),
        (
            "api/AuditedFlow.java",
            "@Service public class AuditedFlow extends BaseFlow {
    @Override public void save() { super.save(); }
}
",
        ),
    ]);
    let ix = CallIndex::build(&r);
    let g = flow(
        &r,
        &ix,
        "sym:api/AuditedFlow.java#AuditedFlow.save",
        &FlowOptions::default(),
    )
    .unwrap();
    assert!(g.edges.iter().any(|e| e.dst.ends_with("BaseFlow.save")));
    assert!(
        !g.edges
            .iter()
            .any(|e| e.src.ends_with("#BaseFlow.save") && e.dst.ends_with("AuditedFlow.save")),
        "{:?}",
        g.edges
    );
}

// 4 ----------------------------------------------------------------------------------

const GATEWAYS: [(&str, &str); 3] = [
    (
        "api/Gateway.java",
        "public interface Gateway { void charge(); }",
    ),
    (
        "api/Card.java",
        "@Service @Primary public class Card implements Gateway { @Override public void charge() { } }",
    ),
    (
        "api/Wire.java",
        "@Service(\"wire\") public class Wire implements Gateway { @Override public void charge() { } }",
    ),
];

fn gateways(more: &[(&str, &str)]) -> Repo {
    let mut v = GATEWAYS.to_vec();
    v.extend_from_slice(more);
    repo(&v)
}

#[test]
fn a_receiver_spring_did_not_inject_is_not_narrowed() {
    let r = gateways(&[(
        "api/Till.java",
        "@Service public class Till {
    @Autowired private Gateway gateway;
    void injected() { gateway.charge(); }
    void given(Gateway g) { g.charge(); }
    void shadowed(Gateway gateway) { gateway.charge(); }
    void local() { Gateway gw = pick(); gw.charge(); }
    void explicit() { this.gateway.charge(); }
}
",
    )]);
    let t = callers(&r, "Wire.charge");
    for m in ["given", "shadowed", "local"] {
        assert!(t.contains(&format!("Till.{m}()")), "{m}: {t}");
    }
    assert!(!t.contains("Till.injected()"), "{t}");
    assert!(!t.contains("Till.explicit()"), "{t}");
    let t = callers(&r, "Card.charge");
    assert!(
        t.contains("Till.injected() [resolved, via Gateway.charge (Spring bean)]"),
        "{t}"
    );
    assert!(
        t.contains("Till.given() [resolved, via Gateway.charge]"),
        "{t}"
    );
}

#[test]
fn an_injected_parameter_is_followed_into_its_field_and_loops() {
    let r = gateways(&[
        (
            "api/Till.java",
            "@Service public class Till {
    private final Gateway main;
    private Gateway other;
    public Till(Gateway gateway) { this.main = Objects.requireNonNull(gateway); }
    void viaField() { main.charge(); }
    void notInjected() { other.charge(); }
}
",
        ),
        (
            "api/Batch.java",
            "@Service public class Batch {
    private final List<Gateway> all;
    public Batch(@Qualifier(\"wire\") List<Gateway> gateways) { this.all = List.copyOf(gateways); }
    void loop() { for (Gateway g : all) { g.charge(); } }
    void lambda() { this.all.forEach(g -> g.charge()); }
    void typed() { all.stream().forEach((Gateway g) -> g.charge()); }
}
",
        ),
    ]);
    let t = callers(&r, "Wire.charge");
    assert!(!t.contains("Till.viaField()"), "{t}");
    assert!(t.contains("Till.notInjected()"), "{t}");
    for m in ["loop", "lambda", "typed"] {
        let want = format!("Batch.{m}() [resolved, via Gateway.charge (Spring bean)]");
        assert!(t.contains(&want), "{want}: {t}");
    }
    let t = callers(&r, "Card.charge");
    assert!(!t.contains("Batch."), "{t}");
}

#[test]
fn a_constructor_that_delegates_hands_its_parameters_over() {
    let r = gateways(&[(
        "api/Till.java",
        "@Service public class Till {
    private final Gateway main;
    private final int tries;
    @Autowired public Till(Gateway gateway) { this(gateway, 3); }
    Till(Gateway any, int tries) { this.main = any; this.tries = tries; }
    void pay() { main.charge(); }
}
",
    )]);
    let t = callers(&r, "Wire.charge");
    assert!(!t.contains("Till.pay()"), "{t}");
    let t = callers(&r, "Card.charge");
    assert!(
        t.contains("Till.pay() [resolved, via Gateway.charge (Spring bean)]"),
        "{t}"
    );
}

// 5 ----------------------------------------------------------------------------------

#[test]
fn every_collection_takes_every_bean_and_a_provider_streams_them() {
    let r = gateways(&[(
        "api/Pool.java",
        "@Service public class Pool {
    private final Deque<Gateway> queue;
    private final ConcurrentMap<String, Gateway> byName;
    private final ObjectProvider<Gateway> provider;
    public Pool(Deque<Gateway> queue, ConcurrentMap<String, Gateway> byName,
                ObjectProvider<Gateway> provider) {
        this.queue = queue; this.byName = byName; this.provider = provider;
    }
    void deque() { queue.forEach(g -> g.charge()); }
    void map() { byName.values().forEach(g -> g.charge()); }
    void stream() { provider.orderedStream().forEach(g -> g.charge()); }
    void each() { provider.forEach(g -> g.charge()); }
    void one() { provider.getObject().charge(); }
    void both() { provider.forEach(g -> g.charge()); provider.getObject().charge(); }
}
",
    )]);
    let t = callers(&r, "Wire.charge");
    for m in ["deque", "map", "stream", "each", "both"] {
        let want = format!("Pool.{m}() [resolved, via Gateway.charge (Spring bean)]");
        assert!(t.contains(&want), "{want}: {t}");
    }
    assert!(!t.contains("Pool.one()"), "{t}");
    let t = callers(&r, "Gateway.charge");
    assert!(t.contains("Deque<Gateway>) api/Pool.java"), "{t}");
    assert!(!t.contains("@Primary, of 2"), "{t}");
}

// 6 ----------------------------------------------------------------------------------

#[test]
fn qualifiers_and_names_written_as_constants_and_aliases() {
    let r = gateways(&[
        (
            "api/Names.java",
            "public class Names { public static final String WIRE = \"wire\"; public static final String SLOW = \"slow\"; }",
        ),
        (
            "api/Slow.java",
            "@Service(Names.SLOW) public class Slow implements Gateway { @Override public void charge() { } }",
        ),
        (
            "api/Extra.java",
            "@Configuration public class Extra { @Bean({\"first\", \"second\"}) Gateway made() { return null; } @Bean(Elsewhere.OTHER) Gateway odd() { return null; } }",
        ),
        (
            "api/Till.java",
            "@Service public class Till {
    @Autowired @Qualifier(Names.WIRE) private Gateway constant;
    @Autowired @Qualifier(Names.SLOW) private Gateway slow;
    @Autowired @Qualifier(\"second\") private Gateway alias;
    @Autowired @Qualifier(Elsewhere.NAME) private Gateway unknown;
    void byUnknown() { unknown.charge(); }
}
",
        ),
    ]);
    let t = callers(&r, "Gateway.charge");
    for want in [
        "field constant, Gateway @Qualifier(\"wire\")) api/Till.java:3: Wire (qualified)",
        "field slow, Gateway @Qualifier(\"slow\")) api/Till.java:4: Slow (qualified)",
        "field alias, Gateway @Qualifier(\"second\")) api/Till.java:5: first (@Bean) (qualified)",
        "field unknown, Gateway @Qualifier(?)) api/Till.java:6: qualified by a name not known",
    ] {
        assert!(t.contains(want), "{want}: {t}");
    }
    // A bean whose name is not known is shown by the method that makes it.
    assert!(t.contains("odd (@Bean)"), "{t}");
    // Not narrowed: the primary one does not win over a qualifier.
    let t = callers(&r, "Wire.charge");
    assert!(t.contains("Till.byUnknown()"), "{t}");
}

// 10 ---------------------------------------------------------------------------------

#[test]
fn a_spring_data_repository_is_a_bean_and_a_type_without_one_is_not() {
    let r = repo(&[
        (
            "api/Order.java",
            "@Document(\"orders\") public class Order { }",
        ),
        (
            "api/Orders.java",
            "public interface Orders extends MongoRepository<Order, String> { Order findByCode(String c); }",
        ),
        (
            "api/Remote.java",
            "public interface Remote { void ping(); }",
        ),
        (
            "api/Desk.java",
            "@Service public class Desk {
    @Autowired private Orders orders;
    @Autowired private Remote remote;
    void work() { orders.findByCode(\"a\"); remote.ping(); }
}
",
        ),
    ]);
    let t = callers(&r, "Orders.findByCode");
    assert!(t.contains("Orders (the only bean)"), "{t}");
    assert!(!t.contains("no bean of the type"), "{t}");
    let t = callers(&r, "Remote.ping");
    assert!(!t.contains("the Remote bean"), "{t}");
    assert!(t.contains("no bean of the type in the repo"), "{t}");
}

// `@Resource` ------------------------------------------------------------------------

#[test]
fn a_resource_without_a_name_is_found_by_its_member_before_its_type() {
    for pkg in ["javax", "jakarta"] {
        let till = format!(
            "@Service public class Till {{
    @{pkg}.annotation.Resource private Gateway wire;
    @{pkg}.annotation.Resource private Gateway other;
    @{pkg}.annotation.Resource(name = \"wire\") private Gateway named;
    private Gateway held;
    @{pkg}.annotation.Resource public void setWire(Gateway any) {{ this.held = any; }}
    @{pkg}.annotation.Resource private List<Gateway> card;
    void byField() {{ wire.charge(); }}
    void byType() {{ other.charge(); }}
    void byName() {{ named.charge(); }}
    void bySetter() {{ held.charge(); }}
    void all() {{ card.forEach(g -> g.charge()); }}
}}
"
        );
        let r = gateways(&[("api/Till.java", &till)]);
        let t = callers(&r, "Wire.charge");
        for m in ["byField", "byName", "bySetter", "all"] {
            let want = format!("Till.{m}() [resolved, via Gateway.charge (Spring bean)]");
            assert!(t.contains(&want), "{pkg} {want}: {t}");
        }
        assert!(!t.contains("Till.byType()"), "{pkg}: {t}");
        let t = callers(&r, "Card.charge");
        for m in ["byType", "all"] {
            assert!(t.contains(&format!("Till.{m}()")), "{pkg} {m}: {t}");
        }
        for m in ["byField", "byName", "bySetter"] {
            assert!(!t.contains(&format!("Till.{m}()")), "{pkg} {m}: {t}");
        }
        let t = callers(&r, "Gateway.charge");
        for want in [
            "field wire, Gateway @Resource) api/Till.java:3: Wire (by name, of 2)",
            "field other, Gateway @Resource) api/Till.java:4: Card (@Primary, of 2)",
            "field named, Gateway @Resource(name = \"wire\")) api/Till.java:5: Wire (by name, of 2)",
            "setter any, Gateway @Resource) api/Till.java:7: Wire (by name, of 2)",
        ] {
            assert!(t.contains(want), "{pkg} {want}: {t}");
        }
    }
}

#[test]
fn a_resource_narrows_nothing_it_cannot_tell() {
    // The name of a candidate is not known.
    let r = gateways(&[
        (
            "api/Odd.java",
            "@Service(Elsewhere.NAME) public class Odd implements Gateway { @Override public void charge() { } }",
        ),
        (
            "api/Till.java",
            "@Service public class Till {
    @jakarta.annotation.Resource private Gateway wire;
    @jakarta.annotation.Resource(name = \"wire\") private Gateway named;
    void byField() { wire.charge(); }
    void byName() { named.charge(); }
}
",
        ),
    ]);
    for sym in ["Card.charge", "Wire.charge", "Odd.charge"] {
        let t = callers(&r, sym);
        assert!(t.contains("Till.byField()"), "{sym}: {t}");
        assert!(t.contains("Till.byName()"), "{sym}: {t}");
    }
    // A name written as an expression, or looked up elsewhere; a qualifier is no name.
    let r = gateways(&[
        (
            "api/Slow.java",
            "@Service @Qualifier(\"late\") public class Slow implements Gateway { @Override public void charge() { } }",
        ),
        (
            "api/Till.java",
            "@Service public class Till {
    @jakarta.annotation.Resource(name = Elsewhere.NAME) private Gateway wire;
    @jakarta.annotation.Resource(lookup = \"java:/wire\") private Gateway slow;
    @jakarta.annotation.Resource(name = \"late\") private Gateway late;
    void expression() { wire.charge(); }
    void lookup() { slow.charge(); }
    void qualifier() { late.charge(); }
}
",
        ),
    ]);
    for sym in ["Card.charge", "Wire.charge"] {
        let t = callers(&r, sym);
        for m in ["expression", "lookup", "qualifier"] {
            assert!(t.contains(&format!("Till.{m}()")), "{sym} {m}: {t}");
        }
    }
    let t = callers(&r, "Gateway.charge");
    assert!(
        t.contains("@Resource(name = \"late\")) api/Till.java:5: no bean of that name"),
        "{t}"
    );
    assert!(
        t.contains("@Resource(name = ?)) api/Till.java:3: named by what is not known, 3 fit"),
        "{t}"
    );
}

#[test]
fn a_nested_bean_and_a_generated_mapper_have_the_names_spring_gives_them() {
    // `outer.Wire`, `gateMapperImpl`: no field is called like that.
    let r = gateways(&[
        (
            "api/Outer.java",
            "public class Outer { @Service static class Inner implements Gateway { @Override public void charge() { } } }",
        ),
        (
            "api/GateMapper.java",
            "@org.mapstruct.Mapper(componentModel = \"spring\") public interface GateMapper extends Gateway { }",
        ),
        (
            "api/Till.java",
            "@Service public class Till {
    @jakarta.annotation.Resource private Gateway inner;
    @jakarta.annotation.Resource private Gateway gateMapper;
    void one() { inner.charge(); }
    void two() { gateMapper.charge(); }
}
",
        ),
    ]);
    let t = callers(&r, "Card.charge");
    assert!(t.contains("Till.one()") && t.contains("Till.two()"), "{t}");
    let t = callers(&r, "Gateway.charge");
    assert_eq!(t.matches("Card (@Primary, of 4)").count(), 2, "{t}");
}

// Second review ----------------------------------------------------------------------

fn flow_of(r: &Repo, from: &str) -> Vec<(String, String)> {
    use cartograph_query::flow::{CallIndex, FlowOptions, flow};
    let ix = CallIndex::build(r);
    let name = |id: &str| id.rsplit('#').next().unwrap_or(id).to_string();
    flow(r, &ix, from, &FlowOptions::default())
        .unwrap()
        .edges
        .iter()
        .map(|e| (name(&e.src), name(&e.dst)))
        .collect()
}

#[test]
fn flows_go_on_from_an_abstract_class_to_its_beans() {
    let r = repo(&[
        (
            "api/Exporter.java",
            "public abstract class Exporter {
    public abstract void write();
    public void run() { write(); }
    public void close() { done(); }
    void done() { }
}
",
        ),
        (
            "api/CsvExporter.java",
            "@Service public class CsvExporter extends Exporter {
    @Override public void write() { flush(); }
    @Override public void close() { super.close(); }
    void flush() { }
}
",
        ),
        (
            "api/Desk.java",
            "@Service public class Desk {
    @Autowired private Exporter exporter;
    void direct() { exporter.write(); }
    void template() { exporter.run(); }
}
",
        ),
    ]);
    let has = |v: &[(String, String)], a: &str, b: &str| v.iter().any(|(x, y)| x == a && y == b);
    let f = flow_of(&r, "sym:api/Desk.java#Desk.direct");
    assert!(has(&f, "CsvExporter.write", "CsvExporter.flush"), "{f:?}");
    let f = flow_of(&r, "sym:api/Desk.java#Desk.template");
    assert!(has(&f, "CsvExporter.write", "CsvExporter.flush"), "{f:?}");
    let f = flow_of(&r, "sym:api/CsvExporter.java#CsvExporter.close");
    assert!(has(&f, "CsvExporter.close", "Exporter.close"), "{f:?}");
    assert!(!has(&f, "Exporter.close", "CsvExporter.close"), "{f:?}");
}

#[test]
fn two_beans_of_a_type_with_names_not_known_are_two() {
    let r = gateways(&[
        (
            "api/Extra.java",
            "@Configuration public class Extra {
    @Bean(Elsewhere.A) Gateway one() { return null; }
    @Bean(Elsewhere.B) Gateway two() { return null; }
}
",
        ),
        (
            "api/Till.java",
            "@Service public class Till {
    @Autowired private List<Gateway> all;
    void pay() { all.forEach(g -> g.charge()); }
}
",
        ),
    ]);
    let t = callers(&r, "Gateway.charge");
    assert!(
        t.contains("4 beans: Card, Wire, one (@Bean), two (@Bean)"),
        "{t}"
    );
}

#[test]
fn aliases_count_for_qualifiers_and_member_names() {
    let r = repo(&[
        (
            "api/Printer.java",
            "public interface Printer { void print(); }",
        ),
        (
            "api/Config.java",
            "@Configuration public class Config {
    @Bean({\"main\", Elsewhere.ALIAS}) Printer main() { return null; }
    @Bean({\"laser\", \"fast\"}) Printer laser() { return null; }
}
",
        ),
        (
            "api/Desk.java",
            "@Service public class Desk {
    @Autowired @Qualifier(\"other\") private Printer unknown;
    @Autowired private Printer fast;
    void a() { unknown.print(); fast.print(); }
}
",
        ),
    ]);
    let t = callers(&r, "Printer.print");
    assert!(
        t.contains("@Qualifier(\"other\")) api/Desk.java:3: qualified by a name not known, 1 fit: main (@Bean)"),
        "{t}"
    );
    assert!(
        t.contains("field fast, Printer) api/Desk.java:4: laser (@Bean) (by name, of 2)"),
        "{t}"
    );
}

#[test]
fn a_lombok_field_another_method_sets_is_not_narrowed() {
    let r = gateways(&[(
        "api/Till.java",
        "@lombok.AllArgsConstructor @Service public class Till {
    private Gateway gateway;
    private Gateway kept;
    public void use(Gateway other) { this.gateway = other; }
    void pay() { gateway.charge(); }
    void keep() { kept.charge(); }
}
",
    )]);
    let t = callers(&r, "Wire.charge");
    assert!(t.contains("Till.pay()"), "{t}");
    assert!(!t.contains("Till.keep()"), "{t}");
}

#[test]
fn a_parameter_given_another_value_is_not_what_spring_injects() {
    let r = gateways(&[
        (
            "api/Retry.java",
            "public class Retry implements Gateway { Retry(Gateway g) { } @Override public void charge() { } }",
        ),
        (
            "api/Till.java",
            "@Service public class Till {
    private final Gateway main;
    private Gateway twice;
    public Till(Gateway gateway, Gateway second) {
        gateway = new Retry(gateway);
        this.main = gateway;
        this.twice = second;
        this.twice = new Retry(this.twice);
        gateway.charge();
    }
    void pay() { main.charge(); }
    void again() { twice.charge(); }
}
",
        ),
        (
            "api/Safe.java",
            "@Service public class Safe {
    private final Gateway main;
    public Safe(Gateway gateway) {
        gateway = Objects.requireNonNull(gateway);
        this.main = gateway;
    }
    void pay() { main.charge(); }
}
",
        ),
    ]);
    let t = callers(&r, "Wire.charge");
    assert!(t.contains("Till.pay()"), "{t}");
    assert!(t.contains("Till.again()"), "{t}");
    assert!(
        t.contains("api/Till.java (3)"),
        "the constructor's own call: {t}"
    );
    assert!(!t.contains("Safe.pay()"), "{t}");
}

#[test]
fn a_repository_interface_takes_the_name_of_its_stereotype() {
    let r = repo(&[
        (
            "api/Order.java",
            "@Document(\"orders\") public class Order { }",
        ),
        (
            "api/Orders.java",
            "public interface Orders extends MongoRepository<Order, String> { Order findByCode(String c); }",
        ),
        (
            "api/LiveOrders.java",
            "@Repository(\"live\") public interface LiveOrders extends Orders, MongoRepository<Order, String> { }",
        ),
        (
            "api/Desk.java",
            "@Service public class Desk {
    @Autowired @Qualifier(\"live\") private Orders orders;
    void work() { orders.findByCode(\"a\"); }
}
",
        ),
    ]);
    let t = callers(&r, "Orders.findByCode");
    assert!(t.contains("LiveOrders (qualified)"), "{t}");
}

#[test]
fn a_bean_injected_by_its_supertype_is_injected_somewhere() {
    let r = repo(&[
        (
            "api/Printer.java",
            "public interface Printer { void print(); }",
        ),
        (
            "api/Laser.java",
            "public class Laser implements Printer { @Override public void print() { } public void warm() { } }",
        ),
        (
            "api/Alone.java",
            "public class Alone { public void hum() { } }",
        ),
        (
            "api/Config.java",
            "@Configuration public class Config {
    @Bean Laser laser() { return new Laser(); }
    @Bean Alone alone() { return new Alone(); }
}
",
        ),
        (
            "api/Desk.java",
            "@Service public class Desk {
    @Autowired private Printer printer;
    void a() { printer.print(); }
}
",
        ),
    ]);
    let t = callers(&r, "Laser.warm");
    assert!(!t.contains("injected nowhere"), "{t}");
    assert!(
        t.contains("injected into Desk (field printer, Printer) api/Desk.java:3"),
        "{t}"
    );
    let t = callers(&r, "Alone.hum");
    assert!(
        t.contains("Spring: 1 bean of Alone: alone (@Bean); no injection point of it found"),
        "{t}"
    );
}
