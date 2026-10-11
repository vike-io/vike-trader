//! A PAGED tick read of a GROUPED series through the data daemon returns every row the store holds
//! — the regression test for
//! `docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md`.
//!
//! # Why this file exists
//!
//! From v0.1.34 a `RemoteHistStore` read of a grouped series (Polymarket's recorder families) lost
//! rows with NO error: a `TsRange::all()` read of a token absent from its group's first part came
//! back EMPTY, and a token's later rows went missing whenever another symbol traded after its last
//! row in a part. Every shape below was planted, run through the real pager on loopback and found
//! RED on `2ed3bb8eb` for quote, trade and book alike, while the three `ctrl_*` controls were green.
//! Two defects in the store, both in how a capped read was sized:
//!
//! * **(a) an EMPTY page while the range goes on.** A budget sized a block by its parts' row counts,
//!   and a grouped part counts EVERY symbol in it, so one block could hold none of the asked-for
//!   symbol's rows. The pager stops on the first empty page (`a*` shapes).
//! * **(b) two layouts, two cuts.** A symbol in BOTH its per-symbol series and a `group=` directory
//!   was narrowed in each separately, so the merged page was complete only up to the smaller cut,
//!   and the pager continued past the other layout's rows (`b*` shapes).
//!
//! # What the assertions are worth
//!
//! Each shape compares the PAGED wire answer with a direct UNBOUNDED scan of the same store, as a
//! multiset on `(ts, a per-row discriminator)` — so a missing row and a duplicated one are both
//! named — after first checking that the unbounded scan holds every row the shape PLANTED. That
//! first check is what makes a loss the pager's rather than the fixture's: the rows are on disk.
//! The page size is the client's real one (`RemoteHistStore`'s private `PAGE_ROWS`, restated here
//! as [`PAGE`]), because the hazard only exists for a group part at least one page long — a test
//! with an injected small page would prove the loop and say nothing about the constant every
//! caller gets. It is the grouped twin of `crates/vike-datahub/tests/wire_six_verbs_roundtrip.rs`'s
//! `a_scan_wider_than_one_page_comes_back_whole`.
//!
//! On a failure the quote pages are traced (start, row count, first and last `ts`, all relative to
//! [`T0`]), which is how the original reproduction localized both defects.
//!
//! Behind `serve-datafusion`, like its twin: the feature that makes `DataFusionHist` nameable here.
#![cfg(feature = "serve-datafusion")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_datahub_client::{DatahubClient, RemoteHistStore};
use vike_model::{BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

mod common;
use common::spawn_server;

const V: &str = "polymarket";
const G: &str = "fam";
/// Every planted row sits on ONE UTC date, so every commit below is exactly one manifest part.
const T0: i64 = 1_700_000_000_000;
/// `RemoteHistStore::PAGE_ROWS`, which is private; restated so the trace asks what the pager asks,
/// and so a "busy" group part below means one holding at least this many rows.
const PAGE: u32 = 10_000;

enum Part {
    /// One `append_*_grouped` commit into `group=fam`: every `(symbol, ts)` row in ONE part.
    Group(Vec<(&'static str, i64)>),
    /// One per-symbol `append_*` commit for `symbol`: one part.
    Series(&'static str, Vec<i64>),
}

fn quote(sym: &str, ts: i64, d: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: d as f64,
        ask: d as f64 + 0.5,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: sym.to_string(),
    }
}

fn trade(sym: &str, ts: i64, d: i64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: 0,
        price: d as f64,
        size: 1.0,
        is_buyer_maker: false,
        symbol: sym.to_string(),
    }
}

/// One bid and one ask level, so ONE event is TWO stored rows (the budget counts rows).
fn book(sym: &str, ts: i64, d: i64) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: 0,
        seq: d as u64,
        kind: BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: vec![BookLevel { price: d as f64, qty: 1.0 }],
        asks: vec![BookLevel { price: d as f64 + 0.5, qty: 1.0 }],
        symbol: sym.to_string(),
    }
}

