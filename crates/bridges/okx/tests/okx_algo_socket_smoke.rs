//! The OKX ALGO socket on the DEMO account: log in on the BUSINESS url, subscribe `orders-algo`, place a
//! far-from-market stop, cancel it through the bridge, and see the stop's `canceled` come back on the
//! socket as `OrderCanceled` under the stop's own id. The live proof of
//! `crates/bridges/okx/src/user_data.rs`'s `open_okx_user_data_ws` on the business url and of
//! `crates/bridges/okx/src/event_mapper.rs`'s `map_okx_algo_frame` over a frame the venue just sent
//! (`tests/offline/algo_channel.rs` replays the captured frames offline). It does NOT run the pump
//! thread or its resync.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_algo_socket_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY: one `conditional` SELL stop at HALF the last price (it
//! cannot trigger in a smoke's lifetime) for the instrument's minimum size, `reduceOnly`; the demo
//! account is flat and the stop is cancelled before the test ends (a `Drop` guard cancels it on every
//! exit path). Gated like every `*_smoke.rs` here: the DEMO account's creds through
//! `vike_secrets::smoke_directory` (asked for `AccountTier::Demo`), a `tracing::warn!` and an early
//! return when absent; every transport is `UreqOkxTransport::new(true)` (the simulated-trading header)
//! and the socket url is `DEMO_BUSINESS_WS`. Credentials are never printed.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::user_data::{OpenOutcome, StreamError, StreamMsg, UserStream};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, now_ms};
use vike_okx::event_mapper::map_okx_algo_frame;
use vike_okx::perp::{
    DEMO_BUSINESS_WS, OkxPerpRest, PATH_INSTRUMENTS, REST, parse_okx_perp_instruments,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_okx::user_data::open_okx_user_data_ws;
use vike_okx::ws_auth::PING_TEXT;
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTC-USDT-SWAP";

type SwapRest = OkxPerpRest<UreqOkxTransport>;

fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

/// The demo (simulated-trading) SWAP client. DEMO ONLY: the creds were asked for as `Demo` and every
/// transport sends the simulated-trading header.
fn build_rest(creds: &Credentials) -> SwapRest {
    let public = UreqOkxTransport::new(true);
    assert!(public.simulated, "this smoke is DEMO ONLY: the simulated-trading header must be on");
    let info = public
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[SYMBOL]
        .clone();
    OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

/// Cancels the stop when dropped (idempotent: the bridge's second cancel of a cancelled stop is `Ok`).
struct CancelOnDrop<'a> {
    rest: &'a SwapRest,
    coid: &'a str,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.rest.cancel_order(self.coid) {
            println!("CLEANUP: bridge cancel answered {} {}", e.code, e.msg);
        }
    }
}

/// Read the socket for up to `within`, mapping every text frame; stop at the first batch of events
/// that satisfies `done`. Returns every event seen.
fn read_until(
    ws: &mut impl UserStream,
    within: Duration,
    done: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline && !done(&seen) {
        match ws.recv() {
            Ok(StreamMsg::Text(text)) => {
                if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) {
                    println!("FRAME {}", frame.get("data").map_or(text.clone(), |d| d.to_string()));
                    seen.extend(map_okx_algo_frame(&frame));
                }
            }
            Ok(_) | Err(StreamError::Timeout) => {}
            Err(StreamError::Closed(why)) => panic!("the algo socket closed: {why}"),
        }
    }
    seen
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-triggering) demo stop — run manually (see module doc)"]
fn okx_algo_socket_delivers_a_stops_cancel() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    let Some(passphrase) = creds.passphrase.as_ref() else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO passphrase absent");
        return;
    };
    let rest = build_rest(&creds);

    // --- 1. the business socket: login + subscribe `orders-algo` -------------------------------
    let stop = AtomicBool::new(false);
    let gate = vike_okx::ratelimit::ws_rate_gate();
    let mut ws = match open_okx_user_data_ws(
        DEMO_BUSINESS_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        passphrase.expose(),
        now_ms,
        "orders-algo",
        "SWAP",
        &stop,
        &gate,
    ) {
        OpenOutcome::Ready(ws) => ws,
        OpenOutcome::Transport(e) => panic!("okx algo socket connect failed: {e}"),
        OpenOutcome::Auth(e) => panic!("okx algo socket auth failed: {e}"),
        OpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
    };
    println!(
        "MEASURED the business socket accepted login + subscribe(orders-algo) on {DEMO_BUSINESS_WS}"
    );

    // --- 2. place a far SELL stop, see its `live` on the socket (mapped to nothing) --------------
    let mark = rest.last_price().expect("ticker");
    assert!(mark > 0.0, "live mark required");
    let qty = rest.properties.min_qty * rest.ct_val;
    let coid = format!("vtsock{}", now_ms() % 100_000_000);
    let request: OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL, "side": -1, "qty": qty,
        "order_type": "stop", "trigger_price": mark * 0.5, "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&rest, &request);
    let _cleanup = CancelOnDrop { rest: &rest, coid: &coid };
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "the demo stop: {events:?}");
    let _ = ws.send_text(PING_TEXT);
    let live = read_until(&mut ws, Duration::from_secs(3), |_| false);
    println!("MEASURED events mapped from the stop's `live` push: {live:?}");
    assert!(live.is_empty(), "`live` maps to nothing: the REST ack accepted the stop");

    // --- 3. cancel it through the bridge; the socket pushes `canceled` --------------------------
    rest.cancel_order(&coid).expect("the bridge cancels the stop");
    let seen = read_until(&mut ws, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid))
    });
    println!("MEASURED events mapped from the socket after the cancel: {seen:?}");
    assert!(
        seen.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid)),
        "the socket must push the stop's cancel as OrderCanceled({coid}): {seen:?}"
    );
}
