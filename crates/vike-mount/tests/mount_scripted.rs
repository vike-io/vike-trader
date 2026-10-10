//! OFFLINE end-to-end mount test: a SCRIPTED Polymarket-shaped 0–1 feed → the PRODUCTION live core
//! → the A-S [`vike_mm::SpreadMaker`] → the paper exchange, through the SAME
//! [`vike_mount::build_paper_maker_core`] + [`vike_mount::MakerSink`] the live bin uses. No
//! network, creds or `polymarket` feature.
//!
//! Proves: the maker QUOTES BOTH SIDES around the reservation price → the tick→bar synth
//! ([`vike_mount::TickBarSynthesizer`]) drives PAPER FILLS → INVENTORY is managed (a dip fills the
//! bid → long; a spike fills the ask → flat). Deterministic: the synth runs on event time and fills
//! only on bar closes, so the resting quote at each fill is the one the last tick set.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::wait_until;
use vike_data::LiveDataSink;
use vike_model::{BookLevel, EquitySample, L2Book, QuoteTick};
use vike_mount::{
    MakerMountConfig, MakerSink, PaperFill, PaperHalt, PaperMountOpts, build_paper_maker_core_with,
};

/// This binary's OWN operator-HALT sentinel: a path it NAMES and never CREATES.
///
/// ⚠ **Every mount below is HALT-armed by design, so without this these tests inherit the
/// operator's kill switch off the box.** `build_paper_maker_core` resolves the process-wide
/// sentinel (`<project>/settings/state/HALT`, else `<exe_dir>/HALT`); while it exists no quote
/// rests and the tests fail as "maker never posted a two-sided quote". MEASURED on the CI box with a
/// real file there: `scripted_ticks_drive_maker_quote_fill_and_inventory` and
/// `equity_sample_closure_fires_while_a_position_is_open` went red. Pinned, not `set_var`:
/// `halt_path_from_env` memoizes in a `OnceLock`.
///
/// ⚠ The caller must BIND the returned `tempfile::TempDir` for the test's duration
/// (`common::no_halt` carries why).
fn no_halt() -> (tempfile::TempDir, PaperHalt) {
    common::no_halt("vike-mount-mount-scripted-owns-this-halt-")
}

/// [`PaperMountOpts::default`] with this binary's own sentinel pinned (what
/// [`vike_mount::build_paper_maker_core`] gives, minus the inherited kill switch); bind the guard.
fn unhalted_opts() -> (tempfile::TempDir, PaperMountOpts) {
    let (root, halt) = no_halt();
    (root, PaperMountOpts { halt, ..Default::default() })
}

const VENUE: &str = "polymarket";
const TOKEN: &str = "SCRIPT_OUTCOME_TOKEN";
const INTERVAL_MS: i64 = 60_000;
const QTY: f64 = 20.0;

/// Far-future resolution: a positive A-S horizon and no near-resolution blackout.
const RESOLUTION_TS: i64 = 3_000_000_000;

/// [`wait_until`] that re-sends the SAME quote each poll, for anything on REAL wall-clock time
/// (the `vike_core::CoreConfig::equity_sample` timer): the core re-checks armed timers only when
/// the NEXT ingest arrives (no idle waker unless `submit_ack_timeout` is set too —
/// `CoreThread::run`'s "OS waker thread" doc), so a silent wait never sees the fire. `ts` grows
/// 1ms per poll, inside the synth window: no bar close, no extra fill.
fn poke_until(sink: &MakerSink, start_ts: i64, secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut ts = start_ts;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
        ts += 1;
        quote(sink, ts, 0.49, 0.51);
    }
}

/// One scripted 0–1 quote frame: drives the synth AND the maker's `on_quote_tick`, as the live
/// `MakerSink` forwards Polymarket's derived L1 quotes.
fn quote(sink: &MakerSink, ts: i64, bid: f64, ask: f64) {
    sink.quote(
        VENUE,
        TOKEN,
        QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: TOKEN.into(),
        },
    );
}

fn buys(fills: &Arc<Mutex<Vec<PaperFill>>>) -> Vec<PaperFill> {
    fills.lock().unwrap().iter().filter(|f| f.side > 0).cloned().collect()
}
fn sells(fills: &Arc<Mutex<Vec<PaperFill>>>) -> Vec<PaperFill> {
    fills.lock().unwrap().iter().filter(|f| f.side < 0).cloned().collect()
}
fn position(handle: &vike_core::CoreHandle) -> f64 {
    handle.snapshot().positions.iter().find(|p| p.symbol == TOKEN).map_or(0.0, |p| p.size)
}

