//! Fixture-driven, NO network. The `create_combo` request/response fixtures are the documented
//! shapes quoted in the module doc; the mapping tests pin the orientation/scale law, and the
//! credit-combo tests pin the ONE thing a live mistake would cost real money: the price sign.
use super::*;
use serde_json::json;
use std::assert_matches;

fn call_spread() -> Vec<ComboLeg> {
    vec![
        ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
        ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
    ]
}

/// The documented `create_combo` request body: one `trades` entry per leg, `amount` = |ratio|,
/// `direction` from the ratio's SIGN. No side/qty leaks into the DEFINITION.
#[test]
fn create_combo_params_match_documented_shape() {
    let params = build_create_combo_params(&call_spread());
    assert_eq!(
        params,
        json!({"trades": [
            {"instrument_name": "BTC-27MAR26-100000-C", "amount": 1, "direction": "buy"},
            {"instrument_name": "BTC-27MAR26-120000-C", "amount": 1, "direction": "sell"},
        ]})
    );
}

/// Ratios >1 keep their magnitude in `amount` (a 1x2 ratio spread), sign still in `direction`.
#[test]
fn create_combo_params_carry_ratio_magnitude() {
    let legs =
        vec![ComboLeg { symbol: "A".into(), ratio: 1 }, ComboLeg { symbol: "B".into(), ratio: -2 }];
    let params = build_create_combo_params(&legs);
    let trades = params["trades"].as_array().unwrap();
    assert_eq!(trades[1]["amount"], json!(2));
    assert_eq!(trades[1]["direction"], json!("sell"));
}

fn combo_result(state: &str, legs: Value) -> Value {
    json!({
        "id": "BTC-CS-27MAR26-100000_120000",
        "instrument_id": 12345,
        "state": state,
        "state_timestamp": 1_700_000_000_000i64,
        "creation_timestamp": 1_700_000_000_000i64,
        "legs": legs,
    })
}

#[test]
fn parses_documented_create_combo_result() {
    let v = combo_result(
        "active",
        json!([
            {"instrument_name": "BTC-27MAR26-100000-C", "amount": 1},
            {"instrument_name": "BTC-27MAR26-120000-C", "amount": -1},
        ]),
    );
    let combo = parse_combo(&v).expect("documented result must parse");
    assert_eq!(combo.id, "BTC-CS-27MAR26-100000_120000");
    assert!(combo.is_active());
    assert_eq!(combo.legs, call_spread());
}

#[test]
fn parse_rejects_malformed_results() {
    assert!(parse_combo(&json!({"legs": []})).is_none(), "no id");
    assert!(parse_combo(&json!({"id": "X"})).is_none(), "no legs");
    assert!(
        parse_combo(&json!({"id": "", "legs": [{"instrument_name":"A","amount":1}]})).is_none()
    );
    assert!(parse_combo(&json!({"id": "X", "legs": []})).is_none(), "empty legs");
}

/// A venue that encodes the multiplier as `1.0` must still parse as ratio 1, not 0 — a silent
/// 0 would be read as "no leg" and mis-scale the whole combo.
#[test]
fn parse_accepts_float_encoded_amounts() {
    let v = combo_result(
        "active",
        json!([
            {"instrument_name": "A", "amount": 1.0},
            {"instrument_name": "B", "amount": -1.0},
        ]),
    );
    let combo = parse_combo(&v).unwrap();
    assert_eq!(combo.legs[0].ratio, 1);
    assert_eq!(combo.legs[1].ratio, -1);
}

/// Deribit stringifies some numerics: `"1"`, `"-2"`, `"2.0"` are all the documented integer
/// multiplier and must decode EXPLICITLY — not coerce to ratio 0 and lean on the downstream
/// zero-ratio guard.
#[test]
fn parse_accepts_string_encoded_amounts() {
    let v = combo_result(
        "active",
        json!([
            {"instrument_name": "A", "amount": "1"},
            {"instrument_name": "B", "amount": "-2"},
            {"instrument_name": "C", "amount": "2.0"},
        ]),
    );
    let combo = parse_combo(&v).unwrap();
    assert_eq!(
        combo.legs.iter().map(|l| l.ratio).collect::<Vec<_>>(),
        vec![1, -2, 2],
        "string-encoded multipliers decode to their integer values"
    );
}