/// Plant every part into all three grouped kinds. Each row gets a unique discriminator `d`, which
/// is what the comparison keys on besides `ts`.
fn plant(store: &DataFusionHist, parts: &[Part]) {
    let mut d: i64 = 0;
    for (i, part) in parts.iter().enumerate() {
        let key = format!("part{i}");
        let rows: Vec<(&str, i64)> = match part {
            Part::Group(rows) => rows.iter().map(|(s, ts)| (*s, *ts)).collect(),
            Part::Series(s, ts) => ts.iter().map(|t| (*s, *t)).collect(),
        };
        let (mut qs, mut trs, mut bs) = (Vec::new(), Vec::new(), Vec::new());
        for (s, ts) in &rows {
            d += 1;
            qs.push(quote(s, *ts, d));
            trs.push(trade(s, *ts, d));
            bs.push(book(s, *ts, d));
        }
        match part {
            Part::Group(_) => {
                assert_eq!(store.append_quotes_grouped(V, G, &qs, Some(&key)).unwrap(), qs.len());
                assert_eq!(store.append_trades_grouped(V, G, &trs, Some(&key)).unwrap(), trs.len());
                assert!(store.append_book_updates_grouped(V, G, &bs, Some(&key)).unwrap() > 0);
            }
            Part::Series(s, _) => {
                assert_eq!(store.append_quotes(V, s, &qs, Some(&key)).unwrap(), qs.len());
                assert_eq!(store.append_trades(V, s, &trs, Some(&key)).unwrap(), trs.len());
                assert!(store.append_book_updates(V, s, &bs, Some(&key)).unwrap() > 0);
            }
        }
    }
}

fn in_range(ts: i64, r: TsRange) -> bool {
    r.start.is_none_or(|s| ts >= s) && r.end.is_none_or(|e| ts <= e)
}

/// How many rows of `sym` the plan put inside `range` — the unbounded scan must hand back exactly
/// this many, which is the proof the rows are ON DISK and only the paged read can lose them.
fn planted(parts: &[Part], sym: &str, range: TsRange) -> usize {
    parts
        .iter()
        .map(|p| match p {
            Part::Group(rows) => {
                rows.iter().filter(|(s, ts)| *s == sym && in_range(*ts, range)).count()
            }
            Part::Series(s, ts) if *s == sym => ts.iter().filter(|t| in_range(**t, range)).count(),
            Part::Series(..) => 0,
        })
        .sum()
}

struct Outcome {
    kind: &'static str,
    local: usize,
    wire: usize,
    missing: Vec<i64>,
    extra: Vec<i64>,
    ascending: bool,
}

/// Multiset difference on `(ts, discriminator)`: what the unbounded scan holds and the wire did
/// not return (`missing`), and the reverse (`extra`, which would mean a row came back twice).
fn outcome(kind: &'static str, local: Vec<(i64, u64)>, wire: Vec<(i64, u64)>) -> Outcome {
    let ascending = wire.windows(2).all(|w| w[0].0 <= w[1].0);
    let mut counts: BTreeMap<(i64, u64), i64> = BTreeMap::new();
    for k in &local {
        *counts.entry(*k).or_default() += 1;
    }
    for k in &wire {
        *counts.entry(*k).or_default() -= 1;
    }
    let (mut missing, mut extra) = (Vec::new(), Vec::new());
    for ((ts, _), c) in counts {
        for _ in 0..c.max(0) {
            missing.push(ts - T0);
        }
        for _ in 0..(-c).max(0) {
            extra.push(ts - T0);
        }
    }
    Outcome { kind, local: local.len(), wire: wire.len(), missing, extra, ascending }
}

