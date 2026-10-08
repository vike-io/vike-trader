//! `PARAM_KEYS` / `PARAM_ROUTES`: exhaustive, consistent with each other, and `misrouted_params`.

use super::*;
use crate::registry::keys::{PARAM_KEYS, ParamKeys, ParamType, param_keys};
use crate::registry::routes::{
    PARAM_ROUTES, ParamRoutes, RouteKind, misrouted_params, param_routes,
};

#[test]
fn param_keys_table_is_exhaustive() {
    // Same construction as `live_capable_table_is_exhaustive`: adding a registry arm without
    // declaring what its params table may contain fails HERE, because an undeclared reader is
    // one whose typos a live mount cannot catch.
    for name in PORTABLE_STRATEGIES {
        assert_eq!(
            PARAM_KEYS.iter().filter(|(n, _)| n == name).count(),
            1,
            "{name} needs exactly one PARAM_KEYS row"
        );
    }
    for (name, keys) in PARAM_KEYS {
        assert!(
            PORTABLE_STRATEGIES.contains(name),
            "PARAM_KEYS names {name}, which is not on the roster"
        );
        match keys {
            ParamKeys::Declared(k) => {
                assert!(!k.is_empty(), "{name} declares an EMPTY key set — say NotEnumerated");
                let mut sorted: Vec<&str> = k.iter().map(|(n, _)| *n).collect();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(sorted.len(), k.len(), "{name} declares a key twice");
            }
            ParamKeys::NotEnumerated(why) => {
                assert!(why.len() > 20, "{name}'s NotEnumerated reason is too thin: {why:?}")
            }
        }
    }
}

/// A declared key whose NAME is route-shaped, in the vocabulary this workspace's params readers
/// actually use; direction 2 below forces a [`PARAM_ROUTES`] row for every one of them.
///
/// ⚠ A NAME heuristic, deliberately, and a LOWER bound — a route key called `market` would pass
/// unseen. The alternative is guessing at reader bodies the `param_keys_gate.rs` scanner declines
/// to slice; the heuristic can only over-fire, and an over-fire is one written row, not a silent
/// hole. `the_route_shape_gate_can_fail` is its mutation self-test.
fn is_route_shaped(key: &str) -> bool {
    key == "symbol"
        || key.starts_with("symbol_")
        || key == "venue"
        || key == "venues"
        || key.starts_with("venue_")
}

#[test]
fn param_routes_table_is_exhaustive() {
    // Same construction as `param_keys_table_is_exhaustive` and for the harder reason: an
    // unclassified name is one whose route keys nothing refuses, and a route key nothing
    // refuses names an instrument real orders do not go to.
    for name in PORTABLE_STRATEGIES {
        assert_eq!(
            PARAM_ROUTES.iter().filter(|(n, _)| n == name).count(),
            1,
            "{name} needs exactly one PARAM_ROUTES row"
        );
    }
    for (name, routes) in PARAM_ROUTES {
        assert!(
            PORTABLE_STRATEGIES.contains(name),
            "PARAM_ROUTES names {name}, which is not on the roster"
        );
        match routes {
            ParamRoutes::SingleLeg(_) => {}
            ParamRoutes::MultiLeg(keys, why) => {
                assert!(
                    !keys.is_empty(),
                    "{name} is MultiLeg with no route key — say SingleLeg(&[])"
                );
                assert!(why.len() > 20, "{name}'s MultiLeg reason is too thin to act on: {why:?}");
            }
            ParamRoutes::NotEnumerated(why) => assert!(
                why.len() > 20,
                "{name}'s NotEnumerated reason is too thin to act on: {why:?}"
            ),
        }
    }
}

/// Direction 1: a route row may only name keys that name something.
///
/// The key must be a DECLARED [`PARAM_KEYS`] key of the same name (a route row over a key no
/// reader reads would refuse a profile for a knob that never existed), and its declared
/// [`ParamType`] must match what [`misrouted_params`] reads it through — `Str` for a
/// `Symbol`/`Venue`, `Table` for a `VenueMap`. That second half is what stops the two tables
/// drifting into a check that silently never fires: `misrouted_params` reads a `Symbol` key
/// with `as_str`, so a key declared `Number` would match nothing, forever, green.
#[test]
fn every_route_key_is_a_declared_key_of_the_right_type() {
    for (name, routes) in PARAM_ROUTES {
        let keys: &[(&str, RouteKind)] = match routes {
            ParamRoutes::SingleLeg(k) | ParamRoutes::MultiLeg(k, _) => k,
            ParamRoutes::NotEnumerated(_) => {
                // Mirrors its PARAM_KEYS row, and must: a name whose key set is unknown cannot
                // have a known route subset.
                assert!(
                    matches!(param_keys(name), Some(ParamKeys::NotEnumerated(_))),
                    "{name}'s PARAM_ROUTES row is NotEnumerated but its PARAM_KEYS row is not"
                );
                continue;
            }
        };
        let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
            panic!("{name} declares route keys but enumerates no params keys");
        };
        for (key, kind) in keys {
            let Some((_, ty)) = declared.iter().find(|(n, _)| n == key) else {
                panic!("{name}'s route key `{key}` is not a declared PARAM_KEYS key");
            };
            let want = match kind {
                RouteKind::Symbol | RouteKind::Venue => ParamType::Str,
                RouteKind::VenueMap => ParamType::Table,
            };
            assert_eq!(
                *ty, want,
                "{name}'s route key `{key}` is {kind:?}, which `misrouted_params` reads as \
                     {want:?} — but PARAM_KEYS declares it {ty:?}, so the check would never fire"
            );
        }
    }
}

