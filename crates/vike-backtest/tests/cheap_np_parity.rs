//! `cheap_np` parity gate — THE correctness proof for the fair-value port (backlog G11).
//!
//! Runs the Rust gate ([`vike_backtest::fair_value::cheap_gate`], the same predicate
//! [`vike_backtest::CheapNp`] fires on) over the SAME pre-joined taker-BUY prints the Python
//! oracle scored, and diffs the result entry-for-entry against the Python's own answer.
//!
//! Provenance. `.cheapnp_ref/signals.parquet` (7,147,999 rows) is a pre-joined cache: one row per
//! Polymarket taker BUY print with the BTC spot state of that instant attached, INCLUDING a
//! pre-computed `sigma_sec`. Scoring it therefore validates the gate — `p_up` / `wc` / band / TTE /
//! first-print-per-window / settlement — INDEPENDENTLY of `trailing_sigma`, which is pinned
//! separately below against the oracle's own outputs. `.cheapnp_ref/gen_reference.py` is the Python
//! that produced the 12,639-entry reference; `tests/fixtures/cheap_np/make_fixtures.py` re-derives
//! it bit-identically (verified: 0 mismatches against `cheap_np_ref.parquet`) and emits the
//! fixtures this file reads.
//!
//! Two tiers, because the full input is 148 MB of parquet that must never be committed:
//!
//! * **always-run** — `signals_sample.csv` / `entries_sample.csv`: every raw print of 60 whole
//!   5-minute windows (10,973 rows -> 27 entries), diffed entry-for-entry.
//! * **`#[ignore]`d, self-skipping** — the full 7.1M rows from the gitignored
//!   `.cheapnp_ref/signals.bin`, reproducing the published reference numbers exactly. Run with:
//!
//!   ```sh
//!   cargo test -p vike-backtest --test cheap_np_parity -- --ignored --nocapture
//!   ```
//!
//! Counts are asserted EXACT. The aggregates are asserted to a tight absolute tolerance and no
//! looser: the formulas are identical f64, so the only legitimate difference is the summation ORDER
//! (polars sums pairwise/SIMD; this file uses `vike_model::py_sum`). Widening a tolerance to make
//! one pass is forbidden — a real divergence means the gate diverged.

use std::path::PathBuf;

use vike_backtest::fair_value::{H, THETA, cheap_gate, fee, trailing_sigma};
use vike_model::py_sum;

// ---------------------------------------------------------------------------------------------
// the row set
// ---------------------------------------------------------------------------------------------

/// One pre-joined taker-BUY print. `winning_index < 0` = the market has no on-chain resolution.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Row {
    sts: u32,
    fts: u32,
    t: i32,
    oidx: u8,
    winning_index: i8,
    ask: f64,
    s_open: f64,
    s_now: f64,
    sigma: f64,
}

/// One selected entry (the row the gate picked for a window), as the Python reference records it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    sts: u32,
    fts: u32,
    oidx: u8,
    winning_index: i8,
    ask: f64,
    edge: f64,
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cheap_np")
}

/// The gitignored full-size inputs, staged at the repo root (`crates/vike-backtest/../..`).
fn staged(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(".cheapnp_ref").join(name)
}

fn read_signals_csv(path: &PathBuf) -> Vec<Row> {
    let text = std::fs::read_to_string(path).expect("signals_sample.csv");
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        "sts,fts,t,outcome_index,winning_index,ask,s_open,s_now,sigma_sec"
    );
    lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            assert_eq!(f.len(), 9, "malformed row: {l}");
            Row {
                sts: f[0].parse().unwrap(),
                fts: f[1].parse().unwrap(),
                t: f[2].parse().unwrap(),
                oidx: f[3].parse().unwrap(),
                winning_index: f[4].parse().unwrap(),
                ask: f[5].parse().unwrap(),
                s_open: f[6].parse().unwrap(),
                s_now: f[7].parse().unwrap(),
                sigma: f[8].parse().unwrap(),
            }
        })
        .collect()
}

