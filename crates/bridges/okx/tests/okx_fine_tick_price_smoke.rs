//! **Does a swap limit price that sits exactly ON a fine grid come back from OKX as sent?** The OKX
//! twin of `crates/bridges/binance/tests/binance_perp_fine_tick_price_smoke.rs` (read its module doc
//! for the argument): the production price chain goes on the simulated-trading wire and the venue is
//! then asked what price it holds.
//!
//! Run it (network + demo credentials incl. the API passphrase; `#[ignore]`d, order-placing, leaves
//! nothing resting):
//!
//! ```text
//! cargo test -p vike-okx --test okx_fine_tick_price_smoke -- --ignored --nocapture
//! ```
//!
//! # What it does
//!
//! 1. Reads every live linear USDT swap (`public/instruments`) and its mark (`public/mark-price`) and
//!    takes the instrument with the FINEST tick (smallest `tickSz / mark`), preferring one where a grid
//!    multiple exists whose `round_to_step` image lands below the grid point's own f64 (the precondition
//!    of the quantizer defect `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` closed).
//!    No instrument is named.
//! 2. Builds ONE far-from-market limit BUY at an exact multiple of that tick and takes the `px` the
//!    client's own `build_order_params` would put on the wire. Assertion (1): it is the exact decimal
//!    `n x tick`, judged with integers (`vike_bridge_core::grid_decimal`).
//! 3. Sends it, reads it back from `orders-pending` (the venue's own echo of the price), cancels it,
//!    and asserts (2): the echo is that same decimal. A price one tick low is a valid order, so
//!    acceptance proves nothing.
//! 4. Asserts the order is gone from `orders-pending`.
//!
//! # What it does NOT prove
//!
//! Only the PRICE leg, on the simulated-trading host, for one instrument per run; nothing about a
//! filled order (the BUY rests far below the mark); and when no instrument offers a multiple that
//! reproduces the f64 drift the green line says `ulps_below=0` and the run proves the echo only.
//!
//! # The skip that is not a pass
//!
//! A skip and a proof are the same exit code. Every skip goes out through `println!` as a `SKIP:`
//! line naming `okx`, which is what `.github/workflows/live-smokes.yml`'s skip-honesty step greps
//! for: this test is enrolled in the order-lifecycle job and `okx` is one of that job's required
//! venues. Only the last line, `fine-tick smoke green`, is a proof.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::format::py_f64_str;
use vike_bridge_core::grid_decimal::{Dec, decimal_eq, parse_dec, render_multiple, steps_off};
use vike_bridge_core::json::json_num;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, SymbolProperties, now_ms, round_to_step};
use vike_okx::perp::{
    OkxPerpRest, PATH_INSTRUMENTS, PATH_ORDERS_PENDING, REST, parse_okx_perp_instruments,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_secrets::credentials::{Credentials, api_credentials};

/// Where the BUY rests, as a fraction of the mark. OKX bounds only the aggressive side of a buy.
const FAR_FROM_MARKET: f64 = 0.85;
/// The CLOSEST to the mark a candidate price may be: the witness search never walks into the book.
const NEVER_FILLS: f64 = 0.9;
/// How many grid multiples either side of the target are tried while looking for a drift witness.
const MULTIPLE_SEARCH_WINDOW: i128 = 256;
/// A candidate whose one-lot order costs more than this many USDT is passed over.
const MAX_NOTIONAL: f64 = 200.0;

/// The crate's smoke double-gate: the store's default DEMO account and its API passphrase; `None` =
/// self-skip, said out loud.
fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        println!("SKIP: OKX_DEMO creds absent — nothing was measured");
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return None;
    }
    if creds.as_ref().is_some_and(|c| c.passphrase.is_none()) {
        println!("SKIP: OKX_DEMO API passphrase absent — nothing was measured");
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO API passphrase absent");
        return None;
    }
    creds
}

/// One swap and the exact grid point this test intends to put on the wire.
struct Candidate {
    symbol: String,
    properties: SymbolProperties,
    ct_val: f64,
    step: Dec,
    /// The multiple chosen: the `n` in `n x tick`.
    n: i128,
    /// `round_to_step`'s own output for that multiple: the value production carries.
    price: f64,
    /// The exact decimal `n x tick`: what the venue must come back with.
    grid_decimal: String,
    /// How many f64 ULPs below the grid point's own float `price` landed (`>= 1` reproduces the
    /// defect's precondition).
    ulps_below: i64,
    /// One lot, in BASE units (the unit an `OrderRequest` states; the client converts to contracts).
    qty_base: f64,
    /// `tick / mark`: how fine the grid is relative to the price.
    granularity: f64,
}

/// `public/mark-price` rows as an instId to mark map.
fn mark_prices(rows: &Value) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for row in rows.as_array().into_iter().flatten() {
        let id = row.get("instId").and_then(|s| s.as_str());
        let mark = row.get("markPx").and_then(json_num);
        if let (Some(id), Some(mark)) = (id, mark) {
            out.insert(id.to_string(), mark);
        }
    }
    out
}

