use super::*;
use serde_json::json;

fn fixture() -> Symbology {
    let meta = json!({
        "universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]
    });
    Symbology::from_meta(&meta, &fixture_spot())
}

/// HYPE spot on mainnet: token index 150, spot pair index 107 -> coin "@107"; `PURR/USDC` the one literal coin.
fn fixture_spot() -> serde_json::Value {
    json!({
        "tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "PURR", "szDecimals": 0, "index": 1},
            {"name": "HYPE", "szDecimals": 2, "index": 150}
        ],
        "universe": [
            {"name": "PURR/USDC", "tokens": [1, 0], "index": 0, "isCanonical": true},
            {"name": "@107", "tokens": [150, 0], "index": 107, "isCanonical": true}
        ]
    })
}

/// A `spotMeta` naming USDC as token 0: the collateral token every builder-dex fixture below states.
fn usdc() -> serde_json::Value {
    json!({"tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}], "universe": []})
}

#[test]
fn perp_asset_id_is_the_universe_array_index() {
    let s = fixture();
    let btc = s.by_symbol("BTCUSDC.P").expect("BTCUSDC.P");
    assert_eq!(btc.asset_id, 0);
    assert_eq!(btc.coin, "BTC");
    assert_eq!(btc.asset_class, AssetClass::CryptoPerp);
    assert_eq!(btc.sz_decimals, 5);
    assert_eq!(btc.max_leverage, Some(40));
    assert_eq!(s.by_symbol("ETHUSDC.P").unwrap().asset_id, 1);
}

/// **A row is held under its USER name** (decision 0290): the translator's spelling of the row the kept list holds,
/// the venue's listing name and coin beside it. A perpetual is `BTCUSDC.P` (a case-kept coin upper case,
/// `KPEPEUSDC.P` for `kPEPE`), a spot pair `HYPEUSDC`; an old spelling (`BTC`, `kPEPE`, `HYPE/USDC`) names nothing.
#[test]
fn a_row_is_held_under_its_user_name_beside_its_listing_name_and_coin() {
    let meta = json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "kPEPE", "szDecimals": 0, "maxLeverage": 10}
    ]});
    let s = Symbology::from_meta(&meta, &fixture_spot());
    for (user, raw, coin) in [
        ("BTCUSDC.P", "BTC", "BTC"),
        ("KPEPEUSDC.P", "kPEPE", "kPEPE"),
        ("HYPEUSDC", "HYPE/USDC", "@107"),
        ("PURRUSDC", "PURR/USDC", "PURR/USDC"),
    ] {
        let row = s.by_symbol(user).expect(user);
        assert_eq!(
            (row.symbol.as_str(), row.raw_symbol.as_str(), row.coin.as_str()),
            (user, raw, coin)
        );
        assert_eq!(s.symbol_for_coin(coin), Some(user), "{coin} names {user} back");
    }
    for old in ["BTC", "kPEPE", "KPEPE", "HYPE/USDC", "@107", "btcusdc.p"] {
        assert!(s.by_symbol(old).is_none(), "{old} is no user name");
    }
}

/// **Two rows answering one user name name neither** (decision 0290): `kPEPE` and `KPEPE` are both `KPEPEUSDC.P` as the
/// writer stores them, and two pairs of one base and quote token name both `FOOUSDC`; each name is refused whole (no
/// order is signed on a guess), and every other row is held as before. Every row is still listed.
#[test]
fn two_rows_answering_one_user_name_name_neither() {
    let meta = json!({"universe": [
        {"name": "kPEPE", "szDecimals": 0},
        {"name": "BTC", "szDecimals": 5},
        {"name": "KPEPE", "szDecimals": 0}
    ]});
    let spot = json!({
        "tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "FOO", "szDecimals": 1, "index": 7},
            {"name": "FOO", "szDecimals": 1, "index": 8}
        ],
        "universe": [
            {"name": "@3", "tokens": [7, 0], "index": 3},
            {"name": "@4", "tokens": [8, 0], "index": 4}
        ]
    });
    let kept = crate::test_support::written(&meta, &spot, &[]);
    let s = Symbology::from_meta_named_by(&meta, &spot, Some(kept.as_slice()));
    assert!(s.by_symbol("KPEPEUSDC.P").is_none(), "kPEPE and KPEPE answer it");
    assert!(s.by_coin("kPEPE").is_none() && s.by_coin("KPEPE").is_none());
    assert!(s.by_symbol("FOOUSDC").is_none(), "two FOO tokens answer it");
    assert!(s.by_coin("@3").is_none() && s.by_coin("@4").is_none());
    assert_eq!(s.by_symbol("BTCUSDC.P").map(|i| i.asset_id), Some(1), "the rest is held");
    assert_eq!(s.len(), 1);
    assert_eq!(s.listed().len(), 5, "every row is listed");
}

