//! LIVE Polymarket **mounted-engine** smoke — the proof that a strategy can place an order on
//! Polymarket from vike, through the ordinary `ExecutionClient` seam, with fills/cancels returning
//! on the authenticated user WS the start starts for itself.
//!
//! This is the end-to-end twin of `polymarket_user_smoke.rs`. That one drives the venue by hand
//! (build → sign → REST → attach a pump) to prove each primitive; this one goes through
//! [`vike_polymarket::live_runtime_for_account`] — the call
//! `crates/bridges/polymarket/src/adapter.rs`'s `VenueAdapterPolymarket` makes, over the
//! default `live` polymarket account's fields — and asserts the CONTRACT a started venue owes:
//!
//! 1. `submit` emits `OrderSubmitted` synchronously, then the venue side emits `OrderAccepted`
//!    carrying the CLOB order id (the emitter split);
//! 2. a cancel issued **out of band** (direct REST, NOT through the client) still comes back as an
//!    `OrderCanceled` re-keyed to OUR order client id — which can only happen if the start really did start
//!    the user-WS pump inside its exec thread, and if the pump's **empty `markets` subscribe is
//!    account-wide** (the one server-side convention `crate::exec_plane::adapter::polymarket_execution_markets` documents
//!    but cannot prove offline);
//! 3. no order silently vanishes — the test cancels through the client on every failure path.
//!
//! ⚠ REAL MONEY. Polymarket has NO testnet. The order is a BUY at 0.01 on a token whose midpoint is
//! ≥ 0.15, so it CANNOT fill; the notional is ~$1.20 and it is cancelled within seconds either way.
//! `#[ignore]`d and self-skipping without the account's `PRIVATE_KEY`, like every other venue's
//! live smoke.
//!
//! Run it from a permitted region (order placement is geo-blocked) — either natively on the Dublin
//! host, or locally over the SOCKS tunnel with BOTH lanes proxied. It asserts the Irish egress
//! itself, once its credentials are found and before anything trades (decision 0095: the region is
//! the smoke's constant, `vike_polymarket::DUBLIN_EGRESS_COUNTRY`, not a variable):
//!
//! ```sh
//! cargo test -p vike-polymarket --features polymarket --test polymarket_smokes \
//!   -- --ignored --nocapture polymarket_execution_mount_smoke::
//! ```
//! The tunnel is `venue.polymarket.*` in the settings database (see `tests/polymarket_smokes/egress_support.rs`).

use std::time::{Duration, Instant};

use vike_execution::{ExecutionClient, event_channel};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};
use vike_polymarket::{get_json, live_runtime_for_account};

use crate::egress_support;

/// The CLOB base of the release's seed (the smoke's own reads and its out-of-band cancel).
fn clob() -> String {
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol};
    egress_support::seed_row(UrlChannel::Trade, UrlProtocol::Rest, "")
}

use vike_polymarket::test_support::wait_event;

/// How long [`wait_event`] sleeps on the start's empty ingest lane.
const POLL: Duration = Duration::from_millis(150);

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// A liquid token with a midpoint safely away from our resting price → `(token_id, condition_id)`.
fn discover_token() -> Option<(String, String)> {
    let markets = get_json(&clob(), "/sampling-markets", "").ok()?;
    let empty = Vec::new();
    for m in markets.get("data").and_then(|d| d.as_array()).unwrap_or(&empty) {
        let condition_id =
            m.get("condition_id").and_then(|c| c.as_str()).unwrap_or_default().to_string();
        if condition_id.is_empty() {
            continue;
        }
        for tk in m.get("tokens").and_then(|t| t.as_array()).unwrap_or(&empty) {
            let Some(tid) = tk.get("token_id").and_then(|x| x.as_str()).filter(|s| !s.is_empty())
            else {
                continue;
            };
            let mid = get_json(&clob(), "/midpoint", &format!("token_id={tid}"))
                .ok()
                .and_then(|v| v.get("mid").and_then(|m| m.as_str())?.parse::<f64>().ok());
            if mid.is_some_and(|mid| (0.15..0.85).contains(&mid)) {
                return Some((tid.to_string(), condition_id));
            }
        }
    }
    None
}

