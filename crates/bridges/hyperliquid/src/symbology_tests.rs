use super::*;
use serde_json::json;

fn fixture() -> Symbology {
    let meta = json!({
        "universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]
    });
    // HYPE spot on mainnet: token index 150, spot pair index 107 -> coin "@107".
    let spot = json!({
        "tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "PURR", "szDecimals": 0, "index": 1},
            {"name": "HYPE", "szDecimals": 2, "index": 150}
        ],
        "universe": [
            {"name": "PURR/USDC", "tokens": [1, 0], "index": 0, "isCanonical": true},
            {"name": "@107", "tokens": [150, 0], "index": 107, "isCanonical": true}
        ]
    });
    Symbology::from_meta(&meta, &spot)
}

#[test]
fn perp_asset_id_is_the_universe_array_index() {
    let s = fixture();
    let btc = s.by_symbol("BTC").expect("BTC");
    assert_eq!(btc.asset_id, 0);
    assert_eq!(btc.coin, "BTC");
    assert_eq!(btc.product, Product::Perp);
    assert_eq!(btc.sz_decimals, 5);
    assert_eq!(btc.max_leverage, Some(40));
    assert_eq!(s.by_symbol("ETH").unwrap().asset_id, 1);
}

#[test]
fn spot_asset_id_is_10000_plus_pair_index() {
    let s = fixture();
    // PURR/USDC: pair index 0 -> asset id 10000; coin is the literal name, NOT "@0".
    let purr = s.by_symbol("PURR/USDC").expect("PURR/USDC");
    assert_eq!(purr.asset_id, SPOT_ASSET_OFFSET);
    assert_eq!(purr.coin, "PURR/USDC");
    assert_eq!(purr.product, Product::Spot);
    assert_eq!(purr.sz_decimals, 0); // base token (PURR) szDecimals
    assert!(purr.max_leverage.is_none());
}

#[test]
fn spot_at_index_coin_and_positional_base_quote() {
    let s = fixture();
    // "@107" pair, tokens [150, 0] -> base HYPE / quote USDC -> symbol "HYPE/USDC".
    let hype = s.by_symbol("HYPE/USDC").expect("HYPE/USDC");
    assert_eq!(hype.coin, "@107");
    assert_eq!(hype.asset_id, SPOT_ASSET_OFFSET + 107);
    assert_eq!(hype.sz_decimals, 2);
}

#[test]
fn coin_and_symbol_round_trip() {
    let s = fixture();
    assert_eq!(s.coin_for("BTC"), Some("BTC"));
    assert_eq!(s.asset_id_for("HYPE/USDC"), Some(10107));
    assert_eq!(s.symbol_for_coin("@107"), Some("HYPE/USDC"));
    assert_eq!(s.by_coin("PURR/USDC").unwrap().symbol, "PURR/USDC");
    assert_eq!(s.len(), 4);
}

#[test]
fn malformed_spot_pair_is_skipped() {
    let meta = json!({"universe": []});
    let spot = json!({
        "tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}],
        "universe": [{"name": "@2", "tokens": [99], "index": 2}] // 1 token -> skip
    });
    let s = Symbology::from_meta(&meta, &spot);
    assert!(s.is_empty());
}

#[test]
fn core_rows_carry_no_dex_tag() {
    let s = fixture();
    assert_eq!(s.by_symbol("BTC").unwrap().dex, None);
    assert_eq!(s.by_symbol("HYPE/USDC").unwrap().dex, None);
}

#[test]
fn parse_perp_dexs_skips_core_null_and_indexes_builders_from_one() {
    // The first element is the core dex (null); builder dexs follow at array index 1, 2, …
    let v = json!([
        null,
        {"name": "test", "fullName": "test dex", "deployer": "0xabc", "oracleUpdater": null},
        {"name": "vntls", "fullName": "Ventuals", "deployer": "0xdef"}
    ]);
    let dexs = parse_perp_dexs(&v);
    assert_eq!(dexs.len(), 2);
    assert_eq!(dexs[0].index, 1, "first builder dex is perp_dex_index 1 (0 = core null)");
    assert_eq!(dexs[0].name, "test");
    assert_eq!(dexs[0].full_name, "test dex");
    assert_eq!(dexs[0].deployer, "0xabc");
    assert_eq!(dexs[1].index, 2);
    assert_eq!(dexs[1].name, "vntls");
}

