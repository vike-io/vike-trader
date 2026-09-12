//! `store_rates` — measure the RATE DISTRIBUTIONS of a recorded market-data series, from the real
//! store, with this workspace's own engine.
//!
//! ## Why this exists
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`'s §11 Q4 declares every
//! wire constant UNCALIBRATED — `MD_MAX_DEPTH_LEVELS`, `MD_MAILBOX_CAP`, `MD_TAPE_CAP`,
//! `MD_PUBLISH_INTERVAL`, `MD_LINGER`, `MD_LAPSE_BUDGET` and the caps — and asks for a real
//! BTCUSDT depth+tape measurement before a default is chosen. **An average cannot size a mailbox:**
//! a 173 rows/s mean says nothing about the second in which 40 snapshots arrive at once, and every
//! one of those constants is a bound on a BURST. So this reports DISTRIBUTIONS (p50/p90/p99/p99.9/
//! max), never means alone.
//!
//! ## Why an EXAMPLE and not a `[[bin]]`
//!
//! A `[[bin]]` joins the release roster, the container link set and the multicall table; a probe
//! that is run by hand a few times a year should join none of them. `required-features =
//! ["hist-datafusion"]` in `crates/vike-data/Cargo.toml` keeps it out of a default-feature build
//! entirely (the `crates/vike-chart` / `crates/vike-studio` `png-export` examples are the
//! precedent).
//!
//! ## What it reuses, and what it therefore CANNOT get wrong
//!
//! Every byte it reads comes back through `crate::DataFusionHist` and the `HistStore` verbs
//! (`scan_depth` / `scan_book_updates` / `scan_trades` / `scan_quotes`). It builds no path, globs
//! no directory and parses no manifest: the store's own manifest file-index does the part
//! selection, the store's own codecs do the decode, and the store's own
//! `book_updates_from_rows` does the REGROUPING that turns rows back into UPDATES. So "what is one
//! update" is not this file's opinion — an update is a run of rows sharing one `seq` (and kind, and
//! symbol), which is exactly what the wire would publish as one frame.
//!
//! ⚠ **The store is opened with `DataFusionHist::open_read_only`.** The ordinary `open` performs
//! two writes — it `create_dir_all`s the root and runs the WAL crash-recovery sweep, which takes
//! each affected series' lock — and this probe is pointed at a root a live recorder is writing.
//!
//! ⚠ **No SQL.** `Cargo.toml` pins `datafusion` with `default-features = false` and does NOT enable
//! its `sql` feature (the whole workspace contains zero `.sql(` call sites), so
//! `SessionContext::sql` does not exist here. The aggregation is DataFusion's DataFrame API where
//! it is pushed down (part prune + `ts` predicate + column projection) and an exact streaming fold
//! in Rust over the decoded rows above that.
//!
//! ## Memory
//!
//! The window is walked in `--chunk-min` sub-windows and folded into BTreeMap HISTOGRAMS, so
//! percentiles are exact (nearest-rank over the full population, not sampled) while resident memory
//! is O(one sub-window's rows + distinct values), independent of the window length.
//!
//! ```text
//! cargo run --release -p vike-data --features hist-datafusion --example store_rates -- \
//!     --store /path/to/hist --kind depth --venue binance --symbol BTCUSDT.P \
//!     --from 1757289600000 --to 1757293200000
//! ```

use std::collections::BTreeMap;
use std::error::Error;

use vike_data::{DataFusionHist, HistStore, SeriesId, TsRange};
use vike_model::{BookUpdate, BookUpdateKind, L2Book, Level, QuoteTick, TradeTick};

/// Sample one folded book in this many updates when measuring the FRAME size the `Book` lane would
/// actually put on the wire. The folded state is re-serialized to do it, and doing that on every
/// update of a 6 M-row window is minutes of `serde_json` for a distribution that converges in
/// thousands of samples. The level-count histogram beside it is NOT sampled.
const FRAME_SAMPLE_EVERY: u64 = 512;

/// The clamp the design proposes for `MD_MAX_DEPTH_LEVELS`, in levels PER SIDE. Sampled frame sizes
/// are reported both unclamped and at this value, so the clamp's effect is measured rather than
/// argued.
const PROPOSED_CLAMP_PER_SIDE: usize = 50;

// ---------------------------------------------------------------------------------------------
// Histogram: exact percentiles at bounded memory.
// ---------------------------------------------------------------------------------------------

