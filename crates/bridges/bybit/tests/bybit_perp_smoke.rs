//! R6 slice-4 LIVE Bybit smoke (network + demo creds — run manually):
//!     cargo test -p vike-bybit --test integration -- --ignored --nocapture bybit_perp_smoke::
//!
//! The V5 linear ladder, live: set_leverage (110043-safe) → reconcile → private-WS
//! auth+subscribe handshake → MARKET BUY filled via the execution topic → SELL 2×
//! (FLIPS TO A REAL SHORT — live signed reconcile gate) → BUY back to flat → funding
//! poller probe → teardown.
//!
//! **The venue is read the way production reads it**: through `vike_bybit::reconcile`, the
//! factory `crates/bridges/bybit/src/adapter.rs` builds the live reconcile client with, whose rows
//! come from `crates/bridges/bybit/src/reconcile/mod.rs`'s `parse_positions` and
//! `parse_wallet_balance`. Until 2026-11-08 it seeded the engine from the legacy snapshot path
//! (deleted in unit A2). A production engine starts FLAT and books only what the stream
//! folds, so this one does too: the engine's assertions are ABSOLUTE.

use std::time::Duration;

use serde_json::json;
use vike_bridge_core::rest::ExecutionClientRest;
use vike_bridge_core::smoke::{wait_filled, wait_pump_ready};
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::funding::BybitFundingPoller;
use vike_bybit::execution::rest::{
    PATH_INSTRUMENTS, VenueRestBybit, parse_bybit_perp_instruments, seed_endpoints,
};
use vike_bybit::execution::user_data::spawn_bybit_perp_user_data;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_core::{CoreConfig, spawn_core};
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_model::RiskLimits;

use vike_model::now_ms;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;

/// The linear perpetual's USER name (decision 0290): what the engine, its orders, the pump, the reconcile
/// client and the
/// funding poll name, signed as its kept row's `BTCUSDT` in `linear` (`vike_bybit::history::kept_list_for_tests`).
const SYMBOL: &str = "BTCUSDT.P";

