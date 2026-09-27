//! **A MEASUREMENT harness. It stores nothing, decides nothing, and validates nothing.**
//!
//! It answers one question by actually fetching: *for each (venue, instrument type, interval), does
//! this venue serve klines through the collector path this tree ships, and how many rows come back?*
//! The output is a markdown matrix on stdout for a human to read. There is no capability table here,
//! no menu, nothing asserted about a venue's answer — an operator reads the table and decides.
//!
//! # Why it lives here, behind `backfill-serve`
//!
//! The measurement is only worth anything if it goes through the SAME dispatch the daemon uses, so
//! it drives [`real_backfill_table`] — the constructor `serve_with_backfill` mounts — rather than
//! calling `vike_backfill::backfill_binance_klines` and siblings by hand. That constructor is behind
//! `backfill-serve`, so this file is too; a default or `serve-datafusion` build compiles it to
//! nothing. The `backfill-serve` lane (the justfile's `hist` recipe,
//! `cargo test -p vike-datahub --features backfill-serve`) therefore gives it a compile and clippy
//! witness on every PR that touches the collectors.
//!
//! ⚠ **Every network probe is `#[ignore]`d, and that is load-bearing rather than tidy.** The
//! `backfill-serve` lane RUNS this binary — it is not a clippy-only lane — so without the attribute
//! a merge gate would fire real venue REST calls from the box that signs orders, on every PR, on a
//! shared public IP. `#[ignore]` (not a second feature) because the feature that gates the file is
//! already the one that compiles the collectors: a `probe` feature would have to be enabled
//! alongside `backfill-serve` and would then be one more lane spelling nobody reads.
//! [`the_probe_roster_only_names_venues_the_production_table_dispatches`] is the one test here that
//! runs unattended, and it touches no network.
//!
//! # What the run writes, and where
//!
//! Two throwaway [`DataFusionHist`] stores under `CARGO_TARGET_TMPDIR` — the crate's own
//! `target/tmp`, which is wherever the lane's target directory is (NVMe on the verification boxes),
//! never the shared system temp directory and never a live store. Both are `tempfile` guards, so
//! they self-delete on the panic path too. Nothing here can reach an operator's data.
//!
//! **TWO stores, one per window shape, and that is the dedup fix.** The collectors are idempotent by
//! `vike_backfill`'s kline commit key (`venue:symbol:interval:start-end`). The two shapes below
//! coincide exactly at `1m` — a `10 x 1m` window IS a ten-minute window — so a single store would
//! answer the second call `0 rows` and that zero would read as "the venue served nothing". Splitting
//! the STORE (rather than perturbing the WINDOW) keeps the two shapes measuring the same window at
//! `1m`, which is the honest comparison, and makes a key collision structurally impossible for every
//! other interval too.
//!
//! # The two window shapes, and why one is not enough
//!
//! * **sized** — `[now - 10*width, now]`. Measures CAPABILITY: ten bars is a request the venue can
//!   answer for any interval it serves at all.
//! * **fixed10m** — `[now - 10min, now]`. Measures what a SMALL request returns. For an interval
//!   coarser than ten minutes this is a window containing no bar OPEN, so `0` here beside a
//!   non-zero `sized` is "the venue serves it, the window was too narrow", not "unsupported".
//!
//! `now` is captured ONCE for the whole run, so the two shapes differ only in their start.
//!
//! # The three error origins, kept apart
//!
//! An empty cell and a failed cell are different facts, and so are the failures:
//!
//! * `T` — the BRIDGE's own interval table refused it (`vike_bybit::data::interval_code` /
//!   `vike_okx::data::bar_code`). **The request never left the box.** Binance has no such table —
//!   `vike_binance::family::klines::klines_url` interpolates the caller's interval straight into the
//!   query string — so a binance `T` is impossible by construction and a binance refusal is always
//!   the venue's.
//! * `V` — the venue answered and the answer was an error (HTTP status, bybit `retCode`, okx `code`).
//!   The request WAS spent.
//! * `S` — `DataFusionHist::append_bars` refused the rows.
//!
//! The classification reads the error TEXT, which is what the wire carries too: the collectors
//! stringify through `vike_backfill`'s `CollectError`, whose `Display` prefixes `venue fetch: ` or
//! `hist store: `.
//!
//! # Running it
//!
//! ```text
//! cargo test -p vike-datahub --features backfill-serve --test venue_interval_matrix \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` is not decoration: the probes share one public IP and each venue's pager paces
//! itself on the assumption that it is the only thing talking to that host from this process.

