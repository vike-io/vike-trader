//! R6 slice-5 LIVE OKX smoke (network + demo creds — run manually):
//!     cargo test -p vike-okx --test integration -- --ignored --nocapture okx_perp_smoke::
//!
//! The V5 SWAP ladder, live on the simulated-trading demo: set_leverage → reconcile →
//! private-WS login+subscribe (Cloudflare-fronted endpoint) → MARKET BUY in
//! CONTRACTS filled via the orders channel → SELL 2× (FLIPS TO A REAL SHORT — live
//! signed reconcile in BASE units) → BUY back to flat → bills funding probe → teardown.
//!
//! **The venue is read the way production reads it**: through `vike_okx::reconcile`, the factory
//! `crates/bridges/okx/src/adapter.rs` builds the live reconcile client with, whose rows come from
//! `crates/bridges/okx/src/reconcile/mod.rs`'s `parse_positions` and `parse_balance`. Until 2026-11-08
//! both tests seeded the engine from the legacy snapshot path (deleted in unit A2). A
//! production engine starts FLAT and books only what the stream folds, so these do too: the
//! engine's assertions are ABSOLUTE.
//!
//! The engine, its orders, the stream's fills and the reconcile reports speak the USER name [`SYMBOL`] (decision 0290,
//! over the held kept list); the REST client, the private pump's fallback, the reconcile client and the funding poll
//! take the instId [`INST_ID`], as the exec spawn hands them theirs.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::ExecutionClientRest;
use vike_bridge_core::smoke::{wait_filled, wait_pump_ready};
use vike_bridge_core::transport::json::RestTransportJson;
use vike_core::{CoreConfig, spawn_core};
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_model::AccountTier;
use vike_model::OrderStatus;
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_okx::execution::funding::OkxFundingPoller;
use vike_okx::execution::rest::{PATH_INSTRUMENTS, VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::execution::user_data::spawn_okx_perp_user_data;
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_secrets::credentials::api_credentials;

/// The venue's instId: what the REST client, the pump, the reconcile client and the funding poll sign.
const INST_ID: &str = "BTC-USDT-SWAP";
/// The USER name of [`INST_ID`] (decision 0290): the engine's, its orders' and every event's.
const SYMBOL: &str = "BTCUSDT.P";

use vike_model::now_ms;

fn submit_market(
    handle: &vike_core::CoreHandle,
    order_client_id: &str,
    side: i32,
    base_quantity: f64,
) {
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL,
        "side": side, "qty": base_quantity, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

fn submit_limit(
    handle: &vike_core::CoreHandle,
    order_client_id: &str,
    side: i32,
    base_quantity: f64,
    price: f64,
) {
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL,
        "side": side, "qty": base_quantity, "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

/// Poll the snapshot until `order_client_id` reaches `want` (or a terminal reject), returning that view.
fn wait_status(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>,
    order_client_id: &str,
    want: OrderStatus,
) -> vike_execution::OrderView {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.order_client_id == order_client_id) {
            if o.status == want {
                return vike_execution::OrderView::clone(o);
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
            "{order_client_id} never reached {want:?}; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn position_size(cell: &std::sync::Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>) -> f64 {
    cell.load().positions.iter().find(|p| p.symbol == SYMBOL).map_or(0.0, |p| p.size)
}

/// FEES-AUDIT 2026-10-26 §4.12. The ledger's fees must be a CHARGE (OKX states a charge as a NEGATIVE
/// `fillFee`, which the mapper negates: a sign slip would book a rebate) and of the right MAGNITUDE:
/// within `[½ x maker, 1.5 x taker]` of the notional traded, read off the registry row
/// (`fee_schedule_for("okx")`: 2/5 bps; measured +2.0/+5.0 on the node). Every leg is a market order,
/// so the true figure is the taker rate. The old `fees_paid != 0.0` passed for either failure.
fn assert_fees_match_schedule(fees_paid: f64, notional: f64) {
    let (maker, taker) = vike_venues::fee_schedule_for("okx").maker_taker_rates();
    let (lo, hi) = (0.5 * maker * notional, 1.5 * taker * notional);
    assert!(
        fees_paid > 0.0 && (lo..=hi).contains(&fees_paid),
        "fees_paid {fees_paid} must be a charge within the schedule's [{lo}, {hi}] for {notional} of notional"
    );
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_ladder_long_short_flat() {
    vike_log::test_init();
    vike_okx::instrument::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "okx") else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(
            &demo_net().rest,
            PATH_INSTRUMENTS,
            &[("instType", "SWAP".into()), ("instId", INST_ID.into())],
        )
        .expect("public instruments");
    let instrument = parse_okx_perp_instruments(
        &vike_okx::transport::client::unwrap_okx(info)
            .map(|d| json!({"data": d}))
            .expect("instruments data"),
    )[INST_ID]
        .clone();
    tracing::info!(
        target: "vike_okx",
        "okx properties: tick={} lotSz={} minSz={} ctVal={}",
        instrument.properties.tick_size, instrument.properties.step_size, instrument.properties.min_qty, instrument.ct_val
    );

    let make_rest = || VenueRestOkx {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: rest_transport(true)
            .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        base_url: demo_net().rest,
        symbol: INST_ID.to_string(),
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let rest = make_rest();
    rest.set_leverage().expect("set_leverage");
    let mark = rest.last_price().expect("ticker");
    // The venue side, through the production reconcile factory (the demo type's addresses).
    let recon = vike_okx::recon_client(&creds, INST_ID, instrument.ct_val, &demo_net())
        .expect("okx recon_client is always Some");
    let rows = recon.fetch_position_status_reports().expect("fetch_position_status_reports");
    let base: f64 = rows.iter().filter(|p| p.symbol == SYMBOL).map(|p| p.qty).sum();
    let balance = recon.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_okx", "reconcile: venue position={base} balance={balance:?} mark={mark}");
    let probe = make_rest();

    let engine = Engine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(rest),
        "okx",
        SYMBOL,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = spawn_okx_perp_user_data(
        demo_net().private_websocket,
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        creds.passphrase.as_ref().map(|p| p.expose().clone()).unwrap_or_default(),
        INST_ID.to_string(),
        instrument.ct_val,
        handle.event_sender(),
        ready.clone(),
    );
    wait_pump_ready(&ready); // condition-based settle: proceed only once the pump has subscribed

    // per-leg BASE qty = minSz contracts (BTC-USDT-SWAP: 0.01 ct × 0.01 BTC = 0.0001 BTC)
    let leg_base = instrument.properties.min_qty * instrument.ct_val;
    tracing::info!(
        target: "vike_okx",
        "leg: {} contracts = {leg_base} BTC (~${:.0})", instrument.properties.min_qty,
        leg_base * mark
    );

    // LONG leg
    let b1 = format!("vtr6okB{}", now_ms() % 100_000_000);
    submit_market(&handle, &b1, 1, leg_base);
    let buy = wait_filled(|| cell.load_full(), &b1, Duration::from_secs(25));
    tracing::info!(target: "vike_okx", "LONG filled: {} base @ {}", buy.filled_qty, buy.average_fill_price);
    assert!(position_size(&cell) > leg_base * 0.5, "long folded into the flat-started engine");

    // FLIP to SHORT
    let s1 = format!("vtr6okS{}", now_ms() % 100_000_000);
    submit_market(&handle, &s1, -1, leg_base * 2.0);
    wait_filled(|| cell.load_full(), &s1, Duration::from_secs(25));
    let flipped = position_size(&cell);
    assert!(flipped < -leg_base * 0.5, "the engine must be net SHORT: now={flipped}");

    // THE live signed-SHORT reconcile gate (venue pos in contracts -> base via ctVal), through the
    // production report parser. ⚠ ASSUMES the demo account's residual position (`base`, read before
    // trading) is under ONE leg: the venue holds `base - leg_base` now, which is negative only then.
    let live = recon.fetch_position_status_reports().expect("live reconcile while short");
    tracing::info!(target: "vike_okx", "LIVE SHORT reconcile (base units): {live:?}");
    assert!(
        live.iter().any(|p| p.qty < 0.0),
        "venue reconcile must report the signed SHORT (venue base was {base}): {live:?}"
    );

    // back to flat
    let b2 = format!("vtr6okF{}", now_ms() % 100_000_000);
    submit_market(&handle, &b2, 1, leg_base);
    wait_filled(|| cell.load_full(), &b2, Duration::from_secs(25));
    let fin = position_size(&cell);
    tracing::info!(
        target: "vike_okx",
        "final: size={fin} realized={} fees={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid
    );
    assert!(fin.abs() < leg_base * 0.5, "round trip must flatten: final={fin}");
    // four legs of `leg_base` (long, then 2x short, then back): 4 x leg_base x mark of market notional
    assert_fees_match_schedule(cell.load().portfolio.fees_paid, 4.0 * leg_base * mark);
    assert!(cell.load().fault.is_none());

    // bills funding probe: the signed call path works.
    // Floor 0 = no spawn floor, so the probe sees the venue's whole default window.
    let mut poller = OkxFundingPoller::new(&probe, INST_ID, 0);
    let funding = poller.poll().expect("live funding poll");
    tracing::info!(target: "vike_okx", "funding bills poller: {} rows decoded", funding.len());

    // A45: with an instrument admitted the poller drops `instId` and reads the account's SWAP bills in ONE
    // request. The venue must accept that shape (instType + type, no instId) and the poll must still work.
    let admitted =
        std::sync::Arc::<vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols>::default();
    // The admitted list holds instIds (the reconcile client's, the funding poll's and the replays' shared list).
    assert!(admitted.admit("ETH-USDT-SWAP", INST_ID), "an instrument other than the mounted one");
    let mut wide = OkxFundingPoller::new(&probe, INST_ID, 0).with_admitted(admitted);
    let first = wide.poll().expect("live account-wide funding poll");
    let again = wide.poll().expect("live account-wide funding poll, second read");
    println!(
        "MEASURED the account-wide bills read (no instId): the first poll folded {} rows (the \
         admitted instrument's backlog is marked seen, the mounted one's folds), the second {}",
        first.len(),
        again.len()
    );
    assert!(again.is_empty(), "nothing re-emits on the second read of the same window");

    feed.shutdown().expect("okx user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_okx", "okx ladder green: leverage -> long -> SHORT (live signed reconcile) -> flat -> teardown");
}

/// LIVE native-modify smoke (demo): rest a far-from-market limit so it stays ACCEPTED, modify its
/// qty+price via the native /api/v5/trade/amend-order endpoint, verify the OrderModified folds and
/// the resting terms change, then cancel. Never fills (price is ~50% below market).
#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_native_modify_reprices_resting_limit() {
    vike_log::test_init();
    vike_okx::instrument::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "okx") else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate());
    let info = transport
        .public(
            &demo_net().rest,
            PATH_INSTRUMENTS,
            &[("instType", "SWAP".into()), ("instId", INST_ID.into())],
        )
        .expect("public instruments");
    let instrument = parse_okx_perp_instruments(
        &vike_okx::transport::client::unwrap_okx(info)
            .map(|d| json!({ "data": d }))
            .expect("instruments data"),
    )[INST_ID]
        .clone();

    let rest = VenueRestOkx {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: rest_transport(true)
            .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        base_url: demo_net().rest,
        symbol: INST_ID.to_string(),
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    rest.set_leverage().expect("set_leverage");
    let mark = rest.last_price().expect("ticker");

    // No position seed: a production engine starts FLAT (see the module doc).
    let engine = Engine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(rest),
        "okx",
        SYMBOL,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = spawn_okx_perp_user_data(
        demo_net().private_websocket,
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        creds.passphrase.as_ref().map(|p| p.expose().clone()).unwrap_or_default(),
        INST_ID.to_string(),
        instrument.ct_val,
        handle.event_sender(),
        ready.clone(),
    );
    wait_pump_ready(&ready);

    let leg_base = instrument.properties.min_qty * instrument.ct_val; // one contract in BASE units
    let px0 = mark * 0.5; // far below market → the BUY rests, never fills
    let order_client_id = format!("vtAmdOkx{}", now_ms() % 100_000_000);
    submit_limit(&handle, &order_client_id, 1, leg_base, px0);
    let resting = wait_status(&cell, &order_client_id, OrderStatus::Accepted);
    tracing::info!(target: "vike_okx", "resting limit: qty={} px={:?}", resting.qty, resting.price);

    // native modify: double the size, nudge the price up (still well below market)
    let new_qty = leg_base * 2.0;
    let new_price_value = mark * 0.6;
    handle
        .try_command(Command::Order(OrderIntent::Modify {
            order_client_id: order_client_id.clone(),
            new_qty: Some(new_qty),
            new_price: Some(new_price_value),
            new_trigger_price: None,
        }))
        .unwrap();

    // wait for the OrderModified (native /amend-order ack) to fold: resting qty grows
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snap = cell.load_full();
        let o = snap
            .orders
            .iter()
            .find(|o| o.order_client_id == order_client_id)
            .expect("order present");
        if o.qty > resting.qty * 1.5 {
            tracing::info!(target: "vike_okx", "MODIFIED: qty {} -> {} px {:?} -> {:?}", resting.qty, o.qty, resting.price, o.price);
            assert!(o.price.unwrap_or(0.0) > px0, "price moved up on modify");
            break;
        }
        assert!(Instant::now() < deadline, "modify never folded; recent: {:?}", snap.recent_events);
        std::thread::sleep(Duration::from_millis(250));
    }

    handle.try_command(Command::Order(OrderIntent::Cancel(order_client_id))).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert!(cell.load().fault.is_none());
    feed.shutdown().expect("okx user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_okx", "okx native-modify green: rest -> modify (qty+px) -> cancel -> teardown");
}