/// **A name is READ off the kept row, the ids off THIS network's listing** (the owner's ruling «Б», 2026-11-16): the
/// kept row `BTC` stores `BTCUSDC.STORED.P`, which is not what the grammar spells, and states mainnet's asset id 3; the
/// kept `HYPE/USDC` states a coin `@1035`. Over a `meta` listing `BTC` at index 0 and a `spotMeta` listing the pair at
/// `@107`, the STORED names answer with this network's asset id and coin, and the spelled `BTCUSDC.P` names nothing.
#[test]
fn a_stored_name_answers_with_this_networks_asset_id_and_coin() {
    let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]});
    let mut kept = crate::test_support::written(&meta, &fixture_spot(), &[]);
    for row in &mut kept {
        let (user, coin, asset_id) = match row.raw_symbol.as_str() {
            "BTC" => ("BTCUSDC.STORED.P", "BTC", 3),
            "HYPE/USDC" => ("HYPEUSDC.STORED", "@1035", 11_035),
            _ => continue,
        };
        row.naming = Some(vike_model::InstrumentNaming::User(user.to_string()));
        row.wire.coin = Some(coin.to_string());
        row.wire.asset_id = Some(asset_id);
    }
    let s = Symbology::from_meta_named_by(&meta, &fixture_spot(), Some(kept.as_slice()));
    let btc = s.by_symbol("BTCUSDC.STORED.P").expect("the stored name");
    assert_eq!((btc.asset_id, btc.coin.as_str(), btc.raw_symbol.as_str()), (0, "BTC", "BTC"));
    assert!(s.by_symbol("BTCUSDC.P").is_none(), "the grammar's spelling is not read");
    let hype = s.by_symbol("HYPEUSDC.STORED").expect("the stored name");
    assert_eq!((hype.asset_id, hype.coin.as_str()), (SPOT_ASSET_OFFSET + 107, "@107"));
    assert_eq!(s.symbol_for_coin("@107"), Some("HYPEUSDC.STORED"));
    assert!(s.by_coin("@1035").is_none(), "the kept row's coin is mainnet's, never read");
    assert_eq!(s.by_symbol("PURRUSDC").map(|i| i.asset_id), Some(SPOT_ASSET_OFFSET));
    assert_eq!(s.why_unnamed(), None);
}

/// **A row that stores no name is left out, and so is every row with no kept list; the listing stands either way**
/// (the owner's ruling «Б»): a list written before names were stored names its unnamed rows nothing and says the
/// refresh; with no kept list nothing is named, the catalog still lists every row, and the words name the refresh.
#[test]
fn a_row_with_no_stored_name_or_no_kept_list_is_left_out_and_the_listing_stands() {
    let meta = json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let mut kept = crate::test_support::written(&meta, &fixture_spot(), &[]);
    kept.iter_mut().filter(|row| row.raw_symbol == "ETH").for_each(|row| row.naming = None);
    let s = Symbology::from_meta_named_by(&meta, &fixture_spot(), Some(kept.as_slice()));
    assert_eq!(s.by_symbol("BTCUSDC.P").map(|i| i.asset_id), Some(0));
    assert!(s.by_symbol("ETHUSDC.P").is_none() && s.by_coin("ETH").is_none(), "pre-«Б»: unnamed");
    assert_eq!(s.why_unnamed(), Some(vike_catalog::predates_symbol_names("hyperliquid").as_str()));

    let none = Symbology::from_meta_named_by(&meta, &fixture_spot(), None);
    assert!(none.is_empty() && none.by_symbol("BTCUSDC.P").is_none(), "no kept list: no name");
    assert_eq!(
        none.why_unnamed(),
        Some(vike_catalog::no_kept_list_refusal("hyperliquid").as_str())
    );
    let raw: Vec<&str> = none.listed().iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(raw, ["BTC", "ETH", "PURR/USDC", "HYPE/USDC"], "the listing needs no kept list");
    let rows = crate::catalog::instruments_from_symbology(&none);
    assert_eq!(rows.len(), 4, "and the catalog lists every row of it");
    assert_eq!(rows[0].wire.asset_id, Some(0));
}