#![cfg(feature = "backfill-serve")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_datahub::backfill::{BackfillTable, real_backfill_table};

/// The interval vocabulary this harness asks about — deliberately WIDER than anything in the tree.
///
/// The point is to find each venue's real edge, so the list is the union of what the three venues'
/// own tables name plus `1s` and `3d`, which only some of them do. Two of these (`1w`, `1M`) have no
/// width in `vike_model::time::interval_ms`'s grammar at all — see [`probe_width_ms`].
static INTERVALS: [&str; 15] =
    ["1s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "3d", "1w", "1M"];

/// The interval a MISROUTE probe (see [`Probe::intervals`]) is measured at — one cell is enough to
/// establish where an unaddressable instrument type actually lands, and spending fifteen on it would
/// be fifteen requests bought for one fact.
static MISROUTE_INTERVAL: [&str; 1] = [REFERENCE_INTERVAL];

/// The one interval at which every row that wrote bars also prints its FIRST BAR back out of the
/// store, so the tapes can be compared side by side. It is [`MISROUTE_INTERVAL`] by construction: a
/// misroute row is the one whose answer most needs identifying, and an identification is worth
/// nothing without the addressable rows' own bars printed in the same units on the same run.
const REFERENCE_INTERVAL: &str = "1h";

/// How wide one bar of `interval` is, in ms — **the harness's OWN table, and it has to be.**
///
/// `vike_model::time::interval_ms` splits on a single trailing char and knows `s`/`m`/`h`/`d`, so it
/// answers `None` for `1w` and `1M`. A sized window needs a width for every interval probed, so this
/// table is where the two extra rows live. It is a PROBE parameter, not a vocabulary: nothing here
/// widens what the store, the supervisor or the pacer accept, and that gap is itself one of the
/// findings this harness exists to surface.
///
/// `1M` is thirty days. A calendar month is not a fixed multiple and this harness does not pretend
/// otherwise — the number only has to buy a window that spans about ten monthly bars.
fn probe_width_ms(interval: &str) -> i64 {
    match interval {
        "1s" => 1_000,
        "1m" => 60_000,
        "3m" => 180_000,
        "5m" => 300_000,
        "15m" => 900_000,
        "30m" => 1_800_000,
        "1h" => 3_600_000,
        "2h" => 7_200_000,
        "4h" => 14_400_000,
        "6h" => 21_600_000,
        "12h" => 43_200_000,
        "1d" => 86_400_000,
        "3d" => 259_200_000,
        "1w" => 604_800_000,
        "1M" => 2_592_000_000,
        other => panic!("probe_width_ms has no row for {other:?} — add one beside the interval"),
    }
}

/// One (venue, instrument type, symbol) row of the matrix.
struct Probe {
    /// The venue id the production table dispatches on.
    venue: &'static str,
    /// What THIS TREE calls the instrument type, read from the bridge rather than from a taxonomy.
    kind: &'static str,
    /// The symbol as a CALLER hands it to the collector — the store/series key, suffix included.
    symbol: &'static str,
    /// Which intervals to probe. [`INTERVALS`] for an addressable type; [`MISROUTE_INTERVAL`] for a
    /// type the collector cannot address, where the finding is WHERE the request lands, not which
    /// intervals it serves.
    intervals: &'static [&'static str],
    /// Why this row is here, printed under the table. For a misroute probe this is the finding.
    note: &'static str,
}

