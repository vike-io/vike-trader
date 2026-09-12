//! Raw-frame re-parse (raw-first WS frame tap, docs/superpowers/specs/2026-07-11-raw-frame-tap-design.md):
//! the offline twin of the live Polymarket feed. [`GzFileStream`] replays captured raw frames as a
//! `vike_polymarket::MarketStream`, so `run_session` re-decodes them through the SAME pump; the
//! `poly_reparse` bin wires it to a `RecorderSink` to regenerate normalized ticks into the store.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use flate2::read::GzDecoder;
use vike_polymarket::{MarketStream, StreamErr};

/// Shared receive-ns of the frame currently being decoded — written by [`GzFileStream`] per
/// `read_frame`, read by `LocalTsRewriteSink` (Task 5) to restore the captured receive time as
/// `local_ts` on the regenerated ticks.
pub type RecvNsCell = Arc<AtomicI64>;

/// Replays captured raw frames as a `MarketStream`. `read_frame` yields each gz line's JSON text
/// (everything after the first `\t`), writing that line's `local_ns` (the prefix) into the shared
/// cell first; at end of all files it returns `Closed("eof")` — the clean termination `run_session`
/// treats like a normal disconnect (identical to the scripted tests' end-of-script). `send_text` is
/// a no-op and `since_last_frame` is `ZERO`, so the idle/keepalive watchdog never trips on a replay.
pub struct GzFileStream {
    files: std::vec::IntoIter<PathBuf>,
    cur: Option<std::io::Lines<BufReader<GzDecoder<std::fs::File>>>>,
    cell: RecvNsCell,
}

impl GzFileStream {
    pub fn new(files: Vec<PathBuf>, cell: RecvNsCell) -> Self {
        Self { files: files.into_iter(), cur: None, cell }
    }

    /// Open the next file's line reader; returns false when no files remain.
    fn open_next(&mut self) -> bool {
        for path in self.files.by_ref() {
            match std::fs::File::open(&path) {
                Ok(f) => {
                    self.cur = Some(BufReader::new(GzDecoder::new(f)).lines());
                    return true;
                }
                Err(e) => {
                    tracing::warn!(?path, %e, "poly_reparse: skipping unreadable raw file");
                }
            }
        }
        false
    }
}

impl MarketStream for GzFileStream {
    fn read_frame(&mut self) -> Result<String, StreamErr> {
        loop {
            if self.cur.is_none() && !self.open_next() {
                return Err(StreamErr::Closed("eof".into()));
            }
            // read the next line of the current file; on a torn/failed line, stop this file
            let next = self.cur.as_mut().and_then(|it| it.next());
            match next {
                Some(Ok(line)) => {
                    // split ONCE on the first tab: <local_ns>\t<json>
                    if let Some((ns, text)) = line.split_once('\t') {
                        if let Ok(n) = ns.parse::<i64>() {
                            self.cell.store(n, Ordering::Relaxed);
                        }
                        return Ok(text.to_string());
                    }
                    // malformed line (no tab) — skip it, keep reading this file
                }
                Some(Err(_)) | None => {
                    self.cur = None; // torn tail or clean EOF of this file → advance to the next
                }
            }
        }
    }

    fn send_text(&mut self, _s: &str) -> Result<(), StreamErr> {
        Ok(()) // no-op: nothing to send on a replay
    }

    fn since_last_frame(&self) -> Duration {
        Duration::ZERO // never idle: a replay must not trip the idle watchdog
    }
}

/// Sorted list of `date=`-partitioned gz files for a token in `[from_date, to_date]` inclusive
/// (dates as `YYYY-MM-DD`). Layout mirrors the `RawTap` writer:
/// `<root>/polymarket/<token>/date=YYYY-MM-DD/frames-*.jsonl.gz`.
pub fn gz_files_for(
    root: &std::path::Path,
    token: &str,
    from_date: &str,
    to_date: &str,
) -> Vec<PathBuf> {
    let token_dir = root.join("polymarket").join(token);
    let mut out = Vec::new();
    let Ok(days) = std::fs::read_dir(&token_dir) else {
        return out;
    };
    let mut day_dirs: Vec<(String, PathBuf)> = Vec::new();
    for e in days.flatten() {
        let p = e.path();
        if let Some(name) = p.file_name().and_then(|n| n.to_str())
            && let Some(date) = name.strip_prefix("date=")
            && date >= from_date
            && date <= to_date
        {
            day_dirs.push((date.to_string(), p));
        }
    }
    day_dirs.sort_by(|a, b| a.0.cmp(&b.0)); // ascending by date
    for (_date, dir) in day_dirs {
        let mut parts: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "gz"))
            .collect();
        parts.sort(); // frames-00000000, frames-00000001, ... (zero-padded → lexical == numeric)
        out.extend(parts);
    }
    out
}

