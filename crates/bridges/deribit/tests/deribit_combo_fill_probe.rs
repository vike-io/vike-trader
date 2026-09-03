//! Combo FILL-SHAPE evidence probe (combo gate 4 — the fill-drop hazard):
//!
//!     cargo test -p vike-deribit --test deribit_combo_fill_probe -- --ignored --nocapture
//!
//! `#[ignore]`d and double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! `DERIBIT_DEMO_*` creds in the workspace `.env`; self-skips with a `tracing::warn!` when absent).
//!
//! WHY THIS EXISTS: the combo design doc (§4 "Fills") and PR-4 both rest on Deribit's DOCUMENTED
//! claim that a combo execution reports trades "both for the combo and the individual legs", with
//! `combo_id`/`combo_trade_id` on the rows — but no fill payload was ever CAPTURED (the #512
//! lifecycle smoke deliberately rests far-from-market and cancels). The `Account` fold design
//! (which rows fold, which are wrap-only) is money math and must be driven by the venue's REAL
//! frames, not the announcement blog. This probe executes a combo that actually FILLS — the
//! venue-minimum amount crossed into an EXISTING market-maker-quoted testnet combo book (futures
//! spreads are two-sided-quoted on testnet) — and captures, VERBATIM:
//!
//!   1. every `user.trades.any.any.raw` WS frame around the execution (the exact frames the
//!      production pump feeds `map_deribit_private`),
//!   2. the `private/buy` JSON-RPC result (its embedded `trades[]` array),
//!   3. the post-fill `private/get_user_trades_by_currency` REST view (what a resync replays),
//!   4. the position deltas (proof the venue books PER-LEG positions), then the reduce-only
//!      closes of each leg (whose fills double as the NON-combo control rows), ending flat.
//!
//! SELF-CROSS IS BLOCKED — captured 2026-07-19 (testnet): resting a combo sell and IOC-buying it
//! back from the SAME account does NOT match; the taker comes back `order_state: "cancelled"`,
//! `cancel_reason: "order_overlap"`, `trades: []`. That is Deribit's self-trade prevention, which
//! is why this probe needs a real counterparty (the quoted combo book) at all.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::Message;

use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::ws::configure_ws_stream;
use vike_deribit::rpc::parse_response;
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_WS};
use vike_deribit::ws_auth::{build_client_credentials_auth, build_private_subscribe};

type RawWs = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

/// One authed JSON-RPC call; Err carries the venue's RAW error (evidence discipline: capture the
/// raw response, never guess past it) instead of panicking — a venue refusal is itself evidence.
fn try_rpc(t: &mut DeribitOrderTransport, method: &str, params: Value) -> Result<Value, String> {
    let resp = t.call(method, &params).map_err(|e| format!("{method} transport error: {e:?}"))?;
    if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
        return Err(format!("{method} venue error: {err}"));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

fn rpc(t: &mut DeribitOrderTransport, method: &str, params: Value) -> Value {
    try_rpc(t, method, params).unwrap_or_else(|e| panic!("{e}"))
}

/// The account's nonzero BTC positions across ALL kinds (futures + options + any) —
/// the flatness baseline/final comparison, sorted for stable diffing.
fn all_positions(t: &mut DeribitOrderTransport) -> Vec<(String, f64)> {
    let result = rpc(t, "private/get_positions", json!({"currency": "BTC"}));
    let mut out: Vec<(String, f64)> = result
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|p| {
            (
                p.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                p.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0),
            )
        })
        .filter(|(_, s)| *s != 0.0)
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Open a RAW `user.trades.any.any.raw` capture socket: connect → public/auth → private/subscribe,
/// hand-rolled so every subsequent frame is read VERBATIM (the production pump would map them).
fn open_capture_ws(client_id: &str, client_secret: &str) -> RawWs {
    let (mut socket, _resp) = tungstenite::connect(TESTNET_WS).expect("capture WS connect");
    configure_ws_stream(&socket, Duration::from_secs(1));
    let auth = build_client_credentials_auth(client_id, client_secret, None, 1);
    socket.send(Message::Text(auth.to_string().into())).expect("auth send");
    await_ack(&mut socket, 1);
    let sub = build_private_subscribe(&["user.trades.any.any.raw".to_string()], 2);
    socket.send(Message::Text(sub.to_string().into())).expect("subscribe send");
    await_ack(&mut socket, 2);
    socket
}

