use super::*;
// `TickScheme`/`round_price_tiered` arrive through `super::*` (the parent's own imports);
// these are the ones only the tests need.
use crate::instrument::tick_scheme::TickTier;
use crate::scalar::{nz_step, round_to, round_to_step};

/// The absent grid — the state EVERY venue but deribit is in — must fold to the inert `1.0`,
/// so notional math is byte-identical to a world without the field.
#[test]
fn absent_contract_size_is_multiplier_one() {
    assert_eq!(SymbolProperties::default().multiplier(), 1.0);
    assert_eq!(SymbolProperties { contract_size: 0.0, ..Default::default() }.multiplier(), 1.0);
}

/// A real contract size rides through untouched — this is the whole point of the field.
#[test]
fn real_contract_size_is_the_multiplier() {
    let p = SymbolProperties { contract_size: 10.0, ..Default::default() };
    assert_eq!(p.multiplier(), 10.0);
    // and it composes into notional the way the cap computes it
    assert_eq!(2.0_f64.abs() * 50_000.0_f64.abs() * p.multiplier(), 1_000_000.0);
}

/// A malformed venue value must never zero out or invert a notional cap — a `0.0` multiplier
/// would make every order measure as zero notional and pass any cap.
#[test]
fn degenerate_contract_sizes_fold_to_one() {
    for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0] {
        let p = SymbolProperties { contract_size: bad, ..Default::default() };
        assert_eq!(p.multiplier(), 1.0, "contract_size {bad} must fold to 1.0");
    }
}

/// THE serde-compatibility pin. `SymbolProperties` is persisted in the `kind=properties` PIT
/// series, so a payload written BEFORE `contract_size` existed must still deserialize. If
/// `#[serde(default)]` is ever dropped, this test fails rather than a live store breaking.
#[test]
fn deserializes_payload_written_before_contract_size_existed() {
    let legacy = r#"{"tick_size":0.5,"step_size":0.1,"min_qty":0.01,
                         "max_qty":0.0,"min_notional":5.0}"#;
    let got: SymbolProperties = serde_json::from_str(legacy).expect("legacy row must decode");
    assert_eq!(got.contract_size, 0.0, "absent field → the absent convention");
    assert_eq!(got.multiplier(), 1.0, "…and therefore the inert multiplier");
    assert_eq!(got.tick_size, 0.5);
    assert_eq!(got.min_notional, 5.0);
}

/// Round-trip WITH the field, so the new column is genuinely carried (not silently skipped).
#[test]
fn round_trips_with_and_without_contract_size() {
    for cs in [0.0, 1.0, 10.0] {
        let p = SymbolProperties {
            tick_size: 0.5,
            step_size: 0.1,
            min_qty: 0.01,
            max_qty: 0.0,
            min_notional: 5.0,
            contract_size: cs,
            tick_scheme: None,
            taker_hold_ms: 0,
            asset_class: None,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<SymbolProperties>(&s).unwrap(), p);
    }
}

// ---- tiered tick scheme (the optional, price-dependent grid) ------------------------------

/// The grid the deribit live smoke documents dodging: base `0.0001`, `0.0005` above `0.005`.
fn deribit_option_scheme() -> TickScheme {
    let tiers = [TickTier { above_price: 0.005, tick_size: 0.0005 }];
    TickScheme::new(0.0001, &tiers).expect("valid deribit grid")
}

fn deribit_option_properties() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.0001,
        step_size: 0.1,
        min_qty: 0.1,
        max_qty: 0.0,
        min_notional: 0.0,
        contract_size: 1.0,
        tick_scheme: Some(deribit_option_scheme()),
        taker_hold_ms: 0,
        // A Deribit BTC option, and this fixture is the one place in this module that names a
        // real instrument rather than a synthetic grid.
        asset_class: Some(AssetClass::Option),
    }
}

/// THE off-path pin. With no scheme — every venue but a Deribit option — `effective_tick` is
/// the scalar field verbatim and `round_price` is byte-for-byte the expression in use today.
#[test]
fn tierless_properties_round_exactly_as_today() {
    for tick in [0.0, 0.0001, 0.01, 0.5] {
        let p = SymbolProperties { tick_size: tick, ..Default::default() };
        assert!(p.tick_scheme.is_none());
        for v in [0.0, 1.23456, -1.23456, 2.5, 0.00499, 1e9] {
            assert_eq!(p.effective_tick(v), tick, "the scalar tick, unconditionally");
            assert_eq!(
                p.round_price(v),
                round_to(v, nz_step(tick)),
                "v={v} tick={tick} must be today's expression"
            );
        }
    }
    // ...including the absent-is-UNCONSTRAINED convention: a 0.0 tick is identity, not NaN.
    let unconstrained = SymbolProperties::default();
    assert_eq!(unconstrained.round_price(1.23456), 1.23456);
}

