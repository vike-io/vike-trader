//! LIVE Polymarket **mounted-engine** smoke — the proof that a strategy can place an order on
//! Polymarket from vike, through the ordinary `ExecutionClient` seam, with fills/cancels returning
//! on the authenticated user WS the mount starts for itself.
//!
//! This is the end-to-end twin of `polymarket_user_smoke.rs`. That one drives the venue by hand
//! (build → sign → REST → attach a pump) to prove each primitive; this one goes through
//! [`vike_polymarket::live_mount_from_vars`] — the exact call `vike_mount::make_engine`'s
//! `("polymarket", _)` arm makes — and asserts the CONTRACT a mounted venue owes:
//!
//! 1. `submit` emits `OrderSubmitted` synchronously, then the venue side emits `OrderAccepted`
//!    carrying the CLOB order id (the emitter split);
//! 2. a cancel issued **out of band** (direct REST, NOT through the client) still comes back as an
//!    `OrderCanceled` re-keyed to OUR coid — which can only happen if the mount really did start
//!    the user-WS pump inside its exec thread, and if the pump's **empty `markets` subscribe is
//!    account-wide** (the one server-side convention `crate::mount::poly_exec_markets` documents
//!    but cannot prove offline);
//! 3. no order silently vanishes — the test cancels through the client on every failure path.
//!
//! ⚠ REAL MONEY. Polymarket has NO testnet. The order is a BUY at 0.01 on a token whose midpoint is
//! ≥ 0.15, so it CANNOT fill; the notional is ~$1.20 and it is cancelled within seconds either way.
//! `#[ignore]`d and self-skipping without `POLY_PRIVATE_KEY`, like every other venue's live smoke.
//!
//! Run it from a permitted region (order placement is geo-blocked) — either natively on the Dublin
//! host, or locally over the SOCKS tunnel with BOTH lanes proxied:
//!
//! ```sh
//! POLY_SOCKS_PROXY=socks5h://127.0.0.1:11080 POLY_WS_PROXY_ENABLED=1 \
//!   POLY_EXPECT_EGRESS_COUNTRY=IE \
//!   cargo test -p vike-polymarket --features polymarket --test poly_exec_mount_smoke \
//!   -- --ignored --nocapture
//! ```

// The whole library is behind the non-default `polymarket` feature (audit G2).
#![cfg(feature = "polymarket")]