fn read_entries_csv(path: &PathBuf) -> Vec<Entry> {
    let text = std::fs::read_to_string(path).expect("entries_sample.csv");
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap(), "sts,fts,outcome_index,winning_index,ask,edge");
    lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            assert_eq!(f.len(), 6, "malformed entry: {l}");
            Entry {
                sts: f[0].parse().unwrap(),
                fts: f[1].parse().unwrap(),
                oidx: f[2].parse().unwrap(),
                winning_index: f[3].parse().unwrap(),
                ask: f[4].parse().unwrap(),
                edge: f[5].parse().unwrap(),
            }
        })
        .collect()
}

/// Fixed-width little-endian records written by `make_fixtures.py` — see its module doc for the
/// layout. Plain `std::fs` on purpose: reading parquet from this test would mean adding an
/// arrow/parquet dependency to the workspace for one `#[ignore]`d assertion.
fn read_signals_bin(path: &PathBuf) -> Vec<Row> {
    const SZ: usize = 48;
    let raw = std::fs::read(path).expect("signals.bin");
    assert_eq!(raw.len() % SZ, 0, "truncated signals.bin");
    (0..raw.len() / SZ)
        .map(|i| {
            let r = &raw[i * SZ..(i + 1) * SZ];
            let u32_at = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
            let f64_at = |o: usize| f64::from_le_bytes(r[o..o + 8].try_into().unwrap());
            Row {
                sts: u32_at(0),
                fts: u32_at(4),
                t: i32::from_le_bytes(r[8..12].try_into().unwrap()),
                oidx: r[12],
                winning_index: r[13] as i8,
                ask: f64_at(16),
                s_open: f64_at(24),
                s_now: f64_at(32),
                sigma: f64_at(40),
            }
        })
        .collect()
}

