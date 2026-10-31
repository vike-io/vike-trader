//! **Does a perpetual limit price that sits exactly ON a fine grid come back from the venue as sent?**
//! The USDⓈ-M perp twin of `binance_fine_tick_price_smoke.rs` (spot): it puts the production price
//! chain on the fapi demo wire and then asks the venue what price it holds.
//!
//! Run it (network + demo credentials; `#[ignore]`d, order-placing, leaves nothing resting):
//!
//! ```text
//! cargo test -p vike-binance --test binance_perp_fine_tick_price_smoke -- --ignored --nocapture
//! ```
//!
//! # What it does
//!
//! 1. Reads the whole fapi grid and the mark of every perpetual, and takes the instrument with the
//!    FINEST tick (smallest `tick / mark`), preferring one where a grid multiple exists whose
//!    `round_to_step` image lands below the grid point's own f64 (the precondition of the quantizer
//!    defect `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` closed; `ulps_below`).
//!    No symbol is named: a hard-coded pair would keep passing the day the venue coarsened its tick.
//! 2. Builds ONE far-from-market limit BUY at an exact multiple of that tick and takes the `price`
//!    the client's own `build_order_params` would put on the wire. Assertion (1): that string is the
//!    exact decimal `n x tick` (integer arithmetic; a float compare would be the defect judging
//!    itself). It fails before a byte is sent when the local quantizer regressed.
//! 3. Sends it, reads the order back with `GET /fapi/v1/order` (the venue's own echo of the price),
//!    cancels it, and asserts (2): the echo is that same decimal, trailing zeros aside. A price one
//!    tick low is a perfectly valid order, so acceptance proves nothing; the echo is the witness.
//! 4. Asserts the order is gone from `openOrders`.
//!
//! # What it does NOT prove
//!
//! - Only the PRICE leg, on the demo host, for one instrument per run. The quantity leg is logged
//!   (`origQty`) and not gated.
//! - Nothing about a FILLED order: the order rests at a fraction of the mark and never trades.
//! - When no instrument offers a multiple that reproduces the f64 drift, the run says so
//!   (`ulps_below` of zero in the green line) and proves the echo only, not the defect.
//!
//! # The skip that is not a pass
//!
//! A skip and a proof are the same exit code. Both skips go out through `println!` (a `tracing`
//! line alone vanishes under `RUST_LOG=error`) as `SKIP: ...` lines that name the venue, which is
//! what `.github/workflows/live-smokes.yml`'s skip-honesty step greps for: this test is enrolled in
//! the order-lifecycle job and `binance` is one of that job's required venues, so a run that
//! measured nothing reads red there. Only the last line, `fine-tick smoke green`, is a proof.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde_json::Value;
use vike_binance::family::order_map::binance_broker_coid;
use vike_binance::family::perp_series_symbol;
use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS, PATH_ORDER,
    parse_binance_perp_instruments,
};
use vike_bridge_core::format::py_f64_str;
use vike_bridge_core::grid_decimal::{Dec, decimal_eq, parse_dec, render_multiple, steps_off};
use vike_bridge_core::json::json_num;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, SymbolProperties, now_ms, round_to_step};
use vike_secrets::credentials::{Credentials, api_credentials};

/// Where the BUY rests, as a fraction of the mark. fapi's `PERCENT_PRICE` bounds only the aggressive
/// side of a BUY, so a BUY far below the mark is accepted and cannot cross.
const FAR_FROM_MARKET: f64 = 0.85;
/// The CLOSEST to the mark a candidate price may be: the witness search never walks into the book.
const NEVER_FILLS: f64 = 0.9;
/// How many grid multiples either side of the target are tried while looking for one that
/// reproduces the defect's precondition.
const MULTIPLE_SEARCH_WINDOW: i128 = 256;
/// Notional headroom over the symbol's `MIN_NOTIONAL`.
const NOTIONAL_MARGIN: f64 = 1.6;
/// A candidate whose cheapest legal order costs more than this many USDT is passed over.
const MAX_NOTIONAL: f64 = 200.0;
/// Free USDT required, as a multiple of the order's notional.
const FUNDING_HEADROOM: f64 = 3.0;