/// The pager's own loop (`RemoteHistStore::paged`), spelled over a raw client so every page is
/// visible: start, row count, first and last `ts` (all relative to `T0`).
fn trace_quote_pages(addr: SocketAddr, sym: &str, range: TsRange) -> Vec<String> {
    let mut c = DatahubClient::connect(addr).expect("connect");
    let mut lines = Vec::new();
    let mut start = range.start;
    for _ in 0..64 {
        let page =
            c.scan_quotes(V, sym, TsRange { start, end: range.end }, Some(PAGE)).expect("page");
        let first = page.first().map(|r| r.ts - T0);
        let last = page.last().map(|r| r.ts);
        lines.push(format!(
            "    page from {:?}: {} rows, ts {:?}..{:?}",
            start.map(|s| s - T0),
            page.len(),
            first,
            last.map(|t| t - T0)
        ));
        let Some(last) = last else { break };
        if range.end.is_some_and(|e| last >= e) {
            break;
        }
        start = Some(last + 1);
    }
    lines
}

fn span(v: &[i64]) -> String {
    match (v.first(), v.last()) {
        (Some(a), Some(b)) => format!("{a}..{b}"),
        _ => "-".to_string(),
    }
}

/// Plant `parts`, read `sym` over `range` locally (unbounded) and over the wire (paged), print the
/// verdict per kind, and fail if any kind lost or duplicated a row or came back out of `ts` order.
fn run_shape(name: &str, parts: Vec<Part>, sym: &str, range: TsRange) {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = DataFusionHist::open(dir.path()).expect("open store");
    plant(&store, &parts);
    let expected = planted(&parts, sym, range);
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    let addr = spawn_server(store.clone());
    let remote = RemoteHistStore::new(addr.to_string());

    let mut outs = Vec::new();
    let local = store.scan_quotes(V, sym, range).unwrap();
    let wire = remote.scan_quotes(V, sym, range).unwrap();
    outs.push(outcome(
        "quote",
        local.iter().map(|r| (r.ts, r.bid.to_bits())).collect(),
        wire.iter().map(|r| (r.ts, r.bid.to_bits())).collect(),
    ));
    let local = store.scan_trades(V, sym, range).unwrap();
    let wire = remote.scan_trades(V, sym, range).unwrap();
    outs.push(outcome(
        "trade",
        local.iter().map(|r| (r.ts, r.price.to_bits())).collect(),
        wire.iter().map(|r| (r.ts, r.price.to_bits())).collect(),
    ));
    let local = store.scan_book_updates(V, sym, range).unwrap();
    let wire = remote.scan_book_updates(V, sym, range).unwrap();
    outs.push(outcome(
        "book",
        local.iter().map(|r| (r.ts, r.seq)).collect(),
        wire.iter().map(|r| (r.ts, r.seq)).collect(),
    ));

    println!("SHAPE {name} (symbol {sym}, planted {expected})");
    let mut bad = Vec::new();
    for o in &outs {
        let whole = o.missing.is_empty() && o.extra.is_empty() && o.ascending;
        println!(
            "  {:5} {:5} local={} wire={} missing={} [{}] extra={} [{}] ascending={}",
            if whole { "GREEN" } else { "RED" },
            o.kind,
            o.local,
            o.wire,
            o.missing.len(),
            span(&o.missing),
            o.extra.len(),
            span(&o.extra),
            o.ascending
        );
        assert_eq!(
            o.local, expected,
            "{name}/{}: the unbounded scan holds every planted row, or this shape proves nothing",
            o.kind
        );
        if !whole {
            bad.push(format!(
                "{}: {} missing, {} extra, ts-ascending {}",
                o.kind,
                o.missing.len(),
                o.extra.len(),
                o.ascending
            ));
        }
    }
    if !bad.is_empty() {
        for line in trace_quote_pages(addr, sym, range) {
            println!("{line}");
        }
    }
    assert!(bad.is_empty(), "{name}: the paged read differs from the unbounded scan: {bad:?}");
}

fn rows(sym: &'static str, ts: impl Iterator<Item = i64>) -> Vec<(&'static str, i64)> {
    ts.map(|t| (sym, T0 + t)).collect()
}