use vike_data::LiveDataSink;
use vike_model::{Bar, BookUpdate, L2Book, QuoteTick, TradeTick};

/// A `LiveDataSink` decorator that restores the captured receive-ns (from the shared [`RecvNsCell`]
/// the `GzFileStream` updates per frame) as `local_ts` on every regenerated tick, then forwards to
/// `inner` (a `RecorderSink`). All ticks a single frame produces (a `book_update` and its derived
/// `quote`) share that frame's cell value, so the one cell is correct. The folded `book` verb and
/// the bar/status verbs forward unchanged (they carry no `local_ts`, or none is recorded).
pub struct LocalTsRewriteSink {
    inner: Arc<dyn LiveDataSink>,
    cell: RecvNsCell,
}

/// Same ns→ms divisor as `databento::parse::NS_PER_MS`; duplicated (not imported) because
/// `poly-reparse` and `databento` are independent Cargo features and this module must compile
/// without pulling in the other's module tree.
const NS_PER_MS: i64 = 1_000_000;

impl LocalTsRewriteSink {
    pub fn new(inner: Arc<dyn LiveDataSink>, cell: RecvNsCell) -> Self {
        Self { inner, cell }
    }
    /// The captured receive time, epoch-**ms** — `local_ts`'s contractual unit (see
    /// `vike_model::bar`/`orderbook` docs) — converted from the tap's raw receive-ns.
    fn local_ts_ms(&self) -> i64 {
        self.cell.load(Ordering::Relaxed) / NS_PER_MS
    }
}