/// An undecodable leg amount fails the WHOLE parse (→ `Unparseable` reject), never a
/// plausible wrong ratio: `2^32 + 1` used to WRAP to ratio 1 via `as i32`, `1.5` used to
/// truncate to 1, an unparseable string used to coerce to 0.
#[test]
fn parse_rejects_undecodable_amounts_loudly() {
    let bad = [
        json!(4_294_967_297i64), // 2^32 + 1: the old wrapping `as i32` made this ratio 1
        json!(2_147_483_648i64), // i32::MAX + 1
        json!(1.5),              // fractional multiplier is structurally meaningless
        json!(3.0e10),           // float out of i32 range
        json!("spread"),         // non-numeric string
        Value::Null,
    ];
    for amount in bad {
        let v = combo_result(
            "active",
            json!([
                {"instrument_name": "A", "amount": 1},
                {"instrument_name": "B", "amount": amount},
            ]),
        );
        assert!(parse_combo(&v).is_none(), "amount {amount:?} must fail the whole parse");
    }
    // a leg with NO amount key at all is equally undecodable
    let v = combo_result("active", json!([{"instrument_name": "A"}]));
    assert!(parse_combo(&v).is_none(), "missing amount must fail the parse");
}

#[test]
fn identical_legs_map_to_identity() {
    assert_eq!(map_combo(&call_spread(), &call_spread()).unwrap(), ComboMapping::IDENTITY);
}

/// Identity is CANONICAL: an unreduced self-map (+3/-3 vs +3/-3) is still exactly `IDENTITY`,
/// because `map_combo` gcd-reduces the solved rational.
#[test]
fn identical_unreduced_legs_map_to_identity() {
    let legs =
        vec![ComboLeg { symbol: "A".into(), ratio: 3 }, ComboLeg { symbol: "B".into(), ratio: -3 }];
    assert_eq!(map_combo(&legs, &legs).unwrap(), ComboMapping::IDENTITY);
}

/// Venue reordered the legs — pairing is by instrument name, so this is still the identity.
#[test]
fn reordered_legs_still_map_to_identity() {
    let mut venue = call_spread();
    venue.reverse();
    assert_eq!(map_combo(&call_spread(), &venue).unwrap(), ComboMapping::IDENTITY);
}

/// THE inversion case: the venue's canonical combo is our spec flipped. k = -1.
#[test]
fn inverted_venue_combo_maps_to_negative_sign() {
    let venue = vec![
        ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: -1 },
        ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: 1 },
    ];
    let m = map_combo(&call_spread(), &venue).unwrap();
    assert_eq!(m, ComboMapping { num: -1, den: 1 });
    assert_eq!(m.sign(), -1);
    // buying our combo == SELLING the venue's inverse
    assert_eq!(m.venue_side(1), -1);
    assert_eq!(m.venue_side(-1), 1);
}

/// Ratio reduction: we asked for +2/-2, the venue canonicalized to +1/-1. One venue unit is
/// HALF a spec unit, so qty doubles and the per-unit net halves. k = 1/2.
#[test]
fn reduced_venue_ratios_rescale_qty_and_price() {
    let spec =
        vec![ComboLeg { symbol: "A".into(), ratio: 2 }, ComboLeg { symbol: "B".into(), ratio: -2 }];
    let venue =
        vec![ComboLeg { symbol: "A".into(), ratio: 1 }, ComboLeg { symbol: "B".into(), ratio: -1 }];
    let m = map_combo(&spec, &venue).unwrap();
    assert_eq!(m, ComboMapping { num: 1, den: 2 });
    assert_eq!(m.venue_qty(3.0), 6.0); // 3 spec units = 6 half-size venue units
    assert_eq!(m.venue_price(100.0), 50.0); // net per venue unit is half
}