fn cat(parts: Vec<Vec<(&'static str, i64)>>) -> Vec<(&'static str, i64)> {
    parts.into_iter().flatten().collect()
}

// ---- controls: green before the fix and after it -----------------------------------------------

/// Per-symbol layout only, three parts, ties: two rows per `ts` in part 1, and part 2 starting ON
/// part 1's last `ts` (a straddle). A per-symbol part holds only this symbol, so a part that
/// overlaps the next request always has a row of it inside the answered span.
#[test]
fn ctrl_series_only_three_parts_with_ties() {
    let parts = vec![
        Part::Series("A", (0..12_000).map(|i| T0 + i / 2).collect()),
        Part::Series(
            "A",
            std::iter::once(T0 + 5_999).chain((6_000..18_000).map(|t| T0 + t)).collect(),
        ),
        Part::Series("A", (30_000..30_100).map(|t| T0 + t).collect()),
    ];
    run_shape("ctrl_series_only_three_parts_with_ties", parts, "A", TsRange::all());
}

/// A busy group in which the asked-for symbol owns the LAST row of every part: the next request
/// starts past the part's `ts_max`, so the part is not selected again.
#[test]
fn ctrl_group_symbol_owns_every_part_end() {
    let parts = vec![
        Part::Group(cat(vec![
            rows("B", 0..12_000),
            rows("A", 5_000..5_100),
            rows("A", 12_000..12_001),
        ])),
        Part::Group(cat(vec![
            rows("B", 20_000..32_000),
            rows("A", 25_000..25_100),
            rows("A", 32_000..32_001),
        ])),
    ];
    run_shape("ctrl_group_symbol_owns_every_part_end", parts, "A", TsRange::all());
}

/// A tie at the part end: the symbol's last row shares the part's `ts_max` with another symbol's.
/// The condition for loss (a) is a foreign row STRICTLY after the symbol's last row.
#[test]
fn ctrl_a_tie_at_the_part_end_is_safe() {
    let parts = vec![
        Part::Group(cat(vec![rows("B", 0..12_000), rows("A", 11_990..12_000)])),
        Part::Group(cat(vec![rows("B", 20_000..20_100), rows("A", 20_000..20_010)])),
    ];
    run_shape("ctrl_a_tie_at_the_part_end_is_safe", parts, "A", TsRange::all());
}

// ---- defect (a): an empty page ended the scan ---------------------------------------------------

/// The symbol first appears in the group's SECOND part; the first part is another symbol's alone.
/// `TsRange::all()` — what the `cheap_np_*` bins send. Lost before the fix: everything.
#[test]
fn a1_symbol_listed_after_the_groups_first_part_all() {
    let parts = vec![
        Part::Group(rows("B", 0..12_000)),
        Part::Group(cat(vec![rows("A", 20_000..20_050), rows("B", 20_000..20_050)])),
    ];
    run_shape("a1_symbol_listed_after_the_groups_first_part_all", parts, "A", TsRange::all());
}

/// The same layout over a BOUNDED range that starts before the symbol exists — what a backtest's
/// `cfg.range` sends. Lost before the fix: everything.
#[test]
fn a2_symbol_listed_after_the_groups_first_part_bounded() {
    let parts = vec![
        Part::Group(rows("B", 0..12_000)),
        Part::Group(cat(vec![rows("A", 20_000..20_050), rows("B", 20_000..20_050)])),
    ];
    let range = TsRange::of(T0, T0 + 60_000);
    run_shape("a2_symbol_listed_after_the_groups_first_part_bounded", parts, "A", range);
}

/// A sparse symbol in a busy group, present in all three parts but never last in one. Lost before
/// the fix: parts 2 and 3.
#[test]
fn a3_sparse_symbol_in_a_busy_group() {
    let parts = vec![
        Part::Group(cat(vec![rows("B", 0..12_000), rows("A", 100..110)])),
        Part::Group(cat(vec![rows("B", 20_000..32_000), rows("A", 20_100..20_110)])),
        Part::Group(cat(vec![rows("B", 40_000..40_100), rows("A", 40_100..40_110)])),
    ];
    run_shape("a3_sparse_symbol_in_a_busy_group", parts, "A", TsRange::all());
}

/// A DENSE symbol (more than a page in its first part, two rows per `ts`) — being busy itself does
/// not help when another symbol trades after its last row in the part. Lost before the fix: part 2.
#[test]
fn a4_dense_symbol_in_a_busy_group() {
    let parts = vec![
        Part::Group(cat(vec![
            rows("B", 0..12_000),
            (0..15_000).map(|i| ("A", T0 + i / 2)).collect(),
        ])),
        Part::Group(cat(vec![rows("B", 20_000..20_100), rows("A", 20_000..20_100)])),
    ];
    run_shape("a4_dense_symbol_in_a_busy_group", parts, "A", TsRange::all());
}

/// The MINIMAL shape: one row of A inside a 10,000-row part of B, one row of A in the next part.
/// Lost before the fix: the second row.
#[test]
fn a5_minimal_one_foreign_row_after_the_symbol() {
    let parts = vec![
        Part::Group(cat(vec![rows("B", 0..10_000), rows("A", 5_000..5_001)])),
        Part::Group(rows("A", 20_000..20_001)),
    ];
    run_shape("a5_minimal_one_foreign_row_after_the_symbol", parts, "A", TsRange::all());
}

// ---- defect (b): the per-symbol series and the group were clamped separately -------------------

/// The per-symbol series is small (no clamp); the group is busy (clamped at its second part). The
/// first page held a per-symbol row PAST the group's clamp, so the pager continued beyond group
/// rows it was never sent. Lost before the fix: the group's rows past its clamp.
#[test]
fn b1_series_unclamped_group_clamped() {
    let parts = vec![
        Part::Series("A", vec![T0 + 100, T0 + 50_000]),
        Part::Group(cat(vec![rows("B", 0..12_000), rows("A", 10..11), rows("A", 20..21)])),
        Part::Group(cat(vec![rows("B", 30_000..30_100), rows("A", 30_000..30_011)])),
    ];
    run_shape("b1_series_unclamped_group_clamped", parts, "A", TsRange::all());
}

/// The mirror: the per-symbol series is clamped, the group is not. Lost before the fix: the
/// series' rows past its clamp.
#[test]
fn b2_group_unclamped_series_clamped() {
    let parts = vec![
        Part::Series("A", (0..12_000).map(|t| T0 + t).collect()),
        Part::Series("A", (40_000..40_010).map(|t| T0 + t).collect()),
        Part::Group(cat(vec![rows("A", 50_000..50_005), rows("B", 50_000..50_005)])),
    ];
    run_shape("b2_group_unclamped_series_clamped", parts, "A", TsRange::all());
}

/// Localisation: the empty answer was the STORE's, not the server's frame cap. Same layout as
/// `a1`, asked straight of the trait with the page budget — and since the fix, answered with at
/// least a page of rows or the whole range, which for 50 rows is all of them.
#[test]
fn a1_store_level_the_capped_scan_itself_answers() {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = DataFusionHist::open(dir.path()).expect("open store");
    plant(
        &store,
        &[
            Part::Group(rows("B", 0..12_000)),
            Part::Group(cat(vec![rows("A", 20_000..20_050), rows("B", 20_000..20_050)])),
        ],
    );
    let all = store.scan_quotes(V, "A", TsRange::all()).unwrap();
    let capped = store.scan_quotes_capped(V, "A", TsRange::all(), Some(PAGE as usize)).unwrap();
    println!("STORE a1: scan_quotes={} scan_quotes_capped({PAGE})={}", all.len(), capped.len());
    assert_eq!(all.len(), 50);
    assert_eq!(
        capped, all,
        "a capped store scan holds AT LEAST its budget's rows unless it is the whole range — here \
         the whole range, all 50 rows, which the unfixed store answered with NONE"
    );
}
