//! Deribit-specific wiring tests (NO network): the `currency` derivation and the `modify`
//! default-no-op (Deribit has no native amend). The generic `run_loop` command→event tests
//! (mock `VenueRest`) moved WITH `run_loop` to `vike_bridge_core::exec_actor::run_loop_tests`.
use super::*;
use vike_bridge_core::rest::VenueRest;

fn req(coid: &str) -> OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "deribit", "symbol": "BTC-PERPETUAL",
        "side": 1, "qty": 10.0, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

/// `currency`/`kind` derivation (NO network): the perp scopes to BTC/future, an option to
/// BTC/option — the exact split `run` + the pump use to scope reconcile + the fill channel.
#[test]
fn currency_derives_from_symbol() {
    assert_eq!(currency_of("BTC-PERPETUAL"), "BTC");
    assert_eq!(currency_of("ETH-PERPETUAL"), "ETH");
    assert_eq!(currency_of("BTC-1JAN27-100000-C"), "BTC");
    assert_eq!(currency_of("sol-perpetual"), "SOL");
}

/// Deribit has no native amend wired (`private/edit` is not on `DeribitRest`) → `modify_order` is
/// the `VenueRest` DEFAULT no-op: it returns `[]` WITHOUT touching the socket (so an unconnected
/// transport is fine), and the declared caps advertise `supports_modify == false`.
#[test]
fn modify_is_default_noop_for_deribit() {
    let tx = DeribitOrderTransport::new("wss://test.invalid", "id", "secret", None);
    let rest = DeribitRest::new(tx, "BTC-PERPETUAL", SymbolProperties::default(), "BTC");
    assert!(rest.modify_order(&req("c1"), None, Some(51_000.0)).is_empty());
    assert!(!vike_model::caps_for("deribit").supports_modify);
}

/// An OPTION `public/get_instrument` response (real field shape — the same `tick_size` /
/// `min_trade_amount` / `contract_size` trio the r6 fixture pins for the plural
/// `get_instruments` twin) carries its contract size through the grid. Without this the
/// order-entry notional cap measures an option's notional as `qty * price`, missing the
/// per-contract underlying entirely.
#[test]
fn get_instrument_parses_option_contract_size() {
    let resp = json!({"result": {
        "instrument_name": "BTC-8JUL26-62000-C",
        "kind": "option",
        "tick_size": 0.0001,
        "min_trade_amount": 0.1,
        "contract_size": 1.0,
    }});
    let p = parse_get_instrument(&resp).expect("a well-formed option row parses");
    assert_eq!(p.tick_size, 0.0001);
    assert_eq!(p.step_size, 0.1);
    assert_eq!(p.min_qty, 0.1, "min_trade_amount doubles as the min qty");
    assert_eq!(p.contract_size, 1.0);
    assert_eq!(p.multiplier(), 1.0);
}

/// A FUTURE/inverse-perp row: Deribit quotes these per USD-denominated contract, so the
/// contract size is the value that must reach the multiplier grid. Whatever the venue reports
/// rides through untouched — the parse invents no scaling.
#[test]
fn get_instrument_parses_future_contract_size() {
    let resp = json!({"result": {
        "instrument_name": "BTC-PERPETUAL",
        "kind": "future",
        "tick_size": 0.5,
        "min_trade_amount": 10.0,
        "contract_size": 10.0,
    }});
    let p = parse_get_instrument(&resp).expect("a well-formed future row parses");
    assert_eq!(p.tick_size, 0.5);
    assert_eq!(p.contract_size, 10.0);
    assert_eq!(p.multiplier(), 10.0, "this is what closes the notional-cap gap");
}

/// Deribit encodes numbers as JSON strings on some rows (the r6 fixture contains BOTH forms for
/// the very same field), so `contract_size` must decode through `json_num` like its siblings.
#[test]
fn get_instrument_accepts_string_encoded_contract_size() {
    let resp = json!({"result": {
        "tick_size": "0.0001", "min_trade_amount": "0.1", "contract_size": "1",
    }});
    let p = parse_get_instrument(&resp).expect("string-encoded numbers parse");
    assert_eq!(p.contract_size, 1.0);
}

/// An absent or null `contract_size` (the fixture's `BTC-PERPETUAL` row has exactly this) must
/// degrade to the absent convention, NOT fail the parse — the grid stays usable and the
/// multiplier folds to the inert 1.0.
#[test]
fn get_instrument_tolerates_missing_contract_size() {
    for row in [
        json!({"result": {"tick_size": 0.5, "min_trade_amount": 0.1}}),
        json!({"result": {"tick_size": 0.5, "min_trade_amount": 0.1, "contract_size": null}}),
    ] {
        let p = parse_get_instrument(&row).expect("still a valid grid");
        assert_eq!(p.contract_size, 0.0);
        assert_eq!(p.multiplier(), 1.0);
        assert_eq!(p.tick_size, 0.5, "the rest of the grid is unaffected");
    }
}