fn read_entries_bin(path: &PathBuf) -> Vec<Entry> {
    const SZ: usize = 28;
    let raw = std::fs::read(path).expect("entries.bin");
    assert_eq!(raw.len() % SZ, 0, "truncated entries.bin");
    (0..raw.len() / SZ)
        .map(|i| {
            let r = &raw[i * SZ..(i + 1) * SZ];
            Entry {
                sts: u32::from_le_bytes(r[0..4].try_into().unwrap()),
                fts: u32::from_le_bytes(r[4..8].try_into().unwrap()),
                oidx: r[8],
                winning_index: r[9] as i8,
                ask: f64::from_le_bytes(r[12..20].try_into().unwrap()),
                edge: f64::from_le_bytes(r[20..28].try_into().unwrap()),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// the Rust gate — the thing under test
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Stats {
    rows: usize,
    after_band_tte: usize,
    after_theta: usize,
    entries: usize,
    after_resolution_join: usize,
    win_rate: f64,
    avg_ask: f64,
    total_pnl: f64,
    pnl_per_trade: f64,
    sum_edge: f64,
}

/// The `cheap_np` gate, exactly as `gen_reference.py` applies it:
///
/// 1. band `[0.10, 0.35)` AND time-till-expiry `[15, 270]` (both folded into
///    [`cheap_gate`]'s own first two predicates),
/// 2. dual-β worst-case edge `> θ`,
/// 3. the FIRST qualifying print per window — polars sorts `(slug, fts, outcome_index, ask)` and
///    takes the group's first row; since `slug` is `btc-updown-5m-<sts>` (fixed width, so
///    lexicographic == numeric) the group key is `sts`, and "first after that sort" is the row
///    minimising `(fts, outcome_index, ask)`. Ties would fall to stable order; the qualifying set
///    contains NO duplicate keys at all (verified: 681,403 rows, 681,403 distinct keys), so the
///    selection is unconditionally deterministic,
/// 4. inner-join the on-chain resolution (rows with no `winning_index` drop out AFTER selection),
/// 5. `won = (outcome_index == winning_index)`, `pnl = won − ask − fee(ask)` at 1 share.
///
/// Returns the counts, the aggregates, and the selected entries in `(sts, fts)` order.
fn run_gate(rows: &[Row]) -> (Stats, Vec<Entry>) {
    let mut after_band_tte = 0usize;
    // sts -> (key, entry) of the best qualifying print seen so far in that window
    let mut best: std::collections::HashMap<u32, ((u32, u8, f64), Entry)> =
        std::collections::HashMap::new();
    let mut after_theta = 0usize;

    for r in rows {
        let t = r.t as f64;
        // Step 1 is *counted* separately from step 2, so replicate the band+TTE predicate here and
        // let `cheap_gate` re-check it — it is the SAME predicate, and re-running it proves the two
        // agree rather than hiding the band test inside the gate.
        let in_band = (0.10..0.35).contains(&r.ask);
        let tte = H - t;
        if !in_band || !(15.0..=270.0).contains(&tte) {
            continue;
        }
        after_band_tte += 1;

        let Some(edge) = cheap_gate(r.oidx, r.ask, r.s_now, r.s_open, r.sigma, t, THETA, H) else {
            continue;
        };
        after_theta += 1;

        let key = (r.fts, r.oidx, r.ask);
        let e = Entry {
            sts: r.sts,
            fts: r.fts,
            oidx: r.oidx,
            winning_index: r.winning_index,
            ask: r.ask,
            edge,
        };
        best.entry(r.sts)
            .and_modify(|slot| {
                // strict `<` -> first-encountered wins an exact tie (== polars' stable sort)
                if (key.0, key.1) < (slot.0.0, slot.0.1)
                    || ((key.0, key.1) == (slot.0.0, slot.0.1) && key.2 < slot.0.2)
                {
                    *slot = (key, e);
                }
            })
            .or_insert((key, e));
    }

    let mut entries: Vec<Entry> = best.into_values().map(|(_, e)| e).collect();
    entries.sort_by_key(|e| (e.sts, e.fts));
    let n_entries = entries.len();

    let settled: Vec<Entry> = entries.iter().copied().filter(|e| e.winning_index >= 0).collect();
    let n = settled.len();
    let won = |e: &Entry| if e.oidx as i8 == e.winning_index { 1.0f64 } else { 0.0 };
    let pnl = |e: &Entry| won(e) - e.ask - fee(e.ask);

    let total_pnl = py_sum(settled.iter().map(pnl));
    let stats = Stats {
        rows: rows.len(),
        after_band_tte,
        after_theta,
        entries: n_entries,
        after_resolution_join: n,
        win_rate: py_sum(settled.iter().map(won)) / n as f64,
        avg_ask: py_sum(settled.iter().map(|e| e.ask)) / n as f64,
        total_pnl,
        pnl_per_trade: total_pnl / n as f64,
        sum_edge: py_sum(settled.iter().map(|e| e.edge)),
    };
    (stats, settled)
}

/// Absolute tolerance for a reordered f64 sum. `total_pnl` is O(1e3) over 12,639 terms; a pairwise
/// vs compensated sum can differ only in the last few ulps of the accumulator (~1e-13). This is
/// NEVER to be widened: the per-entry diff below is exact, so any aggregate drift beyond this means
/// the entry SET diverged, not the arithmetic.
const SUM_TOL: f64 = 1e-9;
const MEAN_TOL: f64 = 1e-12;

fn assert_close(what: &str, got: f64, want: f64, tol: f64) {
    let d = (got - want).abs();
    assert!(d <= tol, "{what}: got {got:.17e}, want {want:.17e}, |Δ| = {d:.3e} > {tol:.1e}");
}

fn diff_entries(got: &[Entry], want: &[Entry]) {
    assert_eq!(got.len(), want.len(), "entry COUNT differs");
    let mut bad = 0usize;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        // sts/fts/outcome/winning_index/ask are exact integers-or-verbatim-inputs: bit-equal or bust.
        // `edge` is computed, so it is compared to 1 ulp-scale tolerance (it is in fact bit-equal —
        // see the report — but the assert states what the port guarantees).
        let edge_ok = (g.edge - w.edge).abs() <= 1e-15 * w.edge.abs().max(1.0);
        if (g.sts, g.fts, g.oidx, g.winning_index) != (w.sts, w.fts, w.oidx, w.winning_index)
            || g.ask != w.ask
            || !edge_ok
        {
            bad += 1;
            if bad <= 5 {
                eprintln!("entry {i} DIFFERS\n  rust:   {g:?}\n  python: {w:?}");
            }
        }
    }
    assert_eq!(bad, 0, "{bad}/{} entries differ (first 5 printed above)", want.len());
}

// ---------------------------------------------------------------------------------------------
// tier 1 — always-run, committed 60-window sample
// ---------------------------------------------------------------------------------------------

/// Python (`make_fixtures.py`, 60 evenly-spaced whole windows out of 28,779):
/// ```text
/// rows 10973 | band+tte 1046 | theta 907 | entries 27 | settled 27
/// win_rate 0.4444444444444444  avg_ask 0.2794430029610369
/// total_pnl 4.072967414295504  pnl_per_trade 0.15085064497390754
/// ```
#[test]
fn sample_windows_reproduce_the_python_gate_entry_for_entry() {
    let rows = read_signals_csv(&fixtures().join("signals_sample.csv"));
    let want = read_entries_csv(&fixtures().join("entries_sample.csv"));
    let (s, got) = run_gate(&rows);

    assert_eq!(s.rows, 10_973, "fixture row count");
    assert_eq!(s.after_band_tte, 1_046);
    assert_eq!(s.after_theta, 907);
    assert_eq!(s.entries, 27);
    assert_eq!(s.after_resolution_join, 27);

    diff_entries(&got, &want);

    assert_close("win_rate", s.win_rate, 0.444_444_444_444_444_4, MEAN_TOL);
    assert_close("avg_ask", s.avg_ask, 0.279_443_002_961_036_9, MEAN_TOL);
    assert_close("total_pnl", s.total_pnl, 4.072_967_414_295_504, SUM_TOL);
    assert_close("pnl_per_trade", s.pnl_per_trade, 0.150_850_644_973_907_54, MEAN_TOL);
    assert_close("sum_edge", s.sum_edge, 4.415_028_041_702_424, SUM_TOL);
}

/// The committed sample is a sample of WHOLE windows, never of rows — otherwise "first qualifying
/// print per window" would not be the same question. Guard it so a future regeneration that
/// row-samples fails loudly instead of silently weakening the proof.
#[test]
fn sample_fixture_holds_whole_windows() {
    let rows = read_signals_csv(&fixtures().join("signals_sample.csv"));
    let windows: std::collections::HashSet<u32> = rows.iter().map(|r| r.sts).collect();
    assert_eq!(windows.len(), 60, "60 whole 5-minute windows");
    for r in &rows {
        assert_eq!(r.sts % 300, 0, "every window opens on the 300 s grid");
        assert_eq!(r.fts as i64 - r.sts as i64, r.t as i64, "t == fts − sts");
        assert!(r.oidx <= 1, "two outcomes per window");
    }
}

// ---------------------------------------------------------------------------------------------
// tier 2 — the full 7.1M-row reference (gitignored input, self-skipping)
// ---------------------------------------------------------------------------------------------

/// The published reference numbers, reproduced from all 7,147,999 pre-joined prints:
/// ```text
/// after band+tte filter : 785619
/// after theta filter    : 681403
/// entries (1 per window): 12858
/// after resolution join : 12639
/// win_rate 0.3678  avg_ask 0.2791  total_pnl 942.1225  pnl_per_trade 0.074541
/// ```
#[test]
#[ignore = "reads the gitignored 343 MB .cheapnp_ref/signals.bin (see make_fixtures.py)"]
fn full_reference_set_reproduces_every_published_number() {
    let sig = staged("signals.bin");
    let ent = staged("entries.bin");
    if !sig.exists() || !ent.exists() {
        eprintln!("SKIP: {} not staged — regenerate with make_fixtures.py", sig.display());
        return;
    }
    let rows = read_signals_bin(&sig);
    let want = read_entries_bin(&ent);
    let (s, got) = run_gate(&rows);

    eprintln!(
        "rows {} | band+tte {} | theta {} | entries {} | settled {}\n\
         win_rate {:.17} avg_ask {:.17}\ntotal_pnl {:.17} pnl_per_trade {:.17} sum_edge {:.17}",
        s.rows,
        s.after_band_tte,
        s.after_theta,
        s.entries,
        s.after_resolution_join,
        s.win_rate,
        s.avg_ask,
        s.total_pnl,
        s.pnl_per_trade,
        s.sum_edge,
    );

    assert_eq!(s.rows, 7_147_999);
    assert_eq!(s.after_band_tte, 785_619);
    assert_eq!(s.after_theta, 681_403);
    assert_eq!(s.entries, 12_858);
    assert_eq!(s.after_resolution_join, 12_639);

    diff_entries(&got, &want);

    assert_close("win_rate", s.win_rate, 0.367_829_733_364_981_4, MEAN_TOL);
    assert_close("avg_ask", s.avg_ask, 0.279_138_364_024_516_64, MEAN_TOL);
    assert_close("total_pnl", s.total_pnl, 942.122_453_978_319_6, SUM_TOL);
    assert_close("pnl_per_trade", s.pnl_per_trade, 0.074_540_901_493_656_11, MEAN_TOL);
    assert_close("sum_edge", s.sum_edge, 2_086.733_572_270_817_6, SUM_TOL);
}

// ---------------------------------------------------------------------------------------------
// trailing_sigma — pinned against the oracle, since signals.parquet bypasses it
// ---------------------------------------------------------------------------------------------

/// `signals.parquet` ships `sigma_sec` pre-computed, so the gate proof above says NOTHING about
/// [`trailing_sigma`] — yet that estimator is what the LIVE path computes, and its 1-second-grid
/// linear gap interpolation is the single likeliest source of a live parity miss (the underlying
/// spot series is missing ~11% of its seconds). So it is pinned directly against
/// `fair_value_bot/strategy.py:trailing_sigma`, hex-exact, over synthetic series that exercise each
/// documented behaviour: dense, ~11%-sparse, below the warmup floor, duplicate samples inside one
/// second, the lookback cutoff, and a long stall.
#[test]
fn trailing_sigma_matches_the_python_oracle_bit_for_bit() {
    let text = std::fs::read_to_string(fixtures().join("trailing_sigma.json")).unwrap();
    let cases: serde_json::Value = serde_json::from_str(&text).unwrap();
    let cases = cases.as_array().expect("array of cases");
    assert_eq!(cases.len(), 6, "all six documented behaviours must be pinned");

    for c in cases {
        let name = c["name"].as_str().unwrap();
        let lookback = c["lookback_s"].as_f64().unwrap();
        let now = c["now"].as_f64().unwrap();
        let samples: Vec<(f64, f64)> = c["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
            .collect();
        let got = trailing_sigma(samples, lookback, now);

        match c["sigma_bits"].as_str() {
            None => assert_eq!(got, None, "{name}: oracle returned None"),
            Some(hex) => {
                let want = f64::from_bits(u64::from_str_radix(hex, 16).unwrap());
                let got = got.unwrap_or_else(|| panic!("{name}: got None, oracle got {want}"));
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{name}: got {got:.17e} ({:016x}), oracle {want:.17e} ({hex})",
                    got.to_bits()
                );
            }
        }
    }
}