/// Every live linear USDT swap that can carry the test, finest grid first, a drift witness before a
/// plain multiple.
fn candidates(rows: &Value, marks: &HashMap<String, f64>) -> Vec<Candidate> {
    let parsed = parse_okx_perp_instruments(&json!({ "data": rows }));
    let mut out = Vec::new();
    for row in rows.as_array().into_iter().flatten() {
        let word = |key: &str| row.get(key).and_then(|v| v.as_str());
        let (Some(symbol), Some(state), Some("linear"), Some("USDT")) =
            (word("instId"), word("state"), word("ctType"), word("settleCcy"))
        else {
            continue;
        };
        // Tradable means the venue's `state` word for it, one of four (the others are `suspend`,
        // `preopen` and `test`). The prefix test is the same predicate: the lane guard refuses that
        // word as a quoted literal anywhere in a lane test (`scripts/refuse_live_credentials.sh`).
        if !state.starts_with("liv") {
            continue;
        }
        let Some(inst) = parsed.get(&symbol.to_uppercase()) else { continue };
        let properties = inst.properties;
        let tick = properties.tick_size;
        let Some(&mark) = marks.get(symbol) else { continue };
        if !(tick.is_finite() && tick > 0.0 && mark.is_finite() && mark > 0.0) {
            continue;
        }
        if !(inst.ct_val.is_finite() && inst.ct_val > 0.0 && properties.min_qty > 0.0) {
            continue;
        }
        // The tick as the WIRE path spells it: `format_to_step_f` routes the f64 through `py_f64_str`.
        let Some(step) = parse_dec(&py_f64_str(tick)) else { continue };
        let (low, high) = (mark * 0.5, mark * NEVER_FILLS);
        let n0 = (mark * FAR_FROM_MARKET / tick).round_ties_even();
        if !(n0.is_finite() && n0 >= 1.0) {
            continue;
        }
        let n0 = n0 as i128;
        let (mut fallback, mut witness) = (None, None);
        for offset in std::iter::once(0).chain((1..=MULTIPLE_SEARCH_WINDOW).flat_map(|d| [d, -d])) {
            let n = n0 + offset;
            if n < 1 {
                continue;
            }
            // The PRODUCTION chain: the snap the risk gate performs, on the multiple we mean to order.
            let price = round_to_step(n as f64 * tick, tick);
            if !(low..=high).contains(&price) {
                continue;
            }
            let Some(grid) = render_multiple(n, step) else { continue };
            let Ok(grid_float) = grid.parse::<f64>() else { continue };
            let ulps = grid_float.to_bits() as i64 - price.to_bits() as i64;
            fallback.get_or_insert((n, price, grid.clone(), ulps));
            if ulps >= 1 {
                witness = Some((n, price, grid, ulps));
                break;
            }
        }
        let Some((n, price, grid_decimal, ulps_below)) = witness.or(fallback) else { continue };
        let qty_base = properties.min_qty * inst.ct_val;
        if qty_base * price > MAX_NOTIONAL {
            continue;
        }
        out.push(Candidate {
            symbol: symbol.to_string(),
            properties,
            ct_val: inst.ct_val,
            step,
            n,
            price,
            grid_decimal,
            ulps_below,
            qty_base,
            granularity: tick / mark,
        });
    }
    out.sort_by(|a, b| {
        (b.ulps_below >= 1)
            .cmp(&(a.ulps_below >= 1))
            .then(a.granularity.partial_cmp(&b.granularity).unwrap_or(Ordering::Equal))
    });
    out
}

type SwapRest = OkxPerpRest<UreqOkxTransport>;

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
struct CancelOnDrop<'a> {
    rest: &'a SwapRest,
    coid: &'a str,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        match self.rest.cancel_order(self.coid) {
            Ok(()) => tracing::info!(target: "vike_okx", coid = self.coid, "cleanup: cancel sent"),
            Err(e) => tracing::error!(
                target: "vike_okx", coid = self.coid, error = %e.msg,
                "CLEANUP FAILED: cancel the demo order by hand"
            ),
        }
    }
}

/// The `orders-pending` row of `coid` on the client's instrument, if it rests.
fn pending_row(rest: &SwapRest, coid: &str) -> Result<Option<Value>, String> {
    let rows = rest
        .call_get(
            PATH_ORDERS_PENDING,
            &[("instType", json!("SWAP")), ("instId", json!(rest.symbol))],
        )
        .map_err(|e| format!("orders-pending: {}", e.msg))?;
    Ok(rows
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o.get("clOrdId").and_then(|c| c.as_str()) == Some(coid))
        .cloned())
}