/// Structurally different combos must FAIL, never silently map. Guessing here would submit the
/// wrong instrument's book at our price.
#[test]
fn unreconcilable_combos_are_errors_not_guesses() {
    let spec = call_spread();
    // ratios not a common multiple (1:-1 vs 1:-2)
    let bad_ratios = vec![
        ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
        ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -2 },
    ];
    assert_eq!(map_combo(&spec, &bad_ratios), Err(ComboMapError::RatioMismatch));
    // a leg we never asked for
    let wrong_symbol = vec![
        ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
        ComboLeg { symbol: "ETH-27MAR26-5000-C".into(), ratio: -1 },
    ];
    assert_matches!(map_combo(&spec, &wrong_symbol), Err(ComboMapError::LegSymbolMismatch(_)));
    // leg count differs
    assert_eq!(
        map_combo(&spec, &spec[..1]),
        Err(ComboMapError::LegCountMismatch { spec: 2, venue: 1 })
    );
    // a zero ratio from the venue
    let zero = vec![
        ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
        ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: 0 },
    ];
    assert_eq!(map_combo(&spec, &zero), Err(ComboMapError::ZeroRatio));
}

/// Totality: two EMPTY leg lists pass the length/zero checks and used to panic on `paired[0]`
/// — "every function is total" demands a proper error instead.
#[test]
fn empty_leg_lists_are_an_error_not_a_panic() {
    assert_eq!(map_combo(&[], &[]), Err(ComboMapError::EmptyLegs));
}

/// A duplicated spec symbol must not pair two spec legs onto one venue leg — and the error
/// must name the venue leg nothing paired to ("B", the actual offender), not venue[0] ("A",
/// a perfectly valid leg).
#[test]
fn duplicate_spec_symbols_do_not_alias() {
    let spec =
        vec![ComboLeg { symbol: "A".into(), ratio: 1 }, ComboLeg { symbol: "A".into(), ratio: -1 }];
    let venue =
        vec![ComboLeg { symbol: "A".into(), ratio: 1 }, ComboLeg { symbol: "B".into(), ratio: -1 }];
    assert_eq!(map_combo(&spec, &venue), Err(ComboMapError::LegSymbolMismatch("B".into())));
}

/// A PARTIAL `get_instrument` parse (either axis missing or zero) must NOT count as a grid —
/// an OR-acceptance would silently disable quantization on the zero axis while looking like a
/// fetched grid. Both axes or nothing.
#[test]
fn partial_combo_grid_is_rejected_not_half_used() {
    let full = json!({"tick_size": 0.0005, "min_trade_amount": 0.1});
    assert_eq!(parse_combo_grid(&full), Some((0.0005, 0.1)));
    assert_eq!(parse_combo_grid(&json!({"tick_size": 0.0005})), None, "step missing");
    assert_eq!(parse_combo_grid(&json!({"min_trade_amount": 0.1})), None, "tick missing");
    assert_eq!(
        parse_combo_grid(&json!({"tick_size": 0.0, "min_trade_amount": 0.1})),
        None,
        "zero tick"
    );
    assert_eq!(
        parse_combo_grid(&json!({"tick_size": 0.0005, "min_trade_amount": 0.0})),
        None,
        "zero step"
    );
    assert_eq!(parse_combo_grid(&json!({})), None);
}

// ---- the money-critical sign tests ----

/// A CREDIT combo (you are paid to put it on) has a negative net. It must reach the wire
/// negative — no clamp, no abs(), through mapping AND through Decimal quantization.
#[test]
fn credit_combo_net_stays_negative_on_the_wire() {
    let order = build_combo_order("BTC-CS-X", ComboMapping::IDENTITY, 1, 2.0, Some(-0.0125));
    assert_eq!(order.venue_price(), Some(-0.0125));
    let params = build_combo_order_params(&order, "coid-1", 0.0005, 0.1);
    assert_eq!(params["price"], json!(-0.0125));
    assert_eq!(params["type"], json!("limit"));
    assert_eq!(params["instrument_name"], json!("BTC-CS-X"));
    assert_eq!(params["label"], json!("coid-1"));
    assert_eq!(params["post_only"], json!(false));
}