fn await_ack(socket: &mut RawWs, rid: i64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "capture WS ack {rid} timed out");
        let Ok(Message::Text(raw)) = socket.read() else { continue };
        let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else { continue };
        let (fid, _result, error) = parse_response(&frame);
        if fid != Some(rid) {
            continue;
        }
        assert!(error.is_none_or(|e| e.is_null()), "capture WS ack {rid} error: {frame}");
        return;
    }
}

/// Drain the capture socket until `deadline`, returning every raw text frame (and logging each —
/// the frames ARE the deliverable).
fn drain_frames(socket: &mut RawWs, deadline: Instant, tag: &str) -> Vec<String> {
    let mut frames = Vec::new();
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(raw)) => {
                tracing::info!(target: "vike_deribit", tag, raw = %raw.as_str(), "CAPTURED user-data frame (RAW)");
                frames.push(raw.as_str().to_string());
            }
            Ok(Message::Ping(p)) => {
                let _ = socket.send(Message::Pong(p));
            }
            Ok(_) => {}
            Err(e) if vike_bridge_core::ws::is_timeout(&e) => continue,
            Err(e) => panic!("capture WS read: {e}"),
        }
    }
    frames
}

/// Log the fold-design fields of every trade row inside a batch of raw `user.trades` frames.
fn summarize_rows(frames: &[String], tag: &str) -> usize {
    let mut rows = 0usize;
    for f in frames {
        let Ok(v) = serde_json::from_str::<Value>(f) else { continue };
        let Some(data) = v.pointer("/params/data").and_then(|d| d.as_array()) else { continue };
        for item in data {
            rows += 1;
            tracing::info!(
                target: "vike_deribit",
                tag,
                instrument = %item.get("instrument_name").and_then(|x| x.as_str()).unwrap_or(""),
                label = %item.get("label").and_then(|x| x.as_str()).unwrap_or("<ABSENT>"),
                state = %item.get("state").and_then(|x| x.as_str()).unwrap_or("<ABSENT>"),
                combo_id = %item.get("combo_id").and_then(|x| x.as_str()).unwrap_or("<ABSENT>"),
                combo_trade_id = ?item.get("combo_trade_id"),
                order_id = ?item.get("order_id"),
                trade_id = ?item.get("trade_id"),
                direction = %item.get("direction").and_then(|x| x.as_str()).unwrap_or(""),
                amount = ?item.get("amount"),
                price = ?item.get("price"),
                fee = ?item.get("fee"),
                "TRADE ROW (fold-design fields)"
            );
        }
    }
    rows
}