#[test]
#[ignore = "LIVE real-money mainnet + proxy — run manually (see module doc)"]
fn started_engine_place_and_websocket_cancel_roundtrip() {
    vike_log::test_init();
    let settings = egress_support::declare_from_the_settings_database();

    // The default polymarket account at tier `live` (the only tier this venue has) — its FIELDS,
    // never a composed name — and the deployment's named values the start also reads.
    let dir = vike_secrets::smoke_directory();
    let account = dir.credentials_for("polymarket", &AccountLabel::Default, AccountTier::Live);
    let vars = vike_secrets::smoke_vars();
    if account.get("PRIVATE_KEY").is_none() {
        eprintln!("SKIP: no live polymarket PRIVATE_KEY (absent-credentials-is-the-live-gate)");
        return;
    }
    // §0.2 egress guard: the region is asserted before anything trades — a misrouted run fails
    // HERE, naming the observed IP, instead of looking like an ordinary network flake.
    let egress = vike_polymarket::check_expected_egress(
        Some(vike_polymarket::DUBLIN_EGRESS_COUNTRY),
        vike_polymarket::DEFAULT_EGRESS_PROBE,
    )
    .and_then(vike_polymarket::EgressCheck::into_result)
    .unwrap_or_else(|e| panic!("egress guard: {e}"));
    eprintln!("egress OK: {}", egress.expect("a region was declared, so the probe ran"));
    // The exec gate (`flags.poly_exec`) is the ADAPTER's `resolve`, which this smoke bypasses: it calls the factory
    // itself, so nothing here arms or reads the flag (decision 0095's amendment of 2026-11-22: the root hands the
    // adapter a bool).
    // Leave `venue.polymarket.exec_markets` empty ON PURPOSE: the empty (account-wide) subscribe is
    // what this test exists to prove. Assert that, so a stray row can't make the proof vacuous.
    assert!(
        vike_polymarket::polymarket_execution_markets(&settings).is_empty(),
        "this test proves the ACCOUNT-WIDE (empty markets) subscribe — leave \
         venue.polymarket.exec_markets empty"
    );

    let Some((token_id, condition_id)) = discover_token() else {
        eprintln!("SKIP: no token with a safe midpoint");
        return;
    };
    eprintln!("token={token_id} market={condition_id}");

    let (tx, mut rx) = event_channel(4096);
    // The smoke owns a sentinel path it never creates: this test places a real (never-filling) order
    // and must not inherit the kill switch off the box running it.
    let halt = tempfile::Builder::new().prefix("poly-mount-smoke-no-halt-").tempdir().unwrap();
    let no_halt = halt.path().join("HALT");
    let Ok(mut runtime) = live_runtime_for_account(
        &account,
        &vars,
        &AccountLabel::Default,
        Some(&token_id),
        true,
        &tx,
        &settings,
        no_halt,
    ) else {
        panic!("live_runtime_for_account built nothing with creds present — L2 derivation failed?");
    };
    assert!(
        runtime.reconcile.is_some(),
        "want_recon=true + derived L2 ⇒ a recon client over the same registry"
    );
    // Let the user-WS pump connect + subscribe before the order exists, so the CANCELLATION frame
    // cannot be missed for a reason unrelated to what is under test.
    std::thread::sleep(Duration::from_secs(4));

    // A resting BUY at 0.01 against a midpoint ≥ 0.15 — it cannot fill. ~$1.20 notional.
    let order_client_id = format!("mountsmoke-{}", now_ms() % 100_000_000);
    let price = 0.01;
    let qty = (1.2_f64 / price).ceil();
    let req = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "polymarket".to_string(),
        symbol: token_id.clone(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        time_in_force: TimeInForce::Gtc,
        timestamp: now_ms() as i64,
        ..Default::default()
    };
    runtime.client.submit(&req);

    // 1. the emitter split: Rust's synchronous OrderSubmitted …
    let submitted = wait_event(
        &mut rx,
        Instant::now() + Duration::from_secs(10),
        POLL,
        |e| matches!(e, Event::OrderSubmitted(s) if s.order_client_id == order_client_id),
    );
    assert!(submitted.is_some(), "no OrderSubmitted for {order_client_id}");

    // … then the VENUE's OrderAccepted (or OrderRejected — a rejection is a legitimate terminal and
    // must be reported, not hidden behind a timeout).
    let outcome = wait_event(&mut rx, Instant::now() + Duration::from_secs(30), POLL, |e| {
        matches!(e, Event::OrderAccepted(a) if a.order_client_id == order_client_id)
            || matches!(e, Event::OrderRejected(r) if r.order_client_id == order_client_id)
    });
    let venue_order_id = match outcome {
        Some(Event::OrderAccepted(a)) => a
            .venue_order_id
            .map(|v| v.to_string())
            .expect("OrderAccepted carries the CLOB order id"),
        Some(Event::OrderRejected(r)) => panic!("venue REJECTED the mounted order: {}", r.reason),
        _ => panic!(
            "no terminal for {order_client_id} within 30s — the mounted client is not reporting"
        ),
    };
    eprintln!("✅ mounted submit accepted: clob order {venue_order_id}");

    // 2. Cancel OUT OF BAND — a direct REST call, deliberately NOT `start.client.cancel`, so the
    //    only path an `OrderCanceled` can reach the lane by is the user-WS pump the start started.
    let relayer_key = account.get("RELAYER_API_KEY").unwrap_or_default().to_string();
    let relayer_address = account.get("RELAYER_API_KEY_ADDRESS").unwrap_or_default().to_string();
    let mut oob = vike_polymarket::PolymarketConfig {
        private_key: account.get("PRIVATE_KEY").unwrap_or_default().to_string().into(),
        ..Default::default()
    };
    oob.address = vike_bridge_core::eip712::eth_address_from_private_key(oob.private_key.expose())
        .expect("EOA");
    let boot = (now_ms() / 1000) as i64;
    vike_polymarket::ensure_l2(&mut oob, &clob(), boot).expect("derive L2 for the OOB cancel");
    let cancelled = vike_polymarket::cancel_order_relayer(
        &clob(),
        &oob,
        &venue_order_id,
        &relayer_key,
        &relayer_address,
    );

    let websocket_cancel = cancelled.as_ref().ok().and_then(|_| {
        wait_event(
            &mut rx,
            Instant::now() + Duration::from_secs(30),
            POLL,
            |e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id),
        )
    });

    // 3. NEVER leave it resting, whatever happened above (the out-of-band cancel may have failed).
    runtime.client.cancel(&order_client_id);
    std::thread::sleep(Duration::from_secs(2));
    runtime.client.detach();

    if let Err(e) = &cancelled {
        panic!("out-of-band cancel failed ({e}) — order was cancelled through the client instead");
    }
    assert!(
        websocket_cancel.is_some(),
        "the mount's user-WS pump did not deliver a re-keyed OrderCanceled for {order_client_id} within 30s. \
         Either the pump is not running inside the exec thread, or an EMPTY `markets` subscribe is \
         NOT account-wide on this deployment — in which case set `venue.polymarket.exec_markets` \
         explicitly."
    );
    eprintln!(
        "✅ user-WS pump delivered a re-keyed OrderCanceled for {order_client_id} — the mounted return lane is \
         live AND the empty-markets subscribe is account-wide"
    );
}
