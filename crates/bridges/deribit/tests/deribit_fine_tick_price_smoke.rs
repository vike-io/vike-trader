//! **Does a limit price that sits exactly ON Deribit's fine option grid come back from the venue as
//! sent?** The Deribit twin of `crates/bridges/binance/tests/binance_perp_fine_tick_price_smoke.rs` (read
//! its module doc for the argument): the production price chain goes on the testnet wire and the venue
//! is then asked what price it holds.
//!
//! Run it (network + testnet credentials; `#[ignore]`d, order-placing, leaves nothing resting):
//!
//! ```text
//! cargo test -p vike-deribit --test deribit_fine_tick_price_smoke -- --ignored --nocapture
//! ```
//!
//! # What it does
//!
//! 1. Takes the most liquid BTC option with a live ask (the discovery `deribit_tif_smoke.rs` uses) and
//!    reads its row with `public/get_instruments`: the finest grid Deribit lists is an option's base
//!    tick, below the tier boundary where the venue's grid steps up.
//! 2. Builds ONE far-below-the-ask limit BUY at an exact multiple of that tick and takes the `price`
//!    the client's own `build_order_params` would put on the wire (a JSON number: decimal string, then
//!    f64, then the number the venue parses). Assertion (1): it is the exact decimal `n x tick`, judged
//!    with integers (`vike_bridge_core::grid_decimal`), before a byte is sent.
//! 3. Sends it, reads the resting order back through the reconcile snapshot (the venue's open-order
//!    row), cancels it, reads it again from the order history (the venue's record of the cancelled
//!    order) and asserts (2): both are that same price. A price one tick off is a valid order, so
//!    acceptance proves nothing.
//! 4. Asserts the order no longer rests.
//!
//! # What it does NOT prove
//!
//! Only the PRICE leg, on the testnet host, for one option per run; nothing about a filled order (the
//! BUY rests far below the ask). ⚠ No Deribit tick is in the family whose f64 image sits below its
//! decimal (the precondition of the quantizer defect `crates/vike-bridge-core/src/format.rs`'s
//! `grid_multiple_image` closed; `0.0001` and `0.0005` are both above theirs), so this run proves that
//! a fine-grid price survives the decimal, f64, JSON and venue round trip as sent, and cannot reproduce
//! that defect: the green line says `ulps_below` and it is not positive here.
//!
//! # The skip that is not a pass
//!
//! A skip and a proof are the same exit code. Every skip goes out through `println!` as a `SKIP:`
//! line naming `deribit`, which is what `.github/workflows/live-smokes.yml`'s skip-honesty step greps
//! for: this test is enrolled in the order-lifecycle job and `deribit` is one of that job's required
//! venues. Only the last line, `fine-tick smoke green`, is a proof.

use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::format::py_f64_str;
use vike_bridge_core::grid_decimal::{decimal_eq, parse_dec, render_multiple, steps_off};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_deribit::catalog::properties_of;
use vike_deribit::client::DeribitRest;
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, now_ms, round_to_step};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The crate's smoke double-gate: the store's default DEMO account; `None` = self-skip, said out loud.
fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "deribit");
    if creds.is_none() {
        println!("SKIP: DERIBIT_DEMO creds absent — nothing was measured");
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
    }
    creds
}

/// The most liquid BTC option with a live ask: `(instrument name, open interest, ask)`.
fn most_liquid_option(book: &Value) -> Option<(String, f64, f64)> {
    let mut best: Option<(String, f64, f64)> = None;
    for row in book["result"].as_array().into_iter().flatten() {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    best
}

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
struct CancelOnDrop<'a> {
    rest: &'a DeribitRest,
    coid: &'a str,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        match self.rest.cancel_order(self.coid) {
            Ok(()) => {
                tracing::info!(target: "vike_deribit", coid = self.coid, "cleanup: cancel sent")
            }
            Err(e) => tracing::error!(
                target: "vike_deribit", coid = self.coid, error = %e.msg,
                "CLEANUP FAILED: cancel the testnet order by hand"
            ),
        }
    }
}

