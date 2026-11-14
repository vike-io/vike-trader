//! R6 slice-6/7 LIVE Deribit smoke (network + testnet creds — run manually):
//!     cargo test -p vike-deribit --test deribit_smoke -- --ignored --nocapture
//!
//! The options ladder on test.deribit.com: pick the most liquid BTC option (book
//! summary: highest open interest with a live ask) → authed order-WS connect →
//! reconcile → user.trades feed attach → far-below LIMIT rest + cancel + idempotent
//! re-cancel → MARKET BUY min amount filled via the WS (post_only:false trap live) →
//! MARKET SELL reduce-only close → teardown.
//!
//! **The venue is read the way production reads it**: through `vike_deribit::reconcile`, the
//! factory `crates/bridges/deribit/src/adapter.rs` builds the live reconcile client with (its own
//! authed socket), whose rows come from `crates/bridges/deribit/src/reconcile/mod.rs`'s
//! `parse_positions` and `parse_open_orders`. Until 2026-11-08 it seeded the engine from the legacy
//! snapshot path (deleted in unit A2). A production engine starts FLAT and
//! books only what the stream folds, so this one does too: the engine's assertions are ABSOLUTE.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::rest::ExecutionClientRest;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_deribit::execution::rest::{VenueRestDeribit, parse_deribit_option_instruments};
use vike_deribit::execution::user_data::spawn_deribit_user_data_with_resync;
use vike_deribit::test_support::{TESTNET_REST, TESTNET_WEBSOCKET, testnet_urls};
use vike_deribit::transport::client::DeribitTransport;
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_model::AccountTier;
use vike_model::OrderStatus;
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;

use vike_model::now_ms;

fn wait_status(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>,
    order_client_id: &str,
    accept: &[OrderStatus],
    secs: u64,
) -> vike_execution::OrderView {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.order_client_id == order_client_id) {
            if accept.contains(&o.status) {
                return o.clone();
            }
            assert!(
                !matches!(o.status, OrderStatus::Rejected | OrderStatus::Denied),
                "{order_client_id} {:?}: {:?}",
                o.status,
                snap.recent_events
            );
        }
        assert!(
            Instant::now() < deadline,
            "{order_client_id} never reached {accept:?}; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "network + testnet creds — run manually (see module doc)"]
fn deribit_options_ladder() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "deribit") else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    // most liquid BTC option: highest open interest with a live ask (one REST call)
    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut best: Option<(String, f64, f64)> = None; // (name, oi, ask)
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    let (symbol, oi, ask) = best.expect("a live-ask BTC option on testnet");
    tracing::info!(target: "vike_deribit", "instrument: {symbol} (oi={oi}, ask={ask})");

    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let instrument = parse_deribit_option_instruments(&info)[&symbol].clone();
    tracing::info!(target: "vike_deribit", "properties: tick={} step={}", instrument.properties.tick_size, instrument.properties.step_size);

    // authed order transport (JSON-RPC over WS — order entry rides the socket)
    let mut transport = DeribitTransport::new(
        TESTNET_WEBSOCKET,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    transport.connect().expect("order-WS auth");
    let rest = VenueRestDeribit::new(transport, &symbol, instrument.properties, "BTC");
    let recon = vike_deribit::recon_client(&testnet_urls(), &creds, &symbol)
        .expect("the reconcile socket's auth");
    let positions = recon.fetch_position_status_reports().expect("reconcile positions");
    let open_orders = recon.fetch_order_status_reports(0).expect("reconcile open orders");
    tracing::info!(target: "vike_deribit", "reconcile: positions={positions:?} open_orders={}", open_orders.len());
    let base: f64 = positions.iter().map(|p| p.qty).sum();

    let engine = Engine::new(
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(rest),
        "deribit",
        &symbol,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    // The production spawn with no replay and a fresh fence (nothing is submitted around it, so it
    // holds nothing back).
    let events = handle.event_sender();
    let feed = spawn_deribit_user_data_with_resync(
        TESTNET_WEBSOCKET.to_string(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        symbol.clone(),
        events.clone(),
        std::sync::Arc::new(SubmitFence::new(events)),
        Vec::new,
    );
    std::thread::sleep(Duration::from_secs(3)); // auth+subscribe settles

    let qty = instrument.properties.min_qty; // 0.1 BTC-amount for BTC options
    let submit =
        |order_client_id: &str, side: i32, order_type: &str, price: Option<f64>, reduce: bool| {
            let req: vike_model::OrderRequest = serde_json::from_value(json!({
                "order_client_id": order_client_id, "venue": "deribit", "symbol": symbol,
                "side": side, "qty": qty, "order_type": order_type, "price": price,
                "reduce_only": reduce, "ts": now_ms()
            }))
            .unwrap();
            handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
        };

    // resting LIMIT far below the ask, then cancel + idempotent re-cancel.
    // NOTE: option ticks are TIERED (tick_size_steps: 0.0005 above 0.005) — rest below
    // the tier boundary so the base tick_size applies (the instruments parser carries
    // only the base tick, matching Python).
    let rest_order_client_id = format!("vtr6dbR{}", now_ms() % 100_000_000);
    let low_price = (ask * 0.25).min(0.004).max(instrument.properties.tick_size);
    submit(&rest_order_client_id, 1, "limit", Some(low_price), false);
    wait_status(&cell, &rest_order_client_id, &[OrderStatus::Accepted], 15);
    tracing::info!(target: "vike_deribit", "limit resting @ {low_price}");
    handle.try_command(Command::Order(OrderIntent::Cancel(rest_order_client_id.clone()))).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    handle.try_command(Command::Order(OrderIntent::Cancel(rest_order_client_id.clone()))).unwrap(); // idempotent
    std::thread::sleep(Duration::from_secs(1));
    assert!(cell.load().fault.is_none(), "re-cancel must be swallowed");

    // MARKET BUY -> fill arrives on user.trades (post_only:false live)
    let buy_order_client_id = format!("vtr6dbB{}", now_ms() % 100_000_000);
    submit(&buy_order_client_id, 1, "market", None, false);
    let buy = wait_status(&cell, &buy_order_client_id, &[OrderStatus::Filled], 25);
    tracing::info!(target: "vike_deribit", "BUY filled: {} @ {} (fee tracked)", buy.filled_qty, buy.average_fill_price);
    assert!(buy.average_fill_price > 0.0);
    let pos = cell.load().positions.first().map(|p| p.size).unwrap_or(0.0);
    assert!(pos > qty * 0.5, "fill folded into the flat-started engine: {pos} (venue base {base})");

    // MARKET SELL reduce-only close
    let sell_order_client_id = format!("vtr6dbS{}", now_ms() % 100_000_000);
    submit(&sell_order_client_id, -1, "market", None, true);
    wait_status(&cell, &sell_order_client_id, &[OrderStatus::Filled], 25);
    let fin = cell.load().positions.first().map(|p| p.size).unwrap_or(0.0);
    tracing::info!(
        target: "vike_deribit",
        "final: size={fin} realized={} fees={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid
    );
    assert!(fin.abs() < instrument.properties.step_size * 0.5, "round trip must flatten: {fin}");
    assert!(cell.load().fault.is_none());

    feed.shutdown().expect("deribit user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_deribit", "deribit ladder green: auth -> reconcile -> rest/cancel -> market fill via user.trades -> close -> teardown");
}