#[test]
fn spot_asset_id_is_10000_plus_pair_index() {
    let s = fixture();
    // PURR/USDC: pair index 0 -> asset id 10000; coin is the literal name, NOT "@0".
    let purr = s.by_symbol("PURRUSDC").expect("PURRUSDC");
    assert_eq!(purr.asset_id, SPOT_ASSET_OFFSET);
    assert_eq!(purr.coin, "PURR/USDC");
    assert_eq!(purr.asset_class, AssetClass::CryptoSpot);
    assert_eq!(purr.sz_decimals, 0); // base token (PURR) szDecimals
    assert!(purr.max_leverage.is_none());
}

#[test]
fn spot_at_index_coin_and_positional_base_quote() {
    let s = fixture();
    // "@107" pair, tokens [150, 0] -> base HYPE / quote USDC -> listing "HYPE/USDC", user name "HYPEUSDC".
    let hype = s.by_symbol("HYPEUSDC").expect("HYPEUSDC");
    assert_eq!(hype.raw_symbol, "HYPE/USDC");
    assert_eq!(hype.coin, "@107");
    assert_eq!(hype.asset_id, SPOT_ASSET_OFFSET + 107);
    assert_eq!(hype.sz_decimals, 2);
    // base/quote are the token NAMES, carried as fields, not split back off the symbol.
    assert_eq!(hype.base, "HYPE");
    assert_eq!(hype.quote.as_deref(), Some("USDC"));
    // a perp row publishes no quote; its base is the coin.
    let btc = s.by_symbol("BTCUSDC.P").unwrap();
    assert_eq!((btc.base.as_str(), btc.quote.as_deref()), ("BTC", None));
}

#[test]
fn coin_and_symbol_round_trip() {
    let s = fixture();
    assert_eq!(s.coin_for("BTCUSDC.P"), Some("BTC"));
    assert_eq!(s.asset_id_for("HYPEUSDC"), Some(10107));
    assert_eq!(s.symbol_for_coin("@107"), Some("HYPEUSDC"));
    assert_eq!(s.by_coin("PURR/USDC").unwrap().symbol, "PURRUSDC");
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
    assert_eq!(s.by_symbol("BTCUSDC.P").unwrap().dex, None);
    assert_eq!(s.by_symbol("HYPEUSDC").unwrap().dex, None);
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
    // Dex "test" at perp_dex_index 1, markets test:ABC (meta idx 0) and test:XYZ (meta idx 1), collateral USDC.
    let dex_meta = json!({"universe": [
        {"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10},
        {"name": "test:XYZ", "szDecimals": 3}
    ], "collateralToken": 0});
    s.extend_with_perp_dex(&dex_meta, 1, "test", &spot);

    // Core BTC untouched.
    assert_eq!(s.asset_id_for("BTCUSDC.P"), Some(0));
    // test:ABC = 100000 + 1*10000 + 0 = 110000 (the exact HL docs worked example).
    let abc = s.by_symbol("TEST:ABCUSDC.P").expect("TEST:ABCUSDC.P");
    assert_eq!(abc.asset_id, 110_000);
    assert_eq!(abc.coin, "test:ABC", "coin is the dex-qualified name verbatim (wire round-trips)");
    assert_eq!(abc.asset_class, AssetClass::CryptoPerp);
    assert_eq!(abc.dex.as_deref(), Some("test"), "row tagged with its deployer dex");
    assert_eq!(abc.max_leverage, Some(10));
    assert_eq!(abc.base, "ABC", "the base is the coin under the known `test:` prefix");
    assert_eq!(abc.quote.as_deref(), Some("USDC"), "the dex's collateral token");
    assert_eq!(abc.raw_symbol, "test:ABC");
    // test:XYZ = 100000 + 1*10000 + 1 = 110001.
    assert_eq!(s.asset_id_for("TEST:XYZUSDC.P"), Some(110_001));
    // coin ⇄ user name round-trips.
    assert_eq!(s.symbol_for_coin("test:ABC"), Some("TEST:ABCUSDC.P"));
    assert_eq!(s.by_coin("test:XYZ").unwrap().symbol, "TEST:XYZUSDC.P");
}

#[test]
fn extend_with_perp_dex_is_purely_additive_to_core_rows() {
    let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5}]});
    let spot = json!({});
    let core = Symbology::from_meta(&meta, &spot);
    let mut ext = Symbology::from_meta(&meta, &spot);
    ext.extend_with_perp_dex(
        &json!({"universe": [{"name": "test:ABC", "szDecimals": 2}], "collateralToken": 0}),
        1,
        "test",
        &usdc(),
    );
    // The core BTC row is byte-identical (InstrumentRef: Eq) and untagged; only the count grows.
    assert_eq!(core.by_symbol("BTCUSDC.P"), ext.by_symbol("BTCUSDC.P"));
    assert_eq!(ext.by_symbol("BTCUSDC.P").unwrap().dex, None);
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

    assert!(
        !s.by_symbol("BTCUSDC.P").unwrap().only_isolated,
        "absent onlyIsolated ⇒ cross available"
    );
    assert!(s.by_symbol("HPOSUSDC.P").unwrap().only_isolated, "present true ⇒ isolated-only");
    assert!(s.by_symbol("RLBUSDC.P").unwrap().only_isolated, "true survives alongside isDelisted");
    assert!(!s.by_symbol("ETHUSDC.P").unwrap().only_isolated, "explicit false stays false");
    assert!(
        !s.by_symbol("JUNKUSDC.P").unwrap().only_isolated,
        "non-bool degrades to false, not true"
    );

    // The flag is independent of every neighbouring fact it shares a row with — it must not be
    // inferred from low leverage, delisting, or array position.
    assert_eq!(s.by_symbol("HPOSUSDC.P").unwrap().max_leverage, Some(3));
    assert_eq!(s.by_symbol("HPOSUSDC.P").unwrap().asset_id, 1);
    assert_eq!(s.by_symbol("HPOSUSDC.P").unwrap().asset_class, AssetClass::CryptoPerp);
    // Exactly the two `true` rows across the whole universe — no over- or under-matching.
    let iso: Vec<&str> = s.iter().filter(|i| i.only_isolated).map(|i| i.symbol.as_str()).collect();
    assert_eq!(iso, vec!["HPOSUSDC.P", "RLBUSDC.P"]);
}