use std::time::{Duration, Instant};

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};
use vike_polymarket::{CLOB_BASE, get_json, live_mount_from_vars};

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// Poll the mount's ingest lane for an `Event` matching `pred`, up to `deadline`.
fn wait_event(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    deadline: Instant,
    mut pred: impl FnMut(&Event) -> bool,
) -> Option<Event> {
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => {
                if pred(&e) {
                    return Some(e);
                }
            }
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

/// A liquid token with a midpoint safely away from our resting price → `(token_id, condition_id)`.
fn discover_token() -> Option<(String, String)> {
    let markets = get_json(CLOB_BASE, "/sampling-markets", "").ok()?;
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
            let mid = get_json(CLOB_BASE, "/midpoint", &format!("token_id={tid}"))
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
fn mounted_engine_place_and_ws_cancel_roundtrip() {
    vike_log::test_init();
    // §0.2 egress guard: with POLY_EXPECT_EGRESS_COUNTRY set, a misrouted run fails HERE naming the
    // observed IP, instead of looking like an ordinary network flake. Unset ⇒ no call, no-op.
    match vike_polymarket::check_expected_egress()
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!("egress: unchecked (set POLY_EXPECT_EGRESS_COUNTRY=IE)"),
        Err(e) => panic!("egress guard: {e}"),
    }

    let mut vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    if vars.get("POLY_PRIVATE_KEY").filter(|s| !s.is_empty()).is_none() {
        eprintln!("SKIP: POLY_PRIVATE_KEY absent (absent-credentials-is-the-live-gate)");
        return;
    }
    // Arm the exec gate for THIS call only, through the vars map — never `set_var` (unsound under
    // threads; the repo-wide rule). `poly_exec_enabled` reads the map as well as the process env,
    // which is exactly why it does.
    vars.insert(vike_polymarket::POLY_EXEC_ENV.to_string(), "1".to_string());
    // Leave POLY_EXEC_MARKETS unset ON PURPOSE: the empty (account-wide) subscribe is what this
    // test exists to prove. Assert that, so a stray `.env` line can't make the proof vacuous.
    assert!(
        vike_polymarket::poly_exec_markets(&vars).is_empty(),
        "this test proves the ACCOUNT-WIDE (empty markets) subscribe — unset POLY_EXEC_MARKETS"
    );

    let Some((token_id, condition_id)) = discover_token() else {
        eprintln!("SKIP: no token with a safe midpoint");
        return;
    };
    eprintln!("token={token_id} market={condition_id}");

    let (tx, mut rx) = event_channel(4096);
    assert!(vike_polymarket::poly_exec_enabled(&vars));
    let Some(mut mount) = live_mount_from_vars(&vars, Some(&token_id), true, &tx) else {
        panic!("live_mount_from_vars returned None with creds present — L2 derivation failed?");
    };
    assert!(
        mount.recon.is_some(),
        "want_recon=true + derived L2 ⇒ a recon client over the same registry"
    );
    // Let the user-WS pump connect + subscribe before the order exists, so the CANCELLATION frame
    // cannot be missed for a reason unrelated to what is under test.
    std::thread::sleep(Duration::from_secs(4));

    // A resting BUY at 0.01 against a midpoint ≥ 0.15 — it cannot fill. ~$1.20 notional.
    let coid = format!("mountsmoke-{}", now_ms() % 100_000_000);
    let price = 0.01;
    let qty = (1.2_f64 / price).ceil();
    let req = OrderRequest {
        client_order_id: coid.clone(),
        venue: "polymarket".to_string(),
        symbol: token_id.clone(),
        side: 1,
        qty,
        order_type: "limit".to_string(),
        price: Some(price),
        time_in_force: TimeInForce::Gtc,
        ts: now_ms() as i64,
        ..Default::default()
    };
    mount.client.submit(&req);

    // 1. the emitter split: Rust's synchronous OrderSubmitted …
    let submitted = wait_event(
        &mut rx,
        Instant::now() + Duration::from_secs(10),
        |e| matches!(e, Event::OrderSubmitted(s) if s.client_order_id == coid),
    );
    assert!(submitted.is_some(), "no OrderSubmitted for {coid}");

    // … then the VENUE's OrderAccepted (or OrderRejected — a rejection is a legitimate terminal and
    // must be reported, not hidden behind a timeout).
    let outcome = wait_event(&mut rx, Instant::now() + Duration::from_secs(30), |e| {
        matches!(e, Event::OrderAccepted(a) if a.client_order_id == coid)
            || matches!(e, Event::OrderRejected(r) if r.client_order_id == coid)
    });
    let venue_order_id = match outcome {
        Some(Event::OrderAccepted(a)) => a
            .venue_order_id
            .map(|v| v.to_string())
            .expect("OrderAccepted carries the CLOB order id"),
        Some(Event::OrderRejected(r)) => panic!("venue REJECTED the mounted order: {}", r.reason),
        _ => panic!("no terminal for {coid} within 30s — the mounted client is not reporting"),
    };
    eprintln!("✅ mounted submit accepted: clob order {venue_order_id}");

    // 2. Cancel OUT OF BAND — a direct REST call, deliberately NOT `mount.client.cancel`, so the
    //    only path an `OrderCanceled` can reach the lane by is the user-WS pump the mount started.
    let creds_vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let relayer_key = creds_vars.get("POLY_RELAYER_API_KEY").cloned().unwrap_or_default();
    let relayer_addr = creds_vars.get("POLY_RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default();
    let mut oob = vike_polymarket::PolymarketCreds {
        private_key: creds_vars.get("POLY_PRIVATE_KEY").cloned().unwrap_or_default(),
        ..Default::default()
    };
    oob.address = vike_polymarket::eth_address_from_private_key(&oob.private_key).expect("EOA");
    let boot = (now_ms() / 1000) as i64;
    vike_polymarket::ensure_l2(&mut oob, CLOB_BASE, boot).expect("derive L2 for the OOB cancel");
    let cancelled = vike_polymarket::cancel_order_relayer(
        CLOB_BASE,
        &oob,
        &venue_order_id,
        &relayer_key,
        &relayer_addr,
    );

    let ws_cancel = cancelled.as_ref().ok().and_then(|_| {
        wait_event(
            &mut rx,
            Instant::now() + Duration::from_secs(30),
            |e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid),
        )
    });

    // 3. NEVER leave it resting, whatever happened above (the out-of-band cancel may have failed).
    mount.client.cancel(&coid);
    std::thread::sleep(Duration::from_secs(2));
    mount.client.detach();

    if let Err(e) = &cancelled {
        panic!("out-of-band cancel failed ({e}) — order was cancelled through the client instead");
    }
    assert!(
        ws_cancel.is_some(),
        "the mount's user-WS pump did not deliver a re-keyed OrderCanceled for {coid} within 30s. \
         Either the pump is not running inside the exec thread, or an EMPTY `markets` subscribe is \
         NOT account-wide on this deployment — in which case set POLY_EXEC_MARKETS explicitly."
    );
    eprintln!(
        "✅ user-WS pump delivered a re-keyed OrderCanceled for {coid} — the mounted return lane is \
         live AND the empty-markets subscribe is account-wide"
    );
}