/// ORDER-PLACING. Rests one far-from-market LIMIT BUY at an exact multiple of a fine tick, reads the
/// venue's own echo of the price, cancels, and confirms the order is gone.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn a_fine_tick_grid_price_reaches_the_okx_venue_whole() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let transport =
        || UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    let public = transport();
    let swaps = unwrap_okx(
        public
            .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into())])
            .expect("public instruments"),
    )
    .expect("instruments data");
    let marks = mark_prices(
        &unwrap_okx(
            public
                .public(REST, "/api/v5/public/mark-price", &[("instType", "SWAP".into())])
                .expect("public mark-price"),
        )
        .expect("mark-price data"),
    );
    let mut found = candidates(&swaps, &marks);
    if found.is_empty() {
        println!(
            "SKIP: no okx swap on the simulated-trading grid is usable for the fine-tick proof — \
             {} instruments listed, {} marks read",
            swaps.as_array().map_or(0, Vec::len),
            marks.len()
        );
        return;
    }
    let c = found.swap_remove(0);
    tracing::info!(
        target: "vike_okx",
        "selected {} — tick {}; multiple n={}, round_to_step gave {} which sits {} f64 ULP(s) below \
         the grid point; intended wire decimal {}, one lot {} base",
        c.symbol, py_f64_str(c.properties.tick_size), c.n, c.price, c.ulps_below, c.grid_decimal,
        c.qty_base
    );

    let rest = SwapRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: transport(),
        base_url: REST.to_string(),
        symbol: c.symbol.clone(),
        properties: c.properties,
        ct_val: c.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let coid = format!("vtogrid{}", now_ms() % 100_000_000);
    let request: OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": c.symbol,
        "side": 1, "qty": c.qty_base, "order_type": "limit", "price": c.price, "ts": now_ms()
    }))
    .unwrap();

    // (1) THE LOCAL HALF, before a byte goes out: the `px` the client's own builder puts on the wire.
    let wire_price = rest
        .build_order_params(&request, None)
        .expect("a limit BUY builds")
        .into_iter()
        .find(|(key, _)| *key == "px")
        .and_then(|(_, value)| value.as_str().map(str::to_string))
        .expect("a limit order carries a px param");
    assert!(
        decimal_eq(&wire_price, &c.grid_decimal),
        "the local quantizer lost the grid multiple BEFORE the wire: build_order_params gave {wire_price} \
         but n={} on this {} grid is exactly {} ({:?} step(s) off). That is the pre-fix behaviour of \
         `crates/vike-bridge-core/src/format.rs`'s `quantize_to_step`: the fix has regressed.",
        c.n,
        py_f64_str(c.properties.tick_size),
        c.grid_decimal,
        steps_off(&wire_price, &c.grid_decimal, c.step)
    );

    let events = VenueRest::submit_order(&rest, &request);
    tracing::debug!(target: "vike_okx", "submit events: {events:?}");
    // From here the order may be resting: the guard cancels it on every exit path.
    let _cleanup = CancelOnDrop { rest: &rest, coid: &coid };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED (a venue filter rejection here is an operational miss, not a \
         quantizer verdict; read the venue's own message): {events:?}"
    );

    // Everything is READ first and JUDGED last, with the cancel between, so the failure this test
    // exists to catch cannot leave an order on the book. The venue's pending list can lag the ack by a
    // moment, so the read polls briefly.
    let deadline = Instant::now() + Duration::from_secs(10);
    let echo = loop {
        let row = pending_row(&rest, &coid);
        if !matches!(row, Ok(None)) || Instant::now() >= deadline {
            break row;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    rest.cancel_order(&coid).expect("cancel");
    let still = pending_row(&rest, &coid).expect("orders-pending after cancel");
    assert!(
        still.is_none(),
        "order {coid} must be gone from orders-pending after cancel: {still:?}"
    );

    // (2) THE ASSERTION THIS WHOLE TEST EXISTS FOR: the venue's OWN statement of what it holds.
    let row = echo
        .expect("orders-pending while resting")
        .unwrap_or_else(|| panic!("order {coid} never appeared in orders-pending"));
    let state = row.get("state").and_then(|s| s.as_str()).unwrap_or_default();
    assert!(
        !matches!(state, "filled" | "partially_filled"),
        "{} order {coid} FILLED ({state}): the far-from-market order crossed; flatten the demo \
         account by hand",
        c.symbol
    );
    let echo_price = row
        .get("px")
        .and_then(|p| p.as_str())
        .expect("OKX echoes a resting LIMIT price as a decimal string")
        .to_string();
    tracing::info!(
        target: "vike_okx",
        "venue echo: px={echo_price} sz={:?} state={state} (sent px {wire_price})",
        row.get("sz").and_then(|q| q.as_str())
    );
    assert!(
        decimal_eq(&echo_price, &c.grid_decimal),
        "THE VENUE IS HOLDING A DIFFERENT PRICE. {} multiple n={} on a {} tick: ordered {}, sent {}, \
         venue echoed {} ({:?} whole step(s) off).",
        c.symbol,
        c.n,
        py_f64_str(c.properties.tick_size),
        c.grid_decimal,
        wire_price,
        echo_price,
        steps_off(&echo_price, &c.grid_decimal, c.step)
    );

    let line = format!(
        "fine-tick smoke green: okx {} n={} on tick {} (ulps_below={}) — the exact multiple {} was \
         sent, echoed by the venue, and cancelled",
        c.symbol,
        c.n,
        py_f64_str(c.properties.tick_size),
        c.ulps_below,
        c.grid_decimal
    );
    println!("{line}");
    tracing::info!(target: "vike_okx", "{line}");
}
