//! LIVE demo CAPTURE smoke for the cTrader catalog's reference lists — records what a broker
//! really publishes about its symbol universe, so the catalog's class mapping
//! (`crates/bridges/ctrader/src/catalog.rs`'s `kind_of`) is written against REAL names, not
//! guessed ones.
//!
//!     cargo test -p vike-ctrader --test ctrader_catalog_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order, cancels nothing, changes nothing on the account. It runs the normal
//! connect handshake (`vike_ctrader::conn::fetch_symbol_lists`: app auth, account auth, trader,
//! symbol-category list, asset-class list, asset list, symbols list, symbol-by-id) on its OWN
//! socket, drops it, and prints ONE JSON document between two marker lines:
//!
//! - `asset_classes`: every asset class id -> name;
//! - `categories`: every symbol category id -> name + asset class id, with the number of symbols in
//!   it and up to three example names (`category_summary`);
//! - `symbols`: about 40 symbols spread round-robin across the categories, each with its id, name,
//!   description, category id, base/quote asset ids and names, and the full-symbol fields the
//!   catalog reads (digits, pip position, volume grid, swaps);
//! - `assets`: the assets those sampled symbols reference.
//!
//! It prints NO account id, no token and no credential: none of those fields is in the document.
//! The committed fixture `tests/fixtures/ctrader_catalog_capture.json` is this output, copied.
//!
//! Credentials: loaded through `load_workspace_secrets_from_env` (so a `VIKE_CREDENTIAL_SCOPE=demo`
//! run reaches only demo-tier names) and the pure `CtraderConfig::from_vars` gate, demo tier, with
//! no rotation home: the handshake never refreshes, so an expired access token FAILS this smoke
//! rather than rotating the stored pair. Absent creds -> the test SKIPS.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::fetch_symbol_lists;
use vike_secrets::credentials::Environment;

/// How many symbols the capture samples across the categories.
const SAMPLE: usize = 40;
/// Names sampled whatever their category, so the capture always shows the two the catalog's old
/// name heuristic got right and wrong.
const ALWAYS: [&str; 2] = ["EURUSD", "BTCUSD"];

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_catalog_reference_capture_smoke() {
    vike_log::test_init();
    let vars =
        vike_secrets::credentials::load_workspace_secrets_from_env(&std::env::vars().collect());
    let Some(config) = CtraderConfig::from_vars(Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: CTRADER_DEMO creds absent");
        return;
    };

    let lists = fetch_symbol_lists(config.to_conn_config())
        .expect("handshake + reference lists against the real cTrader demo endpoint");
    assert!(!lists.light.is_empty(), "the demo account lists no symbol");

    // Symbols per category (sorted by id inside each), the named ones only.
    let mut by_category: BTreeMap<i64, Vec<(i64, &str)>> = BTreeMap::new();
    for s in &lists.light {
        if let Some(name) = s.symbol_name.as_deref() {
            by_category
                .entry(s.symbol_category_id.unwrap_or(-1))
                .or_default()
                .push((s.symbol_id, name));
        }
    }
    for v in by_category.values_mut() {
        v.sort_unstable();
    }

    // Round-robin across categories until SAMPLE ids are picked, plus the ALWAYS names.
    let mut picked: BTreeSet<i64> = lists
        .light
        .iter()
        .filter(|s| s.symbol_name.as_deref().is_some_and(|n| ALWAYS.contains(&n)))
        .map(|s| s.symbol_id)
        .collect();
    let mut round = 0;
    while picked.len() < SAMPLE {
        let mut any = false;
        for v in by_category.values() {
            if let Some((id, _)) = v.get(round) {
                any = true;
                if picked.len() < SAMPLE {
                    picked.insert(*id);
                }
            }
        }
        if !any {
            break;
        }
        round += 1;
    }

    let asset_name = |id: Option<i64>| -> Value {
        id.and_then(|id| lists.assets.iter().find(|a| a.asset_id == id))
            .map_or(Value::Null, |a| json!(a.name))
    };
    let mut referenced: BTreeSet<i64> = BTreeSet::new();
    let symbols: Vec<Value> = lists
        .light
        .iter()
        .filter(|s| picked.contains(&s.symbol_id))
        .map(|s| {
            referenced.extend(s.base_asset_id);
            referenced.extend(s.quote_asset_id);
            let full = lists.full.iter().find(|f| f.symbol_id == s.symbol_id).map(|f| {
                json!({
                    "digits": f.digits,
                    "pip_position": f.pip_position,
                    "lot_size": f.lot_size,
                    "min_volume": f.min_volume,
                    "step_volume": f.step_volume,
                    "max_volume": f.max_volume,
                    "swap_long": f.swap_long,
                    "swap_short": f.swap_short,
                    "swap_calculation_type": f.swap_calculation_type,
                })
            });
            json!({
                "symbol_id": s.symbol_id,
                "name": s.symbol_name,
                "description": s.description,
                "category_id": s.symbol_category_id,
                "base_asset_id": s.base_asset_id,
                "quote_asset_id": s.quote_asset_id,
                "base_asset_name": asset_name(s.base_asset_id),
                "quote_asset_name": asset_name(s.quote_asset_id),
                "full": full,
            })
        })
        .collect();

    let category_summary: Vec<Value> = lists
        .categories
        .iter()
        .map(|c| {
            let members = by_category.get(&c.id).map(Vec::as_slice).unwrap_or_default();
            let examples: Vec<&str> = members.iter().take(3).map(|(_, n)| *n).collect();
            json!({
                "id": c.id,
                "asset_class_id": c.asset_class_id,
                "name": c.name,
                "symbols": members.len(),
                "examples": examples,
            })
        })
        .collect();
    let uncategorised = by_category.get(&-1).map_or(0, Vec::len);

    let doc = json!({
        "asset_classes": lists.asset_classes.iter().map(|a| json!({"id": a.id, "name": a.name})).collect::<Vec<_>>(),
        "categories": lists.categories.iter().map(|c| json!({"id": c.id, "asset_class_id": c.asset_class_id, "name": c.name})).collect::<Vec<_>>(),
        "category_summary": category_summary,
        "uncategorised_symbols": uncategorised,
        "symbol_count": lists.light.len(),
        "full_symbol_count": lists.full.len(),
        "asset_count": lists.assets.len(),
        "assets": lists.assets.iter().filter(|a| referenced.contains(&a.asset_id)).map(|a| json!({"asset_id": a.asset_id, "name": a.name, "display_name": a.display_name})).collect::<Vec<_>>(),
        "symbols": symbols,
    });
    println!("CTRADER_CATALOG_CAPTURE_BEGIN");
    println!("{}", serde_json::to_string_pretty(&doc).expect("serialize capture"));
    println!("CTRADER_CATALOG_CAPTURE_END");

    assert!(!lists.categories.is_empty(), "the demo broker published no symbol category");
    assert!(!lists.asset_classes.is_empty(), "the demo broker published no asset class");
    assert!(!lists.assets.is_empty(), "the demo broker published no asset");
}
