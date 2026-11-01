//! `history` - the START HISTORY of a strategy: the closed bars a bar key has already lived through, which
//! the datahub SAVES as they close and READS BACK when a strategy subscribes bars, so that a strategy that
//! needs its last N bars at once does not wait for them to accumulate again after every restart.
//!
//! The owner's ruling (2026-10-20) is the whole contract: every price flows gateway, datahub, then the
//! strategies; the datahub stores what the recorder sees; a starting strategy gets its last N bars at once,
//! read from the datahub's OWN database first, and the venue is asked ONLY for the span the database lacks -
//! never for what we already have. This file is the datahub's half of that, in three layers that arrive one
//! at a time:
//!
//! 1. **SAVE** (always on for a venue in [`SAVED_VENUES`], as long as this process compacts its store).
//!    `StratSink`'s `close_bar` hands each CLOSED bar of a key some session holds to a worker thread, which
//!    writes it through the store's existing kline ingest ([`BarWriter`]; production is
//!    `ingest_writer`, behind `backfill-serve`). A bar is written once, at its close; a forming bar is never handed over.
//! 2. **SHADOW** (always on, beside SAVE). When the venue's own seed for a key arrives, the same worker
//!    reads the store's last bars, compares them with the seed and writes ONE plain line to the journal
//!    ([`ShadowReport`]). Nothing a strategy receives changes: the seed goes on to the sessions exactly as
//!    before. The lines are the evidence a later wave needs before it lets the store serve. The worker then
//!    SAVES the seed's bars the store lacks (a restart of the datahub, the gatewayhub or the tradehub leaves a
//!    hole that the venue's seed covers), at most one seed's worth a subscription, each under the same per-bar
//!    key as a live close, so a bar the store already holds is a no-op.
//! 3. **SERVE** (`flags.start_history_from_store`, off by default). At subscribe time the session thread
//!    reads the store, fetches the missing span from the venue's history channel ([`GapSource`]), splices the
//!    two (`splice`) and sends the result as an ordinary bar `Seed` BEFORE the venue is asked, within a time
//!    budget; the venue's own seed, when it arrives, is spliced over what was sent so that a short REST seed
//!    never shrinks it.
//!
//! # Where it sits, and what it never touches
//!
//! * **Nothing per message.** The only calls on a venue client's thread are `close_bar` and `seed_bars` of
//!   `StratSink` (once per bar close and once per subscription), each a `try_send`; the quote, trade, book and
//!   depth verbs do not name this module (`crates/vike-datahub/src/strat/history_tests.rs`'s
//!   `only_the_two_bar_verbs_of_the_sink_name_the_history` holds that by reading `sink.rs`). The store is read
//!   on the session thread at subscribe time (SERVE) or on the worker (SHADOW), never in a sink call.
//! * **No shared home is edited.** `FeedBroker`, `RoutingSink`, the recorder (`RecorderSink`), the kline ingest
//!   and `KlineSource` are CONSUMED: the saver is a holder of its own that rides the strategy sink, so it also
//!   covers a `PerHolder` venue that hands its client the strategy sink directly.
//! * **The series is named as the strategy lane names it** (`EURUSD`, what `StratSink` is called with), not as
//!   OANDA's history lane stores it (`EUR_USD`): the two are different series (`oanda_history_refusal`'s
//!   reason). ⚠ **A symbol that CONTAINS an underscore is never saved** ([`saveable_symbol`]): a strategy
//!   mounted as `EUR_USD` would feed the operator-backfilled DAY-GRID series `bar/oanda/EUR_USD` with per-bar
//!   keys, and a series fed by both ingests stores the bars they share twice, with no verb to retire a key.
//!   It is skipped with one warning per key and counted ([`StartHistoryStats::skipped`]); the writer refuses it
//!   too. The store's door refuses a name with a slash, and a refusal here is a counted, logged hole
//!   ([`StartHistoryStats::failed`]), never a panic.
//! * **At most [`MAX_SAVED_KEYS`] keys are saved** (the first ones seen after a start): every saved bar spends
//!   a commit key the store keeps for ever, so the series' bookkeeping grows with every bar
//!   (`crates/vike-datahub/tests/start_history_store.rs` measures it). A fourth key is skipped and counted.
//!
//! # The two ways a stored series can lose a strategy its newest bar (R1 and R2 of the design)
//!
//! The core REPLACES a bar series whole when a `Seed` arrives (`vike-core`'s `Ingest::BarSeed`), and only
//! appends a close newer than its last bar. So a seed whose newest bar is older than one the core already
//! holds makes the core LOSE that bar, and nothing later puts it back. Two things can make a stored series
//! end early: the saver DROPPED a close (its queue was full or the store refused the row: counted in
//! [`StartHistoryStats`]), or the process was down while bars closed. The rule that holds both is in
//! `prepare`: a start history is sent only when the store's tail plus a fetched gap that answered reaches
//! the newest bar that CAN have closed (a closed bar opened at or before `now - step`, whatever grid the
//! venue's candles sit on: OANDA's two-hour and longer candles open on a 17:00 New York grid, not the epoch
//! grid) and is WITHHELD otherwise (no history channel, the fetch failed, the budget ran out). Withheld is
//! today's behaviour: the strategy gets what the venue seeds. The same rule WITHHOLDS a history with a
//! missing bar inside it (`holes_inside`); a closed market counts as a hole, so a span that crosses a weekend
//! is withheld too and the venue's own seed serves.
//!
//! # Volume
//!
//! One part per closed bar before compaction, so a key at the one-minute step is about fourteen hundred
//! parts a day until a maintenance pass merges them (`crates/vike-store/CLAUDE.md`). That is why the saver
//! is armed only in a process that compacts (`crates/vike-datahub/src/datahub_cli/lanes.rs`'s
//! `arm_start_history`), and why the measurement is `crates/vike-datahub/tests/start_history_store.rs`'s
//! `a_day_of_one_minute_bars_costs_this_on_disk`.
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{HistStore, LiveDataSink, TsRange};
use vike_datahub_client::seed::{SEED_BARS, SEED_INTERVALS};
use vike_marketdata::Bar;
use vike_marketdata::interval::interval_ms;

