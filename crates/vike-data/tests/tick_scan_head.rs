//! The four capped TICK reads on the real DataFusion backend — `scan_quotes_capped`,
//! `scan_trades_capped`, `scan_book_updates_capped` (and through it the shared walk
//! `scan_depth_capped` also takes) — answer a COMPLETE PREFIX of their unbudgeted read holding AT
//! LEAST `budget` rows unless it is the whole range, over a symbol stored in a per-symbol series,
//! a `group=` directory, or BOTH.
//!
//! # Why this file exists
//!
//! A paged datahub read of a grouped series lost rows in silence from v0.1.34
//! (`docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md`): a grouped part's
//! row count counts every symbol in it, so one budgeted block could hold none of the asked-for
//! symbol's rows while the range went on — and the pager stops on an empty page; and the
//! per-symbol series and each group were narrowed SEPARATELY, so the merged answer was complete
//! only to the smaller of their cuts. `crates/vike-datahub/tests/grouped_tick_paging.rs` is that
//! reproduction over the wire; this file is the STORE's half, held to the contract directly:
//!
//! 1. **It meets the contract, shape by shape.** [`judge`] compares each capped answer with what
//!    the range holds — taken from the PLAN, not from a read through the code under test — over
//!    every shape of the reproduction (scaled down: a budget is a number, so a "busy" part here is
//!    one holding more rows than the budgets below), a grid of budgets, and ranges that start
//!    before the symbol, mid-part, and on a timestamp two layouts or two parts share. The `b*` and
//!    `union_*` shapes are the union case: one layout narrowed on its own answers to its own cut,
//!    and only ONE budget over both answers a prefix.
//! 2. **It is still a BOUNDED read.**
//!    [`a_capped_read_never_opens_the_groups_last_part_it_does_not_need`] spoils the group's last
//!    part: a whole-range read now fails, and a capped read the earlier parts can fill still
//!    answers. A fix that "works" by reading everything — dropping the budget for a grouped read —
//!    returns the same rows and fails only here.
//!
//! The judge compares rows as a MULTISET on `(ts, a per-row discriminator)` below each cut, not as
//! a sequence: what the defect loses is rows, and the order of one timestamp's rows across row
//! groups is not what this file is about (`crates/vike-data/tests/bars_head.rs` pins the order half
//! for the walk both families share).
//!
//! Only compiled/run with `--features hist-datafusion`, like `tests/bars_head.rs`.
#![cfg(feature = "hist-datafusion")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::{BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

const V: &str = "polymarket";
const G: &str = "fam";
/// 2023-11-14T22:13:20Z. Every shape's rows sit within a second of it, so on ONE UTC date, and each
/// commit below is exactly one manifest part.
const T0: i64 = 1_700_000_000_000;
const DAY_MS: i64 = 86_400_000;

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

/// One bid and one ask level, so ONE event is TWO stored rows — the budget counts rows, and the
/// judge counts them the same way ([`Kind::Book`]).
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