#[test]
fn scripted_ticks_drive_maker_quote_fill_and_inventory() {
    vike_log::test_init();

    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    assert_eq!(cfg.qty, QTY);
    // The sentinel's owning temp directory, held for the whole test (see `no_halt`).
    let (_halt_root, opts) = unhalted_opts();
    let mount = build_paper_maker_core_with(&cfg, opts);
    let sink = MakerSink::new(&mount.handle, VENUE, TOKEN, cfg.interval.clone(), INTERVAL_MS);

    // ---- Phase 0: settle at mid 0.50 (window 0); the maker posts a two-sided quote ----
    quote(&sink, 1_000, 0.49, 0.51);
    quote(&sink, 2_000, 0.49, 0.51);
    quote(&sink, 3_000, 0.49, 0.51);

    assert!(
        wait_until(10, || {
            let s = mount.handle.snapshot();
            s.orders.iter().filter(|o| o.order_type == "limit").count() >= 2
        }),
        "maker never posted a two-sided quote"
    );
    // BOTH sides, straddling the reservation price (~mid 0.50) — "quotes both sides".
    let snap = mount.handle.snapshot();
    let bid = snap
        .orders
        .iter()
        .find(|o| o.side > 0 && o.order_type == "limit")
        .expect("a resting BID quote");
    let ask = snap
        .orders
        .iter()
        .find(|o| o.side < 0 && o.order_type == "limit")
        .expect("a resting ASK quote");
    let (bid_px, ask_px) = (bid.price.unwrap(), ask.price.unwrap());
    assert!(bid_px < ask_px, "bid {bid_px} must be below ask {ask_px}");
    assert!(bid_px < 0.50 && 0.50 < ask_px, "quotes must straddle the 0.50 mid: {bid_px}/{ask_px}");
    assert!((0.0..1.0).contains(&bid_px) && (0.0..1.0).contains(&ask_px), "0–1 bounded prices");
    assert!((bid.qty - QTY).abs() < 1e-9 && (ask.qty - QTY).abs() < 1e-9, "base quote size");

    // ---- Phase 1: window 1 dips to 0.40, recovers to 0.50 → the resting BID fills (long) ----
    quote(&sink, 61_000, 0.49, 0.51); // boundary: closes the flat window 0 (no fill); opens window 1
    quote(&sink, 62_000, 0.39, 0.41); // mid 0.40 — the dip
    quote(&sink, 63_000, 0.49, 0.51); // mid 0.50 — recovered; the resting bid is back to ~mid−spread
    quote(&sink, 121_000, 0.49, 0.51); // boundary: closes window 1 {O .50 H .50 L .40 C .50} → BID fills

    assert!(wait_until(10, || !buys(&mount.fills).is_empty()), "the resting bid never filled");
    let buy = buys(&mount.fills).remove(0);
    assert!(buy.px < 0.50, "a maker BUY fills below the mid: {}", buy.px);
    assert!((buy.qty - QTY).abs() < 1e-9, "buy fill size = base qty");
    assert!(wait_until(10, || position(&mount.handle) >= QTY - 1e-9), "inventory did not go long");

    // ---- Phase 2: window 2 spikes to 0.60, falls to 0.50 → the resting ASK fills (flat) ----
    quote(&sink, 122_000, 0.59, 0.61); // mid 0.60 — the spike
    quote(&sink, 123_000, 0.49, 0.51); // mid 0.50 — fell back
    quote(&sink, 181_000, 0.49, 0.51); // boundary: closes window 2 {O .50 H .60 L .50 C .50} → ASK fills

    assert!(wait_until(10, || !sells(&mount.fills).is_empty()), "the resting ask never filled");
    let sell = sells(&mount.fills).remove(0);
    assert!(sell.px > 0.50, "a maker SELL fills above the mid: {}", sell.px);
    assert!(sell.px > buy.px, "captured spread: sell {} > buy {}", sell.px, buy.px);
    assert!((sell.qty - QTY).abs() < 1e-9, "sell fill size = base qty");
    // inventory managed: the offsetting ask fill brought the position back to flat.
    assert!(
        wait_until(10, || position(&mount.handle).abs() < 1e-9),
        "inventory not returned to flat"
    );

    // Grab the snapshot cell BEFORE the consuming join, then read the final snapshot from it.
    sink.flush_bar();
    let cell = mount.handle.snapshot_cell();
    mount.handle.shutdown_and_join();
    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "core faulted: {:?}", snap.fault);
    assert!(
        snap.portfolio.realized_pnl > 0.0,
        "the round-trip (buy {} → sell {}) should realize a positive spread; got {}",
        buy.px,
        sell.px,
        snap.portfolio.realized_pnl
    );
    let (nb, ns) = (buys(&mount.fills).len(), sells(&mount.fills).len());
    println!(
        "mount ok: {nb} buy + {ns} sell paper fill(s); buy {} / sell {}; realized {}",
        buy.px, sell.px, snap.portfolio.realized_pnl
    );
}

