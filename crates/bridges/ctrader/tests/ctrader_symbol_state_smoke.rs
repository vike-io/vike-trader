//! LIVE demo READ of what the broker says about whether each symbol may be traded:
//! `ProtoOASymbol.tradingMode` and `ProtoOALightSymbol.enabled`, the two facts
//! `crates/bridges/ctrader/src/trading_state.rs` turns into a refusal in words.
//!
//!     cargo test -p vike-ctrader --test ctrader_symbol_state_smoke -- --ignored --nocapture
//!
//! READ-ONLY: places NO order, cancels nothing, changes nothing on the account. It runs the normal
//! connect handshake (`vike_ctrader::conn::fetch_symbol_lists`) on its OWN socket, drops it, and prints
//! symbol NAMES and counts between two marker lines: how many symbols the demo lists, how many carry each
//! `tradingMode` (and how many state none), how many are `enabled=false`, how many [`Restriction`]s the
//! bridge reads out of them, and up to three example names of each. No account id, no token and no
//! credential is in the output.
//!
//! Credentials as in `ctrader_catalog_smoke.rs`: the DEFAULT demo account's, through
//! `vike_secrets::smoke_directory` and the pure `CtraderConfig::from_credentials` gate with no rotation
//! home (an expired access token FAILS this smoke rather than rotating the stored pair). Absent creds ->
//! the test SKIPS.

use std::collections::BTreeMap;

use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::fetch_symbol_lists;
use vike_ctrader::proto::ProtoOaTradingMode;
use vike_ctrader::symbols::SymbolMap;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn ctrader_symbol_state_read_smoke() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = CtraderConfig::from_credentials(AccountTier::Demo, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };

    let lists = fetch_symbol_lists(config.to_conn_config())
        .expect("handshake + symbol lists against the real cTrader demo endpoint");
    assert!(!lists.light.is_empty(), "the demo account lists no symbol");
    let map = SymbolMap::from_lists(&lists);

    // The raw wire values, by name: `tradingMode` per full symbol (absent = the proto default).
    let mut by_mode: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &lists.full {
        let word = match s.trading_mode {
            None => "(not stated = ENABLED)".to_string(),
            Some(m) => ProtoOaTradingMode::try_from(m)
                .map_or_else(|_| format!("(unknown value {m})"), |e| e.as_str_name().to_string()),
        };
        by_mode
            .entry(word)
            .or_default()
            .push(map.name_of(s.symbol_id).unwrap_or("(unnamed)").to_string());
    }
    // `enabled` per light symbol.
    let mut by_enabled: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &lists.light {
        let word = match s.enabled {
            None => "enabled not stated",
            Some(true) => "enabled=true",
            Some(false) => "enabled=false",
        };
        by_enabled.entry(word.into()).or_default().push(s.symbol_name.clone().unwrap_or_default());
    }
    // What the bridge reads out of those, per restriction.
    let mut read: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in map.ids() {
        if let Some(r) = map.restriction(id) {
            let name = map.name_of(id).unwrap_or("").to_string();
            read.entry(r.wire_word().into()).or_default().push(name);
        }
    }

    println!("CTRADER_SYMBOL_STATE_BEGIN");
    println!("symbols listed: {}  (full entries: {})", lists.light.len(), lists.full.len());
    for (title, groups) in [
        ("tradingMode (full symbol)", &by_mode),
        ("enabled (light symbol)", &by_enabled),
        ("read by the bridge as a restriction", &read),
    ] {
        println!("{title}:");
        for (word, names) in groups {
            let mut names = names.clone();
            names.sort_unstable();
            println!("  {word}: {}  examples: {:?}", names.len(), &names[..names.len().min(3)]);
        }
    }
    let restricted: usize = read.values().map(Vec::len).sum();
    println!("restricted in all (an opening order is refused): {restricted}");
    // The door admits forex pairs only, so say how the restriction falls on each broker asset class:
    // `class: restricted / listed`.
    let mut by_class: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut forex_restricted: Vec<String> = Vec::new();
    for id in map.ids() {
        let class = map.category(id).and_then(|c| c.asset_class).unwrap_or("(no class)");
        let row = by_class.entry(class.to_string()).or_default();
        row.1 += 1;
        if let Some(r) = map.restriction(id) {
            row.0 += 1;
            if class.to_ascii_lowercase().contains("forex") {
                forex_restricted.push(format!(
                    "{} ({})",
                    map.name_of(id).unwrap_or(""),
                    r.wire_word()
                ));
            }
        }
    }
    println!("restricted / listed, by the broker's asset class:");
    for (class, (r, n)) in &by_class {
        println!("  {class}: {r} / {n}");
    }
    forex_restricted.sort_unstable();
    println!("restricted forex pairs: {forex_restricted:?}");
    println!("CTRADER_SYMBOL_STATE_END");

    // The bridge's count must equal an independent count off the raw wire values: a symbol is
    // restricted exactly when its tradingMode is one of the three non-ENABLED words or it is
    // `enabled=false`.
    let off_wire = lists
        .light
        .iter()
        .filter(|l| l.symbol_name.is_some())
        .filter(|l| {
            let mode =
                lists.full.iter().find(|f| f.symbol_id == l.symbol_id).and_then(|f| f.trading_mode);
            l.enabled == Some(false)
                || matches!(
                    mode.and_then(|m| ProtoOaTradingMode::try_from(m).ok()),
                    Some(
                        ProtoOaTradingMode::DisabledWithoutPendingsExecution
                            | ProtoOaTradingMode::DisabledWithPendingsExecution
                            | ProtoOaTradingMode::CloseOnlyMode
                    )
                )
        })
        .count();
    assert_eq!(restricted, off_wire, "the bridge's restricted count disagrees with the wire");
}
