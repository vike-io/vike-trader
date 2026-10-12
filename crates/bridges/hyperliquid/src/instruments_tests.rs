use super::*;
use serde_json::json;

/// Synthetic `meta`/`spotMeta` mirroring the [`crate::symbology`] fixture (BTC/ETH perps; PURR &
/// HYPE spot). Drives the properties derivation with NO network.
fn instruments() -> HyperliquidInstruments {
    let meta = json!({
        "universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]
    });
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
    HyperliquidInstruments::build(&meta, &spot, None)
}

#[test]
fn perp_grid_uses_max_decimals_6() {
    let i = instruments();
    // BTC szDecimals 5 → tick 10^-(6-5)=0.1, step 10^-5, min_qty=step.
    let btc = i.properties("BTC").expect("BTC");
    assert_eq!(btc.tick_size, 0.1);
    assert_eq!(btc.step_size, 1e-5);
    assert_eq!(btc.min_qty, 1e-5);
    assert_eq!(btc.min_notional, MIN_NOTIONAL_USD);
    assert_eq!(btc.max_qty, MAX_QTY);
    // ETH szDecimals 4 → tick 10^-(6-4)=0.01, step 10^-4.
    let eth = i.properties("ETH").expect("ETH");
    assert_eq!(eth.tick_size, 0.01);
    assert_eq!(eth.step_size, 1e-4);
}

#[test]
fn spot_grid_uses_max_decimals_8() {
    let i = instruments();
    // HYPE/USDC base szDecimals 2 → tick 10^-(8-2)=1e-6, step 10^-2=0.01.
    let hype = i.properties("HYPE/USDC").expect("HYPE/USDC");
    assert_eq!(hype.tick_size, 1e-6);
    assert_eq!(hype.step_size, 0.01);
    assert_eq!(hype.min_qty, 0.01);
    // PURR/USDC base szDecimals 0 → tick 10^-8, step 10^0=1.0.
    let purr = i.properties("PURR/USDC").expect("PURR/USDC");
    assert_eq!(purr.tick_size, 1e-8);
    assert_eq!(purr.step_size, 1.0);
    assert_eq!(purr.min_qty, 1.0);
}

#[test]
fn caches_every_symbology_instrument_and_leverage_stays_on_the_ref() {
    let i = instruments();
    assert_eq!(i.len(), 4); // BTC, ETH, PURR/USDC, HYPE/USDC
    assert!(!i.is_empty());
    // symbology still reachable for the leverage / asset-id facts SymbolProperties can't hold.
    assert_eq!(i.symbology().by_symbol("BTC").unwrap().max_leverage, Some(40));
    assert_eq!(i.symbology().asset_id_for("HYPE/USDC"), Some(10107));
    // unknown symbol → no grid.
    assert!(i.properties("DOGE").is_none());
    // iteration yields all four in insertion order (perps first).
    let syms: Vec<&str> = i.iter().map(|(s, _)| s).collect();
    assert_eq!(syms, vec!["BTC", "ETH", "PURR/USDC", "HYPE/USDC"]);
}

/// `only_isolated` survives the whole load path (meta → `Symbology` → cached instruments) and
/// stays OFF the [`SymbolProperties`] grid. The grid of an isolated-only asset must be
/// byte-identical to a cross asset with the same `szDecimals`: this fact constrains the margin
/// mode, never the rounding — so wiring it can not have moved any order's price or size.
#[test]
fn only_isolated_reaches_the_loaded_universe_without_touching_the_grid() {
    let meta = json!({
        "universe": [
            {"name": "BTC", "szDecimals": 2, "maxLeverage": 40},
            {"name": "HPOS", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true}
        ]
    });
    let i = HyperliquidInstruments::build(&meta, &json!({}), None);
    // Reachable through the symbology, exactly like `max_leverage`.
    assert!(i.symbology().by_symbol("HPOS").unwrap().only_isolated);
    assert!(!i.symbology().by_symbol("BTC").unwrap().only_isolated);
    // Same szDecimals ⇒ byte-identical placement grid, isolated-only or not.
    assert_eq!(i.properties("HPOS").expect("HPOS"), i.properties("BTC").expect("BTC"));
}

#[test]
fn empty_universe_yields_no_properties() {
    let empty = HyperliquidInstruments::build(&json!({"universe": []}), &json!({}), None);
    assert!(empty.is_empty());
    assert!(empty.properties("BTC").is_none());
}

// ---- HIP-3 opt-in enumeration (the `load_from` fetch seam — no network) ----

use std::cell::RefCell;

