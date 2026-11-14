//! `Engine::fill_drop_reason` answers what the fold DOES: for every guard of the `Fill` arm of
//! `on_event` (and `Account::apply_fill`'s dedup and venue halves) one engine state and one fill, asserted twice:
//! the named reason, and `on_event` dropping that very fill. A clean fill reads `None` and folds. The reconcile
//! confirm (`vike_core`'s `confirm_reconcile`) refuses on the reason, so the two must never disagree.

use vike_execution::testing::RecordingClient;
use vike_execution::{AdmitOutcome, Engine, EventBusFold, FillDrop, SymbolAdmission};
use vike_model::events::FillEvent;
use vike_model::{ContractValue, OrderRecord, OrderRequest};

use crate::quote_currency::{
    MOUNTED, VENUE, engine, engine_in, fill_event, fold, plain, usdjpy_engine,
};

/// An order of the engine's registry on `symbol`, which the engine does not trade (an option ticket's shape).
fn own_order_on(e: &mut Engine<RecordingClient>, symbol: &str) {
    let req = OrderRequest {
        order_client_id: "own1".into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(2000.0),
        ..Default::default()
    };
    e.registry.insert("own1".to_string(), OrderRecord::new(req));
}

#[test]
fn every_guard_of_the_fill_fold_is_named_and_is_what_the_fold_does() {
    type Case = (&'static str, Engine<RecordingClient>, FillEvent, Option<FillDrop>);
    let mut cases: Vec<Case> = vec![
        ("clean", engine(), fill_event("t1", MOUNTED, 1, 1.0, 1.1), None),
        (
            "untraded",
            engine(),
            fill_event("t1", "GBPUSD", 1, 1.0, 1.3),
            Some(FillDrop::UntradedSymbol),
        ),
    ];

    let mut inverse = engine();
    let btc =
        SymbolAdmission { contract_value: Some(ContractValue::inverse(1.0)), ..plain("BTCUSD") };
    assert_eq!(inverse.admit_symbol(&btc), AdmitOutcome::Admitted);
    assert!(inverse.account.kind_is_unknown_for(MOUNTED), "PREMISE: no grid row for the mount");
    cases.push((
        "inverse kind",
        inverse,
        fill_event("t1", MOUNTED, 1, 1.0, 1.1),
        Some(FillDrop::UnknownContractKind),
    ));

    let mut foreign = usdjpy_engine(1.0 / 150.0);
    own_order_on(&mut foreign, "XAUUSD");
    let ticket =
        FillEvent { order_client_id: "own1".into(), ..fill_event("t1", "XAUUSD", 1, 1.0, 2000.0) };
    cases.push(("foreign quote", foreign, ticket, Some(FillDrop::UnknownQuoteCurrency)));

    let mut unrated = engine_in("EUR");
    assert_eq!(unrated.declare_mount_quote_currency("USD"), Ok(()));
    cases.push((
        "unrated",
        unrated,
        fill_event("t1", MOUNTED, 1, 1.0, 1.16),
        Some(FillDrop::UnratedQuote),
    ));

    cases.push((
        "non-finite",
        engine(),
        fill_event("t1", MOUNTED, 1, 1.0, f64::NAN),
        Some(FillDrop::NonFinite),
    ));
    cases.push((
        "foreign venue",
        engine(),
        FillEvent { venue: "binance".into(), ..fill_event("t1", MOUNTED, 1, 1.0, 1.1) },
        Some(FillDrop::ForeignVenue),
    ));

    let mut folded = engine();
    assert_eq!(fold(&mut folded, fill_event("t1", MOUNTED, 1, 1.0, 1.1)), EventBusFold::Applied);
    cases.push((
        "duplicate",
        folded,
        fill_event("t1", MOUNTED, 1, 1.0, 1.1),
        Some(FillDrop::AlreadyFolded),
    ));
    let mut collided = engine();
    assert_eq!(fold(&mut collided, fill_event("t1", MOUNTED, 1, 1.0, 1.1)), EventBusFold::Applied);
    cases.push((
        "collision",
        collided,
        fill_event("t1", MOUNTED, -1, 2.0, 1.2),
        Some(FillDrop::AlreadyFolded),
    ));

    for (name, mut e, f, want) in cases {
        assert_eq!(e.fill_drop_reason(&f), want, "{name}: the named reason");
        let did = fold(&mut e, f);
        let expected = if want.is_some() { EventBusFold::Dropped } else { EventBusFold::Applied };
        assert_eq!(did, expected, "{name}: the fold did what the reason said");
        if let Some(reason) = want {
            assert!(!reason.words().is_empty(), "{name}: an operator sentence");
        }
    }
}