/// The venues whose closed bars are saved, and whose start history is compared (and, behind the flag,
/// served). OANDA first (decision 0193, wave A); a venue joins by a row here and a line of its own proof.
pub const SAVED_VENUES: &[&str] = &["oanda"];

/// How long ONE key's start history may take on the session thread, store read and gap fetch together. The
/// tradehub waits `STRAT_SUB_REPLY` for the `SubOk`, and a first connection to a venue with keys already
/// spends almost all of it (`limits.rs`'s `const _` chain holds this one second inside it).
pub const PRELOAD_KEY_BUDGET: Duration = Duration::from_secs(1);

/// How long ALL the start histories of one session may take together: a session that subscribes many keys
/// stops asking the store after this, and its later keys get what the venue seeds.
pub const PRELOAD_SESSION_BUDGET: Duration = Duration::from_secs(5);

/// How many closed bars a start history carries: the chart's one-shot gap seed fetches the same number
/// (`vike_datahub_client::seed::SEED_BARS`), so the two cannot disagree about what "the last bars" means, and
/// the client names no number (decision 0058: a cost is the server's constant).
pub const START_BARS: usize = SEED_BARS as usize;

/// The save queue's depth. A close is one small message a minute per key, so a full queue means the store
/// is stuck, and a close that finds it full is COUNTED as dropped and announced, never waited for.
const QUEUE_CAP: usize = 1024;

/// How many shadow reports [`StartHistory::reports`] keeps (the journal has all of them).
const REPORT_RING: usize = 64;

/// How many keys are saved at most (the first ones seen after a start). Every saved bar spends a commit key
/// the store keeps for as long as the series lives, so a series' bookkeeping grows with every bar; three keys
/// are what the shadow acceptance needs, and a wider use waits for the store-side answer to that growth
/// (`crates/vike-datahub/tests/start_history_store.rs` prints the figures).
pub const MAX_SAVED_KEYS: usize = 3;

/// The least span of time a read of the store looks back over, whatever the step: a one-minute key read at
/// twice its 600 bars would see twenty hours, and from a Saturday evening to a Monday morning that is an
/// empty store. Four days cover an ordinary weekend and a long one in part.
const MIN_READ_SPAN_MS: i64 = 96 * 3_600_000;

/// The most bars of a venue's seed one subscription saves: one seed's worth (OANDA seeds three hundred).
const REPAIR_CAP: usize = 300;

/// Whether a bar of `interval` on `venue` is saved: a saved venue, at a step the chart seed also serves (one
/// minute and up). A sub-minute step would write a part per few seconds per key.
pub fn covers(venue: &str, interval: &str) -> bool {
    SAVED_VENUES.contains(&venue) && SEED_INTERVALS.contains(&interval)
}

/// Whether a symbol may be saved under the name the strategy lane gives it: **one that contains an underscore
/// may not** (OANDA's own instrument spelling, `EUR_USD`, is the name of the operator-backfilled day-grid
/// series; per-bar keys written into it would store the bars the two ingests share twice).
pub fn saveable_symbol(symbol: &str) -> bool {
    !symbol.contains('_')
}

/// Where one closed bar is written: `(venue, symbol, interval, bar)` and the rows written (`0`: the bar was
/// already stored). A closure, so the module is feature-free and a test hands it a double; production is
/// `ingest_writer`, behind `backfill-serve`.
pub type BarWriter = Box<dyn Fn(&str, &str, &str, &Bar) -> Result<usize, String> + Send + 'static>;

