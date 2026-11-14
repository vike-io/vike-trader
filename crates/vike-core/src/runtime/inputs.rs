//! A live mount's INPUTS (decision 0271's live half): the stored series its strategy reads by name
//! (`vike_model::Strategy::inputs`), answered through `vike_model::OrderBroker::input` as the last of its bars
//! that had CLOSED by the decision. The rule is the backtest's, one helper: `vike_marketdata::Bar::closed_at`,
//! which the simulator's view (`vike-simulator`'s `InputView`) steps with too. A bar step decides at its CLOSE (the
//! mount's `bar.ts` plus its interval), every other lane at its event's time.
//!
//! # Where the bars come from
//!
//! The HOST reads them through [`CoreConfig::input_opener`] (`vike-tradehub` reads the DATAHUB, the owner's
//! ruling of 2026-10-20): at mount, the input's stored history up to now; after that the host keeps it fresh
//! on the BAR lane under the input's [`InputRead::key`]. A venue's own candles come as the mount's own bars
//! do, over the strategy lane (`Ingest::BarSeed` / `Ingest::BarClose`); a lane's bars (a user's CSV import),
//! which nothing streams, are re-read by the host on a cold timer and sent as a seed under a key no venue
//! feed names. Every bar of an input key is filed into each mount input that reads it
//! ([`CoreThread::feed_inputs`]). A key no mount HEARS ([`CoreThread::input_only`]) is the inputs' alone: it
//! fills no paper order, moves no mark, fires no conditional and steps no strategy, because a higher
//! timeframe's range spans prices from before the orders resting now.
//!
//! # Refused at mount, by name
//!
//! Both doors open every declared input ([`open_mount_inputs`]) once the mount's engine resolved. A name the
//! host cannot resolve or read, an input it holds no bar of, or a core with no opener at all (a replay
//! core, or a test rig's) refuses the mount by name, as a backtest refuses a run; nothing is mounted. A restart
//! opens them again: nothing of an input is journaled or snapshotted.
//!
//! # The stale hold
//!
//! An input whose newest bar CLOSED more than two of its intervals before the decision is stale. **An input
//! whose market keeps a known trading calendar** ([`InputRead::calendar`]: an FX pair, an alpaca share) counts
//! only its market's OPEN time toward those two intervals, so a weekend never makes it stale, and a bar that is
//! only LATE holds nothing; it is stale once the calendar says a bar is MISSING: two of its bars of open market
//! passed since its newest closed, or a later bar arrived while the bar the decision should read did not
//! (`LiveInput::stale_at`, the grid in `runtime/bar_grid.rs`). A crypto input keeps the plain rule.
//! Its mount's OPENING orders are then refused by name ([`CoreThread::input_hold`], at the drain), while a covered
//! reduce, a cancel, an amend and a mass cancel pass: decision 0243's posture for a stale conversion rate. It
//! is said once a stale stretch, at WARN and on the ring, and once when the input is fresh again. A bar that
//! arrives past a period its market traded in leaves a hole in the history, said once too (`INPUT GAP`).
//!
//! # A shared close
//!
//! A decision at a close it shares with an input bar that has not arrived (the input's next bar closes exactly
//! then: [`CoreThread::shared_close_input`]) is held until that bar is filed here or a short grace passes, so live
//! reads what the backtest reads whichever close the feed delivers first: `runtime/shared_close.rs`.
//!
//! # The cost
//!
//! A core whose mounts read no input pays one `is_empty()` per bar message and one per-mount `Option` read
//! per strategy dispatch (beside `mount_delays`'), and one more per mount per bar step for the shared close:
//! no per-tick work. A mount with inputs is lent its set by
//! `Arc::clone` per dispatch, and `OrderBroker::input` is a binary search of the input's bars, paid by the
//! strategy that asks.

use super::strategy_drive::subscriptions::Audience;
use super::*;