/// A response with no `result` / no `tick_size` still yields `None` so the caller falls back to
/// its fallback grid — the new field did not loosen that gate.
#[test]
fn get_instrument_rejects_unusable_rows() {
    assert!(parse_get_instrument(&json!({})).is_none(), "no result envelope");
    assert!(
        parse_get_instrument(&json!({"result": {"contract_size": 10.0}})).is_none(),
        "contract_size alone is not a grid — tick_size still gates"
    );
}

// ---- the class the grid is stored with (venue `kind` + `settlement_period`) ----------------

/// Every kind this venue mounts reaches `SymbolProperties` through THIS parser — it is the one
/// `vike-mount`'s deribit arm calls and the one whose grid the PIT properties recorder writes —
/// so each carries the class the venue itself published. Until this landed the field was a flat
/// `None` for all three.
#[test]
fn get_instrument_carries_the_venues_own_class_for_every_kind() {
    let grid = |kind: &str, period: &str| {
        parse_get_instrument(&json!({"result": {
            "instrument_name": "BTC-PERPETUAL",
            "kind": kind,
            "settlement_period": period,
            "tick_size": 0.5,
            "min_trade_amount": 10.0,
        }}))
        .expect("a well-formed grid")
        .asset_class
    };
    assert_eq!(grid("option", "month"), Some(vike_model::AssetClass::Option));
    assert_eq!(grid("future", "perpetual"), Some(vike_model::AssetClass::CryptoPerp));
    assert_eq!(grid("future", "month"), Some(vike_model::AssetClass::CryptoFuture));
}

/// The classification is a REUSE, not a second judgement: this parser and
/// `crates/bridges/deribit/src/catalog.rs`'s `parse_instruments` are handed the same row and
/// must answer the same word. A copy of the mapping here would be free to drift from it.
#[test]
fn the_grid_and_the_catalog_classify_one_row_identically() {
    for (kind, period) in [("option", "month"), ("future", "perpetual"), ("future", "month")] {
        let row = json!({
            "instrument_name": "BTC-27JUN25", "kind": kind, "settlement_period": period,
            "is_active": true, "tick_size": 0.5, "min_trade_amount": 10.0,
        });
        let from_catalog = crate::catalog::parse_instruments(&json!({"result": [row.clone()]}));
        let from_grid = parse_get_instrument(&json!({"result": row})).expect("a valid grid");
        assert_eq!(from_grid.asset_class, Some(from_catalog[0].asset_class), "{kind}/{period}");
    }
}

/// A kind the venue publishes and this tree does not classify (`spot`, the two combo kinds) —
/// or a row carrying no `kind` at all, which is what every pre-existing fixture in this module
/// looks like — still parses into a USABLE grid with the class honestly ABSENT. The class is
/// never inferred from the instrument name
/// (`docs/decisions/0061-an-instrument-names-its-kind.md`).
#[test]
fn an_unclassifiable_kind_still_yields_a_grid_with_no_class() {
    for kind in [Some("spot"), Some("future_combo"), Some("option_combo"), None] {
        let mut result = json!({"tick_size": 0.5, "min_trade_amount": 10.0});
        result["instrument_name"] = json!("BTC-PERPETUAL");
        if let Some(k) = kind {
            result["kind"] = json!(k);
        }
        let p = parse_get_instrument(&json!({"result": result})).expect("still a valid grid");
        assert_eq!(p.asset_class, None, "{kind:?}");
        assert_eq!(p.tick_size, 0.5, "the grid itself is unaffected");
        assert_eq!(p.min_qty, 10.0);
    }
}

// ---- tiered tick scheme (deribit `tick_size_steps` → SymbolProperties::tick_scheme) --------