/// The rows, each read out of its bridge's own routing code.
///
/// ⚠ Two instrument types in this list are **not addressable** and are probed anyway, at one
/// interval, so that the claim is an OBSERVATION rather than a code reading: the symbol is handed in
/// and the harness reports where the request actually went.
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            venue: "binance",
            kind: "spot",
            symbol: "BTCUSDT",
            intervals: &INTERVALS,
            note: "api.binance.com/api/v3/klines — `vike_binance::data`'s spot klines base, reached \
                   because `range_target` finds no `vike_catalog::PERP_SUFFIX`.",
        },
        Probe {
            venue: "binance",
            kind: "usdm-perp",
            symbol: "BTCUSDT.P",
            intervals: &INTERVALS,
            note: "fapi.binance.com/fapi/v1/klines — `vike_binance::data`'s fapi klines base. The \
                   `.P` suffix is stripped for the wire and kept as the series key.",
        },
        Probe {
            venue: "binance",
            kind: "coinm-futures",
            symbol: "BTCUSD_PERP.P",
            intervals: &INTERVALS,
            note: "dapi.binance.com/dapi/v1/klines — `vike_binance::data`'s THIRD host. The `.P` \
                   suffix says PERPETUAL (which binance's own listing agrees it is) and \
                   `vike_binance::instruments` — the venue's COIN-M listing, read once per \
                   process — is what says the book is coin-margined, so the route is neither a \
                   symbol-shape guess nor a class claim. ⚠ The host is not what made this \
                   reachable: fapi serves this tape byte-identically. \
                   `vike_binance::family::klines::VolumeColumn` is — index 5 here is a CONTRACT \
                   COUNT, not the base asset.",
        },
        Probe {
            venue: "binance",
            kind: "coinm-futures (BARE — the venue refuses it)",
            symbol: "BTCUSD_PERP",
            intervals: &MISROUTE_INTERVAL,
            note: "THE ORIGINAL FINDING, kept because the REFUSAL is the measurement now. A bare \
                   symbol carries no perpetual marker, so `vike_binance::data::route_target` sends \
                   it to SPOT and spot answers `-1121 Invalid symbol`. That is an ERROR rather \
                   than another book's tape, which is exactly what \
                   `vike_catalog::addressing_for`'s binance `BareSymbol::Unambiguous` claims — and \
                   binance's three `symbol` sets being disjoint is why it holds. The row above is \
                   the spelling that works.",
        },
        Probe {
            venue: "bybit",
            kind: "spot",
            symbol: "BTCUSDT",
            intervals: &INTERVALS,
            note: "`category=spot` on /v5/market/kline — `vike_bybit::data::Category::Spot`, where \
                   an unclaimed unambiguous bare symbol routes.",
        },
        Probe {
            venue: "bybit",
            kind: "linear-perp",
            symbol: "BTCUSDT.P",
            intervals: &INTERVALS,
            note: "`category=linear` — `vike_bybit::data::route_target` asks the venue's own \
                   listings which derivative book carries the stripped symbol \
                   (`vike_bybit::instruments::perp_book`). Bybit V5 unifies the categories under \
                   one host, so this is a query parameter rather than a second host.",
        },
        Probe {
            venue: "bybit",
            kind: "inverse-perp",
            symbol: "BTCUSD.P",
            intervals: &INTERVALS,
            note: "ADDRESSABLE since the 0061 phase-4 routing: `vike_bybit::data::Category` has an \
                   Inverse arm and the book is a venue-LISTING membership rather than a reading of \
                   the symbol's shape. ⚠ THE ORIGINAL FINDING HERE WAS HALF WRONG and is corrected \
                   rather than deleted: it said an inverse symbol was asked of the SPOT book \
                   'which does not list it'. Bybit DOES list BTCUSD on spot (MEASURED 2026-09-16: \
                   spot n inverse = {BTCUSD, ETHUSD}) — the misroute served a VESTIGIAL spot tape, \
                   which is why it was silent rather than an error, and is exactly what \
                   `vike_bybit::data::ambiguous_bare_symbol_refusal` now refuses for a BARE symbol. \
                   The unsuffixed spelling is therefore deliberately NOT probed here: it is a \
                   refusal, not a route.",
        },
        Probe {
            venue: "okx",
            kind: "spot",
            symbol: "BTC-USDT",
            intervals: &INTERVALS,
            note: "`instId=BTC-USDT` on /api/v5/market/history-candles.",
        },
        Probe {
            venue: "okx",
            kind: "linear-swap",
            symbol: "BTC-USDT-SWAP",
            intervals: &INTERVALS,
            note: "`vike_okx::data`'s candles URL interpolates the caller's symbol into `instId` \
                   VERBATIM — no suffix logic, no category. OKX instrument types are therefore \
                   addressable by symbol alone, with no bridge change.",
        },
        Probe {
            venue: "okx",
            kind: "inverse-swap",
            symbol: "BTC-USD-SWAP",
            intervals: &INTERVALS,
            note: "The same verbatim `instId` path — this is the type binance and bybit cannot reach \
                   at all, and OKX reaches it for free.",
        },
    ]
}

