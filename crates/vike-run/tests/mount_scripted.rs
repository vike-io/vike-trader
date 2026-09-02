//! OFFLINE end-to-end mount test: a SCRIPTED Polymarket-shaped tick feed → the PRODUCTION live core
//! → the A-S [`vike_mm::SpreadMaker`] → the paper exchange. NO network, NO creds, NO geo access, NO
//! `polymarket` feature — it drives the SAME [`vike_run::build_paper_maker_core`] + [`vike_run::MakerSink`]
//! the live bin uses, differing only in that the feed is a scripted 0–1 quote stream instead of the
//! real `Feeds`.
//!
//! It proves the full chain: scripted 0–1 book/quote ticks → the maker QUOTES BOTH SIDES around the
//! A-S reservation price → the tick→bar synth ([`vike_run::TickBarSynthesizer`]) drives PAPER FILLS →
//! INVENTORY is managed (a dip-and-recover fills the resting bid → long; a spike-and-fall fills the
//! resting ask → back to flat). Deterministic: the synth is event-time-driven off the scripted tick
//! timestamps, and fills happen only on synth-bar closes (never mid-window), so the resting quote at
//! each fill is exactly the one the last processed tick set.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_model::{EquitySample, L2Book, QuoteTick};
use vike_run::{
    build_paper_maker_core_with, MakerMountConfig, MakerSink, PaperFill, PaperHalt, PaperMountOpts,
};

/// This binary's OWN operator-HALT sentinel: a path it NAMES and never CREATES.
///
/// ⚠ **Every mount below is HALT-armed by design, so without this these tests inherit the
/// operator's kill switch off the box that runs them.** `build_paper_maker_core` resolves the
/// process-wide sentinel (`VIKE_HALT_FILE`, else `<project>/settings/state/HALT`, else
/// `<exe_dir>/HALT`); while that file exists the paper book refuses every opening submit, so the
/// maker's quotes never rest and these tests fail with "maker never posted a two-sided quote" —
/// a message that names nothing to do with halt. MEASURED on the CI box: with `VIKE_HALT_FILE` pointing
/// at a real file, `scripted_ticks_drive_maker_quote_fill_and_inventory` and
/// `equity_sample_closure_fires_while_a_position_is_open` both went red; without it, green. CI is
/// green only because no runner happens to have the file.
///
/// Pinning is the cure and not `set_var`: `halt_path_from_env` memoizes in a `OnceLock`, and this
/// workspace does not mutate the environment under threads.
///
/// ⚠ The sentinel is a child of a `tempfile::TempDir` returned ALONGSIDE it, and the caller must
/// BIND that guard for the test's duration. The name used to be
/// `env::temp_dir().join(format!("vike-run-mount-scripted-owns-this-halt-{pid}"))`, and while
/// nothing ever created it — so it leaked nothing — "this path does not exist" was a claim about
/// everything else on a shared `/tmp`, not about this run. A freshly-minted random root makes it a
/// property of THIS run: no stale entry can inhabit it, so the mounts below cannot inherit a kill
/// switch from a leftover, and the guard removes the root it created.
fn no_halt() -> (tempfile::TempDir, PaperHalt) {
    let root = tempfile::Builder::new()
        .prefix("vike-run-mount-scripted-owns-this-halt-")
        .tempdir()
        .expect("temp sentinel root");
    let sentinel = root.path().join("HALT");
    (root, PaperHalt::Pinned(sentinel))
}

/// [`PaperMountOpts::default`] with this binary's own sentinel pinned — what
/// [`vike_run::build_paper_maker_core`] would give, minus the inherited kill switch. The
/// [`no_halt`] guard rides along; bind it for the whole test.
fn unhalted_opts() -> (tempfile::TempDir, PaperMountOpts) {
    let (root, halt) = no_halt();
    (root, PaperMountOpts { halt, ..Default::default() })
}

const VENUE: &str = "polymarket";
const TOKEN: &str = "SCRIPT_OUTCOME_TOKEN";
const INTERVAL_MS: i64 = 60_000;
const QTY: f64 = 20.0;

/// Far-future resolution so the A-S horizon is positive and the near-resolution blackout never
/// engages (the maker quotes normally, not the maximally-wide blackout stance).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// Poll `cond` up to `secs`, returning whether it became true (the core folds on its own thread; the
/// scripted sends are lossless+ordered, so this is just waiting for the coalesced snapshot/fill).
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Like [`wait_until`], but also re-sends the SAME resting quote on every poll — needed for
/// anything gated on REAL wall-clock time, such as the equity sampler's
/// `vike_core::CoreConfig::equity_sample` timer (portfolio-observer PR-3 T6). The core thread only
/// re-checks its armed timers at the drain-loop boundary reached when the NEXT ingest message
/// arrives (there is no idle waker unless `submit_ack_timeout` is ALSO set — see
/// `CoreThread::run`'s "OS waker thread" doc), so a silent [`wait_until`] would never observe a
/// wall-clock-driven fire; this nudges the core instead. `ts` is bumped by 1ms per poll, staying
/// well inside the current synth window for any realistic `secs` budget here — no bar close, no
/// extra fill, the resting quote is unchanged.
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