/// What the host's [`InputOpener`] answers for one input name.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRead {
    /// The `(venue, symbol, interval)` the input's bars arrive under on the bar lane after the mount: a
    /// venue's own candles as the strategy lane streams them, or, for a lane, a key no venue feed names (the
    /// host spells it; `vike-tradehub` leads the venue with the lane, `user-mine:binance`).
    pub key: SeriesKey,
    /// One bar's length in ms: a bar opened at `ts` closes at `ts + interval_ms`.
    pub interval_ms: i64,
    /// Its stored bars up to now, in any order (sorted at mount).
    pub bars: Vec<Bar>,
    /// The trading calendar its market keeps, when its KEPT row states a class with closed time (an FX pair, an
    /// alpaca share: `vike_catalog::trading_calendar`, the backtest's coverage rule); `None` for a market open
    /// every minute and for one whose calendar is not known. With one, only its market's open time counts
    /// toward the two bars that make it STALE, and closed time never does ([`CoreThread::input_hold`]);
    /// without one, the plain rule of two bars stands.
    pub calendar: Option<vike_venues::SessionCalendar>,
}

/// The [`CoreConfig::input_opener`] shape: an input NAME, as the strategy declares it, and the mount's own
/// venue, symbol and interval (a one-part name, `"1d"`, reads that symbol at its bar length, the grammar a
/// backtest reads names by; the interval decides how deep the history is, as a backtest's own bar length
/// does), to the series it reads and its stored history up to now, or why it cannot be read. Called at mount
/// ON THE FOLD THREAD (cold, like a runtime mount's resolve), once per declared name, so the host BOUNDS the
/// read (`vike-tradehub` refuses the mount by name past its bound); the host keeps the series fresh on the
/// bar lane from then on.
pub type InputOpener = Box<dyn FnMut(&str, &str, &str, &str) -> Result<InputRead, String> + Send>;

/// One input of a mount: the name its strategy reads, the key its bars arrive under, their length, and
/// every bar of it the core holds, oldest first.
#[derive(Debug, Clone)]
pub(crate) struct LiveInput {
    name: String,
    key: SeriesKey,
    interval_ms: i64,
    bars: Vec<Bar>,
    calendar: Option<vike_venues::SessionCalendar>,
    /// The open and the close of the bar it should get next (`bar_grid::next_bar` of its newest), kept as its
    /// newest moves.
    due_open: i64,
    due: i64,
    /// The instant from which it is stale (`bar_grid::stale_from` of its newest bar's close), kept likewise.
    stale_from: i64,
}

impl LiveInput {
    fn new(
        name: String,
        key: SeriesKey,
        interval_ms: i64,
        bars: Vec<Bar>,
        calendar: Option<vike_venues::SessionCalendar>,
    ) -> Self {
        let mut s = LiveInput {
            name,
            key,
            interval_ms,
            bars,
            calendar,
            due_open: 0,
            due: 0,
            stale_from: i64::MAX,
        };
        s.refresh_due();
        s
    }

    /// Recompute the bar it should get next and [`Self::stale_from`] from its newest (`runtime/bar_grid.rs`).
    fn refresh_due(&mut self) {
        let Some(last) = self.bars.last().map(|b| b.timestamp) else { return };
        let calendar = self.calendar.as_ref();
        (self.due_open, self.due) = super::bar_grid::next_bar(last, self.interval_ms, calendar);
        let close = last.saturating_add(self.interval_ms);
        self.stale_from = super::bar_grid::stale_from(close, self.interval_ms, calendar);
    }

    /// **Whether the input is STALE at the decision `at`**, as `Some((closed, missing))`: the close of the
    /// newest bar the decision reads and, for an input with a calendar, the close of the bar its calendar says
    /// is MISSING. Without a calendar: its newest bar closed more than two of its bars before. With one, a bar
    /// that is only LATE holds nothing; a bar is missing once its market's OPEN time since the newest bar's
    /// close ran past two of its bars (closed hours do not count), or once a LATER bar arrived while the bar
    /// the decision should read did not (a decision behind its input's feed).
    fn stale_at(&self, at: i64) -> Option<(i64, Option<i64>)> {
        let last = self.bars.last()?;
        let closed = last.timestamp.saturating_add(self.interval_ms);
        if self.calendar.is_none() || self.interval_ms <= 0 {
            return (at.saturating_sub(closed) > self.interval_ms.saturating_mul(2))
                .then_some((closed, None));
        }
        let read = self.bars.partition_point(|b| b.closed_at(self.interval_ms, at));
        if read == self.bars.len() {
            return (at > self.stale_from).then_some((closed, Some(self.due)));
        }
        let newest = &self.bars[read.checked_sub(1)?];
        let (_, due) =
            super::bar_grid::next_bar(newest.timestamp, self.interval_ms, self.calendar.as_ref());
        (due <= at).then_some((newest.timestamp.saturating_add(self.interval_ms), Some(due)))
    }
}

