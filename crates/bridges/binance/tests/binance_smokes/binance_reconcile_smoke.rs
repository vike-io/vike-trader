//! Reconciliation-activation Task 8 LIVE demo smoke: proves the real `ReconcileClientBinance`
//! (`vike_binance::reconcile`, the client `vike_binance::adapter::VenueAdapterBinance` wires up for
//! a live binance start, which reconciles by default since 2026-09-06) actually fetches AND
//! parses against the live Binance demo REST API — not just the literal fixture bodies
//! `tests/offline/reconcile_client_parse.rs` exercises offline (no network). Constructed exactly like
//! that factory: same signer (`BinanceHmacSigner`), same rate-gated `UreqTransport`, same demo base
//! URLs (the seed's demo rows, `crate::demo_credentials::demo_hosts`'s `spot_rest` / `fapi_rest`). (⚠ This named `vike-app`'s
//! `build_recon_client`, behind `VIKE_RECONCILE=1`, until 2026-09-28: the construction moved into
//! this crate's own factory, the GUI starts no venue, and a live start no longer needs the flag.)
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture binance_reconcile_smoke::
//!
//! Three scenarios, each its own `#[ignore]`d `#[test]`, double-gated exactly like every other
//! `*_smoke.rs` in this crate (see e.g. `binance_demo_smoke.rs`): network + the
//! default DEMO account's key pair (`vike_secrets::smoke_directory` + `api_credentials`), self-skip
//! (a `tracing::warn!`, then an early `return`) when creds are absent — factored out here as
//! [`load_demo_credentials`] since three tests in this one file share the identical gate.
//!
//!   - `binance_spot_reconcile_fetch_smoke` / `binance_perp_reconcile_fetch_smoke` — READ-ONLY,
//!     safe to run any time, never places an order: construct the real `ReconcileClientBinance` in
//!     both its `Spot` and `Perp` modes and call all four `ReconcileClient` methods
//!     (`fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
//!     `fetch_position_status_reports`), asserting each succeeds and parses into plausible,
//!     internally-consistent values (right venue/symbol on every row; spot's position report is
//!     always `Ok(vec![])`; perp's `positionRisk` always echoes the requested `BTCUSDT` even flat —
//!     see `reconcile/mod.rs`'s module doc for why both of those are load-bearing contracts, not
//!     incidental). The SYMBOL is the lane's own: a spot row names the bare `BTCUSDT`, while every
//!     perp row names the perp SERIES label `BTCUSDT.P` (the string a perp engine on and
//!     `vike_execution::recon::diff` matches against), although the request names the bare one.
//!   - `binance_reconcile_order_lifecycle_smoke` — ORDER-PLACING (kept separate, clearly marked
//!     `#[ignore]`d): mirrors `binance_demo_smoke.rs`'s far-from-market LIMIT ladder (a BUY at
//!     half the live mark, sized to clear `min_notional`, that can never fill), but the
//!     verification step is the `ReconcileClient` report fetch itself, proving the exact seam the
//!     live reconcile engine depends on: place -> the order becomes visible through
//!     `ReconcileClientBinance::fetch_order_status_reports` with `NEW` normalized to `ACCEPTED` ->
//!     cancel -> the next fetch no longer shows it. It also reads `fetch_balance` before, while
//!     the order rests and after the cancel: the cash the reconcile compares must not move with
//!     the quote the BUY locks (the venue moves it from `free` to `locked`).

use crate::demo_credentials::{demo_hosts, load_demo_credentials};
use std::assert_matches;
use vike_binance::ReconcileClientBinance;
use vike_binance::execution::rest::spot::{
    PATH_ACCOUNT, PATH_EXCHANGE_INFO, VenueRestBinanceSpot, parse_symbol_properties,
};
use vike_binance::family::perp_series_symbol;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::now_ms;