/// The wall clock in epoch milliseconds, a parameter so a test stands on an exact bar boundary.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// A venue's history channel for the span the store lacks. The span is `[start_ms, end_ms]` of bar OPEN
/// times; the answer may be empty (a closed market) and that is an answer, while an `Err` is a fetch that
/// did not happen. Production is [`GatewayGap`]; a test hands a script.
pub trait GapSource: Send + Sync {
    /// The bars of `(venue, symbol, interval)` whose open time lies in `[start_ms, end_ms]`, in any order.
    fn fetch(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, String>;
}

// ---- the pure pieces ----------------------------------------------------------------------------------------

/// `bars` ascending by time with ONE row per time: of two rows with the same time the LATER one in the input
/// wins (a live write against a backfill of the same bar; the store does not promise an order for them).
fn ascending_unique(mut bars: Vec<Bar>) -> Vec<Bar> {
    bars.sort_by_key(|b| b.ts);
    let mut out: Vec<Bar> = Vec::with_capacity(bars.len());
    for bar in bars {
        match out.last_mut() {
            Some(last) if last.ts == bar.ts => *last = bar,
            _ => out.push(bar),
        }
    }
    out
}

/// The CLOSED bars of `bars` at `now_ms` for a `step_ms` bar: ascending, one per time ([`ascending_unique`]),
/// the bar whose interval has not ended dropped. A bar in the store is closed by construction (the saver
/// writes closes only), so this is a belt: a forming bar written by anything else never reaches a strategy.
pub(crate) fn closed_ascending(bars: Vec<Bar>, step_ms: i64, now_ms: i64) -> Vec<Bar> {
    let mut out = ascending_unique(bars);
    out.retain(|b| b.ts.saturating_add(step_ms) <= now_ms);
    out
}

/// The newest `keep` bars of `bars`.
fn newest(mut bars: Vec<Bar>, keep: usize) -> Vec<Bar> {
    let extra = bars.len().saturating_sub(keep);
    bars.drain(..extra);
    bars
}

/// The bars of every part, joined: ascending, one per time, the LATER part winning a time both hold
/// (`parts` run from the weakest source to the strongest), the newest `keep`. It is a union, so a bar any
/// part has is kept: nothing a strategy could already hold is dropped by a source that lacks it.
pub(crate) fn splice(parts: &[&[Bar]], keep: usize) -> Vec<Bar> {
    let all: Vec<Bar> = parts.iter().flat_map(|p| p.iter().cloned()).collect();
    newest(ascending_unique(all), keep)
}

/// The step of `interval`, when it is a positive width.
fn step_of(interval: &str) -> Option<i64> {
    interval_ms(interval).filter(|ms| *ms > 0)
}

/// The latest OPEN time a closed bar can have at `now_ms`: a bar of `step_ms` that opened at `t` has closed
/// once `t + step_ms <= now_ms`, so `now_ms - step_ms`, **whatever grid the venue's candles open on**. (The
/// epoch grid `now.div_euclid(step) * step - step` is right for minutes and wrong for OANDA's two-hour and
/// longer candles, which open on a 17:00 New York grid: at 10:30 UTC in winter the newest closed four-hour
/// candle opened at 06:00, and the epoch grid says 04:00.)
pub(crate) fn newest_open_bound(step_ms: i64, now_ms: i64) -> i64 {
    now_ms.saturating_sub(step_ms)
}

/// How many bars are MISSING between consecutive bars of `bars` (ascending, one per time): a gap of more than
/// one step is `gap / step - 1` slots, and integer division keeps a daylight-saving shift of a long candle's
/// grid (a twenty-five hour day) from counting as a hole. A closed market counts: this cannot tell a lost bar
/// from a weekend, and says so in its callers.
pub(crate) fn holes_inside(bars: &[Bar], step_ms: i64) -> usize {
    bars.windows(2).map(|w| usize::try_from((w[1].ts - w[0].ts) / step_ms - 1).unwrap_or(0)).sum()
}

/// The last `n` CLOSED bars of `(venue, symbol, interval)` at `now_ms`, ascending. The read looks back over
/// twice `n` steps and no less than [`MIN_READ_SPAN_MS`], so a market's closed hours (an FX weekend) still
/// leave bars in it when the series is dense, and fewer, honestly, when it is not.
pub(crate) fn read_tail(
    store: &dyn HistStore,
    venue: &str,
    symbol: &str,
    interval: &str,
    n: usize,
    now_ms: i64,
) -> Result<Vec<Bar>, String> {
    let step = step_of(interval).ok_or_else(|| format!("{interval:?} has no bar width"))?;
    let end = newest_open_bound(step, now_ms);
    let width =
        step.saturating_mul(i64::try_from(n).unwrap_or(0).saturating_mul(2)).max(MIN_READ_SPAN_MS);
    let range = TsRange::of(end.saturating_sub(width), end);
    let bars =
        store.load_bars(venue, symbol, interval, range).map_err(|e| format!("store read: {e}"))?;
    Ok(newest(closed_ascending(bars, step, now_ms), n))
}

/// The span of closed bars a start history is still missing after `stored`: `[from, to]` of bar open times,
/// `None` when the stored tail IS the newest closed bar (`to` is [`newest_open_bound`], not a grid time). An
/// empty `stored` misses the whole `keep` slots, and a store that ended long ago is asked for no more than the
/// newest `keep` slots: older ones would be cut by `splice` anyway, so the venue is never asked for what the
/// start history cannot hold.
pub(crate) fn missing_span(
    stored: &[Bar],
    step_ms: i64,
    now_ms: i64,
    keep: usize,
) -> Option<(i64, i64)> {
    let to = newest_open_bound(step_ms, now_ms);
    let oldest_kept = to
        .saturating_sub(step_ms.saturating_mul(i64::try_from(keep).unwrap_or(1).saturating_sub(1)));
    let from = match stored.last() {
        Some(last) => last.ts.saturating_add(step_ms).max(oldest_kept),
        None => oldest_kept,
    };
    (from <= to).then_some((from, to))
}

/// What [`prepare`] decided.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Prepared {
    /// A start history whose newest bar is the newest closed bar (or the market's last one: a gap fetch that
    /// answered with nothing is an answer).
    Ready {
        /// The bars to send, ascending, at most [`START_BARS`].
        seed: Vec<Bar>,
        /// How many of them the store gave.
        stored: usize,
        /// How many the history channel gave for the missing span.
        fetched: usize,
        /// The span that was missing, if any.
        gap: Option<(i64, i64)>,
    },
    /// Nothing is sent, and `why` says so in words for the journal. The strategy gets today's behaviour.
    Withheld {
        /// The reason, one clause.
        why: String,
        /// How many bars the store gave before the decision.
        stored: usize,
    },
}

/// **The start history of one key**: the store's last bars, the span the store lacks fetched from `gap`, the
/// two spliced - or WITHHELD. The decision that keeps the core from losing its newest bar (module doc) is the
/// one marked below: a missing span that cannot be answered is never papered over by sending the stored tail
/// as if it were the end.
pub(crate) fn prepare(
    store: &dyn HistStore,
    gap: Option<&dyn GapSource>,
    venue: &str,
    symbol: &str,
    interval: &str,
    now_ms: i64,
) -> Prepared {
    let Some(step) = step_of(interval) else {
        return Prepared::Withheld { why: format!("{interval:?} has no bar width"), stored: 0 };
    };
    let stored = match read_tail(store, venue, symbol, interval, START_BARS, now_ms) {
        Ok(bars) => bars,
        Err(why) => return Prepared::Withheld { why, stored: 0 },
    };
    let held = stored.len();
    let span = missing_span(&stored, step, now_ms, START_BARS);
    // A store that ended before the newest `keep` slots began is asked for those slots alone, and its bars are
    // beyond the history's horizon: kept, they would join the fetched ones across a gap nobody asked about.
    let mut stored = stored;
    if let Some((from, _)) = span
        && stored.last().is_some_and(|last| last.ts.saturating_add(step) < from)
    {
        stored.clear();
    }
    let mut fetched: Vec<Bar> = Vec::new();
    if let Some((from, to)) = span {
        // THE RULE: the stored tail is not the newest closed bar, so the history is complete only if the
        // venue answers for the span. No channel, or a failed fetch, withholds everything.
        let Some(gap) = gap else {
            return Prepared::Withheld {
                why: match stored.last() {
                    Some(last) => format!(
                        "the store ends {} min before the newest closed bar and this venue has no history channel here",
                        behind_minutes(Some(last.ts), to)
                    ),
                    None => "the store holds no bar and this venue has no history channel here"
                        .to_string(),
                },
                stored: held,
            };
        };
        match gap.fetch(venue, symbol, interval, from, to) {
            Ok(bars) => {
                fetched = closed_ascending(bars, step, now_ms);
                fetched.retain(|b| b.ts >= from && b.ts <= to);
            }
            Err(e) => {
                return Prepared::Withheld {
                    why: format!("the missing span could not be fetched ({e})"),
                    stored: held,
                };
            }
        }
    }
    let seed = splice(&[stored.as_slice(), fetched.as_slice()], START_BARS);
    if seed.is_empty() {
        return Prepared::Withheld {
            why: "the store holds no bar within the history's horizon and the venue returned none"
                .to_string(),
            stored: held,
        };
    }
    // A bar missing INSIDE the span is not the core's newest bar at risk, but it is a history with a hole that
    // the venue's own seed may not cover (it reaches back three hundred bars, the start history six hundred):
    // withhold it and let the venue's seed serve. A closed market is a hole to this count (a weekend inside
    // the span withholds it too); that is the conservative side of a question only the venue can answer.
    let holes = holes_inside(&seed, step);
    if holes > 0 {
        return Prepared::Withheld {
            why: format!("{holes} bars are missing inside the span the history would cover"),
            stored: held,
        };
    }
    Prepared::Ready { seed, stored: held, fetched: fetched.len(), gap: span }
}

