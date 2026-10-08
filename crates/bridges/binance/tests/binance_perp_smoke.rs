//! R6 slice-3 LIVE perp smoke (network + demo creds — run manually):
//!     cargo test -p vike-binance --test binance_perp_smoke -- --ignored --nocapture
//!
//! The fapi ladder, live: set_leverage → reconcile read → listenKey WS attach → MARKET BUY
//! (long) filled via ORDER_TRADE_UPDATE → MARKET SELL 2× (FLIPS TO A REAL SHORT — the
//! signed-negative reconcile gate, live) → BUY back to flat → teardown.
//!
//! **The engine is mounted the way production mounts a perp.** The engine, the orders and the
//! pump's fallback symbol all carry the SERIES label `BTCUSDT.P` —
//! `crates/bridges/binance/src/family/mod.rs`'s `perp_series_symbol` of the exchange symbol, the
//! function the picker's catalog id is spelled by. That id is the symbol
//! `crates/vike-mount/src/engine.rs`'s `make_engine` mounts the engine on and the one
//! `crates/bridges/binance/src/exec.rs`'s `run_perp` hands the pump; only the REST clients keep the
//! bare exchange symbol, as `run_perp`'s own clients do. Until 2026-10-03 this smoke mounted on the
//! bare `BTCUSDT` — the one configuration in which the perp lane's then-bare fill label matched — so
//! it passed while every production perp fill was dropped from the position.
//!
//! **No position seed, deliberately.** It used to seed the engine with `Command::ApplySnapshot`
//! from `BinancePerpRest::reconcile_positions`, the legacy snapshot path, which still labels the bare
//! symbol and which no shipped binary sends (`crates/vike-core/src/runtime/handle.rs`'s
//! `spawn_periodic_reconcile` is its one sender, and nothing outside tests calls it). A production
//! engine starts FLAT and books only what the stream folds, so this one does too: its assertions are
//! ABSOLUTE (it holds exactly what this smoke traded), and the venue is read separately, through the
//! production reconcile client — `vike_binance::recon_client`, the factory
//! `crates/bridges/binance/src/mount.rs`'s `BinanceVenueMount` builds on the same series label,
//! whose position rows come from `parse_perp_position_risk` and carry that label.

use std::time::{Duration, Instant};

use vike_binance::family::perp_series_symbol;
use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, DEMO_FAPI_WS, PATH_EXCHANGE_INFO,
    parse_binance_perp_instruments,
};
use vike_binance::perp_user_data::spawn_binance_perp_user_data;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::LiveRestClient;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, OrderStatus, RiskGate, RiskLimits,
};
use vike_model::PositionStatusReport;

use vike_model::now_ms;

/// The instrument as the venue names it on the wire — what the fapi REST clients are built on.
const API_SYMBOL: &str = "BTCUSDT";