/// THE probe (see module doc). It crosses the venue-minimum amount into a quoted testnet combo
/// book (a REAL fill against a market maker), captures the trade rows, then reduce-only-closes
/// the leg positions — the account must end flat vs its pre-probe book.
#[test]
#[ignore = "network + demo creds — executes a REAL tiny testnet combo fill, then closes the legs flat"]
fn deribit_combo_fill_shape_probe() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("deribit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    let mut order = DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    order.connect().expect("order-WS auth");
    let baseline = all_positions(&mut order);
    tracing::info!(target: "vike_deribit", ?baseline, "pre-probe positions (all kinds)");

    // ---- find a QUOTED combo book (testnet MMs two-side-quote BTC futures spreads) ----
    let ids: Vec<String> =
        rpc(&mut order, "public/get_combo_ids", json!({"currency": "BTC", "state": "active"}))
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
    let mut pick: Option<(String, f64, f64)> = None; // (id, ask, ask_amount)
    for id in &ids {
        let t = rpc(&mut order, "public/ticker", json!({"instrument_name": id}));
        let ask = t.get("best_ask_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let ask_amt = t.get("best_ask_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bid = t.get("best_bid_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        // prefer a TWO-sided book (tight = cheap to cross); require a real ask to buy into
        if ask > 0.0 && ask_amt > 0.0 && bid > 0.0 {
            pick = Some((id.clone(), ask, ask_amt));
            break;
        }
        if pick.is_none() && ask > 0.0 && ask_amt > 0.0 {
            pick = Some((id.clone(), ask, ask_amt));
        }
    }
    let Some((combo_id, ask, ask_amt)) = pick else {
        tracing::warn!(target: "vike_deribit", scanned = ids.len(), "NO quoted combo book on testnet right now — no fill evidence possible");
        return;
    };

    // the combo book's own grid: tick + min amount (the probe trades the venue MINIMUM)
    let grid = rpc(&mut order, "public/get_instrument", json!({"instrument_name": combo_id}));
    let tick = grid.get("tick_size").and_then(|v| v.as_f64()).unwrap_or(0.5);
    let min_amt = grid.get("min_trade_amount").and_then(|v| v.as_f64()).unwrap_or(10.0);
    let amount = min_amt.min(ask_amt);
    tracing::info!(target: "vike_deribit", combo_id = %combo_id, ask, ask_amt, tick, amount, "quoted combo book picked");

    let mut capture = open_capture_ws(&creds.api_key, &creds.api_secret);

    // ---- cross the ask: IOC buy at ask + a few ticks (fills at ≤ price or cancels) ----
    let ts = vike_model::clock::now_ms();
    let label = format!("gate4-fill-{ts}");
    let px = ((ask + 5.0 * tick) / tick).round() * tick;
    let buy = try_rpc(
        &mut order,
        "private/buy",
        json!({
            "instrument_name": combo_id, "amount": amount, "type": "limit", "price": px,
            "label": label, "post_only": false, "time_in_force": "immediate_or_cancel",
        }),
    );
    match &buy {
        Ok(r) => {
            tracing::info!(target: "vike_deribit", raw = %r, "CROSS result (RAW — note the embedded trades[])")
        }
        Err(e) => tracing::warn!(target: "vike_deribit", err = %e, "CROSS refused (RAW)"),
    }
    let filled = buy
        .as_ref()
        .ok()
        .and_then(|r| r.get("trades"))
        .and_then(|t| t.as_array())
        .is_some_and(|t| !t.is_empty());

    // ---- capture the WS frames around the execution (the production pump's exact input) ----
    let frames = drain_frames(&mut capture, Instant::now() + Duration::from_secs(8), "combo-fill");
    let rows = summarize_rows(&frames, "combo-fill");
    tracing::info!(target: "vike_deribit", n = frames.len(), rows, "user.trades frames captured for the combo fill");

    // ---- REST twin: what a resync replay would fetch ----
    let hist = rpc(
        &mut order,
        "private/get_user_trades_by_currency",
        json!({"currency": "BTC", "count": 20, "sorting": "desc"}),
    );
    tracing::info!(target: "vike_deribit", raw = %hist, "get_user_trades_by_currency tail (RAW)");

    // ---- per-leg position deltas: the venue's own answer to "what did the combo book" ----
    let after = all_positions(&mut order);
    tracing::info!(target: "vike_deribit", ?after, "post-fill positions (all kinds)");
    let mut deltas: Vec<(String, f64)> = Vec::new();
    for (inst, size) in &after {
        let base = baseline.iter().find(|(i, _)| i == inst).map(|(_, s)| *s).unwrap_or(0.0);
        if (size - base).abs() > 1e-12 {
            deltas.push((inst.clone(), size - base));
        }
    }
    for (inst, base) in &baseline {
        if !after.iter().any(|(i, _)| i == inst) {
            deltas.push((inst.clone(), -base));
        }
    }
    tracing::info!(target: "vike_deribit", ?deltas, "position deltas from the combo fill");

    // ---- flatten: reduce-only market close per leg (their fills = the NON-combo control rows) ----
    for (inst, delta) in &deltas {
        let method = if *delta > 0.0 { "private/sell" } else { "private/buy" };
        let closed = try_rpc(
            &mut order,
            method,
            json!({
                "instrument_name": inst, "amount": delta.abs(), "type": "market",
                "label": format!("gate4-close-{ts}"), "reduce_only": true,
            }),
        );
        tracing::info!(target: "vike_deribit", inst = %inst, delta, result = ?closed, "leg close result (RAW)");
    }
    let close_frames =
        drain_frames(&mut capture, Instant::now() + Duration::from_secs(6), "leg-close");
    summarize_rows(&close_frames, "leg-close");

    let fin = all_positions(&mut order);
    assert_eq!(fin, baseline, "the account must end flat vs its pre-probe book");
    tracing::info!(target: "vike_deribit", filled, rows, "probe complete — account flat");
    if filled {
        assert!(
            rows > 0,
            "cross FILLED but no user.trades rows captured — rerun with a wider window"
        );
    }
    order.close();
}
