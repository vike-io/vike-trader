//! Live demo CLASSES smoke: what FXCM's own Offers table says about every instrument that is not a regular forex
//! pair (metals, indices, commodities, shares, crypto, treasuries), as the shim reads it. READ-ONLY: it places
//! nothing and subscribes nothing.
//!
//! ```text
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux LD_LIBRARY_PATH=$FCSDK_DIR/lib
//! FXCM_OFFERS_OUT=<file> cargo test -p vike-fxcm --features fxcm --test fxcm_classes_smoke -- --ignored --nocapture
//! ```
//!
//! ⚠ It opens its OWN reconcile session (a second login beside the daemon's): see `fxcm_currency_smoke.rs`. Triple-gated
//! like the other `*_smoke.rs`: `#[ignore]`, self-skips with no demo fxcm login, self-skips when no shim loaded.
//!
//! It writes the raw table the shim returned (one JSON array, no account id in it) to `FXCM_OFFERS_OUT` when that is
//! set: that file is the capture the fixture `tests/fixtures/offers_classes.json` is cut from. It then prints one line
//! per instrument TYPE (count, subscription statuses, whether the row carries the terms) so a run says at a glance
//! whether the installed shim sends the second half of a row.

use std::collections::BTreeMap;
use std::time::Duration;

use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_fxcm::{FxcmExecutionClient, recon_client_for};
use vike_model::OrderRequest;
use vike_model::events::Event;

mod common;

/// The registry label this smoke registers its session under (not a label an operator can write).
const ACCOUNT: &str = "classes-smoke";

