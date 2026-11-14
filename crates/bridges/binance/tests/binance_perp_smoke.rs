//! R6 slice-3 LIVE perp smoke (network + demo creds — run manually):
//!     cargo test -p vike-binance --test binance_perp_smoke -- --ignored --nocapture
//!
//! The fapi ladder, live: set_leverage → reconcile read → listenKey WS attach → MARKET BUY
//! (long) filled via ORDER_TRADE_UPDATE → MARKET SELL 2× (FLIPS TO A REAL SHORT — the
//! signed-negative reconcile gate, live) → BUY back to flat → teardown.
//!
//! **The engine is started the way production starts a perp.** The engine, the orders and the
//! pump's fallback symbol all carry the SERIES label `BTCUSDT.P` —
//! `crates/bridges/binance/src/family/mod.rs`'s `perp_series_symbol` of the exchange symbol, the
//! function the picker's catalog id is spelled by. That id is the symbol
//! `crates/vike-tradehub-core/src/engine.rs`'s `make_engine` starts the engine on and the one
//! `crates/bridges/binance/src/execution/mod.rs`'s `run_perp` hands the pump; only the REST clients keep the
//! bare exchange symbol, as `run_perp`'s own clients do. Until 2026-10-03 this smoke started on the
//! bare `BTCUSDT` — the one configuration in which the perp lane's then-bare fill label matched — so
//! it passed while every production perp fill was dropped from the position.
//!
//! **No position seed, deliberately.** It used to seed the engine with `Command::ApplySnapshot`
//! from the legacy snapshot reader, which labelled the bare symbol and which no shipped binary sent;
//! that command and that reader are deleted (unit A2). A production engine starts FLAT and books only what the stream folds, so this one does too: its assertions are
//! ABSOLUTE (it holds exactly what this smoke traded), and the venue is read separately, through the
//! production reconcile client — `vike_binance::reconcile`, the factory
//! `crates/bridges/binance/src/adapter.rs`'s `VenueAdapterBinance` builds on the same series label,
//! whose position rows come from `parse_perp_position_risk` and carry that label.

use std::time::Duration;

use vike_binance::execution::BinanceUrls;
use vike_binance::execution::rest::perp::{
    PATH_EXCHANGE_INFO, VenueRestBinancePerp, parse_binance_perp_instruments,
};
use vike_binance::execution::user_data::perp::spawn_binance_perp_user_data_with_resync;
use vike_binance::family::perp_series_symbol;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::rest::ExecutionClientRest;
use vike_bridge_core::smoke::wait_filled;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_model::AccountTier;
use vike_model::PositionStatusReport;
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;
use vike_venues::venue_urls::VenueUrls;

use vike_model::now_ms;

/// The instrument as the venue names it on the wire — what the fapi REST clients are built on.
const API_SYMBOL: &str = "BTCUSDT";

fn submit_market(
    handle: &vike_core::CoreHandle,
    symbol: &str,
    order_client_id: &str,
    side: i32,
    qty: f64,
    reduce: bool,
) {
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id, "venue": "binance", "symbol": symbol,
        "side": side, "qty": qty, "order_type": "market", "reduce_only": reduce,
        "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

/// The engine's position on the started `series` label.
fn position_size(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>,
    series: &str,
) -> f64 {
    cell.load().positions.iter().find(|p| p.symbol == series).map_or(0.0, |p| p.size)
}

/// The venue's `positionRisk` rows, read as the live reconcile pass reads them. Every row must name
/// the `series` label — the key `vike_execution::recon::diff` matches against the engine's book — and
/// none the bare exchange symbol, so this fails either way the label could go wrong.
fn venue_positions(
    recon: &dyn vike_execution::recon::ReconcileClient,
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
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    // The label the engine on, the orders name and the fills must carry.
    let series = perp_series_symbol(API_SYMBOL);
    // The demo hosts: the release's `venue_url` rows (`crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`).
    let hosts = BinanceUrls::resolve(&VenueUrls::seed("binance"), AccountTier::Demo)
        .expect("the seed holds every binance row");

    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::perp_rest_gate());
    let info = transport
        .public(hosts.fapi_rest, PATH_EXCHANGE_INFO, &[("symbol", API_SYMBOL.into())])
        .expect("fapi exchangeInfo");
    let instrument = parse_binance_perp_instruments(&info)[API_SYMBOL].clone();
    tracing::info!(
        target: "vike_binance",
        "fapi properties: tick={} step={} min_notional={}",
        instrument.properties.tick_size, instrument.properties.step_size, instrument.properties.min_notional
    );

    let rest = VenueRestBinancePerp {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: hosts.fapi_rest.to_string(),
        ty: hosts.ty,
        margining: vike_binance::execution::rest::coin_m::margining_of(API_SYMBOL),
        algo_order_client_ids: Default::default(),
        symbol: API_SYMBOL.to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    };
    rest.set_leverage().expect("set_leverage (idempotent 200)");

    // The venue side, through the production reconcile factory on the SERIES label (the demo
    // `hosts`), exactly as the start builds it.
    let recon = vike_binance::recon_client(&creds, &series, hosts)
        .expect("binance recon_client is always Some");
    let balance = recon.fetch_balance().expect("fetch_balance (perp)");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b > 0.0),
        "fapi USDT wallet balance must be live: {balance:?}"
    );
    let base: f64 = venue_positions(&*recon, &series).iter().map(|p| p.qty).sum();
    tracing::info!(target: "vike_binance", "reconcile: venue {series} position={base} balance={balance:?}");

    let engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(rest),
        "binance",
        &series,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    // No replay: the smoke reads the live stream alone, through a fence no submit uses.
    let fence = std::sync::Arc::new(vike_bridge_core::exec_actor::submit_fence::SubmitFence::new(
        handle.event_sender(),
    ));
    let feed = spawn_binance_perp_user_data_with_resync(
        hosts.fapi_rest.to_string(),
        hosts.fapi_websocket.to_string(),
        false, // the USDⓈ-M book, not COIN-M
        creds.api_key.expose().clone(),
        series.clone(),
        handle.event_sender(),
        fence,
        Vec::new,
        None,
        false, // `venue.binance.trade_lite_fill`'s default: TRADE_LITE frames dropped
        None,  // no broker link id: this smoke's orders go out bare
    );
    std::thread::sleep(Duration::from_secs(3)); // listenKey connect settles

    // min_notional (100 USDT) at ~62k with step 0.001 -> 0.002 BTC per leg
    let qty = 0.002;
    let step = instrument.properties.step_size;

    // LONG leg
    let b1 = format!("vtr6pB{}", now_ms() % 100_000_000);
    submit_market(&handle, &series, &b1, 1, qty, false);
    let buy = wait_filled(|| cell.load_full(), &b1, Duration::from_secs(25));
    tracing::info!(target: "vike_binance", "LONG filled: {} @ {}", buy.filled_qty, buy.average_fill_price);
    let long = position_size(&cell, &series);
    assert!(
        (long - buy.filled_qty).abs() < step * 0.5,
        "long folded into the engine mounted on {series}: position {long}, filled {}",
        buy.filled_qty
    );

    // FLIP to SHORT: sell 2x
    let s1 = format!("vtr6pS{}", now_ms() % 100_000_000);
    submit_market(&handle, &series, &s1, -1, qty * 2.0, false);
    let sell = wait_filled(|| cell.load_full(), &s1, Duration::from_secs(25));
    tracing::info!(target: "vike_binance", "FLIP filled: {} @ {}", sell.filled_qty, sell.average_fill_price);
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
    wait_filled(|| cell.load_full(), &b2, Duration::from_secs(25));
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