/// A count-per-VALUE histogram. Exact for every percentile (nearest-rank over the true
/// population), and bounded by the number of DISTINCT values rather than by the sample count —
/// which is what lets an hour of a 6 M-row series be summarized without holding it.
#[derive(Default)]
struct Hist {
    counts: BTreeMap<u64, u64>,
    n: u64,
    sum: u128,
}

impl Hist {
    fn add(&mut self, v: u64) {
        self.add_n(v, 1);
    }
    fn add_n(&mut self, v: u64, c: u64) {
        if c == 0 {
            return;
        }
        *self.counts.entry(v).or_default() += c;
        self.n += c;
        self.sum += u128::from(v) * u128::from(c);
    }
    /// Nearest-rank percentile: the smallest value at or below which at least `p` of the population
    /// lies. `p` is a fraction in `[0, 1]`.
    fn pct(&self, p: f64) -> u64 {
        if self.n == 0 {
            return 0;
        }
        let rank = ((p * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0u64;
        for (&v, &c) in &self.counts {
            seen += c;
            if seen >= rank {
                return v;
            }
        }
        self.counts.keys().next_back().copied().unwrap_or(0)
    }
    fn max(&self) -> u64 {
        self.counts.keys().next_back().copied().unwrap_or(0)
    }
    fn mean(&self) -> f64 {
        if self.n == 0 { 0.0 } else { self.sum as f64 / self.n as f64 }
    }
    /// The five-number row every table in this tool prints.
    fn row(&self, name: &str, unit: &str) -> String {
        format!(
            "  {name:<26} n={:<12} mean={:<12.2} p50={:<10} p90={:<10} p99={:<10} p99.9={:<10} \
             max={:<10} {unit}",
            self.n,
            self.mean(),
            self.pct(0.50),
            self.pct(0.90),
            self.pct(0.99),
            self.pct(0.999),
            self.max(),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// The wire-size model. Spelled here because the type it models does not exist yet.
// ---------------------------------------------------------------------------------------------

/// The framed JSON bytes of an `MdFrame::Depth(BookSnapshot{..})` carrying NO levels — the fixed
/// cost of one book frame on the wire.
///
/// §4.4 of the design gives `BookSnapshot`'s fields and §4.5 gives the framing (`u32` big-endian
/// length + UTF-8 JSON, no compression), and neither exists in the tree yet — so this builds the
/// declared shape with `serde_json` and MEASURES it rather than estimating. `serde`'s default
/// externally-tagged enum encoding is what wraps it; the `+ 4` is the length prefix.
///
/// The variable half is measured from the REAL levels of each update, so a frame's size is
/// `envelope + json(bids) + json(asks) - 4` (the `- 4` removes the two empty `[]` this envelope
/// already counted).
fn depth_envelope_bytes(venue: &str, symbol: &str) -> usize {
    let v = serde_json::json!({
        "Depth": {
            "venue": venue,
            "symbol": symbol,
            "tick_size": 0.1_f64,
            "bids": [],
            "asks": [],
            "venue_ts": 1_757_289_600_000_i64,
            "venue_seq": 9_876_543_210_u64,
            "seq": 1_234_567_u64,
        }
    });
    serde_json::to_vec(&v).map_or(0, |b| b.len()) + 4
}

/// The framed JSON bytes of an `MdFrame::Trades{..}` carrying NO ticks — the twin of
/// [`depth_envelope_bytes`] for the tape lane.
fn trades_envelope_bytes(venue: &str, symbol: &str) -> usize {
    let v = serde_json::json!({
        "Trades": { "venue": venue, "symbol": symbol, "ticks": [], "seq": 1_234_567_u64 }
    });
    serde_json::to_vec(&v).map_or(0, |b| b.len()) + 4
}

/// JSON bytes of one update's level payload — both sides, as `serde` renders `Vec<(f64, f64)>`.
/// An empty pair of vectors is 4 bytes (`[]` twice), which is what the envelope already carries.
fn level_json_bytes(bids: &[Level], asks: &[Level]) -> usize {
    let b = serde_json::to_vec(bids).map_or(0, |v| v.len());
    let a = serde_json::to_vec(asks).map_or(0, |v| v.len());
    b + a
}

// ---------------------------------------------------------------------------------------------
// The accumulator.
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Acc {
    /// One entry per UTC second that carried at least one update; empty seconds are supplied from
    /// the window span when the per-second histogram is built (an empty second is a real zero, and
    /// dropping it would inflate every percentile).
    per_sec: BTreeMap<i64, u64>,
    levels: Hist,
    gaps_ms: Hist,
    bytes: Hist,
    updates: u64,
    rows: u64,
    level_payload_bytes: u128,
    levels_total: u128,
    last_ts: Option<i64>,
    first_ts: Option<i64>,
    newest_ts: Option<i64>,
    kinds: BTreeMap<&'static str, u64>,
    /// ⚠ The FOLDED book depth after each update — which is what the `Book` lane puts on the wire,
    /// and it is NOT the same number as `levels` above. §7.1: the server folds and the client
    /// REPLACES, so one frame carries a whole `BookSnapshot`. On a conflating `depth` series the two
    /// agree by construction (every stored update IS a full snapshot); on polymarket's lossless
    /// `book` series the stored updates are DELTAS of one or two levels and the frame is the whole
    /// book, so reading `MD_MAX_DEPTH_LEVELS` off the delta size would understate it by orders of
    /// magnitude.
    folded_levels: Hist,
    /// Sampled framed-JSON size of that folded snapshot, unclamped and at
    /// [`PROPOSED_CLAMP_PER_SIDE`]. Sampled at [`FRAME_SAMPLE_EVERY`].
    frame_bytes_full: Hist,
    frame_bytes_clamped: Hist,
    book: Option<L2Book>,
    book_tick: f64,
}

impl Acc {
    fn note_ts(&mut self, ts: i64) {
        *self.per_sec.entry(ts.div_euclid(1000)).or_default() += 1;
        if let Some(prev) = self.last_ts {
            // A negative delta cannot happen on a `(ts, seq)`-sorted scan, but a chunk boundary is
            // where it WOULD if the sort ever changed — so clamp rather than wrap.
            self.gaps_ms.add(ts.saturating_sub(prev).max(0) as u64);
        }
        self.last_ts = Some(ts);
        self.first_ts.get_or_insert(ts);
        self.newest_ts = Some(ts);
    }

    fn add_book(&mut self, u: &BookUpdate, envelope: usize) {
        let levels = u.bids.len() + u.asks.len();
        let payload = level_json_bytes(&u.bids, &u.asks);
        self.updates += 1;
        // A status marker / degenerate empty event is stored as ONE placeholder row (see
        // `store_kind.rs`'s `book` notes), so `max(1)` is the row count, not a fudge.
        self.rows += levels.max(1) as u64;
        self.levels.add(levels as u64);
        self.bytes.add((envelope + payload).saturating_sub(4) as u64);
        self.level_payload_bytes += payload.saturating_sub(4) as u128;
        self.levels_total += levels as u128;
        *self.kinds.entry(kind_name(u.kind)).or_default() += 1;
        self.note_ts(u.ts);
        self.fold(u, envelope);
    }

    /// Apply the update to a running [`L2Book`] — the SAME fold `vike_model` gives the datahub —
    /// and record the resulting book, which is what one wire frame carries.
    ///
    /// The book is rebuilt when `tick_size` changes: it rides on every event precisely because a
    /// venue can move the grid mid-stream (point-in-time by construction — load-bearing near 0/1 on
    /// polymarket), and a book folded on the old grid would mis-bucket every later price.
    fn fold(&mut self, u: &BookUpdate, envelope: usize) {
        match u.kind {
            BookUpdateKind::Delta | BookUpdateKind::Snapshot => {}
            // A status marker carries no levels and folds nothing; §7.3 says the client CLEARS on
            // one, so the book is dropped rather than left to age.
            _ => {
                self.book = None;
                return;
            }
        }
        if self.book.is_none() || self.book_tick != u.tick_size {
            self.book = Some(L2Book::new(u.tick_size));
            self.book_tick = u.tick_size;
        }
        let book = self.book.as_mut().expect("just built");
        match u.kind {
            BookUpdateKind::Snapshot => book.apply_snapshot(u.seq, &u.bids, &u.asks),
            _ => {
                book.apply_delta(u.seq, &u.bids, &u.asks);
            }
        }
        let depth = book.bid_levels() + book.ask_levels();
        self.folded_levels.add(depth as u64);
        if self.updates.is_multiple_of(FRAME_SAMPLE_EVERY) {
            let (fb, fa) = book.top_n(usize::MAX);
            self.frame_bytes_full
                .add((envelope + level_json_bytes(&fb, &fa)).saturating_sub(4) as u64);
            let (cb, ca) = book.top_n(PROPOSED_CLAMP_PER_SIDE);
            self.frame_bytes_clamped
                .add((envelope + level_json_bytes(&cb, &ca)).saturating_sub(4) as u64);
        }
    }

    fn add_trade(&mut self, t: &TradeTick, envelope: usize) {
        self.updates += 1;
        self.rows += 1;
        self.levels.add(1);
        let payload = serde_json::to_vec(t).map_or(0, |v| v.len());
        self.bytes.add((envelope + payload) as u64);
        self.level_payload_bytes += payload as u128;
        self.levels_total += 1;
        *self.kinds.entry("trade").or_default() += 1;
        self.note_ts(t.ts);
    }

    fn add_quote(&mut self, quote: &QuoteTick, envelope: usize) {
        self.updates += 1;
        self.rows += 1;
        self.levels.add(1);
        let payload = serde_json::to_vec(quote).map_or(0, |v| v.len());
        self.bytes.add((envelope + payload) as u64);
        self.level_payload_bytes += payload as u128;
        self.levels_total += 1;
        *self.kinds.entry("quote").or_default() += 1;
        self.note_ts(quote.ts);
    }

    /// Fold another accumulator in — how the GROUP total is derived from its members without a
    /// second pass over the store. Per-second counts ADD (that is the hub-wide arrival rate);
    /// per-update distributions merge; the inter-arrival gap does NOT merge and is deliberately
    /// left empty on the total, because interleaving two symbols' gaps measures nothing.
    fn merge_totals(&mut self, other: &Acc) {
        for (&s, &c) in &other.per_sec {
            *self.per_sec.entry(s).or_default() += c;
        }
        for (&v, &c) in &other.levels.counts {
            self.levels.add_n(v, c);
        }
        for (&v, &c) in &other.bytes.counts {
            self.bytes.add_n(v, c);
        }
        for (&v, &c) in &other.folded_levels.counts {
            self.folded_levels.add_n(v, c);
        }
        for (&v, &c) in &other.frame_bytes_full.counts {
            self.frame_bytes_full.add_n(v, c);
        }
        for (&v, &c) in &other.frame_bytes_clamped.counts {
            self.frame_bytes_clamped.add_n(v, c);
        }
        for (k, c) in &other.kinds {
            *self.kinds.entry(k).or_default() += c;
        }
        self.updates += other.updates;
        self.rows += other.rows;
        self.level_payload_bytes += other.level_payload_bytes;
        self.levels_total += other.levels_total;
        match (self.first_ts, other.first_ts) {
            (Some(a), Some(b)) => self.first_ts = Some(a.min(b)),
            (None, b) => self.first_ts = b,
            _ => {}
        }
        self.newest_ts = self.newest_ts.max(other.newest_ts);
    }

    /// Updates-per-second as a histogram over EVERY second of the window, empty seconds included.
    fn per_second_hist(&self, from_ms: i64, to_ms: i64) -> Hist {
        let mut h = Hist::default();
        let first = from_ms.div_euclid(1000);
        let last = (to_ms - 1).div_euclid(1000);
        let total_secs = (last - first + 1).max(0) as u64;
        let mut busy = 0u64;
        for &c in self.per_sec.values() {
            h.add(c);
            busy += 1;
        }
        h.add_n(0, total_secs.saturating_sub(busy));
        h
    }

    fn report(&self, title: &str, from_ms: i64, to_ms: i64, book_like: bool) {
        let secs = ((to_ms - from_ms) as f64 / 1000.0).max(1.0);
        println!("\n── {title}");
        println!(
            "  updates={}  rows={}  rows/update={:.2}  mean updates/s={:.2}  mean rows/s={:.2}",
            self.updates,
            self.rows,
            if self.updates == 0 { 0.0 } else { self.rows as f64 / self.updates as f64 },
            self.updates as f64 / secs,
            self.rows as f64 / secs,
        );
        let ps = self.per_second_hist(from_ms, to_ms);
        println!("{}", ps.row("updates per second", ""));
        let empty = ps.counts.get(&0).copied().unwrap_or(0);
        println!("  {:<26} {empty} of {} seconds carried NO update", "silent seconds", ps.n);
        if book_like {
            println!("{}", self.levels.row("STORED levels per update", ""));
            println!("{}", self.folded_levels.row("FOLDED book depth (wire)", ""));
            println!("{}", self.frame_bytes_full.row("wire frame, UNCLAMPED", "B (1/512 sampled)"));
            println!(
                "{}",
                self.frame_bytes_clamped.row("wire frame, clamp 50/side", "B (1/512 sampled)")
            );
        }
        println!("{}", self.gaps_ms.row("inter-arrival gap", "ms"));
        println!(
            "{}",
            self.bytes.row(
                if book_like { "STORED update as a frame" } else { "framed JSON bytes/update" },
                "B"
            )
        );
        if self.levels_total > 0 {
            // On a tick lane one "level" IS one tick, so the same ratio means a different thing and
            // must not wear the book lane's name — the first reading of this table read a trade
            // tape's 117 B/tick as a book's bytes-per-price-level.
            let (label, unit) = if book_like {
                ("bytes per price level", "total level JSON / total levels")
            } else {
                ("bytes per tick payload", "total tick JSON / total ticks")
            };
            println!(
                "  {label:<26} {:.2} B  (measured: {unit})",
                self.level_payload_bytes as f64 / self.levels_total as f64
            );
        }
        if !self.kinds.is_empty() {
            let parts: Vec<String> = self.kinds.iter().map(|(k, c)| format!("{k}={c}")).collect();
            println!("  {:<26} {}", "update kinds", parts.join("  "));
        }
        if let (Some(a), Some(b)) = (self.first_ts, self.newest_ts) {
            println!("  {:<26} {a} .. {b}", "observed ts span");
        }
    }
}

fn kind_name(k: BookUpdateKind) -> &'static str {
    match k {
        BookUpdateKind::Delta => "Delta",
        BookUpdateKind::Snapshot => "Snapshot",
        BookUpdateKind::GapStart => "GapStart",
        BookUpdateKind::Stale => "Stale",
        BookUpdateKind::LiveResume => "LiveResume",
    }
}

// ---------------------------------------------------------------------------------------------
// Arguments.
// ---------------------------------------------------------------------------------------------

struct Args {
    store: String,
    kind: String,
    venue: String,
    symbol: Option<String>,
    group: Option<String>,
    from: i64,
    to: i64,
    chunk_min: i64,
    label: String,
    list: bool,
    symbols_only: bool,
    max_symbols: usize,
}

fn usage() -> String {
    "store_rates — rate distributions of a recorded series, read through the store's own engine\n\
     \n\
     store_rates --store <root> --list\n\
     store_rates --store <root> --kind <k> --venue <v> --group <g> --from <ms> --to <ms> \
     --symbols\n\
     store_rates --store <root> --kind <depth|book|trade|quote> --venue <v> \
     (--symbol <s> | --group <g>) --from <ms> --to <ms> [--chunk-min N] [--max-symbols N] \
     [--label TEXT]\n"
        .to_string()
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut a = Args {
        store: String::new(),
        kind: String::new(),
        venue: String::new(),
        symbol: None,
        group: None,
        from: 0,
        to: 0,
        chunk_min: 10,
        label: String::new(),
        list: false,
        symbols_only: false,
        max_symbols: 64,
    };
    let mut it = std::env::args().skip(1);
    while let Some(f) = it.next() {
        let mut val = || -> Result<String, Box<dyn Error>> {
            it.next().ok_or_else(|| -> Box<dyn Error> { format!("{f} needs a value").into() })
        };
        match f.as_str() {
            "--store" => a.store = val()?,
            "--kind" => a.kind = val()?,
            "--venue" => a.venue = val()?,
            "--symbol" => a.symbol = Some(val()?),
            "--group" => a.group = Some(val()?),
            "--from" => a.from = val()?.parse()?,
            "--to" => a.to = val()?.parse()?,
            "--chunk-min" => a.chunk_min = val()?.parse()?,
            "--max-symbols" => a.max_symbols = val()?.parse()?,
            "--label" => a.label = val()?,
            "--list" => a.list = true,
            "--symbols" => a.symbols_only = true,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag {other}\n\n{}", usage()).into()),
        }
    }
    if a.store.is_empty() {
        return Err(format!("--store is required\n\n{}", usage()).into());
    }
    Ok(a)
}

// ---------------------------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    // READ-ONLY: no root creation, no WAL recovery sweep, no series lock. See the constructor's doc
    // — the store this is pointed at has a live recorder writing into it.
    let store = DataFusionHist::open_read_only(&args.store)?;
    println!("store            {}", args.store);
    println!(
        "sizeof           TradeTick={}B  QuoteTick={}B  BookUpdate={}B  Level={}B",
        std::mem::size_of::<TradeTick>(),
        std::mem::size_of::<QuoteTick>(),
        std::mem::size_of::<BookUpdate>(),
        std::mem::size_of::<Level>(),
    );

    if args.list {
        println!("\n── series in the store");
        for id in store.list_series()? {
            let c = store.series_coverage(&id)?;
            println!(
                "  kind={:<12} venue={:<12} symbol={:<24} group={:<16} rows={:<14} bytes={:<14} \
                 parts={:<6} dates={:<4} first={} last={}",
                id.kind,
                id.venue,
                id.symbol,
                id.group.clone().unwrap_or_default(),
                c.rows,
                c.bytes,
                c.parts,
                c.dates,
                c.first_ts,
                c.last_ts,
            );
        }
        return Ok(());
    }

    if args.kind.is_empty() || args.venue.is_empty() {
        return Err(format!("--kind and --venue are required\n\n{}", usage()).into());
    }
    if args.to <= args.from {
        return Err(format!("--to must be after --from\n\n{}", usage()).into());
    }

    // Which instrument(s). A per-symbol series names itself; a GROUPED one is asked what it holds
    // IN THIS WINDOW — a rolling family mints new instruments continuously, so the member set is a
    // function of the window rather than a constant.
    let symbols: Vec<String> = match (&args.symbol, &args.group) {
        (Some(s), _) => vec![s.clone()],
        (None, Some(g)) => {
            let id = SeriesId::grouped(args.kind.as_str(), args.venue.as_str(), g.as_str());
            let mut got = store.series_symbols(&id, TsRange::of(args.from, args.to - 1))?;
            println!("\ngroup {g} holds {} symbol(s) in this window", got.len());
            for s in &got {
                println!("  {s}");
            }
            if args.symbols_only {
                return Ok(());
            }
            if got.len() > args.max_symbols {
                println!(
                    "  ⚠ TRUNCATED to --max-symbols={} — the totals below are a SUBSET, not the \
                     group",
                    args.max_symbols
                );
                got.truncate(args.max_symbols);
            }
            got
        }
        (None, None) => {
            return Err(format!("one of --symbol / --group is required\n\n{}", usage()).into());
        }
    };

    let book_like = matches!(args.kind.as_str(), "depth" | "book");
    let chunk_ms = args.chunk_min.max(1) * 60_000;
    let mut total = Acc::default();
    let mut per_symbol: Vec<(String, Acc)> = Vec::new();

    for sym in &symbols {
        let envelope = if book_like {
            depth_envelope_bytes(&args.venue, sym)
        } else {
            trades_envelope_bytes(&args.venue, sym)
        };
        let mut acc = Acc::default();
        let mut chunk_start = args.from;
        while chunk_start < args.to {
            let chunk_end = (chunk_start + chunk_ms).min(args.to);
            // `TsRange::of` is INCLUSIVE at both ends, so the sub-windows must abut, never overlap.
            let range = TsRange::of(chunk_start, chunk_end - 1);
            match args.kind.as_str() {
                "depth" => {
                    for u in store.scan_depth(&args.venue, sym, range)? {
                        acc.add_book(&u, envelope);
                    }
                }
                "book" => {
                    for u in store.scan_book_updates(&args.venue, sym, range)? {
                        acc.add_book(&u, envelope);
                    }
                }
                "trade" => {
                    for t in store.scan_trades(&args.venue, sym, range)? {
                        acc.add_trade(&t, envelope);
                    }
                }
                "quote" => {
                    for quote in store.scan_quotes(&args.venue, sym, range)? {
                        acc.add_quote(&quote, envelope);
                    }
                }
                other => return Err(format!("unsupported --kind {other}").into()),
            }
            chunk_start = chunk_end;
        }
        println!(
            "\n════ {}{} kind={} venue={} symbol={}  window=[{}, {}) = {:.1} min  envelope={}B",
            args.label,
            if args.label.is_empty() { "" } else { " · " },
            args.kind,
            args.venue,
            sym,
            args.from,
            args.to,
            (args.to - args.from) as f64 / 60_000.0,
            envelope,
        );
        acc.report("this instrument", args.from, args.to, book_like);
        total.merge_totals(&acc);
        per_symbol.push((sym.clone(), acc));
    }

    if per_symbol.len() > 1 {
        println!(
            "\n════ GROUP TOTAL over {} instruments — per-second counts ADD (this is the arrival \
             rate one hub sees); the inter-arrival row is deliberately EMPTY, because interleaving \
             several instruments' gaps measures nothing",
            per_symbol.len()
        );
        total.report("all instruments", args.from, args.to, book_like);
    }
    Ok(())
}