/// Spot rows are `false`: spot has no margin axis, so cross is not "unavailable" there. Pinned
/// so the field is never quietly repurposed into "N/A" for spot (which is what `max_leverage`,
/// deliberately `Option`, means by `None` — the two fields answer different questions).
#[test]
fn spot_rows_are_not_isolated_only_and_leverage_stays_none() {
    let s = fixture();
    for symbol in ["PURRUSDC", "HYPEUSDC"] {
        let instrument = s.by_symbol(symbol).expect(symbol);
        assert!(!instrument.only_isolated, "{symbol}: spot is never isolated-only");
        assert!(instrument.max_leverage.is_none(), "{symbol}: spot has no leverage value");
    }
    // …and the perps in the same fixture (no `onlyIsolated` key at all) are `false` too.
    assert!(!s.by_symbol("BTCUSDC.P").unwrap().only_isolated);
    assert!(s.iter().all(|i| !i.only_isolated), "fixture universe has no isolated-only asset");
}

/// **The per-asset vs per-venue divergence, pinned in both directions.**
///
/// An isolated-only asset and an ordinary one sit in the SAME universe under the SAME
/// `VenueCapabilities` row, and [`InstrumentRef::effective_margin_mode`] must separate them:
///
/// - HPOS (`onlyIsolated: true`) ⇒ `Isolated`, which is what the venue itself reports for a
///   position on it (`recon_client`'s `parse_positions`, `leverage.type == "isolated"`) — and
///   is NOT what `capabilities_for("hyperliquid").default_margin_mode` says. That inequality is
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
    let venue_default =
        vike_venues::capabilities_for(crate::transport::consts::VENUE).default_margin_mode;
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
    for symbol in ["HPOSUSDC.P", "CASHCATUSDC.P"] {
        let instrument = s.by_symbol(symbol).expect(symbol);
        assert_eq!(
            instrument.effective_margin_mode(),
            MarginMode::Isolated,
            "{symbol}: isolated-only ⇒ Isolated, whatever the per-venue row says"
        );
        assert_ne!(
            instrument.effective_margin_mode(),
            venue_default,
            "{symbol}: this INEQUALITY is the divergence — the per-venue field is wrong here"
        );
    }

    // The ordinary row: no narrowing at all, the venue default passes through verbatim.
    let btc = s.by_symbol("BTCUSDC.P").unwrap();
    assert_eq!(btc.effective_margin_mode(), venue_default, "BTC: unnarrowed venue default");
    assert_eq!(btc.effective_margin_mode(), MarginMode::Cross);

    // Spot: no margin axis, no narrowing — the venue default, unchanged.
    let spot = fixture();
    let purr = spot.by_symbol("PURRUSDC").expect("PURRUSDC");
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
        ], "collateralToken": 0}),
        1,
        "test",
        &usdc(),
    );
    assert!(s.by_symbol("TEST:ISOUSDC.P").unwrap().only_isolated, "HIP-3 row reads the flag");
    assert!(!s.by_symbol("TEST:XUSDC.P").unwrap().only_isolated, "HIP-3 absent ⇒ false");
}