/// The core `meta` fixture (BTC/ETH perps) reused across the HIP-3 seam tests.
fn core_meta() -> serde_json::Value {
    json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]})
}
/// The core `spotMeta` fixture (PURR & HYPE spot).
fn core_spot() -> serde_json::Value {
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

#[test]
fn hip3_flag_is_exact_one() {
    assert!(hip3_flag(Some("1")));
    assert!(!hip3_flag(Some("0")));
    assert!(!hip3_flag(Some("true")), "not a fuzzy truthy parse");
    assert!(!hip3_flag(Some("")));
    assert!(!hip3_flag(None), "absent env ⇒ OFF (default)");
}

#[test]
fn hip3_off_fetches_only_core_meta_and_is_byte_identical() {
    let calls = RefCell::new(Vec::<String>::new());
    let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
        let t = body.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        calls.borrow_mut().push(t.clone());
        Ok(match t.as_str() {
            "meta" => core_meta(),
            "spotMeta" => core_spot(),
            other => panic!("HIP-3 OFF must not fetch {other:?}"),
        })
    };
    let off = HyperliquidInstruments::load_from(fetch, None, false).expect("core load");
    // EXACTLY the two core reads, in order — no `perpDexs`, no per-dex `meta`.
    assert_eq!(*calls.borrow(), vec!["meta".to_string(), "spotMeta".to_string()]);
    // …and the resolved universe is byte-identical to the direct core builder.
    let baseline = HyperliquidInstruments::build(&core_meta(), &core_spot(), None);
    let off_refs: Vec<&InstrumentRef> = off.symbology().iter().collect();
    let base_refs: Vec<&InstrumentRef> = baseline.symbology().iter().collect();
    assert_eq!(off_refs, base_refs, "OFF load is byte-identical to the pre-HIP-3 universe");
    assert_eq!(off.len(), baseline.len());
}

#[test]
fn hip3_on_fetches_perp_dexs_and_folds_the_builder_markets() {
    let calls = RefCell::new(Vec::<String>::new());
    let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
        let t = body.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let dex = body.get("dex").and_then(|d| d.as_str()).map(|s| s.to_string());
        calls.borrow_mut().push(match &dex {
            Some(d) => format!("meta:{d}"),
            None => t.clone(),
        });
        Ok(match (t.as_str(), dex.as_deref()) {
            ("meta", None) => core_meta(),
            ("spotMeta", _) => core_spot(),
            ("perpDexs", _) => {
                json!([null, {"name": "test", "fullName": "test dex", "deployer": "0xabc"}])
            }
            ("meta", Some("test")) => {
                json!({"universe": [{"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10}]})
            }
            _ => json!({}),
        })
    };
    let insts = HyperliquidInstruments::load_from(fetch, None, true).expect("hip3 load");
    // The request set: core reads, THEN perpDexs, THEN the per-dex meta (with the dex param).
    assert_eq!(
        *calls.borrow(),
        ["meta", "spotMeta", "perpDexs", "meta:test"].map(String::from).to_vec()
    );
    // Core markets unchanged at their core asset ids.
    assert_eq!(insts.symbology().asset_id_for("BTC"), Some(0));
    // The HIP-3 market appears with the offset asset id, a derived grid, and the dex tag.
    assert_eq!(insts.symbology().asset_id_for("test:ABC"), Some(110_000));
    assert_eq!(insts.symbology().by_symbol("test:ABC").unwrap().dex.as_deref(), Some("test"));
    assert!(insts.properties("test:ABC").is_some(), "HIP-3 market has a derived grid");
    // BTC, ETH, PURR/USDC, HYPE/USDC, test:ABC.
    assert_eq!(insts.len(), 5);
}

#[test]
fn hip3_perp_dexs_fetch_failure_falls_back_to_core_only() {
    let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
        let t = body.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match t {
            "meta" if body.get("dex").is_none() => Ok(core_meta()),
            "spotMeta" => Ok(core_spot()),
            "perpDexs" => Err(VenueApiError { code: 500, msg: "boom".into() }),
            other => panic!("no request expected after perpDexs failed, got {other:?}"),
        }
    };
    let insts = HyperliquidInstruments::load_from(fetch, None, true).expect("core survives");
    // Core universe intact; nothing HIP-3 folded (best-effort).
    assert_eq!(insts.len(), 4);
    assert!(insts.properties("test:ABC").is_none());
    assert_eq!(insts.symbology().asset_id_for("BTC"), Some(0));
}

#[test]
fn hip3_core_meta_error_still_propagates() {
    // A core `meta` failure must fail the whole load (byte-identical error behavior), even with
    // HIP-3 on — the extra reads never mask a core failure.
    let fetch = |_body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
        Err(VenueApiError { code: 503, msg: "core down".into() })
    };
    assert!(HyperliquidInstruments::load_from(fetch, None, true).is_err());
}