#[test]
#[ignore = "live demo; needs the ForexConnect SDK + a demo fxcm account's credentials — read-only, run manually (see module doc)"]
fn fxcm_offers_table_by_instrument_type() {
    vike_log::test_init();
    let Some(c) = common::demo_config() else {
        eprintln!("skip: no demo fxcm account credentials");
        return;
    };
    let Some(_client) = recon_client_for(&c, "EURUSD", ACCOUNT) else {
        assert!(
            !vike_fxcm::sdk_available(),
            "FXCM credentials are configured and the ForexConnect shim IS loaded, yet the reconcile client \
             could not be built — the demo login FAILED (re-run with --nocapture)"
        );
        eprintln!("skip: no ForexConnect shim on this box");
        return;
    };
    println!("LOGIN OK (reconcile session)");

    let reads = vike_fxcm::reads::account_reads(ACCOUNT).expect("registered");
    let raw = reads.offers_json().expect("an Offers table");
    if let Ok(path) = std::env::var("FXCM_OFFERS_OUT") {
        std::fs::write(&path, &raw).expect("write FXCM_OFFERS_OUT");
        println!("RAW TABLE: {} bytes written to {path}", raw.len());
    }
    let rows: serde_json::Value = serde_json::from_str(&raw).expect("the table is JSON");
    let rows = rows.as_array().expect("the table is an array");
    println!("Offers rows read: {}", rows.len());

    // type -> (rows, subscription statuses, rows that carry the terms)
    let mut by_type: BTreeMap<i64, (usize, BTreeMap<String, usize>, usize)> = BTreeMap::new();
    for r in rows {
        let t = r["instrument_type"].as_i64().unwrap_or(-1);
        let e = by_type.entry(t).or_default();
        e.0 += 1;
        *e.1.entry(r["subscription"].as_str().unwrap_or("?").to_string()).or_default() += 1;
        e.2 += usize::from(r.get("min_qty").is_some());
    }
    for (t, (n, statuses, with_terms)) in by_type {
        println!("type {t}: {n} rows, subscription {statuses:?}, {with_terms} carry the terms");
    }
    assert!(!rows.is_empty(), "an empty Offers table");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// One market order through the bridge's own exec client, and every event until the order ends.
fn order(
    client: &mut FxcmExecutionClient,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    symbol: &str,
    side: i32,
    qty: f64,
    reduce_only: bool,
) -> Vec<Event> {
    let coid = format!("fxcls{}", now_ms());
    client.submit(&OrderRequest {
        client_order_id: coid.clone(),
        venue: "fxcm".into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: vike_model::OrderType::Market,
        reduce_only,
        ts: now_ms(),
        ..Default::default()
    });
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let mut seen = Vec::new();
    loop {
        let next =
            rt.block_on(async { tokio::time::timeout(Duration::from_secs(60), rx.recv()).await });
        let ev = match next {
            Ok(Some(Ingest::Event(ev))) => ev,
            Ok(Some(other)) => panic!("expected Ingest::Event, got {other:?}"),
            Ok(None) => panic!("ingest channel closed"),
            Err(_) => panic!("no end to the order {coid} within 60 s; saw {seen:?}"),
        };
        let ended =
            matches!(ev, Event::OrderFilled(_) | Event::OrderCanceled(_) | Event::OrderRejected(_));
        seen.push(ev);
        if ended {
            return seen;
        }
    }
}

/// The first fill among `events`: `(price, quantity)`.
fn first_fill(events: &[Event]) -> Option<(f64, f64)> {
    events.iter().find_map(|e| match e {
        Event::Fill(f) => Some((f.last_px, f.last_qty)),
        _ => None,
    })
}

/// **The money line of one round trip**, machine-readable (the kit's `fxcmclasses-roundtrip.sh` greps it): the
/// closed trade's gross P/L predicted from the production facts (`(close - open) * qty * per_unit * rate`, `per_unit` the
/// contract multiplier and `rate` account-per-quote) against the account's balance move (this demo charges no commission
/// and the trade held nothing overnight). `MATCH` within 3 cents plus 5 percent; `MISMATCH` carries the ratio, which IS
/// the multiplier the engine should have used: the line says what to change, it never changes anything.
fn money_verdict(
    symbol: &str,
    qty: f64,
    facts: &vike_fxcm::home_conversion::ConvertedFacts,
    open: &[Event],
    close: &[Event],
    before: Option<f64>,
    after: Option<f64>,
) -> String {
    let (Some((open_px, open_qty)), Some((close_px, close_qty))) =
        (first_fill(open), first_fill(close))
    else {
        return format!(
            "MONEY {symbol}: NOFILL (the open or the close did not fill; see the events above)"
        );
    };
    let (Some(before), Some(after)) = (before, after) else {
        return format!("MONEY {symbol}: NOBALANCE");
    };
    let per_unit = facts.contract_value.map_or(1.0, |c| c.per_unit);
    let rate = facts.quote.as_ref().map_or(1.0, |q| q.account_per_quote.get());
    let predicted = (close_px - open_px) * open_qty.min(close_qty) * per_unit * rate;
    let delta = after - before;
    let ratio = if predicted == 0.0 { f64::NAN } else { delta / predicted };
    let verdict = if (delta - predicted).abs() <= 0.03 + 0.05 * predicted.abs() {
        "MATCH"
    } else {
        "MISMATCH"
    };
    format!(
        "MONEY {symbol}: open {open_px} close {close_px} qty {qty} per_unit {per_unit} rate {rate} \
         predicted {predicted:.4} delta {delta:.4} ratio {ratio:.4} VERDICT {verdict}"
    )
}

/// **ONE minimum-size round trip per symbol through the bridge: what a contract of each class MEANS in money.**
/// For each symbol of `FXCM_CLASS_SYMBOLS` (comma separated, the engine's spelling: `US30,XAUUSD`): print the venue's
/// Offers row, buy the minimum size (`min_qty`, in base units), close it by trade id, and print both fills and the
/// account balance before and after. The balance delta is the closed trade's gross P/L in the ACCOUNT's currency
/// (this demo charges no commission), so `delta / ((close - open) * qty * rate native -> account)` is the
/// instrument's money factor per unit of `qty`: the figure `ContractMultiplier` is only a guess at.
///
/// **PLACES REAL ORDERS on the demo account**; it ends flat and refuses to start when the account holds a trade or a
/// resting order of the symbol. The market must be open. Run with the daemon's FXCM account stopped, or accept a third
/// session of the login (see the module doc).
#[test]
#[ignore = "live demo; PLACES REAL ORDERS on the demo account (one minimum-size round trip per symbol of FXCM_CLASS_SYMBOLS)"]
fn fxcm_one_minimum_round_trip_per_symbol() {
    vike_log::test_init();
    let Some(cfg) = common::demo_config() else {
        eprintln!("skip: no fxcm demo account credentials");
        return;
    };
    if !vike_fxcm::sdk_available() {
        eprintln!("skip: no ForexConnect shim on this box (NOT a pass)");
        return;
    }
    let symbols = std::env::var("FXCM_CLASS_SYMBOLS").unwrap_or_else(|_| "US30".into());
    for symbol in symbols.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let label = format!("classes-{symbol}");
        let recon = recon_client_for(&cfg, symbol, &label)
            .expect("the reconcile login failed (re-run with --nocapture)");
        println!("LOGIN OK (reconcile session) for {symbol}");
        let reads = vike_fxcm::reads::account_reads(&label).expect("registered");
        let table: serde_json::Value =
            serde_json::from_str(&reads.offers_json().expect("an Offers table")).unwrap();
        let canonical = |name: &str| name.replace('/', "").to_uppercase();
        let Some(row) =
            table.as_array().unwrap().iter().find(|r| {
                canonical(r["instrument"].as_str().unwrap_or("")) == symbol.to_uppercase()
            })
        else {
            println!("ROW {symbol}: not in the Offers table");
            continue;
        };
        println!("ROW {symbol}: {row}");
        // The PRODUCTION facts path (it subscribes the symbol, and its conversion pair, when FXCM holds them not
        // tradable): what the node would admit the symbol with. Its minimum is in BASE UNITS, the unit of `qty`.
        let facts = match vike_fxcm::reads::quoted_facts(&label, symbol) {
            Ok(facts) => facts,
            Err(why) => {
                println!("REFUSED {symbol}: {why}");
                continue;
            }
        };
        println!("FACTS {symbol}: {facts:?}");
        let qty = facts.properties.min_qty;
        assert!(recon.fetch_fill_reports(0).expect("trades").is_empty(), "{symbol} is held");
        assert!(
            recon.fetch_order_status_reports(0).expect("orders").is_empty(),
            "{symbol} has a resting order"
        );
        let (events, mut rx) = event_channel(256);
        let mut client = FxcmExecutionClient::spawn(cfg.clone(), events);
        let bal0 = recon.fetch_balance().ok().flatten();
        let open = order(&mut client, &mut rx, symbol, 1, qty, false);
        println!("OPEN {symbol} qty {qty}: {open:#?}");
        let close = order(&mut client, &mut rx, symbol, -1, qty, true);
        println!("CLOSE {symbol} qty {qty}: {close:#?}");
        let bal1 = recon.fetch_balance().ok().flatten();
        println!("BALANCE {symbol}: before {bal0:?} after {bal1:?}");
        println!("{}", money_verdict(symbol, qty, &facts, &open, &close, bal0, bal1));
        let mut left = recon.fetch_fill_reports(0).expect("trades");
        // A close that did not take is retried (by the path under test) before the account is judged.
        for _ in 0..3 {
            if left.is_empty() {
                break;
            }
            for f in &left {
                let again = order(&mut client, &mut rx, symbol, -f.side, f.last_qty, true);
                println!("CLEANUP {symbol}: {again:?}");
            }
            left = recon.fetch_fill_reports(0).expect("trades");
        }
        client.detach();
        // Whatever the steps said, the account must end flat in this symbol.
        assert!(left.is_empty(), "{symbol} is still held: {left:?}");
    }
    println!("FXCM CLASSES ROUND TRIPS DONE: flat");
}