#[test]
fn parse_perp_dexs_of_nonarray_or_core_only_is_empty() {
    assert!(parse_perp_dexs(&json!({})).is_empty(), "non-array → empty");
    assert!(parse_perp_dexs(&json!([null])).is_empty(), "core-only → no builder dexs");
    // a nameless middle entry is skipped but does NOT renumber the ones after it.
    let v = json!([null, {"deployer": "0x0"}, {"name": "keep"}]);
    let dexs = parse_perp_dexs(&v);
    assert_eq!(dexs.len(), 1);
    assert_eq!(dexs[0].index, 2, "positions stay positional across a skipped entry");
    assert_eq!(dexs[0].name, "keep");
}

#[test]
fn hip3_asset_ids_use_the_offset_schema_and_tag_the_dex() {
    // Core BTC = universe index 0 (asset 0), spot empty.
    let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]});
    let spot = json!({"tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}], "universe": []});
    let mut s = Symbology::from_meta(&meta, &spot);
    // Dex "test" at perp_dex_index 1, markets test:ABC (meta idx 0) and test:XYZ (meta idx 1).
    let dex_meta = json!({"universe": [
        {"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10},
        {"name": "test:XYZ", "szDecimals": 3}
    ]});
    s.extend_with_perp_dex(&dex_meta, 1, "test");

    // Core BTC untouched.
    assert_eq!(s.asset_id_for("BTC"), Some(0));
    // test:ABC = 100000 + 1*10000 + 0 = 110000 (the exact HL docs worked example).
    let abc = s.by_symbol("test:ABC").expect("test:ABC");
    assert_eq!(abc.asset_id, 110_000);
    assert_eq!(abc.coin, "test:ABC", "coin is the dex-qualified name verbatim (wire round-trips)");
    assert_eq!(abc.product, Product::Perp);
    assert_eq!(abc.dex.as_deref(), Some("test"), "row tagged with its deployer dex");
    assert_eq!(abc.max_leverage, Some(10));
    // test:XYZ = 100000 + 1*10000 + 1 = 110001.
    assert_eq!(s.asset_id_for("test:XYZ"), Some(110_001));
    // coin ⇄ symbol round-trips through the qualified name.
    assert_eq!(s.symbol_for_coin("test:ABC"), Some("test:ABC"));
    assert_eq!(s.by_coin("test:XYZ").unwrap().symbol, "test:XYZ");
}

#[test]
fn extend_with_perp_dex_is_purely_additive_to_core_rows() {
    let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5}]});
    let spot = json!({});
    let core = Symbology::from_meta(&meta, &spot);
    let mut ext = Symbology::from_meta(&meta, &spot);
    ext.extend_with_perp_dex(
        &json!({"universe": [{"name": "test:ABC", "szDecimals": 2}]}),
        1,
        "test",
    );
    // The core BTC row is byte-identical (InstrumentRef: Eq) and untagged; only the count grows.
    assert_eq!(core.by_symbol("BTC"), ext.by_symbol("BTC"));
    assert_eq!(ext.by_symbol("BTC").unwrap().dex, None);
    assert_eq!(core.len(), 1);
    assert_eq!(ext.len(), 2);
}