/// WITH a scheme the tick follows the price — the whole point: a limit above the tier boundary
/// snaps onto the COARSE grid the venue requires, not the base one it would otherwise reject.
#[test]
fn tiered_properties_resolve_and_round_by_price() {
    let p = deribit_option_properties();
    assert_eq!(p.effective_tick(0.004), 0.0001);
    assert_eq!(p.effective_tick(0.005), 0.0001, "the boundary belongs to the LOWER tier");
    assert_eq!(p.effective_tick(0.05), 0.0005);
    assert_eq!(p.round_price(0.01234), round_to_step(0.01234, 0.0005));
    assert_ne!(p.round_price(0.01234), round_to_step(0.01234, 0.0001));
    // below the boundary the base grid still applies — and equals the tier-less behavior
    assert_eq!(p.round_price(0.00123), round_to(0.00123, nz_step(0.0001)));
}

/// The builder is the additive way in: same struct, one field flipped.
#[test]
fn with_tick_scheme_only_sets_the_scheme() {
    let flat = SymbolProperties { tick_size: 0.0001, ..Default::default() };
    let scheme = deribit_option_scheme();
    let tiered = flat.with_tick_scheme(scheme);
    assert_eq!(tiered.tick_scheme, Some(scheme));
    assert_eq!(SymbolProperties { tick_scheme: None, ..tiered }, flat, "nothing else moved");
}

/// THE serde-compatibility pin for the new field, BOTH halves:
/// (1) a row written before `tick_scheme` existed still decodes (to `None`), and
/// (2) a tier-less value re-serializes BYTE-IDENTICALLY to a world without the field — proven
///     against a local struct carrying exactly the OLD field set, so the pin does not depend on
///     hand-transcribing serde_json's float formatting.
#[test]
fn tierless_properties_serialize_byte_identically_to_the_old_shape() {
    #[derive(serde::Serialize)]
    struct LegacyProperties {
        tick_size: f64,
        step_size: f64,
        min_qty: f64,
        max_qty: f64,
        min_notional: f64,
        contract_size: f64,
        taker_hold_ms: u32,
    }
    let p = SymbolProperties {
        tick_size: 0.5,
        step_size: 0.1,
        min_qty: 0.01,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 10.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    let legacy = LegacyProperties {
        tick_size: 0.5,
        step_size: 0.1,
        min_qty: 0.01,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 10.0,
        taker_hold_ms: 0,
    };
    let encoded = serde_json::to_string(&p).unwrap();
    assert!(!encoded.contains("tick_scheme"), "an absent scheme emits NO key: {encoded}");
    assert_eq!(encoded, serde_json::to_string(&legacy).unwrap(), "byte-identical when OFF");

    // ...and the legacy payload decodes, with the new field absent.
    let old = r#"{"tick_size":0.5,"step_size":0.1,"min_qty":0.01,
                      "max_qty":0.0,"min_notional":5.0,"contract_size":10.0}"#;
    let got: SymbolProperties = serde_json::from_str(old).expect("legacy row must decode");
    assert_eq!(got, p);
    assert!(got.tick_scheme.is_none());
}

/// Round-trip WITH a scheme, so the new field is genuinely carried (not silently skipped),
/// and the decoded grid still resolves.
#[test]
fn round_trips_with_a_tick_scheme() {
    let p = deribit_option_properties();
    let encoded = serde_json::to_string(&p).unwrap();
    assert!(encoded.contains("tick_scheme"), "a present scheme IS emitted: {encoded}");
    let got: SymbolProperties = serde_json::from_str(&encoded).unwrap();
    assert_eq!(got, p);
    assert_eq!(got.effective_tick(0.05), 0.0005);
    assert_eq!(got.round_price(0.01234), p.round_price(0.01234));
}

// ---- venue taker hold (the venue-declared, per-market order hold) -------------------------