/// An OPTION row carrying `tick_size_steps` builds a tiered `TickScheme` (base tick = the scalar
/// `tick_size`, one step to 0.0005 above 0.005 — the real Deribit BTC-option grid) and attaches
/// it. This is what lets the order-format path snap an above-boundary limit onto the coarse tier
/// grid the venue requires instead of the base grid it rejects.
#[test]
fn get_instrument_parses_option_tick_scheme() {
    let resp = json!({"result": {
        "instrument_name": "BTC-8JUL26-62000-C",
        "kind": "option",
        "tick_size": 0.0001,
        "min_trade_amount": 0.1,
        "contract_size": 1.0,
        "tick_size_steps": [{"above_price": 0.005, "tick_size": 0.0005}],
    }});
    let p = parse_get_instrument(&resp).expect("a tiered option row parses");
    let want = TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }])
        .expect("valid grid");
    assert_eq!(p.tick_scheme, Some(want), "the parsed scheme mirrors the venue tiers");
    let scheme = p.tick_scheme.expect("present");
    assert_eq!(scheme.base_tick(), 0.0001);
    assert_eq!(scheme.tiers().len(), 1);
    assert_eq!(scheme.tiers()[0], TickTier { above_price: 0.005, tick_size: 0.0005 });
    // the tick resolves by price: below the boundary → base, above → the tier tick
    assert_eq!(p.effective_tick(0.004), 0.0001);
    assert_eq!(p.effective_tick(0.005), 0.0001, "the boundary belongs to the LOWER tier");
    assert_eq!(p.effective_tick(0.05), 0.0005);
    // the rest of the grid rides through untouched
    assert_eq!(p.tick_size, 0.0001);
    assert_eq!(p.step_size, 0.1);
    assert_eq!(p.contract_size, 1.0);
}

/// THE base-tick invariant: the built scheme's base tick MUST equal the scalar `tick_size`
/// (`with_tick_scheme`'s caller invariant). Held for a non-round tick too, so it is genuinely
/// seeded from the same field rather than a coincidence of the 0.0001 example above.
#[test]
fn get_instrument_scheme_base_tick_equals_scalar_tick() {
    let resp = json!({"result": {
        "kind": "option", "tick_size": 0.00025, "min_trade_amount": 0.1,
        "tick_size_steps": [{"above_price": 0.01, "tick_size": 0.0005}],
    }});
    let p = parse_get_instrument(&resp).expect("parses");
    let scheme = p.tick_scheme.expect("a scheme is attached");
    assert_eq!(scheme.base_tick(), p.tick_size, "base_tick == the scalar tick_size");
    assert_eq!(scheme.base_tick(), 0.00025);
}

/// Deribit sometimes STRING-encodes numeric fields (the r6 fixture carries both forms). A
/// string-encoded `tick_size_steps` decodes through `json_num` exactly like the scalar grid.
#[test]
fn get_instrument_accepts_string_encoded_tick_size_steps() {
    let resp = json!({"result": {
        "kind": "option", "tick_size": "0.0001", "min_trade_amount": "0.1",
        "tick_size_steps": [{"above_price": "0.005", "tick_size": "0.0005"}],
    }});
    let p = parse_get_instrument(&resp).expect("string-encoded grid parses");
    assert_eq!(
        p.tick_scheme,
        Some(
            TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }]).unwrap()
        )
    );
}

/// A FUTURE/perp row (no `tick_size_steps`) stays SCHEME-LESS — the scalar `tick_size` IS the
/// whole grid, byte-identical to before. `effective_tick` is that scalar unconditionally.
#[test]
fn get_instrument_future_has_no_scheme() {
    let resp = json!({"result": {
        "kind": "future", "tick_size": 0.5, "min_trade_amount": 10.0, "contract_size": 10.0,
    }});
    let p = parse_get_instrument(&resp).expect("a future row parses");
    assert!(p.tick_scheme.is_none(), "no tick_size_steps → scalar grid, no scheme");
    assert_eq!(p.effective_tick(1234.5), 0.5, "the scalar tick, unconditionally");
}

/// An explicit EMPTY `tick_size_steps` is a flat grid → NO scheme (byte-identical to absent),
/// NOT a flat `TickScheme` — a flat scheme would flip the persisted shape from `None` to `Some`.
#[test]
fn get_instrument_empty_tick_size_steps_yields_no_scheme() {
    let resp = json!({"result": {
        "kind": "option", "tick_size": 0.0001, "min_trade_amount": 0.1, "tick_size_steps": [],
    }});
    let p = parse_get_instrument(&resp).expect("parses");
    assert!(p.tick_scheme.is_none(), "empty steps → no scheme");
    assert_eq!(p.tick_size, 0.0001, "the rest of the grid is unaffected");
}

/// A MALFORMED tier (here: no `tick_size`, so `TickScheme::new` rejects it as BadTierTick)
/// degrades to the scalar grid — never a failed whole-grid parse and never a zero-tick grid.
#[test]
fn get_instrument_malformed_tick_size_steps_degrades_to_scalar() {
    let resp = json!({"result": {
        "kind": "option", "tick_size": 0.0001, "min_trade_amount": 0.1,
        "tick_size_steps": [{"above_price": 0.005}],
    }});
    let p = parse_get_instrument(&resp).expect("the grid still parses");
    assert!(p.tick_scheme.is_none(), "a malformed tier degrades to the scalar grid");
    assert_eq!(p.tick_size, 0.0001, "the rest of the grid is unaffected");
}