/// One window shape: a label and the start it derives from `now`.
struct Shape {
    label: &'static str,
    /// `(now_ms, interval) -> start_ms`.
    start: fn(i64, &str) -> i64,
}

/// Ten bars wide — the CAPABILITY question.
fn start_sized(now: i64, interval: &str) -> i64 {
    now - 10 * probe_width_ms(interval)
}

/// Ten minutes wide whatever the interval — the SMALL-REQUEST question.
fn start_fixed_ten_minutes(now: i64, _interval: &str) -> i64 {
    now - 600_000
}

static SHAPES: [Shape; 2] = [
    Shape { label: "sized (10 x interval)", start: start_sized },
    Shape { label: "fixed 10 minutes", start: start_fixed_ten_minutes },
];

/// Where a refusal came from — the three different facts a failed cell can be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The bridge's own interval table. The request never left the box.
    BridgeTable,
    /// The venue answered, and the answer was a refusal. The request was spent.
    Venue,
    /// The hist store refused the rows.
    Store,
}

impl Origin {
    fn tag(self) -> &'static str {
        match self {
            Origin::BridgeTable => "T",
            Origin::Venue => "V",
            Origin::Store => "S",
        }
    }
}

/// Classify a stringified `vike_backfill` `CollectError`.
///
/// `hist store: ` and `venue fetch: ` are that type's own `Display` prefixes. Within a fetch
/// failure, the bridges' pre-flight tables are the only producers of `unsupported interval` —
/// `vike_bybit::data::interval_code` and `vike_okx::data::bar_code` both format exactly that, and
/// both run BEFORE any agent is built, which is what makes the distinction a fact about whether a
/// request was spent rather than a guess about severity.
fn classify(err: &str) -> Origin {
    if err.starts_with("hist store:") {
        Origin::Store
    } else if err.contains("unsupported interval") {
        Origin::BridgeTable
    } else {
        Origin::Venue
    }
}

/// What one cell measured.
struct Outcome {
    rows: Option<usize>,
    err: Option<(Origin, String)>,
    elapsed_ms: u128,
}

/// Run one cell through the production dispatch table.
fn probe_cell(table: &BackfillTable, p: &Probe, interval: &str, start: i64, end: i64) -> Outcome {
    let started = Instant::now();
    let Some(collect) = table.get(p.venue) else {
        return Outcome {
            rows: None,
            err: Some((
                Origin::BridgeTable,
                format!("no collector: the production table dispatches {:?}", table.supported()),
            )),
            elapsed_ms: started.elapsed().as_millis(),
        };
    };
    let result = collect(p.symbol, interval, start, end);
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(rows) => Outcome { rows: Some(rows), err: None, elapsed_ms },
        Err(e) => {
            let origin = classify(&e);
            Outcome { rows: None, err: Some((origin, e)), elapsed_ms }
        }
    }
}