/// Inverting the orientation turns a DEBIT into a CREDIT and vice versa — the sign flip must
/// survive to the wire, together with the side flip.
#[test]
fn inverted_orientation_flips_price_sign_and_side() {
    let inv = ComboMapping { num: -1, den: 1 };
    let order = build_combo_order("BTC-CS-X", inv, 1, 5.0, Some(0.02));
    assert_eq!(order.side, -1, "buying our combo = selling the venue's inverse");
    assert_eq!(order.venue_price(), Some(-0.02), "a debit becomes a credit when inverted");
    let params = build_combo_order_params(&order, "c", 0.0001, 0.1);
    assert_eq!(params["price"], json!(-0.02));
}

/// Quantization must round a negative price TOWARD ZERO and keep it negative (the pinned
/// `format_to_step` behavior), never flip or clamp it to 0-or-positive.
#[test]
fn negative_price_quantizes_toward_zero_and_stays_negative() {
    let order = build_combo_order("C", ComboMapping::IDENTITY, 1, 1.0, Some(-0.01237));
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    let px = params["price"].as_f64().unwrap();
    assert!(px < 0.0, "credit price must stay negative, got {px}");
    assert_eq!(px, -0.0120);
}

/// A zero net (a costless roll/switch) is legal and must submit as a LIMIT at 0, not as a
/// market order — `None` is the only market sentinel.
#[test]
fn zero_net_is_a_limit_not_a_market() {
    let order = build_combo_order("C", ComboMapping::IDENTITY, 1, 1.0, Some(0.0));
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    assert_eq!(params["type"], json!("limit"));
    assert_eq!(params["price"].as_f64().unwrap(), 0.0);
}

/// `None` net = a combo MARKET order: no `price` key at all.
#[test]
fn market_combo_omits_price() {
    let order = build_combo_order("C", ComboMapping::IDENTITY, -1, 4.0, None);
    assert_eq!(order.side, -1);
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    assert_eq!(params["type"], json!("market"));
    assert!(params.get("price").is_none());
}

/// Amount is quantized on the combo instrument's OWN step grid, through the pinned site.
#[test]
fn qty_quantizes_to_the_combo_step_grid() {
    let order = build_combo_order("C", ComboMapping::IDENTITY, 1, 2.37, Some(0.01));
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    assert_eq!(params["amount"].as_f64().unwrap(), 2.3);
}

// ---- the non-dyadic (1/3) exactness tests: THE review-found tick loss ----

fn third_scale_mapping() -> ComboMapping {
    let spec =
        vec![ComboLeg { symbol: "A".into(), ratio: 3 }, ComboLeg { symbol: "B".into(), ratio: -3 }];
    let venue =
        vec![ComboLeg { symbol: "A".into(), ratio: 1 }, ComboLeg { symbol: "B".into(), ratio: -1 }];
    map_combo(&spec, &venue).unwrap()
}

/// THE regression the adversarial review found: k = ⅓ (a +3/-3 spec the venue reduced to
/// +1/-1) has no exact f64. The old `scale: f64` path computed `0.03 · ⅓` one ULP below 0.01
/// and the wire round-down lost a FULL tick (0.0095, not 0.0100 — ~19% of on-grid nets), and
/// `2.3 / ⅓` one ULP below 6.9 losing a full step (6.8 — ~7% of qtys). The rescale now runs
/// inside Decimal, so a spec value whose exact venue image lies ON the grid stays on it.
#[test]
fn third_scale_debit_lands_exactly_on_the_grid() {
    let m = third_scale_mapping();
    assert_eq!(m, ComboMapping { num: 1, den: 3 });
    let order = build_combo_order("C", m, 1, 2.3, Some(0.03));
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    assert_eq!(params["price"].as_f64().unwrap(), 0.0100, "0.03·⅓ = 0.0100 — was 0.0095");
    assert_eq!(params["amount"].as_f64().unwrap(), 6.9, "2.3·3 = 6.9 — was 6.8");
}