#[test]
fn perp_dex_index_two_uses_the_next_stride() {
    // A second builder dex (perp_dex_index 2): 100000 + 2*10000 + 0 = 120000.
    let mut s = Symbology::from_meta(&json!({"universe": []}), &json!({}));
    s.extend_with_perp_dex(
        &json!({"universe": [{"name": "vntls:AAPL", "szDecimals": 2}], "collateralToken": 0}),
        2,
        "vntls",
        &usdc(),
    );
    assert_eq!(s.asset_id_for("VNTLS:AAPLUSDC.P"), Some(120_000));
    assert_eq!(s.by_symbol("VNTLS:AAPLUSDC.P").unwrap().dex.as_deref(), Some("vntls"));
}

/// `isDelisted` and the `marginMode` words, read the way `onlyIsolated` is: present-only-when-true,
/// and an unreadable value never manufactures a restriction. A delisted row still occupies its
/// `meta.universe` slot (its index is the order asset id of every row after it), so it is parsed,
/// not dropped. `strictIsolated` and `noCross` both remove the cross option, exactly as
/// `onlyIsolated` does, but they read as `no_cross` (the facts verb refuses them), NOT as `only_isolated`.
#[test]
fn delisted_and_margin_mode_words_are_read_present_only_and_degrade_to_false() {
    let meta = json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "OLD", "szDecimals": 1, "maxLeverage": 3, "isDelisted": true},
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25, "isDelisted": false},
        {"name": "JUNK", "szDecimals": 1, "isDelisted": "true", "marginMode": 7},
        {"name": "STRICT", "szDecimals": 1, "maxLeverage": 3, "marginMode": "strictIsolated"},
        {"name": "NOCROSS", "szDecimals": 1, "maxLeverage": 5, "marginMode": "noCross"},
        {"name": "FUTURE", "szDecimals": 1, "maxLeverage": 5, "marginMode": "someNewWord"},
    ]});
    let s = Symbology::from_meta(&meta, &json!({}));
    let delisted: Vec<&str> = s.iter().filter(|i| i.delisted).map(|i| i.symbol.as_str()).collect();
    assert_eq!(delisted, vec!["OLDUSDC.P"], "only the row that says `isDelisted: true`");
    let no_cross: Vec<&str> = s.iter().filter(|i| i.no_cross).map(|i| i.symbol.as_str()).collect();
    assert_eq!(
        no_cross,
        vec!["STRICTUSDC.P", "NOCROSSUSDC.P"],
        "a margin-mode word that removes cross"
    );
    // ⚠ PINNED UNCHANGED: those words are NOT `only_isolated`, so the margin mode of a MOUNTED coin
    // reads exactly as it did before the any-symbol flip (only `onlyIsolated: true` narrows it).
    assert!(s.iter().all(|i| !i.only_isolated), "marginMode never sets only_isolated");
    let venue_default = vike_venues::capabilities_for("hyperliquid").default_margin_mode;
    for coin in ["STRICTUSDC.P", "NOCROSSUSDC.P", "FUTUREUSDC.P"] {
        assert_eq!(s.by_symbol(coin).unwrap().effective_margin_mode(), venue_default, "{coin}");
    }
    assert_eq!(s.by_symbol("OLDUSDC.P").unwrap().asset_id, 1, "a delisted row keeps its index");
    assert_eq!(s.by_symbol("ETHUSDC.P").unwrap().asset_id, 2);
}

/// A HIP-3 row and a spot pair read the flags through the same code (spot has neither).
#[test]
fn hip3_rows_carry_delisted_and_spot_never_does() {
    let mut s = Symbology::from_meta(&json!({"universe": []}), &json!({}));
    s.extend_with_perp_dex(
        &json!({"universe": [
            {"name": "test:GONE", "szDecimals": 2, "maxLeverage": 3, "isDelisted": true},
            {"name": "test:X", "szDecimals": 2, "maxLeverage": 10}
        ], "collateralToken": 0}),
        1,
        "test",
        &usdc(),
    );
    assert!(s.by_symbol("TEST:GONEUSDC.P").unwrap().delisted);
    assert!(!s.by_symbol("TEST:XUSDC.P").unwrap().delisted);
    assert!(s.iter().filter(|i| i.asset_class == AssetClass::CryptoSpot).all(|i| !i.delisted));
}
