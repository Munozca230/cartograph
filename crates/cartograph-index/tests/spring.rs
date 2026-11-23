//! Spring DI and data sinks (`tests/fixtures/workspace-slice`): beans with their names,
//! `@Primary` and qualifiers, the injection points Spring uses (a Lombok constructor, the
//! `@Autowired` one of two constructors, fields), calls typed through a list's elements in
//! stream lambdas, and the collections a method reads or writes (a constant name with an
//! entity class, a Spring Data repository of a `@Document`), never a string that only
//! looks like one.

use cartograph_core::{Confidence, Symbols};
use cartograph_index::{IndexOptions, index};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/workspace-slice")
}

fn symbols() -> Symbols {
    index(&fixture(), &IndexOptions::default()).unwrap().symbols
}

const API: &str = "api/src/main/java/com/shop/api";

#[test]
fn beans_and_injection_points() {
    let s = symbols();
    let beans: Vec<String> = s
        .beans
        .iter()
        .map(|b| {
            let ty = b.ty.rsplit('#').next().unwrap_or("");
            format!("{ty} {}{}", b.name, if b.primary { " primary" } else { "" })
        })
        .collect();
    for want in [
        "SeasonRule seasonRule",
        "VolumeRule volumeRule",
        "CouponRule coupons",
        "CardGateway cardGateway primary",
        "WireGateway wire",
        "OrderServiceImpl orderServiceImpl",
    ] {
        assert!(beans.contains(&want.to_string()), "{want}: {beans:?}");
    }
    assert!(
        !beans.iter().any(|b| b.starts_with("LegacyRule")),
        "{beans:?}"
    );
    let points: Vec<String> = s
        .injections
        .iter()
        .map(|i| {
            format!(
                "{} {} {} {}<{}>{}",
                i.owner.rsplit('#').next().unwrap_or(""),
                i.how,
                i.member,
                i.wrapper,
                i.ty.rsplit('#').next().unwrap_or(""),
                i.qualifier
                    .as_deref()
                    .map_or(String::new(), |q| format!(" @{q}"))
            )
        })
        .collect();
    assert_eq!(
        points,
        [
            "Desk constructor notifier <Notifier>",
            "Desk constructor printer <Printer>",
            "Desk constructor scanner <Scanner>",
            "OrderController constructor orderService <OrderService>",
            "CheckoutService field gateway <PaymentGateway>",
            "CheckoutService field receipts <ReceiptRepository>",
            "CheckoutService field wireGateway <PaymentGateway> @wire",
            "DiscountBoard field rules List<PriceRule>",
            "PricingEngine constructor rules List<PriceRule>",
        ]
    );
}

#[test]
fn calls_in_stream_lambdas_reach_the_interface_method() {
    let s = symbols();
    let target = format!("sym:{API}/pricing/PriceRule.java#PriceRule.discounts");
    let mut at: Vec<String> = s
        .calls
        .iter()
        .filter(|c| c.target == target)
        .map(|c| format!("{}:{} [{}]", c.file, c.line, c.confidence))
        .collect();
    at.sort();
    assert_eq!(
        at,
        [
            format!("{API}/pricing/DiscountBoard.java:15 [resolved]"),
            format!("{API}/pricing/PricingEngine.java:21 [resolved]"),
        ]
    );
}

#[test]
fn data_sinks_by_method() {
    let s = symbols();
    let sinks: Vec<String> = s
        .sinks
        .iter()
        .map(|d| {
            format!(
                "{} {} {} {} [{}]",
                d.symbol
                    .as_deref()
                    .and_then(|x| x.rsplit('#').next())
                    .unwrap_or("-"),
                d.store,
                d.name,
                d.via,
                d.confidence
            )
        })
        .collect();
    assert_eq!(
        sinks,
        [
            "Counters.bump mongo counters getCollection [exact]",
            "OrderStore.byId mongo orders getCollection [resolved]",
            "OrderStore.save mongo orders getCollection [resolved]",
            "CheckoutService.pay mongo receipts ReceiptRepository.save [resolved]",
            "Receipt mongo receipts @Document [exact]",
            "ReceiptRepository mongo receipts MongoRepository<Receipt> [exact]",
        ]
    );
    // A string that names no collection (`next("orders")`) is no sink.
    assert!(
        !s.sinks.iter().any(|d| d
            .symbol
            .as_deref()
            .is_some_and(|x| x.ends_with("OrderServiceImpl.check"))),
        "{sinks:?}"
    );
    assert!(
        s.sinks
            .iter()
            .all(|d| d.confidence != Confidence::Heuristic)
    );
}
