//! Spring DI and slices in the queries (`tests/fixtures/workspace-slice`): callers of an
//! interface method with the beans injected where it is called, callers of a bean's
//! method through the interface (a qualified and a primary gateway), and `path` from a
//! view to an HTTP client continued past the client down to the Mongo collection.

mod common;

use cartograph_query::{Repo, budget, path, symbols};

fn repo() -> Repo {
    common::fixture("workspace-slice")
}

#[test]
fn callers_of_an_interface_method_list_the_injected_beans() {
    let r = repo();
    let a = symbols::callers(&r, "PriceRule.discounts").unwrap();
    let t = &a.text;
    // The calls in stream lambdas, through the injected list.
    assert!(
        t.contains("api/src/main/java/com/shop/api/pricing/PricingEngine.java"),
        "{t}"
    );
    assert!(
        t.contains("api/src/main/java/com/shop/api/pricing/DiscountBoard.java"),
        "{t}"
    );
    assert!(
        t.contains("injected into PricingEngine (constructor, List<PriceRule>)"),
        "{t}"
    );
    assert!(
        t.contains("3 beans: CouponRule, SeasonRule, VolumeRule"),
        "{t}"
    );
    assert!(
        t.contains("injected into DiscountBoard (field rules, List<PriceRule>)"),
        "{t}"
    );
    assert!(t.contains("the same 3 beans"), "{t}");
    // Not a bean: never listed as injected.
    assert!(!t.contains("LegacyRule"), "{t}");
    assert!(budget::tokens(t) <= budget::limits::CALLERS, "{t}");
}

#[test]
fn a_qualifier_and_a_primary_pick_one_gateway_each() {
    let r = repo();
    let a = symbols::callers(&r, "PaymentGateway.charge").unwrap();
    let t = &a.text;
    assert!(
        t.contains("injected into CheckoutService (field gateway, PaymentGateway)"),
        "{t}"
    );
    assert!(t.contains("CardGateway (@Primary, of 2)"), "{t}");
    assert!(
        t.contains("(field wireGateway, PaymentGateway @Qualifier(\"wire\"))"),
        "{t}"
    );
    assert!(t.contains("WireGateway (qualified)"), "{t}");

    // The primary bean is called through the interface where it is injected; the
    // qualified call goes to the other bean.
    let a = symbols::callers(&r, "CardGateway.charge").unwrap();
    let t = &a.text;
    assert!(
        t.contains(":24 CheckoutService.pay() [resolved, via PaymentGateway.charge (Spring bean)]"),
        "{t}"
    );
    assert!(!t.contains(":22 "), "{t}");
    assert!(
        t.contains("1 call through PaymentGateway.charge reaches another bean"),
        "{t}"
    );
    let a = symbols::callers(&r, "WireGateway.charge").unwrap();
    let t = &a.text;
    assert!(
        t.contains(":22 CheckoutService.pay() [resolved, via PaymentGateway.charge (Spring bean)]"),
        "{t}"
    );
    assert!(!t.contains(":24 "), "{t}");
}

#[test]
fn a_bean_of_a_list_is_reached_through_the_interface() {
    let r = repo();
    let a = symbols::callers(&r, "CouponRule.discounts").unwrap();
    let t = &a.text;
    assert!(t.contains("PricingEngine.java"), "{t}");
    assert!(t.contains("via PriceRule.discounts (Spring bean)"), "{t}");
    // Not a bean (it may be built with `new`): a plain supertype hop, never a Spring one.
    let a = symbols::callers(&r, "LegacyRule.discounts").unwrap();
    assert!(a.text.contains("via PriceRule.discounts]"), "{}", a.text);
    assert!(!a.text.contains("Spring bean"), "{}", a.text);
}

#[test]
fn a_call_spring_cannot_narrow_keeps_its_plain_hop() {
    let r = repo();
    let at = |line: u32| format!(":{line} Desk.work()");
    // The bean inherits the method from a base that is no bean.
    let a = symbols::callers(&r, "BaseNotifier.send").unwrap().text;
    assert!(a.contains(&at(18)), "{a}");
    assert!(a.contains("via Notifier.send (Spring bean)"), "{a}");
    // A `@Bean` method that returns the interface: the class is not known.
    let a = symbols::callers(&r, "LaserPrinter.print").unwrap().text;
    assert!(
        a.contains(&at(19)) && a.contains("via Printer.print]"),
        "{a}"
    );
    // No bean of the type in the source.
    let a = symbols::callers(&r, "FlatScanner.scan").unwrap().text;
    assert!(
        a.contains(&at(20)) && a.contains("via Scanner.scan]"),
        "{a}"
    );
}

#[test]
fn path_to_an_http_client_continues_down_to_the_collection() {
    let r = repo();
    let a = path::path(
        &r,
        "desktop:src/renderer/views/OrderView.tsx",
        "desktop:src/main/api/OrderClient.ts",
    )
    .unwrap();
    let t = &a.text;
    let order = [
        "desktop/src/renderer/views/OrderView.tsx",
        "desktop/src/preload/preload.ts",
        "desktop/src/main/modules/OrderModule.ts",
        "desktop/src/main/api/OrderClient.ts",
        "POST /api/orders",
        "api/src/main/java/com/shop/api/order/OrderController.java:15 OrderController.create",
        "api/src/main/java/com/shop/api/order/OrderServiceImpl.java:13 OrderServiceImpl.create (Spring bean of OrderService)",
        "OrderServiceImpl.persist",
        "store/src/main/java/com/shop/store/OrderStore.java",
        "collection orders (Mongo, via Order)",
    ];
    let mut from = 0;
    for want in order {
        match t[from..].find(want) {
            Some(i) => from += i + want.len(),
            None => panic!("{want:?} missing or out of order:\n{t}"),
        }
    }
    // The save chain is the one shown in full; the load chain is there too.
    assert!(t.contains("saveOrder"), "{t}");
    assert!(t.contains("GET /api/orders/{}"), "{t}");
    // A string that only looks like a collection is no sink; the counters collection is
    // another sink of the same request, listed apart.
    assert!(t.contains("counters"), "{t}");
    assert!(budget::tokens(t) <= budget::limits::SLICE, "{t}");
}

#[test]
fn path_without_bridges_stays_strict() {
    let r = repo();
    let a = path::path_with(
        &r,
        "desktop:src/renderer/views/OrderView.tsx",
        "desktop:src/main/api/OrderClient.ts",
        false,
    )
    .unwrap();
    assert!(!a.text.contains("continues"), "{}", a.text);
}

#[test]
fn flows_go_on_from_an_interface_method_to_its_beans() {
    use cartograph_query::flow::{CallIndex, FlowOptions, SPRING_VIA, flow};
    let r = repo();
    let ix = CallIndex::build(&r);
    let g = flow(
        &r,
        &ix,
        "sym:api/src/main/java/com/shop/api/order/OrderController.java#OrderController.create",
        &FlowOptions::default(),
    )
    .unwrap();
    let edge = g
        .edges
        .iter()
        .find(|e| e.dst.ends_with("OrderServiceImpl.create"))
        .unwrap_or_else(|| panic!("{:?}", g.edges));
    assert!(edge.src.ends_with("OrderService.create"), "{edge:?}");
    assert_eq!(edge.via.as_deref(), Some(SPRING_VIA));
    // On down to the store of the other member.
    assert!(
        g.edges.iter().any(|e| e.dst.ends_with("OrderStore.save")),
        "{:?}",
        g.edges
    );
}
