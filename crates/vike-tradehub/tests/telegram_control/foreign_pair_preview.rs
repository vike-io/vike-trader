//! **The chat channel's dry-run sizes a foreign-quoted pair as the TCP one does** (decision 0183: a ceiling is
//! stated in the ACCOUNT's money). `ProdTelegramDeps::preview` composes its own copy of the gate chain, so the
//! rule that the notional ceiling never sizes a pair the symbol door refuses in the pair's own quote money
//! (yen for USDJPY) is a per-call-site claim the TCP path cannot witness: the door's own reason is what a chat
//! operator reads for a pair it will not admit, and a pair it admits is counted in the account's money.

use super::*;

/// A facts source that quotes EVERY symbol in yen at 1/150 dollar per yen, for an account whose account-wide
/// exposure ceiling is armed or not.
struct YenFacts {
    capped: bool,
}

impl vike_tradehub::server::symbol_admission::SymbolFacts for YenFacts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(
        &self,
        route_key: &str,
        _: &str,
        symbol: &str,
    ) -> Result<vike_exec::SymbolAdmission, String> {
        Ok(vike_exec::SymbolAdmission {
            route_key: route_key.into(),
            symbol: symbol.into(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.001),
                lot_size: Some(1.0),
                ..vike_model::SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
            quote_ccy: Some("JPY".into()),
            quote_rate: Some(1.0 / 150.0),
            rate_as_of_ms: Some(0),
            im_requirement: None,
            release: false,
            margin_mode: None,
        })
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(150.0))
    }
    fn exposure_cap_armed(&self, _: &str) -> bool {
        self.capped
    }
}

/// A published `oanda` demo engine mounted on `EURUSD` (a dollar account).
fn oanda_demo_cell() -> std::sync::Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>> {
    let mut snap = vike_exec::CoreSnapshot::empty("oanda", "EURUSD");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "oanda".into(),
        account: None,
        route_key: "oanda".into(),
        symbol: "EURUSD".into(),
        tier: Some(vike_model::AccountTier::Demo),
        ..Default::default()
    }];
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(snap))
}

fn eurjpy(coid: &str, qty: f64) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.to_string(),
        venue: "oanda".into(),
        symbol: "EURJPY".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(168.49),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
    })
}

/// A 100,000 notional ceiling: 1,000 EURJPY at 168.49 is 168,490 yen and 1,123 dollars.
fn chat_deps(sink: vike_core::CommandSink, capped: bool) -> ProdTelegramDeps {
    ProdTelegramDeps::new(
        &config(),
        ControlLimitsConfig { max_notional: Some(100_000.0), rate_per_sec: 1e9 },
        sink,
        oanda_demo_cell(),
        Vec::new(),
    )
    .with_symbol_facts(Some(Arc::new(YenFacts { capped })))
}

/// A pair the door refuses (no exposure ceiling armed) is previewed with the door's reason, never as
/// `order notional 168490.00 exceeds ...`; one it admits is counted in dollars (1,000 passes, 100,000 is
/// 112,326.67 over the ceiling).
///
/// ⚠ KILL PROOF: hand `preview_vet_foreign` the default value for an `Err` (the pre-fix `foreign.flatten()`)
/// and the refused half reads the yen figure.
#[test]
fn the_production_deps_preview_counts_a_foreign_pair_in_the_accounts_money() {
    test_init();
    let mount = build_paper_maker_core(&MakerConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let refused = chat_deps(mount.handle.command_sink(), false);
    let reason = refused.preview(&eurjpy("tg-yen-1", 1000.0)).expect("the door refuses the pair");
    assert!(reason.contains("quoted in JPY"), "{reason}");
    assert!(!reason.contains("order notional"), "{reason}");

    let admits = chat_deps(mount.handle.command_sink(), true);
    assert_eq!(admits.preview(&eurjpy("tg-yen-2", 1000.0)), None, "about 1,123 dollars");
    let reason = admits.preview(&eurjpy("tg-yen-3", 100_000.0)).expect("112,326.67 dollars");
    assert!(reason.contains("order notional 112326.67 exceeds"), "{reason}");
    mount.handle.shutdown_and_join();
}
