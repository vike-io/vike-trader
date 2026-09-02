//! LIVE Polymarket user-channel smoke — real-money Polygon **mainnet** behind the geo proxy.
//! `#[ignore]`d and self-skips unless `POLY_PRIVATE_KEY` is in the workspace `.env`. Order
//! placement is geo-blocked, so run WITH the Dublin SOCKS proxy:
//!
//! ```sh
//! POLY_SOCKS_PROXY=socks5://127.0.0.1:1080 \
//!   cargo test -p vike-polymarket --features polymarket --test polymarket_user_smoke \
//!   -- --ignored --nocapture
//! ```
//!
//! Two layers, both exercising the real `spawn_polymarket_user_data` pump + the shared
//! `PolymarketRegistry` re-keying against the live user WS:
//!
//! - **`place_cancel_roundtrip` (SAFE — no fill risk):** rests a NON-filling BUY @ 0.01 (far below
//!   any real mid), spawns the pump, cancels over REST, and asserts the pump delivers a WS-sourced
//!   `OrderCanceled` **re-keyed to our coid**. Proves connect → subscribe → decode(order
//!   CANCELLATION) → registry re-key → deliver, live.
//! - **`fill_lane` (REAL MONEY — opt-in `POLY_FILL_SMOKE=1`):** crosses the spread with a tiny
//!   marketable order, asserts a pump-delivered fill re-keyed to our coid, then best-effort
//!   flattens. The genuine "trust live fills" gate — the maker/taker trade decode can only be
//!   proven by an actual fill. Keep the notional at the venue minimum.
//!
//! Field shapes (trade `status`, `maker_orders[]`, order `type`) are pinned from Polymarket docs +
//! the Nautilus adapter, not a prior live capture — this smoke is where any residual drift surfaces.

// Integration tests compile unconditionally as their own crate; the whole library is behind the
// non-default `polymarket` feature, so the whole file must be gated or default-feature
// `cargo test` breaks (audit G2 — the gate moved intact with the file, crate-reorg Phase 3, PR I).
#![cfg(feature = "polymarket")]

use std::time::{Duration, Instant};

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_exec::{event_channel, Ingest};
use vike_model::events::Event;
use vike_polymarket::{
    build_order, cancel_order_relayer, decode_user, ensure_l2, eth_address_from_private_key,
    get_json, order_to_json, sign_order_1271, spawn_polymarket_user_data, PolymarketCreds,
    PolymarketRegistry, Side, SignatureType, CLOB_BASE, WS_USER,
};

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// 53-bit salt (the wire carries it as a JSON number; a wider salt rebuilds a different EIP-712
/// hash → "invalid signature"). Vary by call so repeated orders don't collide.
fn salt(nonce: u128) -> u128 {
    (now_ms().wrapping_add(nonce)) & ((1u128 << 53) - 1)
}

struct LiveCtx {
    creds: PolymarketCreds,
    deposit_wallet: String,
    sig_type: SignatureType,
    relayer_key: String,
    relayer_addr: String,
}

/// Load creds + the deposit-wallet/relayer identity, or `None` to self-skip (live gate).
fn load_live_ctx() -> Option<LiveCtx> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let pk = vars.get("POLY_PRIVATE_KEY").filter(|s| !s.is_empty())?.clone();
    let relayer_addr = vars.get("POLY_RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default();
    let relayer_key = vars.get("POLY_RELAYER_API_KEY").cloned().unwrap_or_default();
    let signer = eth_address_from_private_key(&pk).ok()?;
    let mut creds = PolymarketCreds {
        private_key: pk,
        address: signer.clone(),
        relayer_key: relayer_key.clone(),
        relayer_address: relayer_addr.clone(),
        ..Default::default()
    };
    let boot =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    ensure_l2(&mut creds, CLOB_BASE, boot).ok()?;
    // The account's funder is a Polymarket DEPOSIT WALLET → signatureType POLY_1271 (default),
    // maker == the deposit wallet; the EOA key is the authorized signer.
    let deposit_wallet = vars
        .get("POLY_FUNDER")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "0x107C01D04Fd68557ACd52E89dD01972b22803aD5".to_string());
    let sig_type = match vars.get("POLY_SIGNATURE_TYPE").map(String::as_str) {
        Some("0") => SignatureType::Eoa,
        Some("1") => SignatureType::PolyProxy,
        Some("2") => SignatureType::PolyGnosisSafe,
        _ => SignatureType::Poly1271,
    };
    Some(LiveCtx { creds, deposit_wallet, sig_type, relayer_key, relayer_addr })
}

