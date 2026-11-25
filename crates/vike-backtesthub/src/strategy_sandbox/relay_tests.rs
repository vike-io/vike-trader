//! The bar relay through REAL OS pipes: the parent's writer ([`super::answer`]) on one end, the child's reader
//! (`PipeHistStore::load_bars`, under the 8 KiB buffer `std::io::stdin` gives it) on the other, measured with the
//! down pipe at the default size and widened (`vike_sandbox::widen_pipe`, as `super::run` widens it). And
//! (`cached`) a bar read answered from the daemon's BAR CACHE: encoded straight from its kept days, the frames
//! byte for byte the ones the copied answer was encoded into, and the cost of that copy measured against it.

use std::io::{self, BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_hub_protocol::child_pipe::read_message;
use vike_marketdata::Bar;

use super::super::pipe_store::PipeHistStore;
use super::super::wire::ChildFrame;

/// The child's `read` calls on its channel, counted.
struct Counted<R>(R, Arc<AtomicUsize>);

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.1.fetch_add(1, Ordering::Relaxed);
        self.0.read(buf)
    }
}

/// This thread's voluntary context switches (Linux's `/proc/thread-self/status`): each one a block, here in
/// `pipe_read`. `None` elsewhere.
fn blocks() -> Option<u64> {
    std::fs::read_to_string("/proc/thread-self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))?
        .trim()
        .parse()
        .ok()
}

/// `n` plain 1m bars, as a binance kline series crosses.
fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let price = 60_000.0 + (i % 997) as f64 / 7.0;
            let volume = 1.0 + (i % 13) as f64;
            Bar::ohlcv(60_000 * i as i64, price, price + 3.0, price - 3.0, price + 1.0, volume)
        })
        .collect()
}