/// A `tempfile` guard plus the store opened inside it. Both must outlive the sweep — a bare
/// `tempdir().path()` deletes the directory before the first write.
struct Scratch {
    _dir: tempfile::TempDir,
    store: Arc<DataFusionHist>,
}

/// Open a throwaway store under the crate's own `target/tmp`.
///
/// `CARGO_TARGET_TMPDIR` is a compile-time cargo macro, not a process-environment read — nothing
/// here consults the environment — and it resolves inside the target directory, which is on the same
/// (NVMe) filesystem as the checkout doing the verification. Deliberately NOT the system temp
/// directory: `crates/vike-ops/tests/temp_path_gate.rs` carries what a shared `/tmp` costs on a box
/// several users run tests on.
fn scratch(tag: &str) -> Scratch {
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let dir = tempfile::Builder::new()
        .prefix(&format!("venue-interval-matrix-{tag}-"))
        .tempdir_in(root)
        .expect("create a scratch dir under the crate target tmpdir");
    let store =
        Arc::new(DataFusionHist::open(dir.path()).expect("open a throwaway DataFusionHist"));
    Scratch { _dir: dir, store }
}

/// The one test in this file that runs unattended: the probe roster may only name venues the
/// PRODUCTION table actually dispatches, so a probe row can never quietly measure nothing.
///
/// Network-free — `real_backfill_table` builds closures and opens no socket, and
/// `BackfillTable::supported` is the same list the server's unknown-venue error prints.
#[test]
fn the_probe_roster_only_names_venues_the_production_table_dispatches() {
    let sc = scratch("roster");
    let table = real_backfill_table(Arc::clone(&sc.store));
    let supported = table.supported();
    for p in probes() {
        assert!(
            supported.contains(&p.venue),
            "probe row {}/{} names venue {:?}, which the production table does not dispatch \
             (it dispatches {:?}) — a row like that would report a collector error that says \
             nothing about the venue",
            p.venue,
            p.kind,
            p.venue,
            supported,
        );
        assert!(
            !p.intervals.is_empty(),
            "probe row {}/{} would measure no cell at all",
            p.venue,
            p.kind,
        );
    }
}