/// THE serde-compatibility pin for `taker_hold_ms`: a row written BEFORE the field existed —
/// i.e. every `kind=properties` row on every store today — must still decode, to the struct's
/// absent-is-`0` convention. If `#[serde(default)]` is ever dropped, this fails rather than a
/// live store breaking.
#[test]
fn deserializes_payload_written_before_taker_hold_existed() {
    let old = r#"{"tick_size":0.5,"step_size":0.1,"min_qty":0.01,
                      "max_qty":0.0,"min_notional":5.0,"contract_size":10.0}"#;
    let got: SymbolProperties = serde_json::from_str(old).expect("legacy row must decode");
    assert_eq!(got.taker_hold_ms, 0, "absent field → no venue hold");
}

/// Both live Polymarket values ride through a round trip intact — the field is genuinely
/// carried, not silently skipped.
#[test]
fn round_trips_with_both_polymarket_holds() {
    for hold in [0, 250, 3000] {
        let p = SymbolProperties { tick_size: 0.01, taker_hold_ms: hold, ..Default::default() };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<SymbolProperties>(&s).unwrap(), p);
    }
}

/// The default is the inert one: nothing in the tree that does not explicitly set a hold can
/// accidentally acquire one.
#[test]
fn default_declares_no_venue_hold() {
    assert_eq!(SymbolProperties::default().taker_hold_ms, 0);
}

// ---- asset class (what KIND of instrument this grid belongs to) ---------------------------

/// THE serde-compatibility pin for `asset_class`: a row written BEFORE the field existed —
/// i.e. every `kind=properties` row on every store today — must still decode, to `None`.
/// `None` is the honest reading of such a row: nobody recorded a class, so nobody may claim
/// one. If `#[serde(default)]` is ever dropped, this fails rather than a live store breaking.
#[test]
fn deserializes_payload_written_before_asset_class_existed() {
    let old = r#"{"tick_size":0.5,"step_size":0.1,"min_qty":0.01,
                      "max_qty":0.0,"min_notional":5.0,"contract_size":10.0,"taker_hold_ms":0}"#;
    let got: SymbolProperties = serde_json::from_str(old).expect("legacy row must decode");
    assert_eq!(got.asset_class, None, "absent field → nobody said, not a guessed class");
}

/// The other half of the pin: a class-LESS value re-serializes byte-identically to a world
/// without the field — the key is omitted entirely, so `skip_serializing_if` is load-bearing.
/// Proven against a local struct carrying exactly the OLD field set, the same way the
/// `tick_scheme` pin above avoids hand-transcribing serde_json's float formatting.
#[test]
fn classless_properties_serialize_byte_identically_to_the_old_shape() {
    #[derive(serde::Serialize)]
    struct PreAssetClassProperties {
        tick_size: f64,
        step_size: f64,
        min_qty: f64,
        max_qty: f64,
        min_notional: f64,
        contract_size: f64,
        taker_hold_ms: u32,
    }
    let p = SymbolProperties {
        tick_size: 0.5,
        step_size: 0.1,
        min_qty: 0.01,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 10.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    let old = PreAssetClassProperties {
        tick_size: 0.5,
        step_size: 0.1,
        min_qty: 0.01,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 10.0,
        taker_hold_ms: 0,
    };
    let encoded = serde_json::to_string(&p).unwrap();
    assert!(!encoded.contains("asset_class"), "an absent class emits NO key: {encoded}");
    assert_eq!(encoded, serde_json::to_string(&old).unwrap(), "byte-identical when absent");
}

/// EVERY variant rides through a round trip intact — the field is genuinely carried, not
/// silently skipped, and the wire word is the variant's own `sql_word` (the one spelling
/// `AssetClass`'s own module doc pins). Iterating `ALL` rather than naming a few means a new
/// variant joins this test by existing.
#[test]
fn round_trips_every_asset_class() {
    for class in AssetClass::ALL {
        let p =
            SymbolProperties { tick_size: 0.01, asset_class: Some(*class), ..Default::default() };
        let s = serde_json::to_string(&p).unwrap();
        assert!(
            s.contains(&format!("\"asset_class\":\"{}\"", class.sql_word())),
            "a present class IS emitted, as its stored word: {s}"
        );
        assert_eq!(serde_json::from_str::<SymbolProperties>(&s).unwrap(), p);
    }
}

/// The default names no class. Nothing that merely constructs properties with
/// `..Default::default()` — which is nearly every producer in the tree — can accidentally
/// acquire one, so a class in the store is always something a producer chose.
#[test]
fn default_names_no_asset_class() {
    assert_eq!(SymbolProperties::default().asset_class, None);
}