/// One FIRST read of `rows` by a fresh child over fresh pipes, the down pipe widened when `wide`: its wall time,
/// the child's read calls and its blocks. Every bar crosses whole.
fn one_read(rows: &[Bar], wide: bool) -> (Duration, usize, Option<u64>) {
    let store = HistoryStoreMemory::new();
    store.append_bars("binance", "BTCUSDT", "1m", rows, None).expect("seed");
    let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
    let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
    if wide {
        vike_sandbox::widen_pipe(&to_child);
    }
    let parent = std::thread::spawn(move || {
        while let Ok(ChildFrame::Read(read)) = read_message(&mut from_child) {
            super::answer(&mut to_child, &store, read).expect("answer");
        }
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let input = BufReader::new(Counted(child_in, Arc::clone(&calls)));
    let pipe = PipeHistStore::new(Box::new(input), Box::new(child_out));
    let (before, started) = (blocks(), Instant::now());
    let got = pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("piped");
    let (taken, after) = (started.elapsed(), blocks());
    assert!(got == rows, "every bar crosses whole");
    drop(pipe);
    parent.join().expect("the parent's half");
    (taken, calls.load(Ordering::Relaxed), before.zip(after).map(|(b, a)| a - b))
}

/// The widened pipe carries the same bars as the default one.
#[test]
fn a_widened_pipe_crosses_the_same_bars() {
    let rows = bars(40_000);
    one_read(&rows, false);
    one_read(&rows, true);
}

/// **263,521 1m bars** (the CI box's binance BTCUSDT backtest of 2026-11-19), a first read through fresh pipes, five
/// times each way: the median wall time, the child's read calls and its blocks. Run in release, alone.
#[test]
#[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_relay_pipe -- --ignored --nocapture"]
fn measure_the_relay_pipe() {
    let rows = bars(263_521);
    for wide in [false, true] {
        let mut runs: Vec<_> = (0..5).map(|_| one_read(&rows, wide)).collect();
        runs.sort_by_key(|run| run.0);
        let (taken, calls, blocks) = runs[2];
        println!(
            "relay pipe {}: median {taken:?} (best {:?}), {calls} read calls, {blocks:?} blocks",
            if wide { "widened" } else { "default" },
            runs[0].0,
        );
    }
}

/// A bar read answered from the daemon's bar cache (`crate::bar_cache`), against the answer it was before: the kept
/// days COPIED into one `Vec` (`CachedHistoryStore::read` and `send_bars` as of `8d37d5fb2a`, kept here as
/// `old_hit` and `old_send`).
mod cached {
    use std::hash::{DefaultHasher, Hasher};
    use std::io::{self, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use vike_data::{BarDay, HistoryStore, HistoryStoreError, TimestampRange};
    use vike_datahub_client::proto::encode_bar_columns_into;
    use vike_marketdata::Bar;
    use vike_model::MS_PER_DAY;

    use crate::bar_cache::CachedHistoryStore;

    use vike_hub_protocol::child_pipe::write_message;

    use super::super::super::wire::{BARS_PER_FRAME, ReadAnswerFrame, StoreRead};
    use super::super::answer;

    const VENUE: &str = "binance";
    const LANE: &str = "user-x";
    const SYMBOL: &str = "BTCUSDT";
    const INTERVAL: &str = "1m";
    /// The day the cache's clock is in: every stored day is past.
    const TODAY: i64 = 1_000;

    fn day_of(timestamp: i64) -> i64 {
        timestamp.div_euclid(MS_PER_DAY)
    }

    /// `range` widened to the whole UTC days it touches.
    fn whole_days(range: TimestampRange) -> TimestampRange {
        TimestampRange {
            start: range.start.map(|s| day_of(s) * MS_PER_DAY),
            end: range.end.map(|e| (day_of(e) + 1) * MS_PER_DAY - 1),
        }
    }

    /// A store of `bars` (ascending) with the stamps the bar cache keeps days by (the datahub's `SeriesDays`):
    /// every day of the range that holds a bar, stamped once and settled. A lane read answers the same bars, a
    /// budget their head. Its bar reads are counted. With `page` set, a read is handed over as columns pages of that
    /// many bars (`read_columns`, the datahub's handle standing in); 0 answers rows only.
    struct Stamped {
        bars: Vec<Bar>,
        reads: AtomicUsize,
        page: usize,
    }

    impl Stamped {
        fn within(&self, range: TimestampRange) -> &[Bar] {
            let from = self.bars.partition_point(|b| range.start.is_some_and(|s| b.timestamp < s));
            let to = self.bars.partition_point(|b| range.end.is_none_or(|e| b.timestamp <= e));
            &self.bars[from..to.max(from)]
        }
    }

    impl HistoryStore for Stamped {
        fn load_bars(
            &self,
            _venue: &str,
            _symbol: &str,
            _interval: &str,
            range: TimestampRange,
        ) -> Result<Vec<Bar>, HistoryStoreError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.within(range).to_vec())
        }

        fn load_bars_sourced_capped(
            &self,
            venue: &str,
            _source: &str,
            symbol: &str,
            interval: &str,
            range: TimestampRange,
            budget: Option<usize>,
        ) -> Result<Vec<Bar>, HistoryStoreError> {
            let mut bars = self.load_bars(venue, symbol, interval, range)?;
            bars.truncate(budget.unwrap_or(usize::MAX));
            Ok(bars)
        }

        fn series_days(
            &self,
            _kind: vike_model::StoreKindId,
            _venue: &str,
            _source: Option<&str>,
            _symbol: &str,
            _interval: Option<&str>,
            range: TimestampRange,
        ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
            let (mut rows, mut days) = (self.within(whole_days(range)), Vec::new());
            while let Some(first) = rows.first() {
                let day = day_of(first.timestamp);
                days.push(BarDay { day, stamp: 1, provisional: false });
                rows = &rows[rows.partition_point(|b| day_of(b.timestamp) == day)..];
            }
            Ok(Some(days))
        }

        fn read_columns(
            &self,
            kind: vike_model::StoreKindId,
            _venue: &str,
            _source: Option<&str>,
            _symbol: &str,
            _interval: Option<&str>,
            range: TimestampRange,
        ) -> Option<Result<vike_data::SharedColumns, HistoryStoreError>> {
            if self.page == 0 || !matches!(kind, vike_model::StoreKindId::Bar) {
                return None;
            }
            self.reads.fetch_add(1, Ordering::Relaxed);
            let magic = &vike_datahub_client::proto::BAR_COLUMNS;
            Some(Ok(super::ticks::pages_of(magic, self.within(range), self.page)))
        }

        vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
            scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
    }

    /// The daemon's bar cache over `store`, its clock past every stored day.
    fn cache(store: &Arc<Stamped>) -> CachedHistoryStore {
        CachedHistoryStore::new(store.clone(), 1_024, 0, Box::new(|| TODAY * MS_PER_DAY))
    }

    /// One bar a minute over `days` days from day 0: prices from the index, a NaN close, an infinite volume, a NaN
    /// bid (it crosses absent) and a negative-zero ask now and then, and runs of a symbol and of none that straddle
    /// the days, so a part joined from several days crosses every column and the symbol runs.
    fn minutes(days: i64) -> Vec<Bar> {
        (0..days * 1_440)
            .map(|i| Bar {
                timestamp: i * 60_000,
                open: 60_000.0 + (i % 997) as f64 / 7.0,
                high: 60_003.0 + (i % 997) as f64 / 7.0,
                low: 59_997.0,
                close: if i % 4_001 == 5 { f64::NAN } else { 60_001.0 + (i % 13) as f64 },
                volume: if i % 5_003 == 6 { f64::INFINITY } else { 1.0 + (i % 13) as f64 },
                funding: (i % 3 == 0).then_some(1e-4),
                bid: (i % 5 == 0).then_some(if i % 10 == 0 { f64::NAN } else { 59_999.0 }),
                ask: (i % 11 == 0).then_some(-0.0),
                symbol: (i / 1_000 % 3 != 0).then(|| SYMBOL.into()),
            })
            .collect()
    }

    /// The bar reads a run makes of `range`: the series, its lane unbudgeted, and its lane under a budget (which the
    /// cache forwards).
    fn reads(range: TimestampRange) -> [StoreRead; 3] {
        let lane = |budget| StoreRead::LoadBarsSourcedCapped {
            venue: VENUE.to_string(),
            source: LANE.to_string(),
            symbol: SYMBOL.to_string(),
            interval: INTERVAL.to_string(),
            range,
            budget,
        };
        let series = StoreRead::LoadBars {
            venue: VENUE.to_string(),
            symbol: SYMBOL.to_string(),
            interval: INTERVAL.to_string(),
            range,
        };
        [series, lane(None), lane(Some(BARS_PER_FRAME + 5))]
    }

    /// The relay's bar answer as it was written before it was encoded from shared runs: the count, the rows'
    /// `chunks` of `BARS_PER_FRAME`, each encoded from its own slice, then `Done`; the rows dropped on a thread of
    /// their own.
    fn old_send(
        out: &mut impl Write,
        answer: Result<Vec<Bar>, HistoryStoreError>,
    ) -> io::Result<()> {
        let rows = match answer {
            Ok(rows) => rows,
            Err(e) => return write_message(out, &ReadAnswerFrame::<()>::Failed(e)),
        };
        write_message(out, &ReadAnswerFrame::<()>::Count(rows.len()))?;
        let mut frame = Vec::new();
        for part in rows.chunks(BARS_PER_FRAME) {
            frame.clear();
            frame.extend_from_slice(&[0; 4]);
            encode_bar_columns_into(&mut frame, part);
            let len = u32::try_from(frame.len() - 4).expect("a part under the cap");
            frame[..4].copy_from_slice(&len.to_be_bytes());
            out.write_all(&frame)?;
        }
        let _ = std::thread::Builder::new().spawn(move || drop(rows));
        write_message(out, &ReadAnswerFrame::<()>::Done)
    }

    /// The bar cache's HIT as it was answered before: every kept day of `range` copied whole into a `Vec`, then cut
    /// to the range with `retain`. The kept days are `cached`'s own, asked whole.
    fn old_hit(cached: &CachedHistoryStore, range: TimestampRange) -> Vec<Bar> {
        let mut out =
            cached.load_bars(VENUE, SYMBOL, INTERVAL, whole_days(range)).expect("the kept days");
        out.retain(|b| {
            range.start.is_none_or(|s| b.timestamp >= s)
                && range.end.is_none_or(|e| b.timestamp <= e)
        });
        out
    }

    /// What the relay wrote before for `read`: the store's own answer (the bar cache answered those rows too,
    /// `bar_cache_tests.rs`), through [`old_send`].
    fn old_frames(store: &Stamped, read: &StoreRead) -> Vec<u8> {
        let answer = match read {
            StoreRead::LoadBars { venue, symbol, interval, range } => {
                store.load_bars(venue, symbol, interval, *range)
            }
            StoreRead::LoadBarsSourcedCapped { venue, source, symbol, interval, range, budget } => {
                store.load_bars_sourced_capped(venue, source, symbol, interval, *range, *budget)
            }
            other => unreachable!("not a bar read: {other:?}"),
        };
        let mut frames = Vec::new();
        old_send(&mut frames, answer).expect("encode");
        frames
    }

    /// **A bar read served from the cache crosses in the frames the copied answer was encoded into, byte for
    /// byte**: a window inside one kept day, one across whole days, one cut at both ends (its parts each joined from
    /// several days and cut mid-day), an empty window inside a kept day and one in a gap no day lists; each as the
    /// series, its lane and its lane under a budget, read first (the cache fetches and keeps the days) and again (a
    /// hit, which reads no store). The copy the cache made before answered those same frames.
    #[test]
    fn a_cached_read_crosses_in_the_frames_of_the_copy() {
        for page in [0, 1_000] {
            crosses_in_the_frames_of_the_copy(page);
        }
    }

    /// [`a_cached_read_crosses_in_the_frames_of_the_copy`] over a store that hands its bars over in pages of `page`
    /// (1,000 bars: pages straddle the day edges), or as rows with 0.
    fn crosses_in_the_frames_of_the_copy(page: usize) {
        let store = Arc::new(Stamped { bars: minutes(40), reads: AtomicUsize::new(0), page });
        let at = |day: i64, minute: i64| day * MS_PER_DAY + minute * 60_000;
        let windows = [
            ("inside one day", TimestampRange::of(at(5, 100), at(5, 900))),
            ("across whole days", TimestampRange::of(at(2, 0), at(14, 0) - 1)),
            ("cut at both ends", TimestampRange::of(at(1, 421), at(30, 779))),
            ("empty, inside a kept day", TimestampRange::of(at(3, 0) + 10_000, at(3, 0) + 50_000)),
            ("empty, in a gap", TimestampRange::of(at(50, 0), at(60, 0))),
        ];
        for (what, range) in windows {
            let cached = cache(&store);
            for read in reads(range) {
                let old = old_frames(&store, &read);
                let kept = !what.ends_with("gap")
                    && !matches!(read, StoreRead::LoadBarsSourcedCapped { budget: Some(_), .. });
                for pass in ["first read", "again"] {
                    let before = store.reads.load(Ordering::Relaxed);
                    let mut new = Vec::new();
                    answer(&mut new, &cached, read.clone()).expect("answer");
                    assert!(
                        new == old,
                        "{what}, {pass}, {read:?}: {} bytes, not {}",
                        new.len(),
                        old.len()
                    );
                    if pass == "again" && kept {
                        let after = store.reads.load(Ordering::Relaxed);
                        assert_eq!(after, before, "{what}, {read:?}: a hit reads no store");
                    }
                }
            }
            let mut copied = Vec::new();
            old_send(&mut copied, Ok(old_hit(&cached, range))).expect("encode");
            assert!(copied == old_frames(&store, &reads(range)[0]), "{what}: the copy's frames");
            assert_eq!(cached.kept_bytes() > 0, !what.ends_with("gap"), "{what}: days kept");
        }
    }

    /// A pipe that keeps nothing: when its first byte came, how many came, and their digest.
    pub(super) struct Pipe {
        started: Instant,
        first: Option<Duration>,
        bytes: usize,
        digest: DefaultHasher,
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.first.is_none() {
                self.first = Some(self.started.elapsed());
            }
            self.bytes += buf.len();
            self.digest.write(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// This process's minor page faults (`/proc/self/stat`'s tenth field): each a fresh 4 KiB page touched.
    fn minor_faults() -> Option<u64> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().nth(7)?.parse().ok()
    }

    /// Process `pid`'s CPU so far (`/proc/<pid>/stat`'s `utime` and `stime`, every thread's, ended ones included), in
    /// milliseconds at Linux's fixed 100 ticks a second.
    pub(super) fn cpu_ms(pid: &str) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let mut ticks = stat.rsplit_once(')')?.1.split_whitespace().skip(11).map(str::parse::<u64>);
        let (user, system) = (ticks.next()?.ok()?, ticks.next()?.ok()?);
        Some((user + system) * 10)
    }

    /// A `/proc/self/status` line's kB.
    pub(super) fn status_kib(field: &str) -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find_map(|line| line.strip_prefix(field))?;
        line.trim().trim_end_matches("kB").trim().parse().ok()
    }

    /// What one serve took ([`one_serve`]).
    pub(super) struct Served {
        pub(super) first: Duration,
        pub(super) last: Duration,
        pub(super) bytes: usize,
        /// Minor page faults, `None` where `/proc` cannot say.
        pub(super) faults: Option<u64>,
        /// The peak resident growth over the resident set before, in KiB; `None` where `/proc` cannot say.
        pub(super) peak_kib: Option<u64>,
        /// This process's CPU over the serve, in milliseconds ([`cpu_ms`]); `None` where `/proc` cannot say.
        pub(super) cpu_ms: Option<u64>,
        /// The bytes' digest (`DefaultHasher`, the same in every build of one toolchain).
        pub(super) digest: u64,
    }

    /// One serve into a fresh [`Pipe`].
    pub(super) fn one_serve(serve: &dyn Fn(&mut Pipe) -> io::Result<()>) -> Served {
        // The last round's rows are freed on a thread of their own: let it finish.
        std::thread::sleep(Duration::from_millis(100));
        let reset = std::fs::write("/proc/self/clear_refs", "5").is_ok();
        let (resident, faults, cpu) = (status_kib("VmRSS:"), minor_faults(), cpu_ms("self"));
        let mut pipe =
            Pipe { started: Instant::now(), first: None, bytes: 0, digest: DefaultHasher::new() };
        serve(&mut pipe).expect("serve");
        let last = pipe.started.elapsed();
        let cpu_ms = cpu_ms("self").zip(cpu).map(|(after, before)| after - before);
        let faults = minor_faults().zip(faults).map(|(after, before)| after - before);
        let peak_kib = status_kib("VmHWM:")
            .zip(resident)
            .filter(|_| reset)
            .map(|(peak, before)| peak.saturating_sub(before));
        Served {
            first: pipe.first.expect("a first byte"),
            last,
            bytes: pipe.bytes,
            faults,
            peak_kib,
            cpu_ms,
            digest: pipe.digest.finish(),
        }
    }

    /// **What a bar cache HIT of 263,521 one-minute bars costs the relay** (the the CI box run of 2026-11-19: 184 kept
    /// days of binance BTCUSDT 1m, the last cut to its first bar): the copy it was answered by before against the
    /// shared runs it is encoded from now, in turn for seven rounds, printed as median / best: the time to the first
    /// byte on the pipe (the count frame, written once the answer is in hand) and to the last, the minor page
    /// faults (fresh 4 KiB pages: what the copy allocated and touched) and the peak resident growth (`VmHWM`, reset
    /// through `/proc/self/clear_refs`, over the resident set before). Run in release, alone.
    #[test]
    #[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_cached_bar_serve -- --ignored --nocapture"]
    fn measure_the_cached_bar_serve() {
        let bars = super::bars(184 * 1_440);
        let store = Arc::new(Stamped { bars, reads: AtomicUsize::new(0), page: 0 });
        let range = TimestampRange::of(0, 183 * MS_PER_DAY);
        let cached = cache(&store);
        let [read, ..] = reads(range);
        answer(&mut io::sink(), &cached, read.clone()).expect("warm the cache");
        let warm = store.reads.load(Ordering::Relaxed);
        assert_eq!(old_hit(&cached, range).len(), 263_521, "the reference run's bars");
        let old = |pipe: &mut Pipe| old_send(pipe, Ok(old_hit(&cached, range)));
        let new = |pipe: &mut Pipe| answer(pipe, &cached, read.clone());
        let (mut olds, mut news) = (Vec::new(), Vec::new());
        for _ in 0..7 {
            olds.push(one_serve(&old));
            news.push(one_serve(&new));
        }
        assert_eq!(store.reads.load(Ordering::Relaxed), warm, "every serve was a hit");
        assert_eq!(olds[0].bytes, news[0].bytes, "the same bytes either way");
        for (what, runs) in [("copied (before)", olds), ("shared (now)", news)] {
            // The median and the best of one number over the rounds.
            let spread = |of: fn(&Served) -> u64| {
                let mut values: Vec<u64> = runs.iter().map(of).collect();
                values.sort_unstable();
                (values[values.len() / 2], values[0])
            };
            let (first, first_best) = spread(|run| run.first.as_micros() as u64);
            let (last, last_best) = spread(|run| run.last.as_micros() as u64);
            let (faults, _) = spread(|run| run.faults.unwrap_or(0));
            let (peak, _) = spread(|run| run.peak_kib.unwrap_or(0));
            println!(
                "cached bar serve, {what}: first byte {first} us (best {first_best}), last byte {last} us \
                 (best {last_best}), {} bytes, {faults} minor faults ({} MiB of fresh pages), peak resident \
                 growth {} MiB",
                runs[0].bytes,
                faults * 4 / 1024,
                peak / 1024,
            );
        }
    }
}

/// The same for a TICK read and the tick cache.
#[cfg(test)]
#[path = "relay_tick_tests.rs"]
mod ticks;