/// The crate's smoke double-gate: the store's default DEMO account; `None` = self-skip, said out loud.
fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "binance");
    if creds.is_none() {
        println!("SKIP: BINANCE_DEMO creds absent — nothing was measured");
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

/// One perpetual and the exact grid point this test intends to put on the wire.
struct Candidate {
    symbol: String,
    properties: SymbolProperties,
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
    qty: f64,
    /// `tick / mark`: how fine the grid is relative to the price.
    granularity: f64,
}

/// `GET /fapi/v1/premiumIndex` (no symbol) as a symbol to mark-price map.
fn mark_prices(body: &Value) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for row in body.as_array().into_iter().flatten() {
        let symbol = row.get("symbol").and_then(|s| s.as_str());
        let mark = row.get("markPrice").and_then(json_num);
        if let (Some(symbol), Some(mark)) = (symbol, mark) {
            out.insert(symbol.to_string(), mark);
        }
    }
    out
}

/// Every tradable USDT perpetual that can carry the test, finest grid first, a drift witness before
/// a plain multiple.
fn candidates(info: &Value, marks: &HashMap<String, f64>) -> Vec<Candidate> {
    let parsed = parse_binance_perp_instruments(info);
    let mut out = Vec::new();
    for entry in info.get("symbols").and_then(|s| s.as_array()).into_iter().flatten() {
        let word = |key: &str| entry.get(key).and_then(|v| v.as_str());
        let (Some(symbol), Some("TRADING"), Some("PERPETUAL"), Some("USDT")) =
            (word("symbol"), word("status"), word("contractType"), word("quoteAsset"))
        else {
            continue;
        };
        let Some(inst) = parsed.get(&symbol.to_uppercase()) else { continue };
        let properties = inst.properties;
        let tick = properties.tick_size;
        let Some(&mark) = marks.get(symbol) else { continue };
        if !(tick.is_finite() && tick > 0.0 && mark.is_finite() && mark > 0.0) {
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

        // An order size over the venue's own floors, rounded UP a whole lot (the wire quantizes DOWN).
        let step_size = properties.step_size;
        let by_notional = properties.min_notional.max(5.0) * NOTIONAL_MARGIN / price;
        let mut qty = by_notional.max(properties.min_qty).max(step_size);
        if step_size > 0.0 {
            qty = ((qty / step_size).ceil() + 1.0) * step_size;
        }
        let too_big = properties.max_qty > 0.0 && qty > properties.max_qty;
        if !(qty.is_finite() && qty > 0.0) || too_big || qty * price > MAX_NOTIONAL {
            continue;
        }
        out.push(Candidate {
            symbol: symbol.to_string(),
            properties,
            step,
            n,
            price,
            grid_decimal,
            ulps_below,
            qty,
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

type PerpRest = BinancePerpRest<BinanceHmacSigner, UreqTransport>;

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
struct CancelOnDrop<'a> {
    rest: &'a PerpRest,
    coid: &'a str,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        match self.rest.cancel_order(self.coid) {
            Ok(()) => {
                tracing::info!(target: "vike_binance", coid = self.coid, "cleanup: cancel sent")
            }
            Err(e) => tracing::error!(
                target: "vike_binance", coid = self.coid, error = %e.msg,
                "CLEANUP FAILED: cancel the demo order by hand"
            ),
        }
    }
}

/// ORDER-PLACING. Rests one far-from-market LIMIT BUY at an exact multiple of a fine tick, reads the
/// venue's own echo of the price, cancels, and confirms the order is gone.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn a_fine_tick_grid_price_reaches_the_binance_perp_venue_whole() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let transport =
        || UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate());
    let public = transport();
    let info = public.public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[]).expect("fapi exchangeInfo");
    let marks = mark_prices(
        &public.public(DEMO_FAPI_REST, "/fapi/v1/premiumIndex", &[]).expect("fapi premiumIndex"),
    );
    let mut found = candidates(&info, &marks);
    if found.is_empty() {
        println!(
            "SKIP: no binance perpetual on the fapi demo grid is usable for the fine-tick proof — \
             {} symbols listed, {} marks read",
            info.get("symbols").and_then(|s| s.as_array()).map_or(0, Vec::len),
            marks.len()
        );
        return;
    }
    let c = found.swap_remove(0);
    tracing::info!(
        target: "vike_binance",
        "selected {} — tick {}; multiple n={}, round_to_step gave {} which sits {} f64 ULP(s) below \
         the grid point; intended wire decimal {}, qty {}",
        c.symbol, py_f64_str(c.properties.tick_size), c.n, c.price, c.ulps_below, c.grid_decimal, c.qty
    );

    let rest = PerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport: transport(),
        base_url: DEMO_FAPI_REST.to_string(),
        algo_coids: Default::default(),
        symbol: c.symbol.clone(),
        properties: c.properties,
        leverage: 1.0,
    };
    // Clock skew against the demo host's own `/fapi/v1/time` (recvWindow is short).
    let local = now_ms();
    let server = rest
        .transport
        .public(DEMO_FAPI_REST, "/fapi/v1/time", &[])
        .expect("fapi time")
        .get("serverTime")
        .and_then(|t| t.as_i64())
        .expect("serverTime");
    rest.signer.set_offset_ms(server - local);

    let balance = rest
        .transport
        .signed(DEMO_FAPI_REST, "/fapi/v2/balance", "GET", &[], &rest.signer)
        .expect("fapi balance");
    let free = balance
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row.get("asset").and_then(|a| a.as_str()) == Some("USDT"))
        .and_then(|row| row.get("availableBalance").and_then(json_num))
        .unwrap_or(0.0);
    if free < c.qty * c.price * FUNDING_HEADROOM {
        println!(
            "SKIP: the binance fapi demo account holds {free} free USDT, too little to fund the \
             fine-tick order on {} — nothing was measured",
            c.symbol
        );
        return;
    }

    let coid = format!("vtpgrid{}", now_ms() % 100_000_000);
    let request: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": perp_series_symbol(&c.symbol),
        "side": 1, "qty": c.qty, "order_type": "limit", "price": c.price, "ts": now_ms()
    }))
    .unwrap();

    // (1) THE LOCAL HALF, before a byte goes out: the price the client's own builder puts on the wire.
    let wire_price = rest
        .build_order_params(&request)
        .into_iter()
        .find(|(key, _)| *key == "price")
        .map(|(_, value)| value)
        .expect("a limit order carries a price param");
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
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    // From here the order may be resting: the guard cancels it on every exit path.
    let _cleanup = CancelOnDrop { rest: &rest, coid: &coid };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED (a venue filter rejection here is an operational miss, not a \
         quantizer verdict; read the venue's own message): {events:?}"
    );

    // Everything is READ first and JUDGED last, with the cancel between, so the failure this test
    // exists to catch cannot leave an order on the book. Two round trips stand between submit and cancel.
    let query = [
        ("symbol", c.symbol.clone()),
        ("origClientOrderId", binance_broker_coid(rest.link_id.as_deref(), &coid)),
    ];
    let echo = rest.transport.signed(&rest.base_url, PATH_ORDER, "GET", &query, &rest.signer);
    let resting = rest.transport.signed(
        &rest.base_url,
        PATH_OPEN_ORDERS,
        "GET",
        &[("symbol", c.symbol.clone())],
        &rest.signer,
    );
    rest.cancel_order(&coid).expect("cancel");

    let after = rest
        .transport
        .signed(
            &rest.base_url,
            PATH_OPEN_ORDERS,
            "GET",
            &[("symbol", c.symbol.clone())],
            &rest.signer,
        )
        .expect("openOrders after cancel");
    let rests = |rows: &Value| {
        rows.as_array()
            .into_iter()
            .flatten()
            .any(|o| o.get("clientOrderId").and_then(|v| v.as_str()) == Some(coid.as_str()))
    };
    assert!(!rests(&after), "order {coid} must be gone from openOrders after cancel: {after}");

    // (2) THE ASSERTION THIS WHOLE TEST EXISTS FOR: the venue's OWN statement of what it holds.
    let echo = echo.expect("GET /fapi/v1/order, the venue's own echo of the resting price");
    let status = echo.get("status").and_then(|s| s.as_str()).unwrap_or_default();
    assert!(
        !matches!(status, "FILLED" | "PARTIALLY_FILLED"),
        "{} order {coid} FILLED ({status}): the far-from-market order crossed; flatten the demo \
         account by hand",
        c.symbol
    );
    let echo_price = echo
        .get("price")
        .and_then(|p| p.as_str())
        .expect("fapi echoes a resting LIMIT price as a decimal string")
        .to_string();
    tracing::info!(
        target: "vike_binance",
        "venue echo: price={echo_price} origQty={:?} status={status} (sent price {wire_price})",
        echo.get("origQty").and_then(|q| q.as_str())
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
    // The resting list shows it too (corroboration through the other read the reconcile pass uses).
    let resting = resting.expect("openOrders while resting");
    assert!(rests(&resting), "order {coid} must appear in openOrders while resting: {resting}");

    let line = format!(
        "fine-tick smoke green: binance perp {} n={} on tick {} (ulps_below={}) — the exact multiple {} \
         was sent, echoed by the venue, and cancelled",
        c.symbol,
        c.n,
        py_f64_str(c.properties.tick_size),
        c.ulps_below,
        c.grid_decimal
    );
    println!("{line}");
    tracing::info!(target: "vike_binance", "{line}");
}