/// A mount's inputs as of ONE decision: what [`LiveBroker`]'s `OrderBroker::input` answers from. The set is the
/// mount's own, lent by `Arc::clone`; `None` on the broker of a mount that reads no input.
#[derive(Debug, Clone)]
pub struct InputsAt {
    /// The decision's instant: a bar step's CLOSE, any other lane's event time.
    at: i64,
    set: Arc<Vec<LiveInput>>,
}

impl InputsAt {
    /// The bars of `name` that had CLOSED at the decision, oldest first: empty before its first closes,
    /// and for a name the mount did not declare. Public for a sandboxed mount's proxy
    /// (`vike-tradehub-core`'s `remote_strategy`), which sends what this answers off the fold thread.
    pub fn closed_bars(&self, name: &str) -> &[Bar] {
        let Some(s) = self.set.iter().find(|s| s.name == name) else { return &[] };
        &s.bars[..s.bars.partition_point(|b| b.closed_at(s.interval_ms, self.at))]
    }

    /// The decision's instant.
    pub(crate) fn at(&self) -> i64 {
        self.at
    }
}

/// One mount's inputs (`CoreThread::mount_inputs`, parallel to `mounts`).
pub(crate) struct MountInputs {
    set: Arc<Vec<LiveInput>>,
    /// The mount's own bar length in ms (`0` for an interval with none): a bar step decides at its close.
    bar_ms: i64,
    /// The input (its index in `set`) whose staleness was said, until it is fresh again: said once a stretch.
    stale_said: Option<usize>,
    /// The latest decision instant the stale hold judged at (`CoreThread::input_hold`).
    judged_at: i64,
}

impl MountInputs {
    /// The first input whose NEXT bar (the one after the newest it holds, or with a calendar the first its
    /// market trades in: `LiveInput::due`) closes exactly at `close` (a SHARED CLOSE,
    /// `runtime/shared_close.rs`): the one a decision at `close` waits for. An input whose bar closing then
    /// arrived is past it, and one further behind (stale) or with no bar length is never waited for.
    fn awaited(&self, close: i64) -> Option<usize> {
        self.set.iter().position(|s| s.interval_ms > 0 && !s.bars.is_empty() && s.due == close)
    }
}

/// A stale input holding a mount's opening orders: its index in the mount's set, its newest bar's close,
/// the instant it was judged at, and, for an input judged by its calendar, the close of the bar its market
/// traded in that is missing. `Copy`, so the drain carries it with no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaleInput {
    k: usize,
    closed: i64,
    at: i64,
    missing: Option<i64>,
}