fn submit_market(handle: &vike_core::CoreHandle, order_client_id: &str, side: i32, qty: f64) {
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL,
        "side": side, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

/// FEES-AUDIT 2026-10-26 §4.12. The ledger's fees must be a CHARGE (a rebate or a zero is a wrong
/// sign or a lost fee) and of the right MAGNITUDE: within `[½ x maker, 1.5 x taker]` of the notional
/// traded, read off the registry row (`fee_schedule_for("bybit")`: 2/5.5 bps; measured 2.0/5.5 on the
/// node). Every leg here is a market order, so the true figure is the taker rate. The old
/// `fees_paid != 0.0` passed for a negative figure and for one 1,000x off.
fn assert_fees_match_schedule(fees_paid: f64, notional: f64) {
    let (maker, taker) = vike_venues::fee_schedule_for("bybit").maker_taker_rates();
    let (lo, hi) = (0.5 * maker * notional, 1.5 * taker * notional);
    assert!(
        fees_paid > 0.0 && (lo..=hi).contains(&fees_paid),
        "fees_paid {fees_paid} must be a charge within the schedule's [{lo}, {hi}] for {notional} of notional"
    );
}

fn position_size(cell: &std::sync::Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>) -> f64 {
    cell.load().positions.iter().find(|p| p.symbol == SYMBOL).map_or(0.0, |p| p.size)
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_ladder_long_short_flat() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    // The demo hosts, from the release seed's rows.
    let demo = seed_endpoints(AccountTier::Demo);
    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    // instruments-info is public but the signed transport handles it fine
    let info = transport
        .signed(
            &demo.rest,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let instrument = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    tracing::info!(
        target: "vike_bybit",
        "bybit properties: tick={} step={} min_qty={} min_notional={}",
        instrument.properties.tick_size, instrument.properties.step_size, instrument.properties.min_qty,
        instrument.properties.min_notional
    );

    let make_rest = || VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo.rest.clone(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    };
    let rest = make_rest();
    rest.set_leverage().expect("set_leverage (110043-safe)");
    let mark = rest.last_price().expect("tickers");
    // The venue side, through the production reconcile factory (the demo host).
    let recon = vike_bybit::recon_client(&creds, SYMBOL, &demo.rest)
        .expect("bybit recon_client is always Some");
    // A flat symbol's row is not a pinned contract on bybit (`bybit_reconcile_smoke.rs`): no row
    // reads as flat.
    let rows = recon.fetch_position_status_reports().expect("fetch_position_status_reports");
    let base: f64 = rows.iter().map(|p| p.qty).sum();
    let balance = recon.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_bybit", "reconcile: venue position={base} balance={balance:?} mark={mark}");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b > 0.0),
        "UNIFIED USDT wallet balance must be live: {balance:?}"
    );
    let probe = make_rest();

    let engine = Engine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(rest),
        "bybit",
        SYMBOL,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = spawn_bybit_perp_user_data(
        demo.ws.clone(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        SYMBOL.to_string(),
        handle.event_sender(),
        ready.clone(),
        false, // `venue.bybit.fast_exec`'s default: the two-topic subscription
    );
    wait_pump_ready(&ready); // condition-based settle: proceed only once the pump has subscribed

    // per-leg qty: satisfy min_qty AND min_notional at the live mark
    let qty =
        f64::max(instrument.properties.min_qty, (instrument.properties.min_notional * 1.4) / mark);
    let qty = (qty / instrument.properties.step_size).ceil() * instrument.properties.step_size;
    tracing::info!(target: "vike_bybit", "leg qty: {qty}");

    // LONG leg
    let b1 = format!("vtr6byB{}", now_ms() % 100_000_000);
    submit_market(&handle, &b1, 1, qty);
    let buy = wait_filled(|| cell.load_full(), &b1, Duration::from_secs(25));
    tracing::info!(target: "vike_bybit", "LONG filled: {} @ {} (mark {:?})", buy.filled_qty, buy.average_fill_price, mark);
    assert!(position_size(&cell) > qty * 0.5, "long folded into the flat-started engine");

    // FLIP to SHORT
    let s1 = format!("vtr6byS{}", now_ms() % 100_000_000);
    submit_market(&handle, &s1, -1, qty * 2.0);
    wait_filled(|| cell.load_full(), &s1, Duration::from_secs(25));
    let flipped = position_size(&cell);
    assert!(flipped < -qty * 0.5, "the engine must be net SHORT: now={flipped}");

    // THE live signed-SHORT reconcile gate, through the production report parser. ⚠ ASSUMES the
    // demo account's residual BTCUSDT position (`base`, read before trading) is under ONE leg: the
    // venue holds `base - qty` now, which is negative only then.
    let live = recon.fetch_position_status_reports().expect("live reconcile while short");
    tracing::info!(target: "vike_bybit", "LIVE SHORT reconcile: {live:?}");
    assert!(
        live.iter().any(|p| p.qty < 0.0),
        "venue reconcile must report the signed SHORT (venue base was {base}): {live:?}"
    );

    // back to flat
    let b2 = format!("vtr6byF{}", now_ms() % 100_000_000);
    submit_market(&handle, &b2, 1, qty);
    wait_filled(|| cell.load_full(), &b2, Duration::from_secs(25));
    let fin = position_size(&cell);
    tracing::info!(
        target: "vike_bybit",
        "final: size={fin} realized={} fees={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid
    );
    assert!(
        fin.abs() < instrument.properties.step_size * 1.5,
        "round trip must flatten: final={fin}"
    );
    // four legs of `qty` (long, then 2x short, then back): 4 x qty x mark of market-order notional
    assert_fees_match_schedule(cell.load().portfolio.fees_paid, 4.0 * qty * mark);
    assert!(cell.load().fault.is_none());

    // funding poller probe: the call path works (settlements may be empty in the window).
    // Floor 0 = no spawn floor, so the probe sees the venue's whole default window.
    let mut poller = BybitFundingPoller::new(&probe, SYMBOL, 0);
    let funding = poller.poll().expect("live funding poll");
    tracing::info!(target: "vike_bybit", "funding poller: {} settlement rows decoded", funding.len());

    feed.shutdown().expect("bybit user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_bybit", "bybit ladder green: leverage -> long -> SHORT (live signed reconcile) -> flat -> teardown");
}