/// Discover a liquid token with a safe midpoint, plus its market (condition id) and NegRisk flag.
/// Reads route through the proxy (clob is DNS-blocked here).
fn discover_token(mid_range: std::ops::Range<f64>) -> Option<(String, String, bool, f64)> {
    let markets = get_json(CLOB_BASE, "/sampling-markets", "").ok()?;
    let empty = Vec::new();
    let data = markets.get("data").and_then(|d| d.as_array()).unwrap_or(&empty);
    for m in data {
        let neg_risk = m.get("neg_risk").and_then(|n| n.as_bool()).unwrap_or(false);
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
            let mid =
                get_json(CLOB_BASE, "/midpoint", &format!("token_id={tid}")).ok().and_then(|v| {
                    v.get("mid").and_then(|m| m.as_str()).and_then(|s| s.parse::<f64>().ok())
                });
            if let Some(mid) = mid {
                if mid_range.contains(&mid) {
                    return Some((tid.to_string(), condition_id, neg_risk, mid));
                }
            }
        }
    }
    None
}

/// Sign + submit a relayer (deposit-wallet) order; returns the CLOB `orderID`.
fn place_relayer(
    ctx: &LiveCtx,
    token_id: &str,
    price: f64,
    size: f64,
    side: Side,
    neg_risk: bool,
    nonce: u128,
) -> Option<String> {
    use vike_polymarket::submit_order_relayer;
    let order_signer = &ctx.deposit_wallet; // POLY_1271: order.signer == the deposit wallet
    let order = build_order(
        price,
        size,
        side,
        token_id,
        &ctx.deposit_wallet,
        order_signer,
        ctx.sig_type,
        now_ms(),
        salt(nonce),
        neg_risk,
        [0u8; 32],
    );
    let sig = sign_order_1271(&order, &ctx.creds.private_key).ok()?;
    let obj = order_to_json(&order, &sig, 0); // GTC smoke → "no expiry"
    let resp = submit_order_relayer(
        CLOB_BASE,
        &ctx.creds,
        obj,
        "GTC",
        &ctx.relayer_key,
        &ctx.relayer_addr,
    )
    .ok()?;
    resp.get("orderID").and_then(|o| o.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Poll the pump's ingest channel for an `Event` matching `pred`, up to `deadline`.
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
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
        }
    }
    None
}

#[test]
#[ignore = "LIVE real-money mainnet + proxy — run manually (see module doc)"]
fn place_cancel_roundtrip() {
    vike_log::test_init();
    // §0.2 egress guard: with POLY_EXPECT_EGRESS_COUNTRY set (e.g. `IE` for the Dublin/arbdub
    // route) a misrouted run fails HERE, naming the observed IP, instead of looking like an
    // ordinary network flake. Unset (the default) = no network call, no-op.
    match vike_polymarket::check_expected_egress()
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!(
            "egress: unchecked (set POLY_EXPECT_EGRESS_COUNTRY=IE to assert the Dublin route)"
        ),
        Err(e) => panic!("egress guard: {e}"),
    }
    let Some(ctx) = load_live_ctx() else {
        tracing::warn!(target: "vike_polymarket", "SKIP: POLY creds absent");
        return;
    };
    let Some((token_id, condition_id, neg_risk, mid)) = discover_token(0.15..0.85) else {
        tracing::warn!(target: "vike_polymarket", "SKIP: no token with a safe midpoint");
        return;
    };
    tracing::info!(target: "vike_polymarket", "token={token_id} market={condition_id} neg_risk={neg_risk} mid={mid}");

    // Rest a NON-filling BUY @ 0.01 (mid > 0.15 → cannot fill), ~$1.20 notional.
    let price = 0.01;
    let size = (1.2_f64 / price).ceil();
    let orderid = place_relayer(&ctx, &token_id, price, size, Side::Buy, neg_risk, 1)
        .expect("submit accepted (proves the V2 signature)");
    tracing::info!(target: "vike_polymarket", "rested order {orderid}");

    // Register it under a test coid and attach the pump on the SAME registry.
    let coid = format!("smoke-{}", now_ms() % 100_000_000);
    let registry = PolymarketRegistry::new();
    let _ = registry.on_accept(&coid, &orderid, 1);
    let (tx, mut rx) = event_channel(1024);
    let feed = spawn_polymarket_user_data(
        WS_USER.to_string(),
        ctx.creds.clone(),
        vec![condition_id],
        registry.clone(),
        tx,
    );
    std::thread::sleep(Duration::from_secs(3)); // WS connect + subscribe settle

    // Cancel over REST → the venue emits an order CANCELLATION on the USER channel.
    cancel_order_relayer(CLOB_BASE, &ctx.creds, &orderid, &ctx.relayer_key, &ctx.relayer_addr)
        .expect("cancel accepted");

    // The pump must decode that CANCELLATION and re-key it to OUR coid.
    let got = wait_event(
        &mut rx,
        Instant::now() + Duration::from_secs(25),
        |e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid),
    );
    feed.shutdown().expect("pump shutdown");
    assert!(got.is_some(), "pump did not deliver a re-keyed OrderCanceled for {coid} within 25s");
    tracing::info!(target: "vike_polymarket", "✅ pump delivered re-keyed OrderCanceled for {coid}");
}