const SYMBOL: &str = "BTCUSDT";

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_spot_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };

    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let client = ReconcileClientBinance::spot(signer, transport, demo_hosts().spot_rest, SYMBOL);

    // Spot testnet funding isn't a documented API contract (unlike perp, below) — accept
    // `None`/`Some`, but a `Some` value must be a real, non-negative number, never garbage.
    let balance = client.fetch_balance().expect("fetch_balance (spot)");
    tracing::info!(target: "vike_binance", "spot USDT balance (free + locked): {balance:?}");
    match balance {
        Some(b) => assert!(b.is_finite() && b >= 0.0, "implausible spot balance: {b}"),
        None => {
            tracing::warn!(target: "vike_binance", "spot account has no USDT row (unfunded demo account?)")
        }
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports (spot)");
    tracing::info!(target: "vike_binance", "spot open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "binance");
        assert_eq!(o.symbol, SYMBOL);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports (spot)");
    tracing::info!(target: "vike_binance", "spot recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "binance");
        assert_eq!(f.symbol, SYMBOL);
    }

    // Spot has no venue-native position report: the mounted pair's holding is its base coin's wallet
    // figure, a `Cash` floor row with no price (the spot rule). Asserting it live pins the trait
    // dispatch and the venue's `exchangeInfo` row, not just the pure parser.
    let positions =
        client.fetch_position_status_reports().expect("fetch_position_status_reports (spot)");
    assert_eq!(positions.len(), 1, "one row, the mounted pair's holding: {positions:?}");
    assert_eq!(positions[0].margin_mode, vike_model::MarginMode::Cash, "{positions:?}");
    assert!(positions[0].qty >= 0.0 && positions[0].average_price == 0.0, "{positions:?}");

    tracing::info!(target: "vike_binance", "spot reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_perp_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };

    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::perp_rest_gate());
    let client = ReconcileClientBinance::perp(signer, transport, demo_hosts().fapi_rest, SYMBOL);

    // The futures demo wallet is documented/established to auto-fund USDT margin (the SAME
    // account/endpoint `binance_perp_smoke.rs`'s `perp_ladder_long_short_flat` already hard-
    // asserts `balance > 0.0` against) — safe to assert with confidence, unlike spot above.
    let balance = client.fetch_balance().expect("fetch_balance (perp)");
    tracing::info!(target: "vike_binance", "perp USDT wallet balance: {balance:?}");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b > 0.0),
        "perp USDT wallet balance must be live and funded on the futures demo: {balance:?}"
    );

    // The REQUESTS name the bare exchange `SYMBOL` (the client is built on it); every REPORT names
    // the perp by its SERIES label, the string a perp engine on and `recon::diff` keys
    // the local book by. Spelled by the production function, never re-spelled here — and the bare
    // form is asserted ABSENT as well, so this still fails if that function stopped suffixing.
    let series = perp_series_symbol(SYMBOL);

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports (perp)");
    tracing::info!(target: "vike_binance", "perp open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "binance");
        assert_eq!(o.symbol, series, "a perp order report carries the series label");
        assert_ne!(o.symbol, SYMBOL, "a perp order report must not carry the bare symbol");
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports (perp)");
    tracing::info!(target: "vike_binance", "perp recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "binance");
        assert_eq!(f.symbol, series, "a perp fill report carries the series label");
        assert_ne!(f.symbol, SYMBOL, "a perp fill report must not carry the bare symbol");
    }

    // `positionRisk` always echoes every requested symbol, including a flat leg — see
    // `reconcile/mod.rs`'s module doc for why that flat row is load-bearing for `diff::diff`.
    let positions =
        client.fetch_position_status_reports().expect("fetch_position_status_reports (perp)");
    assert!(!positions.is_empty(), "perp positionRisk must echo {SYMBOL} even when flat");
    for p in &positions {
        assert_eq!(p.symbol, series, "a perp position report carries the series label");
        assert_ne!(p.symbol, SYMBOL, "a perp position report must not carry the bare symbol");
    }
    tracing::info!(target: "vike_binance", "perp position: qty={} avg_px={}", positions[0].qty, positions[0].average_price);

    tracing::info!(target: "vike_binance", "perp reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}

/// ORDER-PLACING (separate from the read-only fetch smokes above): rests a far-from-market LIMIT
/// BUY that can never fill, confirms it becomes visible through the real
/// `ReconcileClientBinance::fetch_order_status_reports` (not the raw `openOrders` REST call
/// `binance_demo_smoke.rs` checks against — this smoke proves the RECONCILE seam specifically),
/// cancels it, and confirms the next fetch no longer shows it.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn binance_reconcile_order_lifecycle_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };

    // submit-side client: exact ladder `binance_demo_smoke.rs` uses to get a live mark + place.
    let demo_rest = demo_hosts().spot_rest;
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(demo_rest, PATH_EXCHANGE_INFO, &[("symbol", SYMBOL.into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)[SYMBOL];

    let submit_client = VenueRestBinanceSpot {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: demo_rest.to_string(),
        symbol: SYMBOL.to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = submit_client.server_time_offset(now_ms()).expect("server time");
    submit_client.signer.set_offset_ms(offset);

    let mark = vike_binance::family::execution::spot_fee::ticker_price(
        &submit_client.transport,
        demo_rest,
        SYMBOL,
    )
    .expect("ticker mark must be live");

    // far-from-market LIMIT BUY that must REST: half the mark, notional >= min (never fills).
    let limit_px = mark * 0.5;
    let qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;
    let order_client_id = format!("vtreconsmk{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id, "venue": "binance", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    // a SEPARATE `ReconcileClientBinance` for verification — its own fresh signer/transport
    // instance, the "fresh REST client per purpose" idiom `vike_binance::reconcile`'s doc
    // comment pins ("a FRESH HMAC `ureq` REST client dedicated to reconcile reads"; it named
    // `vike-app::build_recon_client` until 2026-09-28) — submit and reconcile are different
    // purposes, never share one instance.
    let recon_client = ReconcileClientBinance::spot(
        BinanceHmacSigner::new(&creds, now_ms),
        UreqTransport::new("binance")
            .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate()),
        demo_rest,
        SYMBOL,
    );
    // The cash the reconcile compares, read before the order exists (a `Result`: it is judged
    // only after the cancel, so no failed read can leave the order resting).
    let cash_before = recon_client.fetch_balance();

    let events = submit_client.submit_order(&request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    assert_matches!(
        events.last(),
        Some(Event::OrderAccepted(_)),
        "demo order must be ACCEPTED: {events:?}"
    );

    let orders = recon_client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let Some(resting) =
        orders.iter().find(|o| o.order_client_id.as_deref() == Some(order_client_id.as_str()))
    else {
        panic!("order {order_client_id} must appear in reconcile reports: {orders:?}");
    };
    assert_eq!(
        resting.status,
        vike_model::OrderStatus::Accepted,
        "resting NEW order normalizes to ACCEPTED"
    );
    assert_eq!(resting.side, 1, "BUY -> +1");
    assert_eq!(resting.average_price, 0.0, "unfilled -> zero avg_px, no divide-by-zero");
    tracing::info!(target: "vike_binance", "order {order_client_id} visible via ReconClient: status={}", resting.status.as_str());

    // While the BUY rests the venue moves its price x qty of USDT from `free` to `locked`. The
    // figure the reconcile compares with the engine's wallet (which its user-data stream sets to
    // free + locked) must not move with it. Read it (and the venue's own row, for the log)
    // BEFORE the cancel, judge it AFTER, so a failed read cannot leave the order resting.
    let cash_during = recon_client.fetch_balance();
    let body_during =
        submit_client.transport.signed(demo_rest, PATH_ACCOUNT, "GET", &[], &submit_client.signer);
    if let Ok(mut body) = body_during {
        // The venue's whole account body, for the offline fixture: the account id left out and
        // the all-zero asset rows dropped (the demo wallet lists hundreds of them).
        if let Some(object) = body.as_object_mut() {
            object.remove("uid");
            if let Some(rows) = object.get_mut("balances").and_then(|b| b.as_array_mut()) {
                rows.retain(|r| r["free"] != "0.00000000" || r["locked"] != "0.00000000");
            }
        }
        println!("ACCOUNT-BODY-WHILE-RESTING {body}");
    }

    submit_client.cancel_order(&order_client_id).expect("cancel");

    let cash_after = recon_client.fetch_balance();
    println!("CASH before={cash_before:?} during={cash_during:?} after={cash_after:?}");
    if let (Ok(Some(before)), Ok(Some(during)), Ok(Some(after))) =
        (&cash_before, &cash_during, &cash_after)
    {
        // Half the lock is far above any fee and far below the lock itself (about 8 USDT).
        let half_lock = limit_px * qty / 2.0;
        assert!(
            (before - during).abs() < half_lock && (after - during).abs() < half_lock,
            "the reconcile cash moved with a resting BUY's lock: before {before}, while resting \
             {during}, after the cancel {after} (the lock is about {})",
            limit_px * qty
        );
    } else {
        println!(
            "SKIP the cash comparison: a balance read failed or the account holds no USDT row"
        );
    }

    let orders = recon_client
        .fetch_order_status_reports(0)
        .expect("fetch_order_status_reports after cancel");
    assert!(
        orders.iter().all(|o| o.order_client_id.as_deref() != Some(order_client_id.as_str())),
        "order {order_client_id} must be gone from reconcile reports after cancel: {orders:?}"
    );
    tracing::info!(target: "vike_binance", "reconcile order-lifecycle smoke green: place -> visible via ReconClient (ACCEPTED) -> cash unmoved by the lock -> cancel -> gone");
}