/// The CREDIT twin at k = ⅓: `-0.03 · ⅓` is exactly `-0.0100`; truncation toward zero must
/// keep the credit on-grid and negative.
#[test]
fn third_scale_credit_lands_exactly_on_the_grid() {
    let m = third_scale_mapping();
    let order = build_combo_order("C", m, 1, 2.3, Some(-0.03));
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    assert_eq!(params["price"].as_f64().unwrap(), -0.0100);
    assert_eq!(params["amount"].as_f64().unwrap(), 6.9);
}

/// Sign and exactness COMPOSE: the inverted third (k = -⅓) flips side and debit→credit while
/// both wire values stay exactly on their grids.
#[test]
fn inverted_third_scale_flips_sign_and_stays_on_grid() {
    let spec =
        vec![ComboLeg { symbol: "A".into(), ratio: 3 }, ComboLeg { symbol: "B".into(), ratio: -3 }];
    let venue =
        vec![ComboLeg { symbol: "A".into(), ratio: -1 }, ComboLeg { symbol: "B".into(), ratio: 1 }];
    let m = map_combo(&spec, &venue).unwrap();
    assert_eq!(m, ComboMapping { num: -1, den: 3 });
    let order = build_combo_order("C", m, 1, 2.3, Some(0.03));
    assert_eq!(order.side, -1, "buying our combo = selling the venue's inverse");
    let params = build_combo_order_params(&order, "c", 0.0005, 0.1);
    assert_eq!(params["price"].as_f64().unwrap(), -0.0100, "debit → on-grid credit");
    assert_eq!(params["amount"].as_f64().unwrap(), 6.9);
}

/// End-to-end pure round-trip: spec legs → create_combo params → the venue's (inverted,
/// reduced) reply → mapping → the final buy/sell params. This is the whole feature with the
/// network cut out.
#[test]
fn full_pure_round_trip_inverted_and_reduced() {
    let spec = vec![
        ComboLeg { symbol: "BTC-PERPETUAL".into(), ratio: 2 },
        ComboLeg { symbol: "BTC-27MAR26".into(), ratio: -2 },
    ];
    let req = build_create_combo_params(&spec);
    assert_eq!(req["trades"][0]["direction"], json!("buy"));
    assert_eq!(req["trades"][1]["amount"], json!(2));

    // venue canonicalizes to the reduced INVERSE: -1 / +1
    let reply = combo_result(
        "active",
        json!([
            {"instrument_name": "BTC-PERPETUAL", "amount": -1},
            {"instrument_name": "BTC-27MAR26", "amount": 1},
        ]),
    );
    let combo = parse_combo(&reply).unwrap();
    assert!(combo.is_active());
    let m = map_combo(&spec, &combo.legs).unwrap();
    assert_eq!(m, ComboMapping { num: -1, den: 2 });

    // buy 3 spec units at a net DEBIT of 40.0 per spec unit
    let order = build_combo_order(&combo.id, m, 1, 3.0, Some(40.0));
    assert_eq!(order.side, -1); // sell the inverse
    assert_eq!(order.venue_qty(), 6.0); // 3 spec units = 6 half-size venue units
    assert_eq!(order.venue_price(), Some(-20.0)); // inverted + halved: a CREDIT of 20/venue unit
    let params = build_combo_order_params(&order, "coid-9", 0.5, 0.1);
    assert_eq!(params["instrument_name"], json!("BTC-CS-27MAR26-100000_120000"));
    assert_eq!(params["amount"], json!(6.0));
    assert_eq!(params["price"], json!(-20.0));
}

#[test]
fn inactive_combo_is_not_tradable() {
    let v = combo_result("inactive", json!([{"instrument_name":"A","amount":1}]));
    assert!(!parse_combo(&v).unwrap().is_active());
}