impl LiveDataSink for LocalTsRewriteSink {
    fn seed_bars(&self, v: &str, s: &str, i: &str, b: Vec<Bar>) {
        self.inner.seed_bars(v, s, i, b);
    }
    fn close_bar(&self, v: &str, s: &str, i: &str, b: Bar) {
        self.inner.close_bar(v, s, i, b);
    }
    fn forming_bar(&self, v: &str, s: &str, i: &str, b: Bar) {
        self.inner.forming_bar(v, s, i, b);
    }
    fn mark_tick(&self, v: &str, s: &str, p: f64, t: i64) {
        self.inner.mark_tick(v, s, p, t);
    }
    fn bar_close_tick(&self, v: &str, s: &str, p: f64, t: i64) {
        // Forward, not the trait default (a no-op) — this sink is a transparent wrapper.
        self.inner.bar_close_tick(v, s, p, t);
    }
    fn quote(&self, v: &str, s: &str, mut q: QuoteTick) {
        q.local_ts = self.local_ts_ms();
        self.inner.quote(v, s, q);
    }
    fn trade(&self, v: &str, s: &str, mut t: TradeTick) {
        t.local_ts = self.local_ts_ms();
        self.inner.trade(v, s, t);
    }
    fn book(&self, v: &str, s: &str, b: Arc<L2Book>) {
        self.inner.book(v, s, b);
    }
    fn book_update(&self, v: &str, s: &str, mut u: BookUpdate) {
        u.local_ts = self.local_ts_ms();
        self.inner.book_update(v, s, u);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use std::sync::Mutex;
    use vike_data::LiveDataSink;
    use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

    #[derive(Default)]
    struct CapturingSink {
        quotes: Mutex<Vec<QuoteTick>>,
        trades: Mutex<Vec<TradeTick>>,
        books: Mutex<Vec<BookUpdate>>,
    }
    impl LiveDataSink for CapturingSink {
        fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_model::Bar>) {}
        fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
        fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_model::Bar) {}
        fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
        fn quote(&self, _v: &str, _s: &str, q: QuoteTick) {
            self.quotes.lock().unwrap().push(q);
        }
        fn trade(&self, _v: &str, _s: &str, t: TradeTick) {
            self.trades.lock().unwrap().push(t);
        }
        fn book(&self, _v: &str, _s: &str, _b: Arc<vike_model::L2Book>) {}
        fn book_update(&self, _v: &str, _s: &str, u: BookUpdate) {
            self.books.lock().unwrap().push(u);
        }
    }

    fn write_gz(path: &std::path::Path, lines: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = std::fs::File::create(path).unwrap();
        let mut enc = GzEncoder::new(f, Compression::default());
        for l in lines {
            enc.write_all(l.as_bytes()).unwrap();
            enc.write_all(b"\n").unwrap();
        }
        enc.finish().unwrap();
    }

    #[test]
    fn rewrites_local_ts_from_cell_on_quote_trade_book() {
        let cell: RecvNsCell = Arc::new(AtomicI64::new(0));
        let inner = Arc::new(CapturingSink::default());
        let sink = LocalTsRewriteSink::new(inner.clone(), cell.clone());

        // cell holds the tap's raw receive-ns; local_ts is contractually epoch-ms, so the sink
        // must divide by NS_PER_MS (1_000_000) — these values are chosen NOT to be a multiple of
        // 1e6 (…111_222 rather than a round …000_000) so a regression back to raw-ns would fail
        // this assertion instead of passing it by coincidence.
        cell.store(111_222_333, Ordering::Relaxed);
        sink.quote(
            "polymarket",
            "TOK",
            QuoteTick {
                ts: 1,
                local_ts: 999,
                bid: 0.4,
                ask: 0.5,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        );
        cell.store(222_333_444, Ordering::Relaxed);
        sink.trade(
            "polymarket",
            "TOK",
            TradeTick {
                ts: 2,
                local_ts: 999,
                price: 0.45,
                size: 1.0,
                is_buyer_maker: false,
                symbol: String::new(),
            },
        );
        cell.store(333_444_555, Ordering::Relaxed);
        sink.book_update(
            "polymarket",
            "TOK",
            BookUpdate {
                ts: 3,
                local_ts: 999,
                seq: 1,
                kind: BookUpdateKind::Snapshot,
                tick_size: 0.01,
                bids: vec![(0.4, 1.0)],
                asks: vec![(0.5, 1.0)],
                symbol: String::new(),
            },
        );

        assert_eq!(inner.quotes.lock().unwrap()[0].local_ts, 111, "quote local_ts <- cell ns/1e6");
        assert_eq!(inner.trades.lock().unwrap()[0].local_ts, 222, "trade local_ts <- cell ns/1e6");
        assert_eq!(
            inner.books.lock().unwrap()[0].local_ts,
            333,
            "book_update local_ts <- cell ns/1e6"
        );
        // payload otherwise untouched
        assert_eq!(inner.quotes.lock().unwrap()[0].bid.to_bits(), 0.4f64.to_bits());
        assert_eq!(inner.books.lock().unwrap()[0].seq, 1);
    }

    #[test]
    fn reads_lines_in_order_strips_prefix_updates_cell_then_closed() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("polymarket/TOK/date=1970-01-01/frames-00000000.jsonl.gz");
        write_gz(&f1, &["1000\t{\"a\":1}", "2000\t{\"b\":2}"]);
        let cell: RecvNsCell = Arc::new(AtomicI64::new(-1));
        let mut s = GzFileStream::new(vec![f1], cell.clone());

        assert_eq!(s.read_frame().unwrap(), "{\"a\":1}");
        assert_eq!(cell.load(Ordering::Relaxed), 1000, "cell = current frame's local_ns");
        assert_eq!(s.read_frame().unwrap(), "{\"b\":2}");
        assert_eq!(cell.load(Ordering::Relaxed), 2000);
        assert!(matches!(s.read_frame(), Err(StreamErr::Closed(_))), "Closed at EOF");
        assert_eq!(s.since_last_frame(), Duration::ZERO, "never idle");
    }

    #[test]
    fn spans_multiple_day_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("d1.jsonl.gz");
        let f2 = dir.path().join("d2.jsonl.gz");
        write_gz(&f1, &["1\t{\"x\":1}"]);
        write_gz(&f2, &["2\t{\"x\":2}"]);
        let cell: RecvNsCell = Arc::new(AtomicI64::new(-1));
        let mut s = GzFileStream::new(vec![f1, f2], cell);
        assert_eq!(s.read_frame().unwrap(), "{\"x\":1}");
        assert_eq!(s.read_frame().unwrap(), "{\"x\":2}"); // rolls to the next file
        assert!(matches!(s.read_frame(), Err(StreamErr::Closed(_))));
    }
}