/// Minutes between the stored tail (`None`: nothing stored) and the newest closed bar `to`.
fn behind_minutes(tail: Option<i64>, to: i64) -> i64 {
    tail.map_or(0, |t| (to - t).max(0) / 60_000)
}

/// Run `work` on a short-lived named thread and wait at most `budget` for its answer. `None`: it did not
/// answer in time (the thread finishes alone and its answer is dropped), it panicked, or the OS refused the
/// thread - in every case the caller carries on with what it had, which is the lane's behaviour before the
/// start history existed.
fn run_within<T: Send + 'static>(
    budget: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("dh-hist-read".to_string())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .ok()?;
    rx.recv_timeout(budget).ok()
}

// ---- the shadow comparison ----------------------------------------------------------------------------------

/// The first place the store and the venue differ, by time.
#[derive(Debug, Clone, PartialEq)]
pub enum Difference {
    /// Both have the bar and a stored column is not the venue's.
    Field {
        /// Bar open time, epoch milliseconds.
        ts: i64,
        /// Which column.
        field: &'static str,
        /// The stored value (`NaN`: the column is absent).
        store: f64,
        /// The venue's value (`NaN`: the column is absent).
        venue: f64,
    },
    /// The venue has a bar inside the span the store covers and the store has none: a hole.
    StoreLacks {
        /// Bar open time, epoch milliseconds.
        ts: i64,
    },
    /// The store has a bar inside the span the venue seeded and the venue has none.
    VenueLacks {
        /// Bar open time, epoch milliseconds.
        ts: i64,
    },
}

impl Difference {
    fn ts(&self) -> i64 {
        match self {
            Difference::Field { ts, .. }
            | Difference::StoreLacks { ts }
            | Difference::VenueLacks { ts } => *ts,
        }
    }
}

/// What one comparison of the store's last bars against a venue's seed found. Its [`fmt::Display`] is the
/// ONE journal line a subscription writes.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowReport {
    /// The venue.
    pub venue: String,
    /// The series, as the strategy lane names it.
    pub symbol: String,
    /// The bar step word.
    pub interval: String,
    /// Closed bars the store gave (its last [`START_BARS`]).
    pub store_bars: usize,
    /// Bars in the venue's seed.
    pub venue_bars: usize,
    /// Bars both have, by open time.
    pub overlap: usize,
    /// Of those, the ones equal in every stored column.
    pub equal: usize,
    /// Bars missing between the store's own consecutive bars (`holes_inside`: a closed market counts).
    pub holes_inside: usize,
    /// Venue bars inside the store's own span that the store lacks (holes).
    pub store_lacks_inside: usize,
    /// Venue bars newer than the store's newest.
    pub store_behind: usize,
    /// Store bars inside the venue's span that the venue lacks.
    pub store_extra: usize,
    /// The earliest difference, if any.
    pub first_difference: Option<Difference>,
    /// The store's newest bar.
    pub store_newest_ts: Option<i64>,
    /// The venue seed's newest bar.
    pub venue_newest_ts: Option<i64>,
    /// How many minutes the store's newest bar is behind the venue's (`None`: either side is empty).
    pub gap_minutes: Option<i64>,
    /// Bars of the venue's seed the store lacked and the worker then saved (set after the comparison; the
    /// comparison itself is of the store AS IT WAS).
    pub repaired: usize,
}

fn stamp(ts: Option<i64>) -> String {
    ts.map_or_else(|| "none".to_string(), |t| vike_model::time::utc_rfc3339(t.div_euclid(1000)))
}

impl fmt::Display for ShadowReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "vike-datahub start history (shadow): {} {} {}: the store holds {} closed bars (newest {}); \
             holes inside: {}; the venue seeded {} (newest {}); both have {}, equal {}; the store lacks {} \
             inside its span and {} newer than its newest, and has {} the venue lacks; the store is behind \
             the venue by {} min; saved {} of the venue's bars the store lacked; first difference: ",
            self.venue,
            self.symbol,
            self.interval,
            self.store_bars,
            stamp(self.store_newest_ts),
            self.holes_inside,
            self.venue_bars,
            stamp(self.venue_newest_ts),
            self.overlap,
            self.equal,
            self.store_lacks_inside,
            self.store_behind,
            self.store_extra,
            self.gap_minutes.map_or_else(|| "unknown".to_string(), |m| m.to_string()),
            self.repaired,
        )?;
        match &self.first_difference {
            None => write!(f, "none"),
            Some(Difference::Field { ts, field, store, venue }) => {
                write!(f, "{} {field} store {store} venue {venue}", stamp(Some(*ts)))
            }
            Some(Difference::StoreLacks { ts }) => {
                write!(f, "{} the store lacks this bar", stamp(Some(*ts)))
            }
            Some(Difference::VenueLacks { ts }) => {
                write!(f, "{} the venue lacks this bar", stamp(Some(*ts)))
            }
        }
    }
}