/// Send one scripted 0–1 quote frame (mid = (bid+ask)/2). Drives the synth AND the maker's
/// `on_quote_tick`, exactly as the live `MakerSink` forwards Polymarket's derived L1 quotes.
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

    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    assert_eq!(cfg.qty, QTY);
    // `_halt_root` is the sentinel's owning temp directory — held for the whole test (see
    // `no_halt`), removed when it drops.
    let (_halt_root, opts) = unhalted_opts();
    let mount = build_paper_maker_core_with(&cfg, opts);
    let sink = MakerSink::new(&mount.handle, VENUE, TOKEN, cfg.interval.clone(), INTERVAL_MS);

    // ── Phase 0: settle at mid 0.50 (window 0). The maker submits then modifies a two-sided quote. ──
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

    // ── Phase 1: window 1 dips to 0.40 then recovers to 0.50 → the resting BID fills (long). ──
    quote(&sink, 61_000, 0.49, 0.51); // boundary: closes the flat window 0 (no fill); opens window 1
    quote(&sink, 62_000, 0.39, 0.41); // mid 0.40 — the dip
    quote(&sink, 63_000, 0.49, 0.51); // mid 0.50 — recovered; the resting bid is back to ~mid−spread
    quote(&sink, 121_000, 0.49, 0.51); // boundary: closes window 1 {O .50 H .50 L .40 C .50} → BID fills

    assert!(wait_until(10, || !buys(&mount.fills).is_empty()), "the resting bid never filled");
    let buy = buys(&mount.fills).remove(0);
    assert!(buy.px < 0.50, "a maker BUY fills below the mid: {}", buy.px);
    assert!((buy.qty - QTY).abs() < 1e-9, "buy fill size = base qty");
    assert!(wait_until(10, || position(&mount.handle) >= QTY - 1e-9), "inventory did not go long");

    // ── Phase 2: window 2 spikes to 0.60 then falls to 0.50 → the resting ASK fills (back to flat). ──
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

    // Teardown + final invariants. Grab the snapshot cell BEFORE the consuming join, then read the
    // final published snapshot from it (mirrors the r7 gate).
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

/// The L2 book lane also drives the maker: a scripted 0–1 `L2Book` makes `on_order_book` post a
/// two-sided quote (the same `requote` the quote lane uses). Kept separate from the fill flow so its
/// wall-clock book-lane timestamp never perturbs the deterministic synth above.
#[test]
fn scripted_book_tick_also_makes_the_maker_quote() {
    vike_log::test_init();
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    // The sentinel's owning temp directory, held for the whole test (see `no_halt`).
    let (_halt_root, opts) = unhalted_opts();
    let mount = build_paper_maker_core_with(&cfg, opts);
    let sink = MakerSink::new(&mount.handle, VENUE, TOKEN, cfg.interval.clone(), INTERVAL_MS);

    let mut book = L2Book::new(0.01);
    book.apply_snapshot(1, &[(0.49, 100.0), (0.48, 200.0)], &[(0.51, 100.0), (0.52, 200.0)]);
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

/// Portfolio-observer PR-3 T6 (via the audit-F12 options bag): [`build_paper_maker_core_with`]'s
/// `PaperMountOpts::{equity_sample, on_equity_sample}` wire
/// `CoreConfig::equity_sample`/`on_equity_sample` into the SAME production mount the two tests
/// above drive — no real `RecorderSink`/store, just a capturing closure (an
/// `Arc<Mutex<Vec<EquitySample>>>`), proving the composition compiles AND fires end-to-end. Drives
/// the identical dip-and-recover opening sequence `scripted_ticks_drive_maker_quote_fill_and_
/// inventory` uses to get a resting bid filled (long), then [`poke_until`]s the core (the sampler
/// fires on REAL wall-clock cadence, but the core only re-checks armed timers at the next ingest
/// boundary — see that helper's doc) until a sample batch lands, and asserts its shape matches
/// vike-core's own T4 sampler test: a `"TOTAL"` row plus at least one per-venue row.
#[test]
fn equity_sample_closure_fires_while_a_position_is_open() {
    vike_log::test_init();

    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
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

    // Phase 1: window 1 dips to 0.40 then recovers to 0.50 -> the resting BID fills (long) -- a
    // position is now open, which is what arms the equity sampler (`any_position_open`).
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

/// This binary's verdict must not change when an operator HALT sentinel is engaged —
/// `common::assert_indifferent_to_an_engaged_halt_sentinel` carries the argument and the mechanism.
#[test]
fn the_mount_scripted_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
