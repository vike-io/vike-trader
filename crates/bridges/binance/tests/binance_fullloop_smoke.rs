//! R6 slice-2 LIVE full-loop smoke (network + demo creds — run manually):
//!     cargo test -p vike-binance --test integration -- --ignored --nocapture binance_fullloop_smoke::
//!
//! THE R6 gate, live: Command::Submit → th-core RiskGate → LiveBinanceSpotClient REST
//! (accepted) → the venue's executionReport arrives on the WS-API user-data stream →
//! mapper → ingest → dual-dedup fold into the Account + FSM → CoreSnapshot. A MARKET
//! BUY (~$7 notional) then a MARKET SELL flatten, both confirmed FILLED by the venue's
//! own fill stream — no simulated anything.

use std::time::Duration;

use vike_binance::execution::BinanceUrls;
use vike_binance::execution::rest::spot::{
    LiveBinanceSpotClient, PATH_EXCHANGE_INFO, VenueRestBinanceSpot, parse_symbol_properties,
};
use vike_binance::execution::user_data::spot::spawn_binance_user_data_with_resync;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::smoke::wait_filled;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_model::AccountTier;
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;
use vike_venues::venue_urls::VenueUrls;

use vike_model::now_ms;

fn submit_market(handle: &vike_core::CoreHandle, order_client_id: &str, side: i32, qty: f64) {
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id, "venue": "binance", "symbol": "BTCUSDT",
        "side": side, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

/// FEES-AUDIT 2026-10-26 §4.12. The ledger's fees must be a CHARGE (a rebate or a zero is a wrong
/// sign or a lost fee) and of the right MAGNITUDE: within `[½ x maker, 1.5 x taker]` of the notional
/// traded, read off the registry row (`fee_schedule_for("binance")`, the spot lane: 10/10 bps). The
/// old `fees_paid != 0.0 || filled_qty > 0.0` passed for both a negative figure and one 1,000x off,
/// and for a BUY whose BTC fee was booked as that many USDT.
fn assert_fees_match_schedule(fees_paid: f64, notional: f64) {
    let (maker, taker) = vike_venues::fee_schedule_for("binance").maker_taker_rates();
    let (lo, hi) = (0.5 * maker * notional, 1.5 * taker * notional);
    assert!(
        fees_paid > 0.0 && (lo..=hi).contains(&fees_paid),
        "fees_paid {fees_paid} must be a charge within the schedule's [{lo}, {hi}] for {notional} of notional"
    );
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn full_loop_market_round_trip() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    // The demo hosts: the release's `venue_url` rows (`crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`).
    let hosts = BinanceUrls::resolve(&VenueUrls::seed("binance"), AccountTier::Demo)
        .expect("the seed holds every binance row");

    // --- REST client (properties + clock offset) wrapped as the core's ExecutionClient ---
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(hosts.spot_rest, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    let rest = VenueRestBinanceSpot {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: hosts.spot_rest.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = rest.server_time_offset(now_ms()).expect("server time");
    rest.signer.set_offset_ms(offset);
    // No position seed, as a production engine has none (`binance_perp_smoke.rs` argues it): the
    // engine starts FLAT and books what the stream folds. The venue is read through the production
    // reconcile client, which reports the spot lane's USDT balance and no position.
    let recon = vike_binance::recon_client(&creds, "BTCUSDT", hosts)
        .expect("binance recon_client is always Some");
    let balance = recon.fetch_balance().expect("fetch_balance (spot)");
    let mark = vike_binance::family::execution::spot_fee::ticker_price(
        &rest.transport,
        hosts.spot_rest,
        "BTCUSDT",
    )
    .expect("ticker mark must be live");
    tracing::info!(target: "vike_binance", "reconcile: mark={mark} USDT balance={balance:?}");

    // --- the th-core owns the engine; the WS user-data stream feeds its ingest ---
    let engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveBinanceSpotClient::new(rest),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    // No replay: the smoke reads the live stream alone, through a fence no submit uses.
    let fence = std::sync::Arc::new(vike_bridge_core::exec_actor::submit_fence::SubmitFence::new(
        handle.event_sender(),
    ));
    let feed = spawn_binance_user_data_with_resync(
        hosts.spot_websocket.to_string(),
        creds.api_key.expose().clone(),
        creds.api_secret.expose().clone(),
        "BTCUSDT".to_string(),
        handle.event_sender(),
        fence,
        Vec::new,
        None,
        vike_bridge_core::signer::VenueClock::default(),
        None, // no broker link id: this smoke's orders go out bare
    );
    std::thread::sleep(Duration::from_secs(3)); // let the subscribe handshake settle

    // --- MARKET BUY (~$7) -> venue fill via WS -> fold ---
    let qty = (properties.min_notional.max(5.0) * 1.4) / mark;
    let buy_order_client_id = format!("vtr6loopB{}", now_ms() % 100_000_000);
    let base_size = cell.load().positions.first().map_or(0.0, |p| p.size);
    submit_market(&handle, &buy_order_client_id, 1, qty);
    let buy = wait_filled(|| cell.load_full(), &buy_order_client_id, Duration::from_secs(20));
    tracing::info!(
        target: "vike_binance",
        "BUY filled: qty={} avg={} (venue id {:?})",
        buy.filled_qty, buy.average_fill_price, buy.venue_order_id
    );
    assert!(buy.average_fill_price > 0.0);
    let snap = cell.load_full();
    let pos = snap.positions.iter().find(|p| p.symbol == "BTCUSDT").expect("position");
    assert!(pos.size > base_size, "fill must fold into the Account: {} -> {}", base_size, pos.size);
    // the BUY pays its fee in the base coin; the ledger must hold it as quote units (10 bps), not as
    // that many USDT (`family/execution/spot_fee.rs`)
    let buy_notional = buy.filled_qty * buy.average_fill_price;
    assert_fees_match_schedule(snap.portfolio.fees_paid, buy_notional);

    // --- MARKET SELL the same qty -> flatten back ---
    let sell_order_client_id = format!("vtr6loopS{}", now_ms() % 100_000_000);
    submit_market(&handle, &sell_order_client_id, -1, buy.filled_qty);
    let sell = wait_filled(|| cell.load_full(), &sell_order_client_id, Duration::from_secs(20));
    tracing::info!(target: "vike_binance", "SELL filled: qty={} avg={}", sell.filled_qty, sell.average_fill_price);
    let snap = cell.load_full();
    let pos = snap.positions.iter().find(|p| p.symbol == "BTCUSDT").expect("position");
    tracing::info!(
        target: "vike_binance",
        "final: size={} realized={} fees={} equity={} recent={:?}",
        pos.size, snap.portfolio.realized_pnl, snap.portfolio.fees_paid, snap.portfolio.equity,
        &snap.recent_events[snap.recent_events.len().saturating_sub(6)..]
    );
    assert!(
        (pos.size - base_size).abs() < properties.step_size * 2.0,
        "round trip must flatten: base={base_size} final={}",
        pos.size
    );
    assert!(snap.portfolio.realized_pnl != 0.0, "the sell leg realizes PnL");
    assert_fees_match_schedule(
        snap.portfolio.fees_paid,
        buy_notional + sell.filled_qty * sell.average_fill_price,
    );
    assert!(snap.fault.is_none());

    // --- deterministic teardown: WS joined, core drained ---
    feed.shutdown().expect("user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_binance", "full loop green: submit -> accept -> WS fill -> fold -> flatten -> teardown");
}