/// The order-history row of `label` (history can lag the cancel briefly).
fn history_row(rest: &DeribitRest, coid: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let hist = rest.get_order_history(50).expect("order history");
        if let Some(row) = hist
            .as_array()
            .into_iter()
            .flatten()
            .find(|o| o.get("label").and_then(|l| l.as_str()) == Some(coid))
        {
            return row.clone();
        }
        assert!(Instant::now() < deadline, "{coid} never appeared in order history");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// ORDER-PLACING. Rests one far-below-the-ask LIMIT BUY at an exact multiple of the option tick, reads
/// the venue's own record of the price, cancels, and confirms the order is gone.
#[test]
#[ignore = "network + testnet creds — places + cancels a real (non-filling) testnet order — run manually (see module doc)"]
fn a_fine_tick_grid_price_reaches_the_deribit_venue_whole() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let Some((symbol, oi, ask)) = most_liquid_option(&book) else {
        println!(
            "SKIP: no deribit BTC option with a live ask on the testnet — nothing was measured"
        );
        return;
    };
    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let Some(properties) = info["result"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row.get("instrument_name").and_then(|n| n.as_str()) == Some(symbol.as_str()))
        .and_then(properties_of)
    else {
        println!(
            "SKIP: the deribit testnet lists no readable row for {symbol} — nothing was measured"
        );
        return;
    };
    let tick = properties.tick_size;
    let Some(step) = parse_dec(&py_f64_str(tick)) else {
        println!(
            "SKIP: the deribit tick {tick} of {symbol} is not a plain decimal — nothing was measured"
        );
        return;
    };

    // Far below the ask and under the tiered-tick boundary, so the base tick is the grid in force (the
    // rule `deribit_tif_smoke.rs` rests by). A multiple that is not a round number, so the decimal is
    // not trivially short.
    let target = (ask * 0.25).min(0.004).max(tick);
    let mut n = (target / tick).round_ties_even().max(1.0) as i128;
    if n % 10 == 0 && n > 1 {
        n -= 1;
    }
    let price = round_to_step(n as f64 * tick, tick);
    let grid_decimal = render_multiple(n, step).expect("n x tick renders");
    let grid_float: f64 = grid_decimal.parse().expect("the grid decimal parses");
    let ulps_below = grid_float.to_bits() as i64 - price.to_bits() as i64;
    let qty = properties.min_qty;
    tracing::info!(
        target: "vike_deribit",
        "selected {symbol} (oi={oi}, ask={ask}) — tick {}; multiple n={n}, round_to_step gave {price} \
         (ulps_below={ulps_below}); intended wire decimal {grid_decimal}, qty {qty}",
        py_f64_str(tick)
    );

    let mut transport = DeribitOrderTransport::new(
        TESTNET_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    transport.connect().expect("order-WS auth");
    let rest = DeribitRest::new(transport, &symbol, properties, "BTC");

    let coid = format!("vtdgrid{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: "deribit".to_string(),
        symbol: symbol.clone(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        ts: now_ms(),
        ..Default::default()
    };

    // (1) THE LOCAL HALF, before a byte goes out: the number the client's own builder puts on the wire.
    let wire_price = rest.build_order_params(&request)["price"]
        .as_f64()
        .expect("a limit order carries a numeric price param");
    assert!(
        wire_price.to_bits() == grid_float.to_bits(),
        "the local quantizer lost the grid multiple BEFORE the wire: build_order_params gave {wire_price} \
         but n={n} on this {} grid is exactly {grid_decimal} ({:?} step(s) off). That is the pre-fix \
         behaviour of `crates/vike-bridge-core/src/format.rs`'s `quantize_to_step`: the fix has regressed.",
        py_f64_str(tick),
        steps_off(&format!("{wire_price}"), &grid_decimal, step)
    );

    let events = VenueRest::submit_order(&rest, &request);
    tracing::debug!(target: "vike_deribit", "submit events: {events:?}");
    // From here the order may be resting: the guard cancels it on every exit path.
    let _cleanup = CancelOnDrop { rest: &rest, coid: &coid };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "testnet order must be ACCEPTED (a venue rejection here is an operational miss, not a \
         quantizer verdict; read the venue's own message): {events:?}"
    );

    // Everything is READ first and JUDGED last, with the cancel between, so the failure this test
    // exists to catch cannot leave an order on the book.
    let resting = rest.connect();
    rest.cancel_order(&coid).expect("cancel the resting order");
    let row = history_row(&rest, &coid);
    let after = rest.connect().expect("final reconcile");
    assert!(
        after.open_orders.iter().all(|o| o.request.client_order_id != coid),
        "order {coid} must no longer rest after cancel"
    );

    // (2) THE ASSERTION THIS WHOLE TEST EXISTS FOR: the venue's OWN statement of what it holds, from
    // the open-order row while it rested and from the history record after the cancel.
    let snapshot = resting.expect("reconcile while resting");
    let open = snapshot
        .open_orders
        .iter()
        .find(|o| o.request.client_order_id == coid)
        .unwrap_or_else(|| panic!("order {coid} must REST in the open orders"));
    let open_price = open.request.price.expect("a resting limit carries its price");
    let filled = row.get("filled_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert_eq!(filled, 0.0, "{symbol} order {coid} filled: flatten the testnet account by hand");
    let record_price = row
        .get("price")
        .and_then(|p| p.as_f64())
        .expect("Deribit records a limit order's price as a number");
    tracing::info!(
        target: "vike_deribit",
        "venue echo: open price={open_price}, history price={record_price} state={:?} (sent {wire_price})",
        row.get("order_state").and_then(|s| s.as_str())
    );
    for (what, echoed) in [("open-order row", open_price), ("history record", record_price)] {
        assert!(
            echoed.to_bits() == grid_float.to_bits()
                && decimal_eq(&format!("{echoed}"), &grid_decimal),
            "THE VENUE IS HOLDING A DIFFERENT PRICE. {symbol} multiple n={n} on a {} tick: ordered \
             {grid_decimal}, sent {wire_price}, the {what} says {echoed} ({:?} whole step(s) off).",
            py_f64_str(tick),
            steps_off(&format!("{echoed}"), &grid_decimal, step)
        );
    }
    rest.detach();

    let line = format!(
        "fine-tick smoke green: deribit {symbol} n={n} on tick {} (ulps_below={ulps_below}) — the exact \
         multiple {grid_decimal} was sent, echoed by the venue, and cancelled",
        py_f64_str(tick)
    );
    println!("{line}");
    tracing::info!(target: "vike_deribit", "{line}");
}