#[test]
#[ignore = "LIVE REAL MONEY (fills!) — opt-in POLY_FILL_SMOKE=1; run manually"]
fn fill_lane() {
    vike_log::test_init();
    // §0.2 egress guard: with POLY_EXPECT_EGRESS_COUNTRY set (e.g. `IE` for the Dublin/arbdub
    // route) a misrouted run fails HERE, naming the observed IP, instead of looking like an
    // ordinary network flake. Unset (the default) = no network call, no-op.
    match vike_polymarket::check_expected_egress()
        .and_then(vike_polymarket::EgressCheck::into_result)
    {
        Ok(Some(e)) => eprintln!("egress OK: {e}"),
        Ok(None) => eprintln!(
            "egress: unchecked (set POLY_EXPECT_EGRESS_COUNTRY=IE to assert the Dublin route)"
        ),
        Err(e) => panic!("egress guard: {e}"),
    }
    if std::env::var("POLY_FILL_SMOKE").ok().as_deref() != Some("1") {
        tracing::warn!(target: "vike_polymarket", "SKIP: set POLY_FILL_SMOKE=1 to run the real-money fill test");
        return;
    }
    let Some(ctx) = load_live_ctx() else {
        tracing::warn!(target: "vike_polymarket", "SKIP: POLY creds absent");
        return;
    };
    let Some((token_id, condition_id, neg_risk, mid)) = discover_token(0.2..0.8) else {
        tracing::warn!(target: "vike_polymarket", "SKIP: no token with a safe midpoint");
        return;
    };
    // Cross the spread to fill: BUY at mid+0.05 (capped < 1.0). Venue-minimum notional (~$1.10).
    let buy_px = (mid + 0.05).min(0.98);
    let size = (1.1_f64 / buy_px).ceil();
    tracing::warn!(target: "vike_polymarket", "REAL-MONEY fill test: BUY {size} @ {buy_px} on {token_id}");

    let coid = format!("fillsmoke-{}", now_ms() % 100_000_000);
    let registry = PolymarketRegistry::new();
    let (tx, mut rx) = event_channel(1024);
    // The lane the pump feeds, kept for the replay below — production's `client.rs` sends recovered
    // events into exactly this channel, so the smoke must too rather than inspecting them directly.
    let replay_tx = tx.clone();
    let feed = spawn_polymarket_user_data(
        WS_USER.to_string(),
        ctx.creds.clone(),
        vec![condition_id],
        registry.clone(),
        tx,
    );
    std::thread::sleep(Duration::from_secs(3));

    let orderid = place_relayer(&ctx, &token_id, buy_px, size, Side::Buy, neg_risk, 2)
        .expect("marketable submit accepted");
    // THE ACK RACE, live. This order is deliberately MARKETABLE, so the venue routinely matches it
    // before `place_relayer`'s HTTP response gets back here — meaning the pump has already seen the
    // trade frame for an id the registry could not re-key yet, and parked it
    // (`vike_polymarket::pending_events`). `on_accept` hands those frames back; re-decoding them now
    // that the id resolves, and pushing them into the SAME lane, is precisely what `client.rs`'s
    // submit arm does in production. Discarding them instead would make this smoke time out on the
    // very race it exists to prove — after spending real money and leaving a position to flatten —
    // which is why `on_accept` is `#[must_use]`.
    for parked in registry.on_accept(&coid, &orderid, 1) {
        for ev in decode_user(&parked.frame, &registry) {
            let _ = replay_tx.blocking_send(ev);
        }
    }
    tracing::info!(target: "vike_polymarket", "marketable order {orderid} placed");

    // The pump must deliver a fill re-keyed to our coid (bare Fill or the OrderFilled wrap).
    let fill = wait_event(&mut rx, Instant::now() + Duration::from_secs(30), |e| match e {
        Event::Fill(f) => f.client_order_id == coid,
        Event::OrderFilled(w) => w.client_order_id == coid,
        _ => false,
    });

    // ALWAYS best-effort flatten (real money): SELL the same size, crossing down. Ignore errors here
    // — surface them as a warning; the assertion below is what fails the test.
    let sell_px = (mid - 0.05).max(0.02);
    match place_relayer(&ctx, &token_id, sell_px, size, Side::Sell, neg_risk, 3) {
        Some(id) => tracing::info!(target: "vike_polymarket", "flatten SELL {id} @ {sell_px}"),
        None => {
            tracing::error!(target: "vike_polymarket", "⚠ FLATTEN FAILED — check for a dangling {size}-share position on {token_id}")
        }
    }
    feed.shutdown().expect("pump shutdown");
    assert!(
        fill.is_some(),
        "pump did not deliver a re-keyed fill for {coid} within 30s (position was best-effort flattened)"
    );
    tracing::info!(target: "vike_polymarket", "✅ pump delivered a re-keyed fill for {coid}");
}