/// THE SWEEP. Real venue REST calls — see this file's module doc for the command.
///
/// Prints, per window shape, a markdown table of `venue x instrument type x interval` whose cells
/// are the rows written or a one-letter error origin keyed to a numbered legend of verbatim errors.
/// It asserts NOTHING about any venue's answer: an assertion here would be this harness deciding
/// something, which is exactly what it must not do.
///
/// The only panic paths are the harness's own — a scratch store that will not open, or an interval
/// with no width row.
#[test]
#[ignore = "real venue REST calls from a shared public IP — run explicitly, see the module doc"]
fn venue_interval_matrix() {
    let now = vike_model::now_ms();
    let all = probes();
    let mut calls = 0usize;
    let mut by_venue_calls: BTreeMap<&str, usize> = BTreeMap::new();

    println!("\n# venue x instrument type x interval — measured, not decided");
    println!("\n`now` = {now} (epoch ms, captured once for the whole run)");

    for (shape_idx, shape) in SHAPES.iter().enumerate() {
        // One store PER SHAPE — see the module doc's dedup note.
        let sc = scratch(&format!("shape{shape_idx}"));
        let table = real_backfill_table(Arc::clone(&sc.store));

        // Verbatim errors, deduplicated, in first-seen order; cells cite them by index.
        let mut legend: Vec<(Origin, String)> = Vec::new();
        let mut rendered: Vec<(String, Vec<String>)> = Vec::new();

        for p in &all {
            let mut cells = Vec::with_capacity(INTERVALS.len());
            for interval in INTERVALS.iter() {
                if !p.intervals.contains(interval) {
                    cells.push("·".to_string());
                    continue;
                }
                let start = (shape.start)(now, interval);
                let out = probe_cell(&table, p, interval, start, now);
                calls += 1;
                *by_venue_calls.entry(p.venue).or_default() += 1;
                let cell = match (&out.rows, &out.err) {
                    (Some(n), _) => n.to_string(),
                    (None, Some((origin, msg))) => {
                        let idx = match legend.iter().position(|(_, m)| m == msg) {
                            Some(i) => i,
                            None => {
                                legend.push((*origin, msg.clone()));
                                legend.len() - 1
                            }
                        };
                        format!("{}{}", origin.tag(), idx + 1)
                    }
                    (None, None) => unreachable!("a cell is either rows or an error"),
                };
                println!(
                    "cell shape={} venue={} kind={} symbol={} interval={interval} start={start} \
                     end={now} -> {cell} ({} ms)",
                    shape.label, p.venue, p.kind, p.symbol, out.elapsed_ms,
                );
                // WHICH BOOK ANSWERED, at one reference interval, for the rows that wrote any.
                //
                // A row count alone cannot tell a REFUSAL apart from a wrong-book ANSWER, and the
                // difference matters most exactly where the collector cannot address the instrument
                // type it was asked for: a venue that answers such a request writes somebody's bars
                // under the caller's symbol and reports success. Volume is the discriminator that
                // needs no second request — a BTC-denominated spot tape and a USD-denominated
                // inverse-contract tape differ by orders of magnitude, not by a rounding.
                if interval == &REFERENCE_INTERVAL && out.rows.is_some_and(|n| n > 0) {
                    match sc.store.load_bars(p.venue, p.symbol, interval, TsRange::all()) {
                        Ok(bars) => match bars.first() {
                            Some(b) => println!(
                                "  first-bar venue={} kind={} symbol={} ts={} close={} volume={}",
                                p.venue, p.kind, p.symbol, b.ts, b.close, b.volume,
                            ),
                            None => println!(
                                "  first-bar venue={} kind={} symbol={} -> the store read back EMPTY",
                                p.venue, p.kind, p.symbol,
                            ),
                        },
                        Err(e) => println!(
                            "  first-bar venue={} kind={} symbol={} -> read-back failed: {e}",
                            p.venue, p.kind, p.symbol,
                        ),
                    }
                }
                cells.push(cell);
            }
            rendered.push((format!("{} / {} (`{}`)", p.venue, p.kind, p.symbol), cells));
        }

        println!("\n## window shape: {}\n", shape.label);
        println!("| venue / instrument type (symbol) | {} |", INTERVALS.join(" | "));
        println!("|---|{}", "---|".repeat(INTERVALS.len()));
        for (label, cells) in &rendered {
            println!("| {label} | {} |", cells.join(" | "));
        }
        println!(
            "\n`·` = not probed for this row. `T`/`V`/`S` = refused by the bridge's own interval \
             table (never left the box) / by the venue / by the store; the number keys the legend."
        );
        if legend.is_empty() {
            println!("\nno refusals in this shape.");
        } else {
            println!("\n### legend — verbatim errors\n");
            for (i, (origin, msg)) in legend.iter().enumerate() {
                println!("{}. `{}{}` — `{msg}`", i + 1, origin.tag(), i + 1);
            }
        }
    }

    println!("\n## probe notes (why each row is in the table)\n");
    for p in &all {
        println!("* **{} / {}** (`{}`) — {}", p.venue, p.kind, p.symbol, p.note);
    }

    println!("\n## calls issued\n");
    println!("| venue | backfill calls |");
    println!("|---|---|");
    for (venue, n) in &by_venue_calls {
        println!("| {venue} | {n} |");
    }
    println!("| **total** | **{calls}** |");
    println!(
        "\nA CALL is one `BackfillFn` invocation. Requests per call are the pager's business and \
         are not observable from this seam: binance additionally GETs its host's `exchangeInfo` \
         ONCE per call before paging (`vike_binance::family::klines`'s discovered-pacing doc), and \
         bybit/okx GET only pages. A call whose cell reads `T` issued NO request."
    );
}