/// The first stored column in which `store` is not `venue`: `(column, stored, venue's)`. The columns are the
/// bar codec's (open, high, low, close, volume, funding); `bid`, `ask` and `symbol` are never stored.
fn field_difference(store: &Bar, venue: &Bar) -> Option<(&'static str, f64, f64)> {
    let same = |a: f64, b: f64| a == b || (a.is_nan() && b.is_nan());
    for (field, s, v) in [
        ("open", store.open, venue.open),
        ("high", store.high, venue.high),
        ("low", store.low, venue.low),
        ("close", store.close, venue.close),
        ("volume", store.volume, venue.volume),
    ] {
        if !same(s, v) {
            return Some((field, s, v));
        }
    }
    match (store.funding, venue.funding) {
        (None, None) => None,
        (Some(s), Some(v)) if same(s, v) => None,
        (s, v) => Some(("funding", s.unwrap_or(f64::NAN), v.unwrap_or(f64::NAN))),
    }
}

/// Compare the store's last bars with a venue's seed. Pure: the reads happened before it.
pub(crate) fn compare_bars(
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &[Bar],
    seed: &[Bar],
) -> ShadowReport {
    let store = ascending_unique(store.to_vec());
    let seed = ascending_unique(seed.to_vec());
    let stored_at: HashMap<i64, &Bar> = store.iter().map(|b| (b.ts, b)).collect();
    let seeded_at: HashSet<i64> = seed.iter().map(|b| b.ts).collect();
    let (store_first, store_last) = (store.first().map(|b| b.ts), store.last().map(|b| b.ts));
    let (seed_first, seed_last) = (seed.first().map(|b| b.ts), seed.last().map(|b| b.ts));

    let mut report = ShadowReport {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        store_bars: store.len(),
        venue_bars: seed.len(),
        overlap: 0,
        equal: 0,
        holes_inside: step_of(interval).map_or(0, |step| holes_inside(&store, step)),
        store_lacks_inside: 0,
        store_behind: 0,
        store_extra: 0,
        first_difference: None,
        store_newest_ts: store_last,
        venue_newest_ts: seed_last,
        gap_minutes: store_last.zip(seed_last).map(|(s, v)| (v - s).max(0) / 60_000),
        repaired: 0,
    };
    let mut note = |d: Difference| {
        if report.first_difference.as_ref().is_none_or(|first| d.ts() < first.ts()) {
            report.first_difference = Some(d);
        }
    };
    let (mut overlap, mut equal, mut lacks_inside, mut behind, mut extra) = (0, 0, 0, 0, 0);
    for v in &seed {
        match stored_at.get(&v.ts) {
            Some(s) => {
                overlap += 1;
                match field_difference(s, v) {
                    None => equal += 1,
                    Some((field, sv, vv)) => {
                        note(Difference::Field { ts: v.ts, field, store: sv, venue: vv });
                    }
                }
            }
            None => {
                if store_last.is_none_or(|last| v.ts > last) {
                    behind += 1;
                } else if store_first.is_some_and(|first| v.ts > first) {
                    lacks_inside += 1;
                    note(Difference::StoreLacks { ts: v.ts });
                }
            }
        }
    }
    for s in &store {
        let inside = seed_first.is_some_and(|f| s.ts >= f) && seed_last.is_some_and(|l| s.ts <= l);
        if inside && !seeded_at.contains(&s.ts) {
            extra += 1;
            note(Difference::VenueLacks { ts: s.ts });
        }
    }
    report.overlap = overlap;
    report.equal = equal;
    report.store_lacks_inside = lacks_inside;
    report.store_behind = behind;
    report.store_extra = extra;
    report
}

// ---- the handle ---------------------------------------------------------------------------------------------

/// What the saver did, for the journal and the tests. All counts are since the process started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartHistoryStats {
    /// Closed bars written.
    pub saved: u64,
    /// Closed bars the store already had (the idempotent no-op of a repeated close).
    pub already_stored: u64,
    /// Closed bars the store refused: each one a hole in the series, announced when it happened.
    pub failed: u64,
    /// Closed bars the queue could not take: each one a hole in the series, announced when it happened.
    pub dropped: u64,
    /// Bars not saved by rule: a symbol with an underscore ([`saveable_symbol`]) or a key past
    /// [`MAX_SAVED_KEYS`]; counted once per bar and announced once per key.
    pub skipped: u64,
    /// Bars of a venue's seed that the store lacked and the worker saved (also counted in `saved`).
    pub repaired: u64,
}

impl fmt::Display for StartHistoryStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "saved {}, already stored {}, failed {}, dropped {}, skipped {}, of the venue's seed saved {}",
            self.saved, self.already_stored, self.failed, self.dropped, self.skipped, self.repaired
        )
    }
}

#[derive(Default)]
struct Counters {
    saved: AtomicU64,
    already: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
    skipped: AtomicU64,
    repaired: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> StartHistoryStats {
        StartHistoryStats {
            saved: self.saved.load(Ordering::Relaxed),
            already_stored: self.already.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            repaired: self.repaired.load(Ordering::Relaxed),
        }
    }
}

enum Job {
    Save { venue: String, symbol: String, interval: String, bar: Bar },
    Compare { venue: String, symbol: String, interval: String, seed: Vec<Bar> },
    Barrier(mpsc::Sender<()>),
}

type Reports = Arc<Mutex<VecDeque<ShadowReport>>>;

/// The worker thread's state: everything that touches the store from the venue side lives here.
struct Worker {
    write: BarWriter,
    reader: Arc<dyn HistStore + Send + Sync>,
    clock: Clock,
    reports: Reports,
    counters: Arc<Counters>,
}