/// The `onlyIsolated` parse, pinned against the REAL wire encoding rather than a symmetric
/// guess. Fixture rows are verbatim-shaped `meta.universe` entries using the actual live field
/// set (`name`/`szDecimals`/`maxLeverage`/`marginTableId`/`isDelisted`/`onlyIsolated`) and the
/// actual live isolated-only names, so the four cases that matter are each nailed down:
///
/// - **absent** (BTC — how 223 of 232 live rows look) ⇒ `false`, the venue spelling "cross is
///   available". This is the case a naive `Option<bool>` model would leave ambiguous.
/// - **present `true`** (HPOS/RLB — how all 9 live isolated-only rows look) ⇒ `true`.
/// - **explicit `false`** ⇒ `false`. HL emits this on nothing today, but if it ever starts, it
///   must NOT read as `true` — this is the "in either direction" half.
/// - **non-bool junk** (the string `"true"`) ⇒ `false`, never `true`: an unreadable value may
///   not manufacture a restriction the venue did not state.
#[test]
fn only_isolated_matches_the_live_wire_encoding() {
    let meta = json!({"universe": [
        // Absent — the overwhelming majority shape (223/232 live).
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40, "marginTableId": 50},
        // Present `true` — the live isolated-only shape, real names from the 2026-08-05 body.
        {"name": "HPOS", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true},
        {"name": "RLB", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true,
         "isDelisted": true},
        // Explicit `false` — not emitted by HL today; must stay `false`, not flip to `true`.
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25, "onlyIsolated": false},
        // Malformed/non-bool — must degrade to `false` (never invent a restriction).
        {"name": "JUNK", "szDecimals": 1, "onlyIsolated": "true"},
    ]});
    let s = Symbology::from_meta(&meta, &json!({}));

    assert!(!s.by_symbol("BTC").unwrap().only_isolated, "absent onlyIsolated ⇒ cross available");
    assert!(s.by_symbol("HPOS").unwrap().only_isolated, "present true ⇒ isolated-only");
    assert!(s.by_symbol("RLB").unwrap().only_isolated, "true survives alongside isDelisted");
    assert!(!s.by_symbol("ETH").unwrap().only_isolated, "explicit false stays false");
    assert!(!s.by_symbol("JUNK").unwrap().only_isolated, "non-bool degrades to false, not true");

    // The flag is independent of every neighbouring fact it shares a row with — it must not be
    // inferred from low leverage, delisting, or array position.
    assert_eq!(s.by_symbol("HPOS").unwrap().max_leverage, Some(3));
    assert_eq!(s.by_symbol("HPOS").unwrap().asset_id, 1);
    assert_eq!(s.by_symbol("HPOS").unwrap().product, Product::Perp);
    // Exactly the two `true` rows across the whole universe — no over- or under-matching.
    let iso: Vec<&str> = s.iter().filter(|i| i.only_isolated).map(|i| i.symbol.as_str()).collect();
    assert_eq!(iso, vec!["HPOS", "RLB"]);
}

/// Spot rows are `false`: spot has no margin axis, so cross is not "unavailable" there. Pinned
/// so the field is never quietly repurposed into "N/A" for spot (which is what `max_leverage`,
/// deliberately `Option`, means by `None` — the two fields answer different questions).
#[test]
fn spot_rows_are_not_isolated_only_and_leverage_stays_none() {
    let s = fixture();
    for sym in ["PURR/USDC", "HYPE/USDC"] {
        let inst = s.by_symbol(sym).expect(sym);
        assert!(!inst.only_isolated, "{sym}: spot is never isolated-only");
        assert!(inst.max_leverage.is_none(), "{sym}: spot has no leverage value");
    }
    // …and the perps in the same fixture (no `onlyIsolated` key at all) are `false` too.
    assert!(!s.by_symbol("BTC").unwrap().only_isolated);
    assert!(s.iter().all(|i| !i.only_isolated), "fixture universe has no isolated-only asset");
}