/// One planted row: its symbol, its `ts` and its discriminator `d`.
type Planted = (&'static str, i64, i64);

/// Plant every part into all three grouped kinds, each row with a unique discriminator `d`, and
/// return what was planted — the judge's oracle, which owes nothing to the code under test.
fn plant(store: &DataFusionHist, parts: &[Part]) -> Vec<Planted> {
    let mut planted = Vec::new();
    let mut d: i64 = 0;
    for (i, part) in parts.iter().enumerate() {
        let key = format!("part{i}");
        let rows: Vec<(&'static str, i64)> = match part {
            Part::Group(rows) => rows.clone(),
            Part::Series(s, ts) => ts.iter().map(|t| (*s, *t)).collect(),
        };
        let (mut qs, mut trs, mut bs) = (Vec::new(), Vec::new(), Vec::new());
        for (s, ts) in &rows {
            d += 1;
            planted.push((*s, *ts, d));
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
    planted
}

fn rows(sym: &'static str, ts: impl Iterator<Item = i64>) -> Vec<(&'static str, i64)> {
    ts.map(|t| (sym, T0 + t)).collect()
}

fn cat(parts: Vec<Vec<(&'static str, i64)>>) -> Vec<(&'static str, i64)> {
    parts.into_iter().flatten().collect()
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Quote,
    Trade,
    Book,
}

/// One answered row as the judge sees it: `ts`, a discriminator unique to the planted row, and how
/// many STORED rows it stands for (a book event is one per level; a tick is one).
type Row = (i64, u64, usize);

fn read(store: &DataFusionHist, kind: Kind, range: TsRange, budget: Option<usize>) -> Vec<Row> {
    match kind {
        Kind::Quote => store
            .scan_quotes_capped(V, "A", range, budget)
            .unwrap()
            .iter()
            .map(|r| (r.ts, r.bid.to_bits(), 1))
            .collect(),
        Kind::Trade => store
            .scan_trades_capped(V, "A", range, budget)
            .unwrap()
            .iter()
            .map(|r| (r.ts, r.price.to_bits(), 1))
            .collect(),
        Kind::Book => store
            .scan_book_updates_capped(V, "A", range, budget)
            .unwrap()
            .iter()
            .map(|r| (r.ts, r.seq, r.bids.len() + r.asks.len()))
            .collect(),
    }
}

/// What the store must hold for symbol `A` in `range`, as `kind` reads it — straight from the plan.
fn expected(planted: &[Planted], kind: Kind, range: TsRange) -> Vec<Row> {
    let mut out: Vec<Row> = planted
        .iter()
        .filter(|(s, ts, _)| {
            *s == "A"
                && range.start.is_none_or(|lo| *ts >= lo)
                && range.end.is_none_or(|hi| *ts <= hi)
        })
        .map(|(_, ts, d)| match kind {
            Kind::Quote | Kind::Trade => (*ts, (*d as f64).to_bits(), 1),
            Kind::Book => (*ts, *d as u64, 2),
        })
        .collect();
    out.sort_by_key(|r| r.0);
    out
}

fn multiset(rows: &[Row]) -> BTreeMap<(i64, u64), usize> {
    let mut m = BTreeMap::new();
    for (ts, d, _) in rows {
        *m.entry((*ts, *d)).or_default() += 1;
    }
    m
}

/// The contract, judged against `full`, everything the range holds — [`expected`] from the plan,
/// never a read through the code under test, so a defect in the walk cannot be repeated in the thing
/// that judges it.
fn judge(at: &str, capped: &[Row], full: &[Row], budget: usize) {
    assert!(capped.windows(2).all(|w| w[0].0 <= w[1].0), "{at}: a capped answer is ts-ascending");
    // COMPLETE: below its own last `ts`, the capped answer holds exactly what the whole read holds.
    let below: Vec<Row> = match capped.last() {
        Some(last) => full.iter().copied().filter(|r| r.0 <= last.0).collect(),
        None => Vec::new(),
    };
    assert_eq!(
        multiset(capped),
        multiset(&below),
        "{at}: COMPLETE — the capped answer must be every row of the range up to its own last ts; \
         a gap inside it, or a ts split at its end, is lost by a pager in silence"
    );
    // AT LEAST `budget` stored rows, unless the answer is the whole range.
    let stored: usize = capped.iter().map(|r| r.2).sum();
    assert!(
        stored >= budget || multiset(capped) == multiset(full),
        "{at}: AT LEAST the budget — {stored} stored rows ({} answered of {}) while the range goes \
         on; a pager reads a short or EMPTY answer as the end",
        capped.len(),
        full.len()
    );
}

/// Budgets around the scaled "busy" part size (120 rows) and well past it.
const BUDGETS: [usize; 13] = [1, 2, 3, 7, 10, 50, 100, 119, 120, 121, 200, 241, 1_000];

/// Ranges the grid asks about, relative to [`T0`]: the whole range, one starting BEFORE every row,
/// mid-part, on a part's last `ts` (where shapes put ties), at a later part's start, a bounded one,
/// and an end-only one.
fn ranges() -> Vec<TsRange> {
    vec![
        TsRange::all(),
        TsRange { start: Some(T0 - 1), end: None },
        TsRange { start: Some(T0 + 60), end: None },
        TsRange { start: Some(T0 + 119), end: None },
        TsRange { start: Some(T0 + 200), end: None },
        TsRange::of(T0 + 30, T0 + 330),
        TsRange { start: None, end: Some(T0 + 250) },
    ]
}

/// Every shape of the reproduction, scaled down, plus two whose layouts interleave part by part.
fn shapes() -> Vec<(&'static str, Vec<Part>)> {
    vec![
        (
            "ctrl_series_only_three_parts_with_ties",
            vec![
                Part::Series("A", (0..120).map(|i| T0 + i / 2).collect()),
                Part::Series(
                    "A",
                    std::iter::once(T0 + 59).chain((60..180).map(|t| T0 + t)).collect(),
                ),
                Part::Series("A", (300..301).map(|t| T0 + t).collect()),
            ],
        ),
        (
            "ctrl_group_symbol_owns_every_part_end",
            vec![
                Part::Group(cat(vec![rows("B", 0..120), rows("A", 50..51), rows("A", 120..121)])),
                Part::Group(cat(vec![
                    rows("B", 200..320),
                    rows("A", 250..251),
                    rows("A", 320..321),
                ])),
            ],
        ),
        (
            "ctrl_a_tie_at_the_part_end_is_safe",
            vec![
                Part::Group(cat(vec![rows("B", 0..120), rows("A", 110..120)])),
                Part::Group(cat(vec![rows("B", 200..210), rows("A", 200..210)])),
            ],
        ),
        (
            "a1_symbol_listed_after_the_groups_first_part",
            vec![
                Part::Group(rows("B", 0..120)),
                Part::Group(cat(vec![rows("A", 200..250), rows("B", 200..250)])),
            ],
        ),
        (
            "a3_sparse_symbol_in_a_busy_group",
            vec![
                Part::Group(cat(vec![rows("B", 0..120), rows("A", 10..11)])),
                Part::Group(cat(vec![rows("B", 200..320), rows("A", 201..202)])),
                Part::Group(cat(vec![rows("B", 400..401), rows("A", 401..402)])),
            ],
        ),
        (
            "a4_dense_symbol_in_a_busy_group",
            vec![
                Part::Group(cat(vec![
                    rows("B", 0..120),
                    (0..150).map(|i| ("A", T0 + i / 2)).collect(),
                ])),
                Part::Group(cat(vec![rows("B", 200..201), rows("A", 200..210)])),
            ],
        ),
        (
            "a5_minimal_one_foreign_row_after_the_symbol",
            vec![
                Part::Group(cat(vec![rows("B", 0..100), rows("A", 50..51)])),
                Part::Group(rows("A", 200..201)),
            ],
        ),
        (
            "b1_series_unclamped_group_clamped",
            vec![
                Part::Series("A", vec![T0 + 5, T0 + 500]),
                Part::Group(cat(vec![rows("B", 0..120), rows("A", 1..2), rows("A", 2..3)])),
                Part::Group(cat(vec![rows("B", 300..301), rows("A", 300..311)])),
            ],
        ),
        (
            "b2_group_unclamped_series_clamped",
            vec![
                Part::Series("A", (0..120).map(|t| T0 + t).collect()),
                Part::Series("A", (400..410).map(|t| T0 + t).collect()),
                Part::Group(cat(vec![rows("A", 500..505), rows("B", 500..505)])),
            ],
        ),
        (
            // The two layouts interleave part by part, so a budget run per layout cuts each at a
            // different place for almost every budget in the grid.
            "union_interleaved_layouts",
            vec![
                Part::Series("A", (0..10).map(|t| T0 + t).collect()),
                Part::Group(cat(vec![rows("B", 100..220), rows("A", 150..160)])),
                Part::Series("A", (200..210).map(|t| T0 + t).collect()),
                Part::Group(cat(vec![rows("B", 300..420), rows("A", 350..360)])),
                Part::Series("A", (400..410).map(|t| T0 + t).collect()),
            ],
        ),
        (
            // A timestamp held by BOTH layouts: the per-symbol series ends ON the group part's first
            // ts, and the group part straddles it, so no cut may separate the two rows at 120.
            "union_a_ts_shared_by_both_layouts",
            vec![
                Part::Series("A", (0..121).map(|t| T0 + t).collect()),
                Part::Group(cat(vec![rows("A", 120..140), rows("B", 120..260)])),
                Part::Series("A", (300..310).map(|t| T0 + t).collect()),
            ],
        ),
    ]
}

fn grid(kind: Kind) {
    for (name, parts) in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let planted = plant(&store, &parts);
        for range in ranges() {
            let full = expected(&planted, kind, range);
            // The control the whole grid rests on: the UNBUDGETED read holds every planted row.
            assert_eq!(
                multiset(&read(&store, kind, range, None)),
                multiset(&full),
                "{kind:?} {name}, range {range:?}: the unbudgeted read must hold what was planted"
            );
            for budget in BUDGETS {
                let capped = read(&store, kind, range, Some(budget));
                let at = format!("{kind:?} {name}, range {range:?}, budget {budget}");
                judge(&at, &capped, &full, budget);
            }
        }
    }
}

#[test]
fn every_capped_quote_read_meets_the_contract() {
    grid(Kind::Quote);
}

#[test]
fn every_capped_trade_read_meets_the_contract() {
    grid(Kind::Trade);
}

#[test]
fn every_capped_book_read_meets_the_contract() {
    grid(Kind::Book);
}

/// The two shapes that went EMPTY over the wire, pinned in absolute numbers too, so a walk and a
/// judge that were wrong the same way would still fail.
#[test]
fn a_symbol_absent_from_the_groups_first_part_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let (_, parts) = shapes().into_iter().find(|(n, _)| n.starts_with("a1_")).unwrap();
    plant(&store, &parts);
    for kind in [Kind::Quote, Kind::Trade, Kind::Book] {
        for budget in [1, 10, 120] {
            let capped = read(&store, kind, TsRange::all(), Some(budget));
            assert!(
                !capped.is_empty(),
                "{kind:?}, budget {budget}: the group's first part holds no row of A, and the \
                 answer must not be EMPTY while 50 rows of A follow it"
            );
            assert_eq!(capped[0].0, T0 + 200, "{kind:?}: the answer starts at A's first row");
        }
    }
    // b2: the per-symbol series is cut, the group is not — ONE budget holds them to one cut.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let (_, parts) = shapes().into_iter().find(|(n, _)| n.starts_with("b2_")).unwrap();
    plant(&store, &parts);
    let capped = read(&store, Kind::Quote, TsRange { start: Some(T0 + 60), end: None }, Some(100));
    assert_eq!(
        capped.iter().map(|r| r.0).collect::<Vec<_>>(),
        (60..120).chain(400..410).chain(500..505).map(|t| T0 + t).collect::<Vec<_>>(),
        "60 rows of the series' first part, short of 100, so the walk goes on and takes the rest"
    );
}

// ---- the bound -----------------------------------------------------------------------------------

/// The group directory of `kind` under `root`.
fn group_dir(root: &Path, kind: &str) -> PathBuf {
    root.join(format!("kind={kind}")).join(format!("venue={V}")).join(format!("group={G}"))
}

/// The one sealed part in the LATEST `date=` directory of the group — the fixture below writes its
/// last part alone on the next UTC day, so that directory holds exactly it.
fn last_day_part(root: &Path, kind: &str) -> PathBuf {
    let gdir = group_dir(root, kind);
    let mut days: Vec<PathBuf> = std::fs::read_dir(&gdir)
        .unwrap_or_else(|e| panic!("{}: {e}", gdir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.is_dir()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("date="))
        })
        .collect();
    days.sort();
    let last = days.pop().expect("a date directory");
    let mut parts: Vec<PathBuf> = std::fs::read_dir(&last)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
        .collect();
    assert_eq!(parts.len(), 1, "the last day holds the one part written on it: {parts:?}");
    parts.pop().unwrap()
}

/// The one observable difference between a bounded capped read and a whole read cut down
/// afterwards — the shape a no-budget stopgap would take: the capped read never opens a part past
/// the block that completed its count.
///
/// A busy group in which A holds 150 rows in the first part and 60 in the second, then a LAST part,
/// alone on the next UTC day, that is replaced in place by bytes that are not Parquet. A whole-range
/// read cannot survive it (the control proves that), so a capped read the first two parts can fill
/// must answer without having opened it, and one that needs the last part must fail — which proves
/// the part IS read when the count reaches it.
#[test]
fn a_capped_read_never_opens_the_groups_last_part_it_does_not_need() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let planted = plant(
        &store,
        &[
            Part::Group(cat(vec![rows("B", 0..200), rows("A", 0..150)])),
            Part::Group(cat(vec![rows("B", 300..500), rows("A", 300..360)])),
            Part::Group(cat(vec![rows("B", DAY_MS..DAY_MS + 10), rows("A", DAY_MS..DAY_MS + 10)])),
        ],
    );
    let fulls: Vec<(Kind, Vec<Row>)> = [Kind::Quote, Kind::Book]
        .into_iter()
        .map(|k| (k, expected(&planted, k, TsRange::all())))
        .collect();
    for kind in ["quote", "book"] {
        std::fs::write(last_day_part(dir.path(), kind), b"this is not a parquet file").unwrap();
    }
    assert!(
        store.scan_quotes(V, "A", TsRange::all()).is_err()
            && store.scan_book_updates(V, "A", TsRange::all()).is_err(),
        "control: the spoiled part must defeat a whole-range read, or nothing below proves a bound"
    );
    // A's 210 rows of the first day: 150 then 60, two stored rows each for book.
    for (kind, full) in &fulls {
        let fill = match kind {
            Kind::Book => 420,
            _ => 210,
        };
        for budget in [1, 100, 150, 151, fill] {
            let capped = read(&store, *kind, TsRange::all(), Some(budget));
            judge(&format!("{kind:?} spoiled, budget {budget}"), &capped, full, budget);
            assert!(
                capped.iter().all(|r| r.0 < T0 + DAY_MS),
                "{kind:?}: no row of the spoiled day"
            );
        }
    }
    assert!(
        store.scan_quotes_capped(V, "A", TsRange::all(), Some(211)).is_err(),
        "control: a capped read that NEEDS the last part must read it — the bound is the count, not \
         a fixed set of parts"
    );
}