impl Worker {
    fn run(self, jobs: &mpsc::Receiver<Job>) {
        while let Ok(job) = jobs.recv() {
            match job {
                Job::Save { venue, symbol, interval, bar } => {
                    self.save(&venue, &symbol, &interval, &bar);
                }
                Job::Compare { venue, symbol, interval, seed } => {
                    self.compare(&venue, &symbol, &interval, &seed);
                }
                Job::Barrier(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    }

    /// Write one closed bar; `true` when it was newly written (not a repeat, not a refusal).
    fn save(&self, venue: &str, symbol: &str, interval: &str, bar: &Bar) -> bool {
        match (self.write)(venue, symbol, interval, bar) {
            Ok(0) => {
                self.counters.already.fetch_add(1, Ordering::Relaxed);
                false
            }
            Ok(_) => {
                self.counters.saved.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(e) => {
                self.counters.failed.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    venue,
                    symbol,
                    interval,
                    ts = bar.ts,
                    error = %e,
                    "vike-datahub start history: a closed bar was NOT saved (the store refused it); \
                     the stored series has a hole at this bar"
                );
                false
            }
        }
    }

    /// Save the bars of the venue's `seed` that `stored` lacks, the newest [`REPAIR_CAP`] at most: the hole
    /// a restart leaves is a span the venue's seed covers. A bar the store holds anyway (a read that missed it)
    /// is the per-bar key's no-op. Returns how many were newly written.
    fn repair(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        stored: &[Bar],
        seed: &[Bar],
        now_ms: i64,
    ) -> usize {
        let Some(step) = step_of(interval) else { return 0 };
        let held: HashSet<i64> = stored.iter().map(|b| b.ts).collect();
        let mut missing: Vec<&Bar> = seed
            .iter()
            .filter(|b| !held.contains(&b.ts) && b.ts.saturating_add(step) <= now_ms)
            .collect();
        missing.sort_by_key(|b| b.ts);
        missing.dedup_by_key(|b| b.ts);
        // The newest `REPAIR_CAP` of them: a start history is its newest bars.
        let skip = missing.len().saturating_sub(REPAIR_CAP);
        let mut written = 0_usize;
        for bar in missing.into_iter().skip(skip) {
            if self.save(venue, symbol, interval, bar) {
                written += 1;
            }
        }
        self.counters.repaired.fetch_add(written as u64, Ordering::Relaxed);
        written
    }

    fn compare(&self, venue: &str, symbol: &str, interval: &str, seed: &[Bar]) {
        let started = Instant::now();
        let now = (self.clock)();
        match read_tail(self.reader.as_ref(), venue, symbol, interval, START_BARS, now) {
            Ok(stored) => {
                let mut report = compare_bars(venue, symbol, interval, &stored, seed);
                report.repaired = self.repair(venue, symbol, interval, &stored, seed, now);
                tracing::info!(
                    "{report} (read and save took {} ms; the saver so far: {})",
                    started.elapsed().as_millis(),
                    self.counters.snapshot()
                );
                let mut ring = self.reports.lock().unwrap_or_else(PoisonError::into_inner);
                if ring.len() >= REPORT_RING {
                    ring.pop_front();
                }
                ring.push_back(report);
            }
            Err(e) => tracing::warn!(
                venue,
                symbol,
                interval,
                error = %e,
                "vike-datahub start history (shadow): the store could not be read, so the venue's seed \
                 was not compared"
            ),
        }
    }
}

/// The datahub's start-history holder: one per process, attached to the strategy lane
/// (`StratHub::attach_start_history`). Cheap to share; every method on a venue client's thread is a
/// `try_send` or a flag test.
pub struct StartHistory {
    reader: Arc<dyn HistStore + Send + Sync>,
    gap: Option<Arc<dyn GapSource>>,
    serve: bool,
    clock: Clock,
    jobs: SyncSender<Job>,
    reports: Reports,
    counters: Arc<Counters>,
    /// The keys admitted to the saver, at most [`MAX_SAVED_KEYS`] (`(venue, symbol, interval)`).
    keys: Mutex<HashSet<(String, String, String)>>,
    /// The keys already announced as skipped, so each is announced once.
    warned: Mutex<HashSet<(String, String, String)>>,
    /// The keys a PRELOAD claimed and no seed or close has proven live yet: a subscription that fails after
    /// the preload hands such a slot back ([`Self::release_unproven`]); a key some session holds never does.
    unproven: Mutex<HashSet<(String, String, String)>>,
    /// The start history prepared for a SESSION's key, waiting for the venue's own seed to be spliced over it:
    /// keyed by the session too, because a second session that joins a key gets a preload of its own and must
    /// not take the first's pending entry (and the entry goes with its session, `forget_session`).
    stash: Mutex<Stash>,
}

/// The pending histories: `(session, venue, symbol, interval)` to the bars sent to that session.
type Stash = HashMap<(u64, String, String, String), Vec<Bar>>;

impl fmt::Debug for StartHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartHistory")
            .field("serve", &self.serve)
            .field("gap", &self.gap.is_some())
            .finish_non_exhaustive()
    }
}

impl StartHistory {
    /// Start the saver. `reader` is the store the start history is read from, `writer` writes one closed bar,
    /// `gap` is the venue history channel (`None`: SERVE withholds whenever the store is behind) and `serve`
    /// is `flags.start_history_from_store`. Returns `Err` when the OS refuses the worker thread; the caller
    /// logs it and runs without a start history, which is the lane's behaviour before this existed.
    pub fn spawn(
        reader: Arc<dyn HistStore + Send + Sync>,
        writer: BarWriter,
        gap: Option<Arc<dyn GapSource>>,
        serve: bool,
    ) -> std::io::Result<Arc<Self>> {
        Self::spawn_with_clock(reader, writer, gap, serve, Arc::new(vike_model::now_ms))
    }

    /// [`Self::spawn`] with the clock handed in, for a test that stands on an exact bar boundary.
    pub fn spawn_with_clock(
        reader: Arc<dyn HistStore + Send + Sync>,
        writer: BarWriter,
        gap: Option<Arc<dyn GapSource>>,
        serve: bool,
        clock: Clock,
    ) -> std::io::Result<Arc<Self>> {
        let (jobs, inbox) = mpsc::sync_channel(QUEUE_CAP);
        let reports: Reports = Arc::default();
        let counters = Arc::new(Counters::default());
        let worker = Worker {
            write: writer,
            reader: Arc::clone(&reader),
            clock: Arc::clone(&clock),
            reports: Arc::clone(&reports),
            counters: Arc::clone(&counters),
        };
        std::thread::Builder::new()
            .name("dh-bar-save".to_string())
            .spawn(move || worker.run(&inbox))?;
        Ok(Arc::new(StartHistory {
            reader,
            gap,
            serve,
            clock,
            jobs,
            reports,
            counters,
            keys: Mutex::new(HashSet::new()),
            warned: Mutex::new(HashSet::new()),
            unproven: Mutex::new(HashSet::new()),
            stash: Mutex::new(HashMap::new()),
        }))
    }