/// Direction 2: a route-shaped declared key may not go unclassified — the direction that matters
/// for a FUTURE strategy declaring the next `symbol`.
#[test]
fn every_route_shaped_declared_key_has_a_route_row() {
    for (name, keys) in PARAM_KEYS {
        let ParamKeys::Declared(declared) = keys else {
            continue;
        };
        let routed: Vec<&str> = match param_routes(name) {
            Some(ParamRoutes::SingleLeg(k)) | Some(ParamRoutes::MultiLeg(k, _)) => {
                k.iter().map(|(n, _)| *n).collect()
            }
            _ => Vec::new(),
        };
        for (key, _) in declared.iter() {
            if is_route_shaped(key) {
                assert!(
                    routed.contains(key),
                    "{name} declares the route-shaped key `{key}` with no PARAM_ROUTES row: a \
                         mount would OVERRIDE it (`resolve_intent_symbol`/`resolve_intent_venue`) \
                         and nothing would refuse the profile that set it"
                );
            }
        }
    }
}

/// The mutation self-test for the shape heuristic above.
#[test]
fn the_route_shape_gate_can_fail() {
    assert!(is_route_shaped("symbol"), "the real key that carried the defect");
    assert!(is_route_shaped("symbol_a") && is_route_shaped("symbol_b"), "the two-leg spelling");
    assert!(is_route_shaped("venue") && is_route_shaped("venues"), "the venue half");
    // …and it must NOT swallow the knob keys, or direction 2 would demand a route row for every
    // key in the table and the distinction would carry no information.
    for knob in ["size", "qty", "step", "rungs", "tp", "sl", "cooldown_ms", "anchor_price"] {
        assert!(!is_route_shaped(knob), "`{knob}` is a knob, not a route");
    }
    // The gate's own input must be non-empty: if no declared key were route-shaped, direction 2
    // would pass vacuously forever.
    let route_shaped = PARAM_KEYS
        .iter()
        .filter_map(|(_, k)| match k {
            ParamKeys::Declared(d) => Some(d),
            ParamKeys::NotEnumerated(_) => None,
        })
        .flat_map(|d| d.iter())
        .filter(|(key, _)| is_route_shaped(key))
        .count();
    assert!(route_shaped > 0, "no declared key is route-shaped — direction 2 is vacuous");
}

/// `misrouted_params` reports exactly the values a mount would OVERRIDE, and nothing else.
///
/// Each assertion is a distinct arm of the rule rather than a restatement of it: the three
/// symbol states (absent / empty / naming another instrument), the venue half, the routing
/// table's per-row rule, and the two abstentions (multi-leg names, and a wrong TYPE, which is
/// `mistyped_params`' finding).
#[test]
fn misrouted_params_reports_only_what_the_mount_would_override() {
    let p = |src: &str| toml::from_str::<Value>(src).expect("test params parse");
    let keys = |src: &str| -> Vec<String> {
        misrouted_params("buy_hold", &p(src), "polymarket", "MOUNTED")
            .into_iter()
            .map(|m| m.key)
            .collect()
    };
    // ABSENT: the working default — the runtime stamps the mount's symbol onto the bar.
    assert!(keys("size = 1.0").is_empty());
    // EMPTY: a mount that cannot trade, which `resolved_params`' `opt_sym` already reports.
    assert!(keys("symbol = \"\"").is_empty());
    // AGREES: a no-op restatement of the mount, and legal.
    assert!(keys("symbol = \"MOUNTED\"").is_empty());
    // DISAGREES: the defect. One finding, naming BOTH instruments.
    let bad = misrouted_params("buy_hold", &p("symbol = \"OTHER\""), "polymarket", "MOUNTED");
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].key, "symbol");
    let said = bad[0].to_string();
    assert!(said.contains("OTHER") && said.contains("MOUNTED"), "names both: {said}");
    // A wrong TYPE is `mistyped_params`' finding — reporting it here too would hand the
    // operator two sentences about one slip, the second of them confusing.
    assert!(
        misrouted_params("buy_hold", &p("symbol = 7"), "polymarket", "MOUNTED").is_empty(),
        "a non-string symbol belongs to the TYPE check, not the ROUTE check"
    );

    // The VENUE half, on the one Live name that has one.
    let venue_keys = |src: &str| -> Vec<String> {
        misrouted_params("momentum", &p(src), "polymarket", "MOUNTED")
            .into_iter()
            .map(|m| m.key)
            .collect()
    };
    assert!(venue_keys("qty = 1.0").is_empty(), "an absent venue leaves the harness default");
    assert!(venue_keys("venue = \"polymarket\"").is_empty(), "agreeing is legal");
    assert_eq!(venue_keys("venue = \"binance\""), vec!["venue".to_string()]);
    // The routing TABLE, row by row: only this mount's own route is legal.
    assert!(venue_keys("[venues]\nMOUNTED = \"polymarket\"\n").is_empty());
    assert_eq!(
        venue_keys("[venues]\nMOUNTED = \"binance\"\n"),
        vec!["venues.MOUNTED".to_string()],
        "a row naming another VENUE is discarded by `resolve_intent_venue`"
    );
    assert_eq!(
        venue_keys("[venues]\nOTHER = \"polymarket\"\n"),
        vec!["venues.OTHER".to_string()],
        "a row naming another SYMBOL never matches the one series this mount receives"
    );
    assert_eq!(
        venue_keys("[venues]\nMOUNTED = 7\n"),
        vec!["venues.MOUNTED".to_string()],
        "a non-string row is dropped by `harness_venue_map`'s filter_map"
    );

    // MULTI-LEG names abstain: `symbol_a`/`symbol_b` name legs, not this mount's market.
    assert!(
        misrouted_params("pairs_zscore", &p("symbol_a = \"A\"\nsymbol_b = \"B\""), "v", "M")
            .is_empty(),
        "a two-leg name's keys are not claims about a single-leg mount"
    );
    // …and so does an unknown name, and a `NotEnumerated` one.
    assert!(misrouted_params("nope", &p("symbol = \"OTHER\""), "v", "M").is_empty());
    assert!(misrouted_params("spread_maker", &p("symbol = \"OTHER\""), "v", "M").is_empty());
}