/// **The per-asset vs per-venue divergence, pinned in both directions.**
///
/// An isolated-only asset and an ordinary one sit in the SAME universe under the SAME
/// `VenueCaps` row, and [`InstrumentRef::effective_margin_mode`] must separate them:
///
/// - HPOS (`onlyIsolated: true`) ⇒ `Isolated`, which is what the venue itself reports for a
///   position on it (`recon_client`'s `parse_positions`, `leverage.type == "isolated"`) — and
///   is NOT what `caps_for("hyperliquid").default_margin_mode` says. That inequality is
///   asserted explicitly rather than left implicit: it IS the divergence, and a future PR that
///   "fixes" it by flipping the venue-level field to `Isolated` would break every ordinary
///   asset, so the assert must fail loudly if the per-venue field is ever made to agree.
/// - BTC (flag absent) ⇒ exactly the venue default, byte-for-byte — narrowing applies to the
///   9 rows that carry the flag and to nothing else.
///
/// Spot is asserted too: `only_isolated` is false there, so the venue default passes through
/// unchanged. The accessor answers "has cross been removed?" and deliberately makes no claim
/// about spot's cash funding.
#[test]
fn effective_margin_mode_is_per_asset_not_per_venue() {
    let venue_default = vike_model::caps_for(crate::consts::VENUE).default_margin_mode;
    assert_eq!(venue_default, MarginMode::Cross, "the per-venue declaration under test");

    let meta = json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        // Real 2026-08-05 shapes: 8 of the 9 isolated-only assets are also delisted; CASHCAT
        // is the one still live, so the divergence is reachable, not historical.
        {"name": "HPOS", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true,
         "isDelisted": true},
        {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
    ]});
    let s = Symbology::from_meta(&meta, &json!({}));

    // The isolated-only rows: the venue's per-asset truth WINS over the per-venue default.
    for sym in ["HPOS", "CASHCAT"] {
        let inst = s.by_symbol(sym).expect(sym);
        assert_eq!(
            inst.effective_margin_mode(),
            MarginMode::Isolated,
            "{sym}: isolated-only ⇒ Isolated, whatever the per-venue row says"
        );
        assert_ne!(
            inst.effective_margin_mode(),
            venue_default,
            "{sym}: this INEQUALITY is the divergence — the per-venue field is wrong here"
        );
    }

    // The ordinary row: no narrowing at all, the venue default passes through verbatim.
    let btc = s.by_symbol("BTC").unwrap();
    assert_eq!(btc.effective_margin_mode(), venue_default, "BTC: unnarrowed venue default");
    assert_eq!(btc.effective_margin_mode(), MarginMode::Cross);

    // Spot: no margin axis, no narrowing — the venue default, unchanged.
    let spot = fixture();
    let purr = spot.by_symbol("PURR/USDC").expect("PURR/USDC");
    assert!(!purr.only_isolated);
    assert_eq!(purr.effective_margin_mode(), venue_default, "spot: venue default passes through");
}

/// A HIP-3 builder-deployed perp publishes the same row shape, so it can be isolated-only too —
/// the per-dex loader must read the flag, not default it.
#[test]
fn hip3_rows_carry_only_isolated_too() {
    let mut s = Symbology::from_meta(&json!({"universe": []}), &json!({}));
    s.extend_with_perp_dex(
        &json!({"universe": [
            {"name": "test:ISO", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true},
            {"name": "test:X", "szDecimals": 2, "maxLeverage": 10}
        ]}),
        1,
        "test",
    );
    assert!(s.by_symbol("test:ISO").unwrap().only_isolated, "HIP-3 row reads the flag");
    assert!(!s.by_symbol("test:X").unwrap().only_isolated, "HIP-3 absent ⇒ false");
}

#[test]
fn perp_dex_index_two_uses_the_next_stride() {
    // A second builder dex (perp_dex_index 2): 100000 + 2*10000 + 0 = 120000.
    let mut s = Symbology::from_meta(&json!({"universe": []}), &json!({}));
    s.extend_with_perp_dex(
        &json!({"universe": [{"name": "vntls:AAPL", "szDecimals": 2}]}),
        2,
        "vntls",
    );
    assert_eq!(s.asset_id_for("vntls:AAPL"), Some(120_000));
    assert_eq!(s.by_symbol("vntls:AAPL").unwrap().dex.as_deref(), Some("vntls"));
}