    /// Whether the start history is SERVED from the store (the flag), not merely compared.
    pub fn serves(&self) -> bool {
        self.serve
    }

    /// What the saver has done so far.
    pub fn stats(&self) -> StartHistoryStats {
        self.counters.snapshot()
    }

    /// The last shadow reports, oldest first (the journal has every one; this keeps the latest few).
    pub fn reports(&self) -> Vec<ShadowReport> {
        self.reports.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect()
    }

    /// Wait until everything queued before this call has been handled, or `timeout` has passed (the queue
    /// being full counts against the same `timeout`: the send is bounded too). The tests use it in place of a
    /// sleep, and the daemon calls it once at an orderly stop so the closes still queued are written.
    pub fn settle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (ack, done) = mpsc::channel();
        let mut job = Job::Barrier(ack);
        loop {
            match self.jobs.try_send(job) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    job = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        done.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok()
    }

    /// Whether `(venue, symbol, interval)` may be saved (and preloaded): not a symbol with an underscore, and
    /// within [`MAX_SAVED_KEYS`] keys. A refusal is counted per bar and announced once per key. `live`: a seed
    /// or a close asks (the key IS held), not a preload (the subscription may still fail).
    fn admit(&self, venue: &str, symbol: &str, interval: &str, live: bool) -> bool {
        let key = (venue.to_string(), symbol.to_string(), interval.to_string());
        let mut claimed = false;
        let refusal = if !saveable_symbol(symbol) {
            Some(
                "its symbol contains an underscore: that is OANDA's own spelling, the name of the \
                 operator-backfilled day-grid series, and per-bar keys written into it would store the bars \
                 the two ingests share twice",
            )
        } else {
            let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
            if keys.contains(&key) {
                None
            } else if keys.len() >= MAX_SAVED_KEYS {
                Some(
                    "the saver holds its most keys already (every saved bar spends a commit key for good)",
                )
            } else {
                keys.insert(key.clone());
                claimed = true;
                None
            }
        };
        let Some(why) = refusal else {
            let mut unproven = self.unproven.lock().unwrap_or_else(PoisonError::into_inner);
            if live {
                unproven.remove(&key);
            } else if claimed {
                unproven.insert(key);
            }
            return true;
        };
        self.counters.skipped.fetch_add(1, Ordering::Relaxed);
        if self.warned.lock().unwrap_or_else(PoisonError::into_inner).insert(key) {
            tracing::warn!(
                venue,
                symbol,
                interval,
                "vike-datahub start history: this key is NOT saved or preloaded - {why}"
            );
        }
        false
    }

    /// The subscription a preload was made for FAILED (`handle_sub`): give the key's slot back to the saver
    /// unless a seed or a close has since shown the key live. Without it a refused mount would hold one of the
    /// [`MAX_SAVED_KEYS`] slots until the datahub restarts.
    pub(crate) fn release_unproven(&self, venue: &str, symbol: &str, interval: &str) {
        let key = (venue.to_string(), symbol.to_string(), interval.to_string());
        let was_unproven =
            self.unproven.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        if was_unproven {
            self.keys.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        }
    }

    fn enqueue(&self, job: Job, what: (&str, &str, &str)) {
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.jobs.try_send(job)
        {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                venue = what.0,
                symbol = what.1,
                interval = what.2,
                "vike-datahub start history: the save queue could not take this job (the store is \
                 stuck or the saver is gone); a closed bar of this series is NOT saved"
            );
        }
    }

    /// A CLOSED bar of a key some session holds (`StratSink::close_bar`, after the sessions were told).
    pub(crate) fn closed(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if !covers(venue, interval) || !self.admit(venue, symbol, interval, true) {
            return;
        }
        let job = Job::Save {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            bar,
        };
        self.enqueue(job, (venue, symbol, interval));
    }

    /// The venue's own seed of a key some session holds (`StratSink::seed_bars`, as the venue sent it): the
    /// worker reads the store, writes the one journal line, and saves the seed's bars the store lacks.
    pub(crate) fn seeded(&self, venue: &str, symbol: &str, interval: &str, seed: Vec<Bar>) {
        if !covers(venue, interval) || !self.admit(venue, symbol, interval, true) {
            return;
        }
        let job = Job::Compare {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            seed,
        };
        self.enqueue(job, (venue, symbol, interval));
    }

    /// SERVE, on the session thread, before the venue is asked: prepare the key's start history within
    /// `budget` and send it to `to` as a bar seed. Does NOTHING (no read, no allocation) unless the flag is
    /// on and the venue is saved. Returns how long it took, for the session's own budget.
    pub(crate) fn preload(
        &self,
        session: u64,
        venue: &str,
        symbol: &str,
        interval: &str,
        budget: Duration,
        to: &dyn LiveDataSink,
    ) -> Duration {
        if !self.serve || !covers(venue, interval) || !self.admit(venue, symbol, interval, false) {
            return Duration::ZERO;
        }
        let started = Instant::now();
        let (reader, gap, now) = (Arc::clone(&self.reader), self.gap.clone(), (self.clock)());
        let key = (session, venue.to_string(), symbol.to_string(), interval.to_string());
        // Every subscription of a session starts clean: a history prepared for an earlier, refused
        // subscription of this key must not be spliced over the venue's seed of this one.
        self.stash.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        let ask = key.clone();
        let outcome = run_within(budget, move || {
            prepare(reader.as_ref(), gap.as_deref(), &ask.1, &ask.2, &ask.3, now)
        });
        let spent = started.elapsed();
        match outcome {
            None => tracing::warn!(
                venue,
                symbol,
                interval,
                "vike-datahub start history (served): the store and the history channel did not answer \
                 within {} ms; the strategy gets only what the venue seeds",
                budget.as_millis()
            ),
            Some(Prepared::Withheld { why, stored }) => tracing::info!(
                venue,
                symbol,
                interval,
                stored,
                "vike-datahub start history (served): withheld - {why}; the strategy gets only what the \
                 venue seeds"
            ),
            Some(Prepared::Ready { seed, stored, fetched, gap }) => {
                tracing::info!(
                    venue,
                    symbol,
                    interval,
                    "vike-datahub start history (served): {stored} bars from the store, {fetched} \
                     fetched for the span {}, {} sent before the venue is asked (took {} ms)",
                    gap.map_or_else(
                        || "(none was missing)".to_string(),
                        |(from, to)| format!("{} to {}", stamp(Some(from)), stamp(Some(to)))
                    ),
                    seed.len(),
                    spent.as_millis()
                );
                to.seed_bars(venue, symbol, interval, seed.clone());
                self.stash.lock().unwrap_or_else(PoisonError::into_inner).insert(key, seed);
            }
        }
        spent
    }