/// **Open a mount's inputs, or refuse the mount by name** — the rule both doors apply
/// (`resolve_mount_engines` at spawn, `mount_strategy_runtime` at run time), after the engine resolved.
/// `Ok(None)` for a strategy that reads none, which is every strategy before decision 0271.
///
/// Every key the host answered joins `input_keys` whether or not the mount then stands, because the host
/// may already be keeping it fresh. The strategy's own call is guarded like every other strategy-code call
/// site: a panic in it refuses the mount.
pub(crate) fn open_mount_inputs(
    opener: Option<&mut InputOpener>,
    strategy: &(dyn Strategy<LiveBroker> + Send),
    (venue, symbol, interval): (&str, &str, &str),
    input_keys: &mut Vec<SeriesKey>,
) -> Result<Option<MountInputs>, String> {
    let declared = match catch_unwind(AssertUnwindSafe(|| strategy.inputs())) {
        Ok(d) => d,
        Err(payload) => {
            return Err(format!("— its `Strategy::inputs` panicked: {}", panic_text(payload)));
        }
    };
    if declared.is_empty() {
        return Ok(None);
    }
    let Some(open) = opener else {
        let names: Vec<String> = declared.iter().map(|n| format!("`{n}`")).collect();
        return Err(format!(
            "reads the input {} (`Strategy::inputs`), and this core reads no input (it was handed no \
             `CoreConfig::input_opener`; a tradehub reads them from the datahub, on its paper arm and its \
             live one), so it would read nothing where its backtest read one (decision 0271). Mount it on \
             a tradehub, or mount a strategy that reads no input",
            names.join(", ")
        ));
    };
    let mut set: Vec<LiveInput> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for name in &declared {
        if seen.contains(&name.as_str()) {
            continue;
        }
        seen.push(name);
        match open(name, venue, symbol, interval) {
            Ok(read) => {
                if !input_keys.contains(&read.key) {
                    input_keys.push(read.key.clone());
                }
                if read.bars.is_empty() {
                    missing.push(format!("`{name}`: the datahub holds no bar of it up to now"));
                    continue;
                }
                let mut bars = read.bars;
                bars.sort_by_key(|b| b.timestamp);
                bars.dedup_by_key(|b| b.timestamp);
                set.push(LiveInput::new(
                    name.clone(),
                    read.key,
                    read.interval_ms,
                    bars,
                    read.calendar,
                ));
            }
            Err(why) => missing.push(format!("`{name}`: {why}")),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "reads an input the datahub does not serve ({}): a live mount reads every input its strategy \
             declares, as a backtest does, so it is not mounted (decision 0271). Fill it (`vike-cli data \
             hist fetch …` for a venue's series; Studio → Data → Import CSV, or `vike-cli data hist import \
             csv-bars …`, for a user's lane) and mount again",
            missing.join("; ")
        ));
    }
    Ok(Some(MountInputs {
        set: Arc::new(set),
        bar_ms: vike_marketdata::interval::interval_ms(interval).unwrap_or(0),
        stale_said: None,
        judged_at: i64::MIN,
    }))
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Whether `(venue, symbol, interval)` is an input series that no mount HEARS
    /// (`Audience::Bar`, `mount_hears`' one rule): its bars are the inputs' and nothing else's. Asked only
    /// when some mount opened an input (`input_keys` not empty), once per bar message.
    pub(crate) fn input_only(&self, venue: &str, symbol: &str, interval: &str) -> bool {
        self.input_keys.iter().any(|k| k.0 == venue && k.1 == symbol && k.2 == interval)
            && !(0..self.mounts.len())
                .any(|i| self.mount_hears(i, venue, symbol, Audience::Bar { interval }))
    }

    /// File `bars` (ts-ascending) into every mount input whose series is `key`: a newer bar appends, one
    /// opened at the last bar's instant replaces it (a re-close), an older one is already covered and is
    /// dropped. A mount's set is held by no broker between dispatches, so `Arc::make_mut` copies nothing.
    pub(crate) fn feed_inputs(&mut self, key: &SeriesKey, bars: &[Bar]) {
        // A bar its market traded in and no bar came for (an input with a calendar only): said once a hole,
        // `(mount, input, from, to)`, after the borrow below.
        let mut holes: Vec<(usize, usize, i64, i64)> = Vec::new();
        for (m, mi) in self.mount_inputs.iter_mut().enumerate() {
            let Some(mi) = mi else { continue };
            if !mi.set.iter().any(|s| &s.key == key) {
                continue;
            }
            for (k, s) in Arc::make_mut(&mut mi.set).iter_mut().enumerate() {
                if &s.key != key {
                    continue;
                }
                for b in bars {
                    match s.bars.last_mut() {
                        Some(last) if b.timestamp < last.timestamp => {}
                        Some(last) if b.timestamp == last.timestamp => *last = b.clone(),
                        _ => {
                            if s.calendar.is_some() && s.interval_ms > 0 && b.timestamp > s.due_open
                            {
                                holes.push((m, k, s.due_open, b.timestamp));
                            }
                            s.bars.push(b.clone());
                            s.refresh_due();
                        }
                    }
                }
            }
        }
        if !holes.is_empty() {
            self.say_input_holes(&holes);
        }
        // …which may be the bar a held shared close waits for (`runtime/shared_close.rs`). One `is_empty()`
        // while nothing is held.
        if !self.shared_closes.is_empty() {
            self.release_arrived_shared_closes();
        }
    }

    /// Say each hole of `holes` (`(mount, input, from, to)`): the input's history has no bar from `from` to `to`
    /// while its calendar says its market traded then. Once a hole, at WARN and on the ring; the decisions while
    /// it was missing held their opening orders by name (`input_hold`).
    #[cold]
    #[inline(never)]
    fn say_input_holes(&mut self, holes: &[(usize, usize, i64, i64)]) {
        for &(m, k, from, to) in holes {
            let mount_id = self.mount_ids.get(m).cloned().unwrap_or_default();
            let name = self.input_name(m, k).to_string();
            tracing::warn!(
                target: "vike_core::core",
                mount_id = %mount_id,
                input = %name,
                from_ms = from,
                to_ms = to,
                "INPUT GAP: the input has no bar for a span its market traded in; its history holds a hole there"
            );
            self.note(format!(
                "INPUT GAP: `{mount_id}` reads `{name}`, which has no bar from {from} ms to {to} ms while its \
                 market was open: its history holds a hole there"
            ));
        }
    }

    /// **Whether mount `index`'s bar step at the close of its bar opened at `bar_timestamp` is a SHARED CLOSE** with an
    /// input bar that has not arrived (`runtime/shared_close.rs`): the decision's instant and that input's
    /// index, `None` for every other step and for a mount that reads no input (one `Option` read then).
    pub(crate) fn shared_close_input(
        &self,
        index: usize,
        bar_timestamp: i64,
    ) -> Option<(i64, usize)> {
        let mi = self.mount_inputs.get(index)?.as_ref()?;
        if mi.bar_ms <= 0 {
            return None;
        }
        let close = bar_timestamp.saturating_add(mi.bar_ms);
        mi.awaited(close).map(|k| (close, k))
    }

    /// The input mount `index`'s decision at `close` still waits for (its index), `None` once each arrived.
    pub(crate) fn input_awaited(&self, index: usize, close: i64) -> Option<usize> {
        self.mount_inputs.get(index)?.as_ref()?.awaited(close)
    }

    /// Mount `index`'s inputs as of `at` (an event's time), `None` for a mount that reads none.
    pub(crate) fn inputs_at(&self, index: usize, at: i64) -> Option<InputsAt> {
        let mi = self.mount_inputs.get(index)?.as_ref()?;
        Some(InputsAt { at, set: Arc::clone(&mi.set) })
    }

    /// Mount `index`'s inputs as of the CLOSE of its own bar opened at `bar_timestamp`, where a bar step decides.
    pub(crate) fn inputs_at_close(&self, index: usize, bar_timestamp: i64) -> Option<InputsAt> {
        let mi = self.mount_inputs.get(index)?.as_ref()?;
        Some(InputsAt { at: bar_timestamp.saturating_add(mi.bar_ms), set: Arc::clone(&mi.set) })
    }

    /// **The STALE HOLD of mount `mount_index` at the decision `at`**: which input holds its opening orders
    /// (its newest bar closed more than two of its intervals before the instant judged), `None` while every
    /// input is fresh and for a mount that reads no input. The instant judged is the LATEST this mount
    /// decided at: time moves forward, so a lane stamping an earlier instant (a fill's own ts) cannot make a
    /// stale input look fresh. Allocates nothing; the stretch is said ONCE, at WARN and on the ring, when it
    /// starts, and once when the input is fresh again.
    pub(crate) fn input_hold(&mut self, mount_index: usize, at: i64) -> Option<StaleInput> {
        let mi = self.mount_inputs.get_mut(mount_index)?.as_mut()?;
        mi.judged_at = mi.judged_at.max(at);
        let at = mi.judged_at;
        let stale = mi.set.iter().enumerate().find_map(|(k, s)| {
            let (closed, missing) = s.stale_at(at)?;
            Some(StaleInput { k, closed, at, missing })
        });
        let was = std::mem::replace(&mut mi.stale_said, stale.map(|s| s.k));
        if stale.map(|s| s.k) != was {
            self.say_input_staleness(mount_index, stale, was);
        }
        stale
    }

    /// The refusal text of an opening order `stale` holds, built only for an order it refuses.
    #[cold]
    pub(crate) fn input_stale_reason(&self, mount_index: usize, stale: StaleInput) -> String {
        let StaleInput { k, closed, at, missing } = stale;
        let name = self.input_name(mount_index, k);
        match missing {
            None => format!(
                "input-stale: the input `{name}` last closed at {closed} ms, more than two of its bars before \
                 this decision ({at} ms), so an opening order waits for a fresh bar; a reduce is not held \
                 (decision 0271)"
            ),
            Some(due) => format!(
                "input-stale: the input `{name}` is missing its bar closing at {due} ms, a span its market traded \
                 in (a later bar came, or two of its bars of open market passed since its newest closed at \
                 {closed} ms), by this decision ({at} ms), so an opening order waits for a fresh bar; a reduce is \
                 not held (decision 0271)"
            ),
        }
    }

    pub(crate) fn input_name(&self, mount_index: usize, k: usize) -> &str {
        self.mount_inputs[mount_index].as_ref().map_or("", |mi| mi.set[k].name.as_str())
    }

    /// Say a stale stretch starting (or moving to another input), or ending.
    #[cold]
    fn say_input_staleness(
        &mut self,
        mount_index: usize,
        now: Option<StaleInput>,
        was: Option<usize>,
    ) {
        let mount_id = self.mount_ids.get(mount_index).cloned().unwrap_or_default();
        let note = match (now, was) {
            (Some(StaleInput { k, closed, at, missing: None }), _) => {
                let name = self.input_name(mount_index, k).to_string();
                tracing::warn!(
                    target: "vike_core::core",
                    mount_id = %mount_id,
                    input = %name,
                    closed_ms = closed,
                    at_ms = at,
                    "INPUT STALE: the input's newest bar closed more than two of its intervals ago; the \
                     mount's OPENING orders are held until a fresh bar arrives, a reduce is not (said once)"
                );
                format!(
                    "INPUT STALE: `{mount_id}` reads `{name}`, whose newest bar closed at {closed} ms, more \
                     than two of its bars before {at} ms: its opening orders are held, a reduce is not"
                )
            }
            (Some(StaleInput { k, closed, at, missing: Some(due) }), _) => {
                let name = self.input_name(mount_index, k).to_string();
                tracing::warn!(
                    target: "vike_core::core",
                    mount_id = %mount_id,
                    input = %name,
                    closed_ms = closed,
                    due_ms = due,
                    at_ms = at,
                    "INPUT STALE: the input's bar for a span its market traded in is missing (a later bar came, or \
                     two of its bars of open market passed); the mount's OPENING orders are held until a fresh \
                     bar arrives, a reduce is not (said once)"
                );
                format!(
                    "INPUT STALE: `{mount_id}` reads `{name}`, whose bar closing at {due} ms (its market was \
                     open) is missing at {at} ms (its newest closed at {closed} ms): its opening orders are \
                     held, a reduce is not"
                )
            }
            (None, Some(k)) => {
                let name = self.input_name(mount_index, k).to_string();
                tracing::info!(
                    target: "vike_core::core",
                    mount_id = %mount_id,
                    input = %name,
                    "INPUT FRESH AGAIN: the mount's opening orders are no longer held"
                );
                format!(
                    "INPUT FRESH: `{mount_id}` reads `{name}` fresh again; its opening orders are no \
                     longer held"
                )
            }
            (None, None) => return,
        };
        self.note(note);
    }

    /// Whether an order of `side`/`qty` on `symbol` OPENS on mount `mount_index`'s own book: anything but a
    /// covered reduce of the position its engine holds there (`vike_model::is_covered_reduce`, the risk
    /// gate's one predicate).
    pub(crate) fn opens_on_mount(
        &self,
        mount_index: usize,
        venue: &str,
        symbol: &str,
        side: i32,
        qty: f64,
    ) -> bool {
        let engine_index = self.route_of(EngineRoute::Mount(mount_index), venue).unwrap_or(0);
        let (position, _) = self
            .engine(engine_index)
            .position_and_average_of(symbol, vike_model::events::PositionSide::Both);
        !vike_model::is_covered_reduce(side, position, qty)
    }
}