/// The CLAIM behind the refusal, proven rather than asserted: with a params `symbol` that
/// disagrees with the dispatching series, the strategy really does submit under the name it was
/// handed — which is what the mount then overrides. If `BuyHold` ever started ignoring its own
/// `symbol` field, the refusal would be guarding nothing and this goes red.
#[test]
fn a_disagreeing_params_symbol_really_is_what_the_strategy_submits() {
    let params: Value = toml::from_str("size = 3.0\nsymbol = \"OTHER\"\n").unwrap();
    let mut s = BuyHold::from_params(&params);
    let mut broker = RecordingBroker::default();
    // The bar the RUNTIME delivers carries the MOUNT's symbol; the params key overrides it here,
    // and the mount then overrides it back — which is the whole defect.
    s.on_bar(&mut broker, &mounted_bar(1, 1.0));
    assert_eq!(
        broker.orders.iter().map(|(s, _, _)| s.as_str()).collect::<Vec<_>>(),
        vec!["OTHER"],
        "the params symbol must be what reaches the broker — otherwise the mount's override \
             (and the refusal that now prevents it) would be guarding nothing"
    );
}

/// The DECLARED RESIDUAL on the `momentum` route row, proven rather than asserted: the harness's
/// `venue` is a LABEL, not an order destination — what licenses [`resolved_params`] echoing
/// `venue=sim` (`crates/vike-tradehub/src/config/tests/strategy_params.rs`'s
/// `the_echo_reports_the_resolution_and_not_the_input` pins it). Two harnesses differing ONLY in
/// their venue tag must submit IDENTICALLY: [`vike_model::Broker`]'s submit verbs take no venue.
///
/// MUTATION: make the two params tables agree (`venue = "one"` on both) and the equality below
/// holds for the trivial reason instead of the real one — hence the non-vacuity assert.
#[test]
fn the_harness_venue_is_a_label_and_not_an_order_destination() {
    let drive = |venue: &str| -> Vec<(String, i32, f64)> {
        let params: Value = toml::from_str(&format!("qty = 2.0\nvenue = \"{venue}\"\n")).unwrap();
        let mut s = resolve("momentum", &params).expect("momentum resolves");
        let mut broker = RecordingBroker::default();
        // Two bars with a rising price: the controller declines its first invitation (no
        // reference yet) and opens on the second, at the default `threshold = 0`.
        for (i, px) in [1.0_f64, 2.0].into_iter().enumerate() {
            broker.price = px;
            s.on_bar(&mut broker, &mounted_bar(60_000 * (i as i64 + 1), px));
        }
        broker.orders
    };
    let one = drive("A_VENUE");
    let other = drive("ANOTHER_VENUE");
    assert!(
        !one.is_empty(),
        "the harness submitted nothing, so the equality below would hold vacuously"
    );
    assert_eq!(
        one, other,
        "two harnesses differing ONLY in their venue tag produced DIFFERENT orders — the tag \
             would then be an order destination and echoing `venue=sim` really would be a false \
             claim about where orders go"
    );
    // ...and the orders carry the MOUNT's symbol either way — the harness routes off the bar.
    assert!(one.iter().all(|(sym, _, _)| sym == "MOUNTED"), "{one:?}");
}