    /// SERVE, on the venue client's thread, once per session that holds the key: the venue's own seed,
    /// spliced over the start history that was sent to THAT session, so a short REST seed never shrinks what
    /// the strategy was given. Without the flag, or without a prepared history for the session's key, the
    /// venue's seed is returned as it came.
    pub(crate) fn under_venue_seed(
        &self,
        session: u64,
        venue: &str,
        symbol: &str,
        interval: &str,
        venue_seed: &[Bar],
    ) -> Vec<Bar> {
        if !self.serve {
            return venue_seed.to_vec();
        }
        let key = (session, venue.to_string(), symbol.to_string(), interval.to_string());
        let prepared = self.stash.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        match prepared {
            Some(sent) => splice(&[sent.as_slice(), venue_seed], START_BARS),
            None => venue_seed.to_vec(),
        }
    }

    /// A session ended: its pending histories go with it (a joiner never gets a venue seed, so its entry
    /// would otherwise wait for ever).
    pub(crate) fn forget_session(&self, session: u64) {
        self.stash
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(s, ..), _| *s != session);
    }

    /// How many prepared histories still wait for a venue seed (the tests' witness).
    pub fn waiting(&self) -> usize {
        self.stash.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

// ---- production seams ---------------------------------------------------------------------------------------

/// The OANDA history channel through the gatewayhub: `RemoteKlines::credentialed`, built per request (the
/// gatewayhub reads the practice token once per connection, and nothing here keeps a token). The series name
/// the strategy lane uses is turned into OANDA's own instrument name for the fetch (`EURUSD` into `EUR_USD`),
/// and the bars come back under the strategy's name: they are spliced in memory, never written under a second
/// spelling. Any other venue has no row here yet and answers an `Err`, so SERVE withholds for it.
pub struct GatewayGap {
    link: vike_gatewayhub_client::client::GatewayLink,
}

impl GatewayGap {
    /// A gap source over `link`, the process's one gatewayhub link.
    pub fn new(link: vike_gatewayhub_client::client::GatewayLink) -> Self {
        Self { link }
    }
}

impl GapSource for GatewayGap {
    fn fetch(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, String> {
        use vike_data::source::KlineSource;
        if venue != "oanda" {
            return Err(format!("no history channel is wired for venue `{venue}`"));
        }
        let source =
            vike_gatewayhub_client::history::RemoteKlines::credentialed(self.link.clone(), venue);
        source
            .fetch(&vike_catalog::to_oanda_instrument(symbol), interval, start_ms, end_ms)
            .map_err(|e| e.to_string())
    }
}

/// A one-bar `KlineSource`: the existing kline ingest takes its rows from a source, and a closed bar the
/// strategy lane already holds is the whole answer. It touches no network.
#[cfg(feature = "backfill-serve")]
struct ClosedBar {
    venue: String,
    bar: Bar,
}

#[cfg(feature = "backfill-serve")]
impl vike_data::source::KlineSource for ClosedBar {
    fn venue(&self) -> &str {
        &self.venue
    }

    fn fetch(
        &self,
        _symbol: &str,
        _interval: &str,
        _start_ms: i64,
        _end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        Ok(vec![self.bar.clone()])
    }
}

/// The production [`BarWriter`]: one closed bar through `vike_backfill::kline_source::backfill_kline_source`,
/// the store's existing kline ingest. It brings the bar codec, the still-forming-candle guard (a bar whose
/// interval has not ended is refused, so a forming bar can never be stored as closed) and the commit key
/// `{venue}:{symbol}:{interval}:{start}-{end}` spelled for the bar's own window, so writing a bar twice is a
/// no-op and a restart can neither duplicate nor skip one.
#[cfg(feature = "backfill-serve")]
pub fn ingest_writer(store: Arc<vike_store::DataFusionHist>) -> BarWriter {
    Box::new(move |venue, symbol, interval, bar| {
        let step = step_of(interval).ok_or_else(|| format!("{interval:?} has no bar width"))?;
        // The belt behind `StartHistory::admit`: the writer itself never writes into the day-grid series.
        if !saveable_symbol(symbol) {
            return Err(format!(
                "`{symbol}` contains an underscore: it is the name of the operator-backfilled day-grid series, \
                 which per-bar keys would store twice"
            ));
        }
        // The ingest's own still-forming guard answers `Ok(0)`, the very value of an idempotent repeat, so a
        // close this box's clock calls early (a clock behind the venue's) would be dropped and counted as
        // "already stored". Said here as an error instead: a counted, announced hole.
        if bar.ts.saturating_add(step) > vike_model::now_ms() {
            return Err(format!(
                "the bar opened at {} has not ended on this box's clock (is it behind the venue's?)",
                bar.ts
            ));
        }
        let source = ClosedBar { venue: venue.to_string(), bar: bar.clone() };
        vike_backfill::kline_source::backfill_kline_source(
            &store,
            &source,
            symbol,
            interval,
            bar.ts,
            bar.ts.saturating_add(step - 1),
        )
        .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;