fn submit_market(
    handle: &vike_core::CoreHandle,
    symbol: &str,
    coid: &str,
    side: i32,
    qty: f64,
    reduce: bool,
) {
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": symbol,
        "side": side, "qty": qty, "order_type": "market", "reduce_only": reduce,
        "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

fn wait_filled(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    coid: &str,
) -> vike_core::OrderView {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.client_order_id == coid) {
            if o.status == OrderStatus::Filled && o.filled_qty > 0.0 {
                return o.clone();
            }
            assert!(
                !matches!(o.status, OrderStatus::Rejected | OrderStatus::Denied),
                "{coid} {:?}: {:?}",
                o.status,
                snap.recent_events
            );
        }
        assert!(
            Instant::now() < deadline,
            "{coid} never FILLED; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The engine's position on the mounted `series` label.
fn position_size(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    series: &str,
) -> f64 {
    cell.load().positions.iter().find(|p| p.symbol == series).map_or(0.0, |p| p.size)
}

/// The venue's `positionRisk` rows, read as the live reconcile pass reads them. Every row must name
/// the `series` label — the key `vike_exec::recon::diff` matches against the engine's book — and
/// none the bare exchange symbol, so this fails either way the label could go wrong.
fn venue_positions(
    recon: &dyn vike_exec::recon::ReconClient,
    series: &str,
) -> Vec<PositionStatusReport> {
    let rows = recon.fetch_position_status_reports().expect("fetch_position_status_reports (perp)");
    assert!(!rows.is_empty(), "perp positionRisk must echo {API_SYMBOL} even when flat");
    for r in &rows {
        assert_eq!(r.symbol, series, "a perp position report carries the series label: {rows:?}");
        assert_ne!(r.symbol, API_SYMBOL, "a perp position report must not carry the bare symbol");
    }
    rows
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn perp_ladder_long_short_flat() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    // The label the engine is mounted on, the orders name and the fills must carry.
    let series = perp_series_symbol(API_SYMBOL);

    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate());
    let info = transport
        .public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[("symbol", API_SYMBOL.into())])
        .expect("fapi exchangeInfo");
    let inst = parse_binance_perp_instruments(&info)[API_SYMBOL].clone();
    tracing::info!(
        target: "vike_binance",
        "fapi properties: tick={} step={} min_notional={}",
        inst.properties.tick_size, inst.properties.step_size, inst.properties.min_notional
    );

    let rest = BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: DEMO_FAPI_REST.to_string(),
        symbol: API_SYMBOL.to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    rest.set_leverage().expect("set_leverage (idempotent 200)");

    // The venue side, through the production reconcile factory on the SERIES label (demo hosts:
    // `mainnet = false`), exactly as the mount builds it.
    let recon = vike_binance::recon_client(&creds, &series, false)
        .expect("binance recon_client is always Some");
    let balance = recon.fetch_balance().expect("fetch_balance (perp)");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b > 0.0),
        "fapi USDT wallet balance must be live: {balance:?}"
    );
    let base: f64 = venue_positions(&*recon, &series).iter().map(|p| p.qty).sum();
    tracing::info!(target: "vike_binance", "reconcile: venue {series} position={base} balance={balance:?}");

    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "binance",
        &series,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    let feed = spawn_binance_perp_user_data(
        DEMO_FAPI_REST.to_string(),
        DEMO_FAPI_WS.to_string(),
        creds.api_key.clone(),
        series.clone(),
        handle.event_sender(),
        false, // `venue.binance.trade_lite_fill`'s default: TRADE_LITE frames dropped
    );
    std::thread::sleep(Duration::from_secs(3)); // listenKey connect settles

    // min_notional (100 USDT) at ~62k with step 0.001 -> 0.002 BTC per leg
    let qty = 0.002;
    let step = inst.properties.step_size;

    // LONG leg
    let b1 = format!("vtr6pB{}", now_ms() % 100_000_000);
    submit_market(&handle, &series, &b1, 1, qty, false);
    let buy = wait_filled(&cell, &b1);
    tracing::info!(target: "vike_binance", "LONG filled: {} @ {}", buy.filled_qty, buy.avg_fill_px);
    let long = position_size(&cell, &series);
    assert!(
        (long - buy.filled_qty).abs() < step * 0.5,
        "long folded into the engine mounted on {series}: position {long}, filled {}",
        buy.filled_qty
    );

    // FLIP to SHORT: sell 2x
    let s1 = format!("vtr6pS{}", now_ms() % 100_000_000);
    submit_market(&handle, &series, &s1, -1, qty * 2.0, false);
    let sell = wait_filled(&cell, &s1);
    tracing::info!(target: "vike_binance", "FLIP filled: {} @ {}", sell.filled_qty, sell.avg_fill_px);
    let flipped = position_size(&cell, &series);
    assert!(flipped < -qty * 0.5, "the engine's position must be net SHORT: now={flipped}");

    // THE live SHORT-reconcile gate: venue positionAmt is signed-negative.
    // ⚠ ASSUMES the demo account's residual BTCUSDT perp position (`base`, read before trading) is
    // under ONE leg (`qty`, 0.002 BTC): the venue holds `base - qty` now, which is negative only
    // then. So a failure printing `venue base was {base}` with `base >= qty` means the demo account
    // carried a long residual and this gate could not go short — NOT that the signed parse broke.
    // With `base` below one leg the failure is the real one.
    // (The engine-side assertions do not depend on `base`: the engine starts flat.)
    let live = venue_positions(&*recon, &series);
    tracing::info!(target: "vike_binance", "LIVE SHORT reconcile: {live:?}");
    assert!(
        live.iter().any(|p| p.qty < 0.0),
        "venue reconcile must report the signed SHORT (venue base was {base}): {live:?}"
    );

    // BUY back to flat
    let b2 = format!("vtr6pF{}", now_ms() % 100_000_000);
    submit_market(&handle, &series, &b2, 1, qty, false);
    wait_filled(&cell, &b2);
    let fin = position_size(&cell, &series);
    tracing::info!(
        target: "vike_binance",
        "final: size={fin} realized={} fees={} funding={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid,
        cell.load().portfolio.funding_paid
    );
    assert!(fin.abs() < step * 1.5, "round trip must flatten the engine: final={fin}");
    assert!(cell.load().portfolio.realized_pnl != 0.0, "the flip realizes PnL");
    assert!(cell.load().fault.is_none());

    feed.shutdown().expect("perp user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_binance", "perp ladder green: leverage -> long -> SHORT (live signed reconcile) -> flat -> teardown");
}