/// The L2 book lane also drives the maker (`on_order_book`, same `requote`). Separate from the fill
/// flow so its wall-clock timestamp never perturbs the deterministic synth above.
#[test]
fn scripted_book_tick_also_makes_the_maker_quote() {
    vike_log::test_init();
    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    // The sentinel's owning temp directory, held for the whole test (see `no_halt`).
    let (_halt_root, opts) = unhalted_opts();
    let mount = build_paper_maker_core_with(&cfg, opts);
    let sink = MakerSink::new(&mount.handle, VENUE, TOKEN, cfg.interval.clone(), INTERVAL_MS);

    let mut book = L2Book::new(0.01);
    book.apply_snapshot(
        1,
        &[BookLevel::new(0.49, 100.0), BookLevel::new(0.48, 200.0)],
        &[BookLevel::new(0.51, 100.0), BookLevel::new(0.52, 200.0)],
    );
    sink.book(VENUE, TOKEN, Arc::new(book));

    assert!(
        wait_until(10, || {
            let s = mount.handle.snapshot();
            let bid = s.orders.iter().find(|o| o.side > 0 && o.order_type == "limit");
            let ask = s.orders.iter().find(|o| o.side < 0 && o.order_type == "limit");
            matches!((bid, ask), (Some(b), Some(a))
                if b.price.zip(a.price).is_some_and(|(bp, ap)| bp < ap && bp < 0.50 && 0.50 < ap))
        }),
        "the L2 book lane did not produce a two-sided quote"
    );
    mount.handle.shutdown_and_join();
}

/// [`build_paper_maker_core_with`]'s `PaperMountOpts::{equity_sample, on_equity_sample}` reach
/// `CoreConfig` in the SAME production mount and FIRE: a capturing closure, the dip-and-recover
/// sequence to open a long, then [`poke_until`] until a batch lands with a `"TOTAL"` row and a
/// per-venue row (the shape of vike-core's own sampler test).
#[test]
fn equity_sample_closure_fires_while_a_position_is_open() {
    vike_log::test_init();

    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    // The sentinel's owning temp directory, held for the whole test (see `no_halt`).
    let (_halt_root, halt) = no_halt();
    let mount = build_paper_maker_core_with(
        &cfg,
        PaperMountOpts {
            equity_sample: Some(Duration::from_millis(50)),
            on_equity_sample: Some(Box::new(move |rows: &[EquitySample]| {
                cap.lock().unwrap().extend_from_slice(rows)
            })),
            halt,
            ..Default::default()
        },
    );
    let sink = MakerSink::new(&mount.handle, VENUE, TOKEN, cfg.interval.clone(), INTERVAL_MS);

    // Phase 0: settle at mid 0.50 (window 0) so the maker posts a two-sided quote.
    quote(&sink, 1_000, 0.49, 0.51);
    quote(&sink, 2_000, 0.49, 0.51);
    quote(&sink, 3_000, 0.49, 0.51);
    assert!(
        wait_until(10, || {
            let s = mount.handle.snapshot();
            s.orders.iter().filter(|o| o.order_type == "limit").count() >= 2
        }),
        "maker never posted a two-sided quote"
    );

    // Phase 1: the dip fills the BID (long); an open position arms the sampler
    // (`any_position_open`).
    quote(&sink, 61_000, 0.49, 0.51); // boundary: closes the flat window 0 (no fill); opens window 1
    quote(&sink, 62_000, 0.39, 0.41); // mid 0.40 — the dip
    quote(&sink, 63_000, 0.49, 0.51); // mid 0.50 — recovered
    quote(&sink, 121_000, 0.49, 0.51); // boundary: closes window 1 -> BID fills
    assert!(wait_until(10, || position(&mount.handle) >= QTY - 1e-9), "inventory did not go long");

    // The sampler is armed now; poke the core (same resting price, no new fill) until a batch lands.
    assert!(
        poke_until(&sink, 121_000, 10, || !captured.lock().unwrap().is_empty()),
        "equity-sample closure never fired for an open position"
    );
    let rows = captured.lock().unwrap().clone();
    assert!(rows.iter().any(|r| r.venue == "TOTAL"), "batch must include a TOTAL row: {rows:?}");
    assert!(rows.iter().any(|r| r.venue == VENUE), "batch must include a per-venue row: {rows:?}");

    mount.handle.shutdown_and_join();
}

/// The verdict must not change under an engaged HALT sentinel
/// (`common::assert_indifferent_to_an_engaged_halt_sentinel`).
#[test]
fn the_mount_scripted_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
