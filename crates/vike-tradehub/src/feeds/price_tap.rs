//! `PriceTap` — the last price the CEX tick pump delivered for each `(venue, symbol)`, kept BESIDE
//! the core's ingest so the control door can tell whether a symbol it just admitted has a price yet
//! (decision 0174).
//!
//! The core holds prices in its engines' price boards, which only the fold thread reads; the door
//! must not enter the fold. [`DataClientSinkPriceTap`] sits between [`super::pump_book::L2BookFold`] and the
//! core's lane sink, forwards every verb unchanged and records the price of the three that carry
//! one (a quote's mid, a trade, a folded book's mid). One mutex, taken on the FEED's thread per
//! message and on a connection thread per door lookup: never on the fold.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vike_core::MAX_PRICE_AGE;
use vike_data::DataClientSink;
use vike_marketdata::{Bar, L2Book, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};
use vike_venues::link_deadman::DeadManLinkSource;

// How old the last price may be and still be a price is `vike_core::MAX_PRICE_AGE`, the one constant
// the core's risk sweeps (`venue_arming/deadman.rs`'s `price_only_config`) and its held releases read
// too. A pump that died (or a symbol that stopped quoting) must not leave an order to be priced and
// marked at a number from hours ago: past it the answer is "none yet", which the door turns into a
// prompt retry.

/// How often [`DataClientSinkPriceTap`] hands the core a MARK for a lane twin's own symbol while its pump ticks (a venue's mark
/// stream is a one-second feed; the engine reads the slot only when an order or a valuation asks).
const TWIN_MARK_EVERY: Duration = Duration::from_millis(500);

/// **The paper fill clock of a venue whose paper book has no bar series of its own** ([`PriceTap::set_paper_bars`]):
/// the interval a bar folded from the venue's quotes is filed under, and its width. One minute, the window a cTrader or
/// Polymarket paper mount's synthesizer runs at on a `1m` mount (`vike_tradehub_core::TickBarSynthesizer`).
const PAPER_BAR_INTERVAL: (&str, i64) = ("1m", 60_000);

/// How recently the VENUE must have stated a mark of its own for a mount's tap mark ([`PriceTap::set_mount_mark`])
/// to stand aside: a perpetual's kline lane carries the exchange's mark price, and the quote's mid must not
/// alternate with it on the same engine.
const NATIVE_MARK_FRESH: Duration = Duration::from_secs(5);

/// **The symbols this process's account rows mount a DERIVED lane twin on**, `(venue, symbol)`. The twin's symbol is
/// its parent row's `lane_twin_symbol` fact (owner, 2026-11-13: no symbol is compiled in), so the tap learns it from
/// the rows every time this process reads them ([`note_lane_twins`]: the boot's read and the account watcher's at
/// every change of the rows, both before any engine they start can take an order). Only ever GROWS: the tap does not
/// know whether a twin RUNS (a vanished twin is kept up while it holds anything), and a mark for a symbol no engine
/// trades is harmless.
static LANE_TWIN_SYMBOLS: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// **Learn the lane twins `accounts` derive**, and the symbol each one's row states ([`LANE_TWIN_SYMBOLS`]).
pub(crate) fn note_lane_twins(accounts: &vike_secrets::account_directory::AccountDirectory) {
    let found: Vec<(String, String)> = vike_venues::LANE_TWINS
        .iter()
        .filter(|twin| accounts.is_derived_lane_twin(twin.venue, twin.label))
        .filter_map(|twin| {
            let label = vike_model::accounts::account_keys::AccountLabel::parse(twin.label).ok()?;
            let symbol =
                vike_config::account_arming::account_symbol(Some(accounts), twin.venue, &label)?;
            Some((twin.venue.to_string(), symbol.to_string()))
        })
        .collect();
    if !found.is_empty() {
        LANE_TWIN_SYMBOLS.lock().unwrap_or_else(PoisonError::into_inner).extend(found);
    }
}

/// Whether `symbol` is a symbol `venue`'s DERIVED lane twin engine is mounted on, by the rows this process read
/// ([`note_lane_twins`]).
fn is_lane_twin_symbol(venue: &str, symbol: &str) -> bool {
    LANE_TWIN_SYMBOLS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&(venue.to_string(), symbol.to_string()))
}

/// Whether `quote` is two-sided: finite, a positive bid and an ask not below it. A one-sided quote's mid is HALF
/// the price (or negative), which is no mark.
fn two_sided(quote: &TickQuote) -> bool {
    quote.bid.is_finite() && quote.ask.is_finite() && quote.bid > 0.0 && quote.ask >= quote.bid
}

/// One `(venue, symbol)`'s last price, when it arrived, and the last TWO-SIDED top of book (`bid`, `ask`, when it
/// arrived) the quote or book that carried it stated: a trade print moves the price and leaves the top, which is
/// only ever what a venue said was its best bid and best ask ([`PriceTap::door_top`]).
type Last = (f64, Instant, Option<(f64, f64, Instant)>);

/// The last price seen per `(venue, symbol)`, and the symbols whose VENUE socket has a gap open.
#[derive(Default)]
pub struct PriceTap {
    last: Mutex<HashMap<(String, String), Last>>,
    /// `(venue, symbol)` of a LANE TWIN's own symbol -> when the tap last handed the core a mark for it
    /// ([`DataClientSinkPriceTap`]'s rule, [`Self::door_price`]'s reading).
    marked: Mutex<HashMap<(String, String), Instant>>,
    /// `(venue, symbol)` of a LABELLED engine's own symbol whose price lane the engine holds
    /// ([`Self::set_labelled_mark`]): marked in the core like a lane twin's, because the core hands a venue's quote,
    /// trade and book to the venue's default engine only.
    labelled: Mutex<HashSet<(String, String)>>,
    /// `(venue, symbol)` of a strategy MOUNT on a labelled account ([`Self::set_mount_mark`], decision 0245): marked
    /// in the core like a lane twin's, but only while the venue states no mark of its own ([`Self::native`]).
    mounted: Mutex<HashSet<(String, String)>>,
    /// When the VENUE last stated a mark for a [`Self::mounted`] key (the mark a feed passes through
    /// [`DataClientSinkPriceTap::mark_tick`], never the tap's own).
    native: Mutex<HashMap<(String, String), Instant>>,
    /// `(venue, symbol)` with a venue-source `GapStart` not yet closed by a `Live` (the core's link
    /// dead-man keeps the same window), with the stream word that opened it and when ([`Self::gap_open`]).
    /// Closing a pump closes the window with it ([`Self::end_gap`]), so the core never holds one for a
    /// stream nobody listens to any more.
    gaps: Mutex<HashMap<(String, String), (String, i64)>>,
    /// The sink the tap sits in front of ([`DataClientSinkPriceTap::new`] binds it, once): where [`Self::end_gap`] says `Live`.
    /// Unbound on a tap nobody wrapped (a test), where ending a gap only forgets it.
    sink: OnceLock<Arc<dyn DataClientSink>>,
    /// `Some` for a [`Self::labelled_quotes`] tap: the stream label the venue last sent per `(venue, symbol)`.
    labels: Option<Mutex<HashMap<(String, String), StreamDelay>>>,
    /// `true` for a [`Self::venue_clocked`] tap: a quote's age is counted from its VENUE time.
    venue_clocked: bool,
    /// The last TRADE PRINT per `(venue, symbol)` and when it arrived ([`Self::last_trade`]): a `Price` alert's
    /// fallback where the core holds no bar series of the pair (owner, 2026-11-15).
    trades: Mutex<HashMap<(String, String), (f64, Instant)>>,
    /// `(venue, symbol)` of a PAPER engine's own symbol on a venue priced through its demo login
    /// (`crate::feeds::paper_quotes_through_demo`), each with the fold that turns its quotes into the paper book's
    /// bars and their sides ([`Self::set_paper_bars`], decision 0285).
    paper_bars: Mutex<HashMap<(String, String), vike_tradehub_core::TickBarSynthesizer>>,
}

impl PriceTap {
    /// **A tap that ages a QUOTE from the venue's own time, not from its arrival** (oanda). The tick carries both
    /// the venue's `ts` and the receiving machine's `local_timestamp`, so what it was already worth when it got here is
    /// `local_timestamp - ts`; the price is then usable for [`MAX_PRICE_AGE`] minus THAT. OANDA's REST first price
    /// (`crates/bridges/oanda/src/feed/bar/seed.rs`) arrives up to its bound old, and aged from arrival it would
    /// stay a door price for the whole [`MAX_PRICE_AGE`] after it: the door admitting on a price the core's
    /// price-only sweep (which ages by the venue's `ts`, `PriceOnlyConfig::max_price_age`) already calls stale. A
    /// stream tick arrives a few hundred ms old, so for it nothing moves. A tick with no `ts` or no `local_timestamp` is
    /// aged from arrival, as ever. Every other tap ages from arrival: a venue whose quote `ts` is the time of its
    /// last CHANGE (a quiet stock's NBBO) would otherwise be refused while its stream is live.
    pub fn venue_clocked() -> Self {
        Self { venue_clocked: true, ..Self::default() }
    }

    /// How old `quote` already was on arrival, on a [`Self::venue_clocked`] tap (zero on any other, and when
    /// either stamp is missing or the venue's clock reads ahead of ours).
    fn quote_age(&self, quote: &TickQuote) -> Duration {
        if !self.venue_clocked || quote.timestamp <= 0 || quote.local_timestamp <= 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(u64::try_from(quote.local_timestamp - quote.timestamp).unwrap_or(0))
    }

    /// **A tap that prices from a two-sided quote of a stream the venue has LABELLED, whatever the label** (ibkr,
    /// decisions 0140 and 0174: an order MAY be priced off delayed data, the label is information and never a gate;
    /// it used to be `real_time_only`, which refused a delayed or frozen stream by name and so every new stock on
    /// an account with no real-time entitlement). The label is KEPT per `(venue, symbol)` and travels on to the
    /// core (the order's record and its `PRE-TRADE` line, `vike-core`'s `stamp_data_delay`): a real-time label
    /// when the venue serves real-time, which is what IBKR does for any class the account IS entitled to, so
    /// real-time is what prices wherever it exists. A trade print or a folded book never prices here, a
    /// one-sided or crossed quote is ignored, and [`Self::door_price`] answers "none yet" until the stream has a
    /// label at all (so a stream that never labels itself is never a price: fail-closed). Every other tap prices
    /// as it always did.
    ///
    /// ⚠ The price is only as old as its label says: a delayed one is about 15 minutes behind the market (IBKR's
    /// own figure; the tick states none) and a frozen one is the last value IBKR holds, from the close or earlier.
    /// What judges an order at such a mark (a market order's notional, the account exposure, an opted-in price
    /// collar) judges THAT price.
    pub fn labelled_quotes() -> Self {
        Self { labels: Some(Mutex::default()), ..Self::default() }
    }

    /// **What the control door asks** ([`crate::feeds::StrategyLaneCexWatcher::door_price`]): the latest price, or the
    /// reason there can be none. `Ok(None)` is "none yet" (retry); an `Err` is a refusal in words, which no tap
    /// produces now (the seam stays: the door and the facts source speak `Result`).
    ///
    /// **A lane twin's own symbol is "none yet" until the core was handed its mark.** The door reads a price
    /// here as "the engine can be judged at it", and the core routes a venue's quote, trade and book to the
    /// venue's DEFAULT engine (plus the labelled engines that admitted the symbol on demand), so the twin
    /// engine's own mounted symbol is priced in the twin only by the mark [`DataClientSinkPriceTap`] hands the core for it
    /// (`vike-core`'s `drain_market` mirrors a venue mark to every engine of the venue).
    ///
    /// # Errors
    /// None today.
    pub fn door_price(&self, venue: &str, symbol: &str) -> Result<Option<f64>, String> {
        let priced = self.labelled_price(venue, symbol);
        Ok(priced.filter(|_| !is_lane_twin_symbol(venue, symbol) || self.marked(venue, symbol)))
    }

    /// **When the price [`Self::door_price`] answers as `px` was MADE**, epoch ms on this node's clock: the venue's
    /// own time where the lane states one (a price that arrived `age` old is stamped `age` back, [`Self::note_aged`]),
    /// else its arrival. Asked WITH the price, so a newer tick of another price is never paired with it: `None` when
    /// the door has no price for the symbol now or holds another one. An admission carries it as its first mark's
    /// time (`vike_execution::SymbolAdmission::initial_mark_ms`).
    pub fn door_price_made_ms(&self, venue: &str, symbol: &str, px: f64) -> Option<i64> {
        let door = self.door_price(venue, symbol).ok().flatten()?;
        let at = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .filter(|(p, _, _)| p.to_bits() == px.to_bits() && door.to_bits() == px.to_bits())
            .map(|(_, at, _)| *at)?;
        let age = i64::try_from(at.elapsed().as_millis()).unwrap_or(i64::MAX);
        Some(vike_model::now_ms().saturating_sub(age))
    }

    /// **The best bid and best ask the venue last stated for `(venue, symbol)`, as the control door asks for a
    /// post-only order's crossing check**, or `None` when the tap holds no REAL top of book: it has seen no
    /// two-sided quote or book of the symbol, the last one is older than [`MAX_PRICE_AGE`], or the symbol has no
    /// [`Self::door_price`] (a lane twin's own symbol before its mark, a labelled stream that has stated no label).
    /// ⚠ A tap that only saw trade prints holds a last price and no top, and a mid is never turned into a bid and
    /// an ask: the caller then leaves the judgement to the venue. A labelled-quotes stream (ibkr) counts only
    /// while its label says real-time, because a delayed or frozen quote is not the book an order meets.
    pub fn door_top(&self, venue: &str, symbol: &str) -> Option<(f64, f64)> {
        self.door_price(venue, symbol).ok().flatten()?;
        if let Some(labels) = &self.labels {
            let real_time = labels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&(venue.to_string(), symbol.to_string()))
                .is_some_and(|delay| matches!(delay, StreamDelay::RealTime));
            if !real_time {
                return None;
            }
        }
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .and_then(|(_, _, top)| *top)
            .filter(|(_, _, at)| at.elapsed() <= MAX_PRICE_AGE)
            .map(|(bid, ask, _)| (bid, ask))
    }

    /// Whether the tap handed the core a mark for `(venue, symbol)` no longer ago than [`MAX_PRICE_AGE`].
    fn marked(&self, venue: &str, symbol: &str) -> bool {
        self.marked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .is_some_and(|at| at.elapsed() <= MAX_PRICE_AGE)
    }

    /// Turn the mark of a LABELLED engine's own symbol on or off (decision 0198): the registry that holds the
    /// engine's lane (`TradeLeaseRegistry`) says `on` while a labelled engine of `venue` holds `symbol`'s lane open and `off`
    /// when none does, so a symbol nobody holds is never marked.
    pub fn set_labelled_mark(&self, venue: &str, symbol: &str, on: bool) {
        let mut labelled = self.labelled.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (venue.to_string(), symbol.to_string());
        if on {
            labelled.insert(key);
        } else {
            labelled.remove(&key);
        }
    }

    /// Turn the mark of a strategy MOUNT on a labelled account on or off (decision 0245): a mount works by candles
    /// and by ticks, and the core hands a venue's quote, trade and book to the venue's DEFAULT engine only, so the
    /// mounted engine's board was written at its bar closes and nothing between them (the 30-second risk clock
    /// skipped it as stale). Unlike [`Self::set_labelled_mark`] this mark stands aside while the venue states its
    /// own (`NATIVE_MARK_FRESH`, five seconds), so an engine never alternates between an exchange mark and a mid.
    pub fn set_mount_mark(&self, venue: &str, symbol: &str, on: bool) {
        let key = (venue.to_string(), symbol.to_string());
        let mut mounted = self.mounted.lock().unwrap_or_else(PoisonError::into_inner);
        if on {
            mounted.insert(key);
        } else {
            mounted.remove(&key);
            drop(mounted);
            self.native
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&(venue.to_string(), symbol.to_string()));
        }
    }

    /// **Fold the quotes of `(venue, symbol)` into a paper book's bars, or stop** (the owner's ruling of 2026-11-16): the
    /// registry says `on` while a PAPER engine trades `symbol` on a venue whose paper book has no bar series of its own
    /// and is priced through the venue's demo login (`TradeLeaseRegistry::sync_paper_quotes`). Each bar is
    /// [`PAPER_BAR_INTERVAL`] wide, the mid's path with the bid's and the ask's beside it, so the book fills a buy on
    /// the ask and a sell on the bid (decision 0285). A fold switched off forgets its open window. Idempotent.
    pub fn set_paper_bars(&self, venue: &str, symbol: &str, on: bool) {
        let mut bars = self.paper_bars.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (venue.to_string(), symbol.to_string());
        if on {
            bars.entry(key).or_insert_with(|| {
                vike_tradehub_core::TickBarSynthesizer::new(PAPER_BAR_INTERVAL.1)
            });
        } else {
            bars.remove(&key);
        }
    }

    /// The paper bar `quote` closes for `(venue, symbol)` with its sides, when the pair is folded
    /// ([`Self::set_paper_bars`]) and the quote is two-sided and crosses into a new window. One lock on an empty map for
    /// every quote of a node that folds none. A quote with no venue time is placed at its arrival.
    pub(crate) fn paper_bar(
        &self,
        venue: &str,
        symbol: &str,
        quote: &TickQuote,
    ) -> Option<(Bar, Option<vike_marketdata::BarSides>)> {
        let mut bars = self.paper_bars.lock().unwrap_or_else(PoisonError::into_inner);
        if bars.is_empty() || !two_sided(quote) {
            return None;
        }
        let fold = bars.get_mut(&(venue.to_string(), symbol.to_string()))?;
        if quote.timestamp > 0 {
            fold.on_quote(quote)
        } else {
            fold.on_quote(&TickQuote { timestamp: vike_model::now_ms(), ..quote.clone() })
        }
    }

    /// The venue stated its own mark for `(venue, symbol)`: noted only for a [`Self::set_mount_mark`] key, so a
    /// tap with no labelled mount (the common case) takes one lock on an empty set and writes nothing.
    fn note_native_mark(&self, venue: &str, symbol: &str) {
        let key = {
            let mounted = self.mounted.lock().unwrap_or_else(PoisonError::into_inner);
            if mounted.is_empty() {
                return;
            }
            let key = (venue.to_string(), symbol.to_string());
            if !mounted.contains(&key) {
                return;
            }
            key
        };
        self.native.lock().unwrap_or_else(PoisonError::into_inner).insert(key, Instant::now());
    }

    /// Whether a labelled MOUNT's `(venue, symbol)` is marked now: switched on and the venue has not stated a mark
    /// of its own within [`NATIVE_MARK_FRESH`].
    fn marks_mount(&self, venue: &str, symbol: &str) -> bool {
        let key = {
            // Nothing is allocated while no mount is on a labelled account (the common case): this runs per tick.
            let mounted = self.mounted.lock().unwrap_or_else(PoisonError::into_inner);
            if mounted.is_empty() {
                return false;
            }
            let key = (venue.to_string(), symbol.to_string());
            if !mounted.contains(&key) {
                return false;
            }
            key
        };
        !self
            .native
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .is_some_and(|at| at.elapsed() <= NATIVE_MARK_FRESH)
    }

    /// Whether the core is handed a mark for `(venue, symbol)`: a lane twin's own symbol (by the rows read), a
    /// labelled engine's own symbol whose lane is held ([`Self::set_labelled_mark`]), or the symbol of a mount on a
    /// labelled account while the venue states no mark of its own ([`Self::set_mount_mark`]).
    pub(crate) fn marks_symbol(&self, venue: &str, symbol: &str) -> bool {
        if is_lane_twin_symbol(venue, symbol) {
            return true;
        }
        // Nothing is allocated while no labelled engine holds a lane (the common case): this runs per tick.
        let held = {
            let labelled = self.labelled.lock().unwrap_or_else(PoisonError::into_inner);
            !labelled.is_empty() && labelled.contains(&(venue.to_string(), symbol.to_string()))
        };
        held || self.marks_mount(venue, symbol)
    }

    /// Whether a mark for `(venue, symbol)` is due now (none yet, or the last one is [`TWIN_MARK_EVERY`] old),
    /// and if so, note it as handed over: the caller sends it.
    fn mark_due(&self, venue: &str, symbol: &str) -> bool {
        let mut marked = self.marked.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = marked.entry((venue.to_string(), symbol.to_string()));
        match slot {
            std::collections::hash_map::Entry::Occupied(mut held) => {
                if held.get().elapsed() < TWIN_MARK_EVERY {
                    return false;
                }
                held.insert(Instant::now());
            }
            std::collections::hash_map::Entry::Vacant(free) => {
                free.insert(Instant::now());
            }
        }
        true
    }

    /// The price of a [`Self::labelled_quotes`] tap's stream: none until the stream has stated a label, then the
    /// last price whatever the label says. Any other tap's is its last price.
    fn labelled_price(&self, venue: &str, symbol: &str) -> Option<f64> {
        let Some(labels) = &self.labels else { return self.price(venue, symbol) };
        let labelled = labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&(venue.to_string(), symbol.to_string()));
        labelled.then(|| self.price(venue, symbol)).flatten()
    }

    /// Whether a trade print or a folded book may price (not on a [`Self::labelled_quotes`] tap).
    fn marks_from_prints(&self) -> bool {
        self.labels.is_none()
    }

    /// The price one quote records: its mid, but on a labelled-quotes tap only when it is two-sided (finite, a
    /// positive bid, an ask not below it).
    fn quote_price(&self, quote: &TickQuote) -> Option<f64> {
        (self.labels.is_none() || two_sided(quote)).then(|| quote.mid())
    }

    /// Record the label of a quote stream (labelled-quotes taps; the first label and every change). A price noted
    /// under another label is forgotten: it is not this label's, and a delayed price must never be read under a
    /// real-time one the moment the stream is relabelled. A label that is not real-time is said once per change,
    /// in the operator's words (the order's own record and its `PRE-TRADE` line are the core's).
    fn note_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        let Some(labels) = &self.labels else { return };
        if stream != StreamLabel::Quotes.word() {
            return;
        }
        let changed = labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((venue.to_string(), symbol.to_string()), delay)
            != Some(delay);
        if changed {
            match delay {
                StreamDelay::RealTime => {}
                StreamDelay::Delayed { .. } => tracing::info!(
                    venue,
                    symbol,
                    "{venue} {symbol}: delayed data (about 15 minutes old): an order on it is priced off it \
                     and recorded as priced off delayed data (decision 0140; not a gate)"
                ),
                StreamDelay::Frozen => tracing::info!(
                    venue,
                    symbol,
                    "{venue} {symbol}: frozen data (the last value the venue holds, from the close or earlier): \
                     an order on it is priced off it and recorded as priced off frozen data (decision 0140; \
                     not a gate)"
                ),
            }
            self.forget(venue, symbol);
        }
    }

    /// **The last trade PRINT of `symbol` on `venue`** no older than [`MAX_PRICE_AGE`], whatever the tap prices by
    /// (a labelled-quotes tap records prints too): a `Price` alert compares against it where the core holds no bar
    /// series of the pair. `None` when the lane delivered no print in that time.
    pub fn last_trade(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.trades
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .filter(|(_, at)| at.elapsed() <= MAX_PRICE_AGE)
            .map(|(px, _)| *px)
    }

    /// A price for `symbol` on `venue` no older than [`MAX_PRICE_AGE`], or `None` if the pump has
    /// delivered none (in that time).
    pub fn price(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.price_at(venue, symbol, Instant::now())
    }

    fn price_at(&self, venue: &str, symbol: &str, now: Instant) -> Option<f64> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .filter(|(_, at, _)| now.saturating_duration_since(*at) <= MAX_PRICE_AGE)
            .map(|(px, _, _)| *px)
    }

    fn note(&self, venue: &str, symbol: &str, px: f64) {
        self.note_at(venue, symbol, px, Instant::now(), None);
    }

    /// [`Self::note`] for a price that was already `age` old on arrival: it is stamped `age` ago, so
    /// [`Self::price`] gives it up `age` sooner. A price already past [`MAX_PRICE_AGE`] on arrival is
    /// never noted (it would be no price at once, and must not replace a younger one). `top` is the
    /// TWO-SIDED best bid and best ask the same tick stated, when it stated both ([`Self::door_top`]).
    fn note_aged(
        &self,
        venue: &str,
        symbol: &str,
        px: f64,
        age: Duration,
        top: Option<(f64, f64)>,
    ) {
        if age > MAX_PRICE_AGE {
            return;
        }
        let now = Instant::now();
        self.note_at(venue, symbol, px, now.checked_sub(age).unwrap_or(now), top);
    }

    /// One insert under the one lock this tap always took: a `top` replaces the kept one, `None` (a trade print)
    /// leaves it as it was, with its own stamp.
    fn note_at(&self, venue: &str, symbol: &str, px: f64, at: Instant, top: Option<(f64, f64)>) {
        if px.is_finite() && px > 0.0 {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            let slot =
                last.entry((venue.to_string(), symbol.to_string())).or_insert((px, at, None));
            slot.0 = px;
            slot.1 = at;
            if let Some((bid, ask)) = top {
                slot.2 = Some((bid, ask, at));
            }
        }
    }

    /// Whether the venue's own socket has a gap open for `symbol` right now: the core's link dead-man holds a
    /// window for it that only a `Live` closes ([`Self::end_gap`] is the close that sends it).
    pub fn gap_open(&self, venue: &str, symbol: &str) -> bool {
        self.gap_since(venue, symbol).is_some()
    }

    /// When (epoch ms) the open venue gap for `symbol` began, `None` when no gap is open ([`Self::gap_open`]).
    pub fn gap_since(&self, venue: &str, symbol: &str) -> Option<i64> {
        self.gaps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|(_, at_ts_ms)| *at_ts_ms)
    }

    /// **The pump of `symbol` was closed, so its gap is over: say `Live` for it** to the sink the tap sits in
    /// front of, and forget the gap. The core's link dead-man keeps a window per `(venue, symbol, source)` from a
    /// venue `GapStart` until a venue `Live` (`vike-core`'s `runtime/link_deadman.rs`), and a stream whose pump
    /// nobody listens to any more will never say it: left open, the window trips after its grace (cancel and a
    /// HALT of the whole venue) for a symbol no account admitted. The `Live` carries the stream word and the gap
    /// time the `GapStart` did, and nothing is sent for a symbol with no gap open. A no-op without a bound sink.
    ///
    /// ⚠ Call it AFTER the lane is closed (an unsubscribed pump cannot open a new gap behind the `Live`), and
    /// before [`Self::forget`], which clears the gap without telling anyone.
    pub fn end_gap(&self, venue: &str, symbol: &str) {
        let gap = self
            .gaps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(venue.to_string(), symbol.to_string()));
        if let (Some((stream, at_ts_ms)), Some(sink)) = (gap, self.sink.get()) {
            sink.stream_status(
                venue,
                symbol,
                &stream,
                StreamStatus::Live { gap_started_timestamp_ms: Some(at_ts_ms) },
            );
        }
    }

    /// The pump of `symbol` was closed: drop its price and any gap it left open (a re-subscribe starts from
    /// none of either). A pump closed under an open gap goes through [`Self::end_gap`] first, which tells the core.
    pub fn forget(&self, venue: &str, symbol: &str) {
        let key = (venue.to_string(), symbol.to_string());
        self.last.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.trades.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.marked.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.gaps.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
    }

    /// Follow the window the core's link dead-man keeps for the venue source: a `GapStart` opens it, a
    /// `Live` closes it, `Stale` moves nothing, and the strat-link and trade-link words are other sources.
    pub(crate) fn note_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: StreamStatus,
    ) {
        if DeadManLinkSource::of(stream) != DeadManLinkSource::Venue {
            return;
        }
        let key = (venue.to_string(), symbol.to_string());
        let mut gaps = self.gaps.lock().unwrap_or_else(PoisonError::into_inner);
        match status {
            // The FIRST word and time of an outage stay (the core's window is not moved by a repeat either).
            StreamStatus::GapStart { at_timestamp_ms: at_ts_ms } => {
                gaps.entry(key).or_insert_with(|| (stream.to_string(), at_ts_ms));
            }
            StreamStatus::Live { .. } => {
                gaps.remove(&key);
            }
            StreamStatus::Stale { .. } => {}
        }
    }
}

/// A [`DataClientSink`] that records prices into a [`PriceTap`] and forwards everything to `inner`.
pub(crate) struct DataClientSinkPriceTap {
    inner: Arc<dyn DataClientSink>,
    tap: Arc<PriceTap>,
    /// The core's bar lane, where a paper bar folded from quotes goes ([`PriceTap::set_paper_bars`]); `None` on a
    /// sink that hands the core none.
    bars: Option<vike_execution::BarSender>,
}

impl DataClientSinkPriceTap {
    /// Wrap `inner`, recording into `tap`. The tap learns `inner` too, once, so closing a pump under an open
    /// gap can say `Live` where the gap was said ([`PriceTap::end_gap`]).
    pub(crate) fn new(inner: Arc<dyn DataClientSink>, tap: Arc<PriceTap>) -> Self {
        let _ = tap.sink.set(Arc::clone(&inner));
        Self { inner, tap, bars: None }
    }

    /// This sink handing the core the paper bars its tap folds ([`PriceTap::set_paper_bars`]) on `bars`.
    pub(crate) fn with_bars(mut self, bars: vike_execution::BarSender) -> Self {
        self.bars = Some(bars);
        self
    }

    /// **A lane twin's own symbol is priced in its engine by the throttled TOP OF BOOK**, handed to the core
    /// beside the tick on the conflating mark lane. `vike-core` routes a venue's quote, trade and book to the
    /// venue's DEFAULT engine and to the labelled engines holding a symbol admitted on demand
    /// (`CoreThread::admitted_fanout`), and the twin's own symbol is the mounted one: no tick of it ever reached
    /// the twin's price board, so an order there was judged with no price (a market order at notional zero, every
    /// cap passing; a limit order against itself). The lane mirrors to every engine of the venue
    /// (`drain_market`), and the same lane prices a position the twin holds. Throttled to [`TWIN_MARK_EVERY`];
    /// nothing for any other symbol but a LABELLED engine's own symbol while the engine holds its lane (decision
    /// 0198, [`PriceTap::marks_symbol`]): the same reason, the same lane, the same rules for a bad frame.
    ///
    /// ⚠ **The BID and the ASK, never a mid** (owner, 2026-11-15; decision 0208's amendment): the core files them
    /// as a side-aware quote (`DataClientSink::top_of_book_tick`), so a long is valued at the bid and a short at
    /// the ask, and a stop on the symbol triggers on its side (a sell on the bid, a buy on the ask). Only a quote
    /// or a folded book carries a top; a trade print hands the core nothing here.
    ///
    /// Called AFTER the tick is forwarded and BEFORE the price is recorded, so a price the door reads is behind
    /// both of them (the module's forward-first rule).
    ///
    /// ⚠ **Only a usable top is handed over** (review r2 re-review): finite, a positive bid and an ask not below
    /// it ([`two_sided`]'s rule), so a one-sided frame never prices the twin while the door still holds the
    /// previous good price; the throttle is spent only by a top that is sent. (`DataClientSink::top_of_book_tick`
    /// reports no outcome, so "handed over" is all the tap can know: a full ingest queue delays the top to the
    /// next publish.)
    ///
    /// ⚠ **The top's time is the venue's when the tick has one, else its arrival.** Binance's raw `@bookTicker`
    /// carries no event time (`decode_book_ticker`: `ts` 0), and the core's price-only sweep ages a price by
    /// that stamp (`price_only_basis`, bound [`MAX_PRICE_AGE`]): a 0 is a price from 1970, so a live, ticking
    /// twin read as stale for good, its engine was skipped by the margin call and (node-wide) the drawdown
    /// latch, and the door, which ages by arrival, called the same price fresh.
    fn mark_lane_twin(&self, venue: &str, symbol: &str, bid: f64, ask: f64, ts: i64) {
        if bid.is_finite()
            && ask.is_finite()
            && bid > 0.0
            && ask >= bid
            && self.tap.marks_symbol(venue, symbol)
            && self.tap.mark_due(venue, symbol)
        {
            let ts = if ts > 0 { ts } else { vike_model::now_ms() };
            self.inner.top_of_book_tick(venue, symbol, bid, ask, ts);
        }
    }
}

impl DataClientSink for DataClientSinkPriceTap {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.tap.note_native_mark(venue, symbol);
        self.inner.mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }
    fn top_of_book_tick(&self, venue: &str, symbol: &str, bid: f64, ask: f64, ts: i64) {
        self.inner.top_of_book_tick(venue, symbol, bid, ask, ts);
    }
    vike_data::forward_l2_snapshot!(inner);
    // ⚠ **FORWARD FIRST, THEN RECORD, in the three verbs that carry a price.** The door treats a price in this tap
    // as "the engine can be judged at it" (an opening order on a symbol with a feed is held until one exists),
    // and the core folds its ingest queue in order: a tick handed to `inner` BEFORE the price is recorded is
    // ahead of any command the door sends after it sees the price. Recording first left a window in which the
    // door saw the price, sent the order, and the order folded before the tick: an engine with no mark for the
    // symbol (a lane twin's own symbol, whose admission cannot carry the mark) judges a market order at a
    // zero notional and every cap passes it. The price to record is read BEFORE the tick is moved.
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        let (px, ts, age) =
            (self.tap.quote_price(&quote), quote.timestamp, self.tap.quote_age(&quote));
        let markable = two_sided(&quote);
        // The best bid and ask this quote states, read BEFORE the tick is moved: the door's post-only check
        // meets them ([`PriceTap::door_top`]); a one-sided quote states no top.
        let top = markable.then_some((quote.bid, quote.ask));
        // A paper book's bar closes BEFORE the quote that closed it is forwarded (`vike_tradehub_core::TickSink`'s
        // order): the fill uses the quotes that stood through the window.
        if let Some(bars) = &self.bars
            && let Some((bar, sides)) = self.tap.paper_bar(venue, symbol, &quote)
        {
            let _ = bars.close(vike_execution::BarUpdate {
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                interval: PAPER_BAR_INTERVAL.0.to_string(),
                bar,
                sides: sides.map(Box::new),
            });
        }
        self.inner.quote(venue, symbol, quote);
        if let Some((bid, ask)) = top {
            self.mark_lane_twin(venue, symbol, bid, ask, ts);
        }
        if let Some(px) = px {
            // The door sizes a LIVE twin's priceless order at this price: for the twin's own symbol a one-sided
            // quote's half mid is not recorded either (every other symbol records what it always did).
            if markable || !is_lane_twin_symbol(venue, symbol) {
                self.tap.note_aged(venue, symbol, px, age, top);
            }
        }
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        let px = trade.price;
        self.inner.trade(venue, symbol, trade);
        // A print states no top: it is never the lane's price (`mark_lane_twin`), only the door's.
        if self.tap.marks_from_prints() {
            self.tap.note(venue, symbol, px);
        }
        // The last print, for a `Price` alert on a pair the core holds no bar series of (`PriceTap::last_trade`).
        if px.is_finite() && px > 0.0 {
            self.tap
                .trades
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert((venue.to_string(), symbol.to_string()), (px, Instant::now()));
        }
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        // The best bid and ask are read ONCE: the mid is `L2Book::mid`'s own `(bid + ask) / 2` (`Some` for a
        // two-sided book), and a locked-or-crossed or non-positive book states no real top.
        let (mid, top) = match (self.tap.marks_from_prints(), book.best_bid(), book.best_ask()) {
            (true, Some(bid), Some(ask)) => (
                Some((bid.price + ask.price) / 2.0),
                (bid.price > 0.0 && ask.price >= bid.price).then_some((bid.price, ask.price)),
            ),
            _ => (None, None),
        };
        self.inner.book(venue, symbol, book);
        if let Some((bid, ask)) = top {
            self.mark_lane_twin(venue, symbol, bid, ask, vike_model::now_ms());
        }
        if let Some(mid) = mid {
            self.tap.note_at(venue, symbol, mid, Instant::now(), top);
        }
    }
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        self.inner.book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.tap.note_status(venue, symbol, stream, status);
        self.inner.stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.tap.note_delay(venue, symbol, stream, delay);
        self.inner.data_delay(venue, symbol, stream, delay);
    }
    fn funding(&self, venue: &str, symbol: &str, funding: vike_marketdata::FundingRate) {
        self.inner.funding(venue, symbol, funding);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_marketdata::L2BookLevel;

    /// A depth snapshot carries no price the tap records: every argument reaches the sink behind the
    /// tap untouched, once.
    #[test]
    fn a_depth_snapshot_is_forwarded_unchanged() {
        let inner = Arc::new(vike_data::RecordingSink::default());
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(Arc::clone(&inner) as Arc<dyn DataClientSink>, tap);
        let (bids, asks) = (vec![L2BookLevel::new(99.0, 1.0)], vec![L2BookLevel::new(101.0, 2.0)]);
        sink.l2_snapshot("binance", "BTCUSDT", 0.5, bids.clone(), asks.clone(), 7);
        let want = ("binance".to_string(), "BTCUSDT".to_string(), 0.5, bids, asks, 7);
        assert_eq!(inner.l2_snapshots(), [want]);
        assert_eq!(inner.recorded().len(), 1, "one call, nothing else");
    }

    struct Null;
    impl DataClientSink for Null {
        fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
        fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
        fn quote(&self, _: &str, _: &str, _: TickQuote) {}
        fn trade(&self, _: &str, _: &str, _: TickTrade) {}
        fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
    }

    use crate::testutil::quote_of;

    fn print_at(price: f64) -> TickTrade {
        TickTrade {
            timestamp: 1,
            local_timestamp: 0,
            price,
            size: 1.0,
            is_buyer_maker: false,
            symbol: Default::default(),
        }
    }

    const AAPL: &str = "AAPL.SMART.USD";

    fn label(sink: &DataClientSinkPriceTap, delay: StreamDelay) {
        sink.data_delay("ibkr", AAPL, StreamLabel::Quotes.word(), delay);
    }

    /// **A two-sided quote of a stream labelled REAL-TIME prices** on a labelled-quotes tap (ibkr): the mid,
    /// read back through the door's `door_price`.
    #[test]
    fn a_real_time_two_sided_quote_prices_a_labelled_quotes_tap() {
        let tap = Arc::new(PriceTap::labelled_quotes());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "nothing yet");
        label(&sink, StreamDelay::RealTime);
        sink.quote("ibkr", AAPL, quote_of(149.98, 150.02));
        let px = tap.door_price("ibkr", AAPL).expect("a real-time stream").expect("one arrived");
        assert!((px - 150.0).abs() < 1e-9, "{px}");
        assert_eq!(tap.door_price("ibkr", "TSLA.SMART.USD"), Ok(None), "another symbol has none");
    }

    /// **A stock with only DELAYED (or frozen) ticks is priced, not refused** (decision 0140: the label is
    /// information, never a gate; this was "refused by name", and so was every new stock on an account with no
    /// real-time entitlement), and a stream with no label at all is still "none yet": a quote is not a mark
    /// until the venue has said what it is.
    #[test]
    fn a_delayed_or_frozen_quote_prices_and_an_unlabelled_one_does_not() {
        let tap = Arc::new(PriceTap::labelled_quotes());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "no label yet: not a price");

        label(&sink, StreamDelay::DELAYED);
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(150.0)), "delayed prices");

        label(&sink, StreamDelay::Frozen);
        sink.quote("ibkr", AAPL, quote_of(148.0, 152.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(150.0)), "frozen prices");
    }

    /// A one-sided, crossed or non-finite quote is ignored whatever the label, and a trade print or a folded
    /// book never prices.
    #[test]
    fn a_labelled_quotes_tap_ignores_what_is_not_a_two_sided_quote() {
        let tap = Arc::new(PriceTap::labelled_quotes());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        for delay in [StreamDelay::RealTime, StreamDelay::DELAYED] {
            label(&sink, delay);
            for (bid, ask) in
                [(0.0, 151.0), (149.0, 0.0), (151.0, 149.0), (f64::NAN, 151.0), (-1.0, -1.0)]
            {
                sink.quote("ibkr", AAPL, quote_of(bid, ask));
                assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "{delay:?} {bid}/{ask}");
            }
            sink.trade("ibkr", AAPL, print_at(150.0));
            assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "a last trade is not a mark here");
        }
    }

    /// A price noted under one label is forgotten when the label changes: a delayed price must not be served
    /// under a real-time label the moment the stream is relabelled (the order's record would call a quarter of
    /// an hour old data real-time), nor the other way round. What the venue serves real-time is what prices.
    #[test]
    fn a_relabelled_stream_forgets_the_price_of_the_other_label() {
        let tap = Arc::new(PriceTap::labelled_quotes());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        label(&sink, StreamDelay::DELAYED);
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(150.0)));
        label(&sink, StreamDelay::RealTime);
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "the delayed price is gone");
        sink.quote("ibkr", AAPL, quote_of(150.0, 152.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(151.0)), "the real-time one prices");
        label(&sink, StreamDelay::DELAYED);
        assert_eq!(
            tap.door_price("ibkr", AAPL),
            Ok(None),
            "the real-time price is not the delayed one"
        );
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(150.0)));
        // A label of another lane (trades) says nothing about the quote stream: nothing changes, nothing is forgotten.
        sink.data_delay("ibkr", AAPL, "trades", StreamDelay::RealTime);
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(150.0)));
    }

    /// An ordinary tap is untouched by labels and never refuses: it prices as it always did (a trade prices).
    #[test]
    fn an_ordinary_tap_ignores_labels_and_never_refuses() {
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        sink.data_delay("bybit", "ETHUSDT", StreamLabel::Quotes.word(), StreamDelay::DELAYED);
        sink.quote("bybit", "ETHUSDT", quote_of(0.0, 3001.0));
        assert_eq!(
            tap.door_price("bybit", "ETHUSDT"),
            Ok(Some(1500.5)),
            "its quote rule is unchanged"
        );
        sink.trade("bybit", "ETHUSDT", print_at(3002.0));
        assert_eq!(tap.door_price("bybit", "ETHUSDT"), Ok(Some(3002.0)));
    }

    /// **A price the door can see was already handed to the core.** The inner sink (the core's lane) is asked
    /// for the tap's price at the moment it receives each verb: it must still read NONE, i.e. the tap records
    /// AFTER the forward. Reversed, the door could see a price, send its order, and have the order fold ahead
    /// of the tick (the lane twin's own symbol, whose admission cannot carry a mark, would be judged with
    /// none). Quote, trade and book each.
    #[test]
    fn the_tap_records_a_price_only_after_the_core_has_the_tick() {
        crate::testutil::note_the_binance_lane_twin();
        struct Probe {
            tap: Arc<PriceTap>,
            seen: Mutex<Vec<(&'static str, Option<f64>)>>,
        }
        impl DataClientSink for Probe {
            fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
            fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
            fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
            fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
            fn quote(&self, v: &str, s: &str, _: TickQuote) {
                self.seen.lock().unwrap().push(("quote", self.tap.price(v, s)));
            }
            fn trade(&self, v: &str, s: &str, _: TickTrade) {
                self.seen.lock().unwrap().push(("trade", self.tap.price(v, s)));
            }
            fn book(&self, v: &str, s: &str, _: Arc<L2Book>) {
                self.seen.lock().unwrap().push(("book", self.tap.price(v, s)));
            }
        }
        let tap = Arc::new(PriceTap::default());
        let probe = Arc::new(Probe { tap: Arc::clone(&tap), seen: Mutex::default() });
        let sink = DataClientSinkPriceTap::new(
            Arc::clone(&probe) as Arc<dyn DataClientSink>,
            Arc::clone(&tap),
        );
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(1, &[L2BookLevel::new(99.0, 1.0)], &[L2BookLevel::new(101.0, 1.0)]);

        sink.quote("binance", "BTCUSDT.P", quote_of(100.0, 102.0));
        assert_eq!(tap.price("binance", "BTCUSDT.P"), Some(101.0), "recorded after the forward");
        sink.trade("binance", "ETHUSDT.P", print_at(2500.0));
        assert_eq!(tap.price("binance", "ETHUSDT.P"), Some(2500.0));
        sink.book("binance", "SOLUSDT.P", Arc::new(book));
        assert_eq!(tap.price("binance", "SOLUSDT.P"), Some(100.0));

        let seen = probe.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [("quote", None), ("trade", None), ("book", None)],
            "when the core got each tick the tap held no price for its symbol yet"
        );
    }

    #[test]
    fn a_quote_prices_its_symbol_and_nothing_else() {
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        assert_eq!(tap.price("bybit", "ETHUSDT"), None);
        let quote = TickQuote {
            timestamp: 1,
            local_timestamp: 0,
            bid: 3000.0,
            ask: 3001.0,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: Default::default(),
        };
        sink.quote("bybit", "ETHUSDT", quote);
        assert_eq!(tap.price("bybit", "ETHUSDT"), Some(3000.5));
        assert_eq!(tap.price("bybit", "BTCUSDT"), None);
        assert_eq!(tap.price("binance", "ETHUSDT"), None);
    }

    /// **The door's price keeps the time it was MADE**: a price that arrived 20 s old (the venue's own stamp) is
    /// said to be made 20 s ago, not at its arrival, and a price the door does not hold now has no time at all.
    #[test]
    fn the_door_price_is_dated_by_when_it_was_made() {
        let tap = PriceTap::default();
        let before = vike_model::now_ms();
        tap.note_aged("bybit", "ETHUSDT", 3000.0, Duration::from_secs(20), None);
        let made = tap.door_price_made_ms("bybit", "ETHUSDT", 3000.0).expect("the door's price");
        let after = vike_model::now_ms();
        assert!(
            (before - 20_001..=after - 19_999).contains(&made), // 1 ms of rounding either way
            "20 s before the arrival: {made} not in {before}..{after} less 20 s"
        );
        assert_eq!(tap.door_price_made_ms("bybit", "ETHUSDT", 3001.0), None, "another price");
        assert_eq!(tap.door_price_made_ms("bybit", "BTCUSDT", 3000.0), None, "no price");
    }

    /// A price older than [`MAX_PRICE_AGE`] is no price (a dead pump must not mark an order), a fresh one
    /// replaces it, and a forgotten symbol starts from none.
    #[test]
    fn a_stale_price_is_no_price_and_a_forgotten_one_is_gone() {
        let tap = PriceTap::default();
        let now = Instant::now();
        let old = now.checked_sub(MAX_PRICE_AGE + Duration::from_secs(1)).expect("an old instant");
        tap.note_at("bybit", "ETHUSDT", 3000.0, old, None);
        assert_eq!(tap.price_at("bybit", "ETHUSDT", now), None, "older than the limit");
        let edge = now.checked_sub(MAX_PRICE_AGE).expect("an instant");
        tap.note_at("bybit", "ETHUSDT", 3001.0, edge, None);
        assert_eq!(
            tap.price_at("bybit", "ETHUSDT", now),
            Some(3001.0),
            "exactly at the limit stands"
        );
        tap.note("bybit", "ETHUSDT", 3002.0);
        assert_eq!(tap.price("bybit", "ETHUSDT"), Some(3002.0));
        tap.forget("bybit", "ETHUSDT");
        assert_eq!(tap.price("bybit", "ETHUSDT"), None);
    }

    /// A tick the venue stamped `age_ms` before our receive time (a REST first price, say).
    fn quote_aged(age_ms: i64) -> TickQuote {
        TickQuote {
            timestamp: 1_700_000_000_000 - age_ms,
            local_timestamp: 1_700_000_000_000,
            ..quote_of(1.09, 1.0902)
        }
    }

    /// **A venue-clocked tap (oanda) ages a quote from its VENUE time**: a tick already 9 s old on
    /// arrival is a door price for 51 s, not 60, so the door never admits on a price the core's price-only
    /// sweep (which ages by the venue's `ts`) has already given up on; one past the bound on arrival is
    /// no price and replaces nothing; a tick missing either stamp is aged from arrival; and an ORDINARY tap
    /// keeps ageing the very same tick from arrival (a quiet stock's NBBO `ts` is its last CHANGE).
    /// MUTATION PROOF: build the tap with `default()` and the 9 s tick is still a price at 52 s.
    #[test]
    fn a_venue_clocked_tap_ages_a_quote_from_its_venue_time() {
        let tap = Arc::new(PriceTap::venue_clocked());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        let before = Instant::now();
        sink.quote("oanda", "EURUSD", quote_aged(9_000));
        let after = Instant::now();
        let at = |t: Instant, secs| tap.price_at("oanda", "EURUSD", t + Duration::from_secs(secs));
        assert!(at(before, 50).is_some(), "9 s old on arrival: still a price at 50 s");
        assert!(at(after, 52).is_none(), "9 s old on arrival: gone by 51 s, not 60");

        // Past the bound on arrival: no price, and the younger one it meets is not replaced by it.
        sink.quote("oanda", "EURUSD", quote_aged(61_000));
        assert!(at(before, 50).is_some(), "the stale tick replaced nothing");
        sink.quote("oanda", "GBPUSD", quote_aged(61_000));
        assert_eq!(tap.price_at("oanda", "GBPUSD", Instant::now()), None, "and priced nothing");

        // No venue stamp or no receive stamp: aged from arrival, as ever.
        for (ts, local_ts) in [(0, 1_700_000_000_000), (1_699_999_991_000, 0)] {
            sink.quote(
                "oanda",
                "USDJPY",
                TickQuote { timestamp: ts, local_timestamp: local_ts, ..quote_aged(0) },
            );
            let held = tap.price_at("oanda", "USDJPY", Instant::now() + Duration::from_secs(59));
            assert!(held.is_some(), "ts {ts} local_timestamp {local_ts}: aged from arrival");
        }

        // The control: an ordinary tap, the same 9 s tick, aged from arrival.
        let plain = Arc::new(PriceTap::default());
        let plain_sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&plain));
        let before = Instant::now();
        plain_sink.quote("bybit", "EURUSD", quote_aged(9_000));
        let held = plain.price_at("bybit", "EURUSD", before + Duration::from_secs(59));
        assert!(held.is_some(), "an ordinary tap ignores the venue's stamp");
    }

    /// The tap follows the VENUE source's window only: a venue `GapStart` opens it, a venue `Live` closes
    /// it, and the strat link's own word, a `Stale` and another symbol touch nothing.
    #[test]
    fn the_tap_follows_the_venue_sources_gap_window_only() {
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        let gap = StreamStatus::GapStart { at_timestamp_ms: 1 };
        let live = StreamStatus::Live { gap_started_timestamp_ms: Some(1) };
        sink.stream_status(
            "bybit",
            "ETHUSDT",
            vike_venues::link_deadman::STRATEGY_LINK_STREAM,
            gap,
        );
        assert!(!tap.gap_open("bybit", "ETHUSDT"), "the strat link is another source");
        sink.stream_status("bybit", "ETHUSDT", "book", gap);
        assert!(tap.gap_open("bybit", "ETHUSDT"));
        assert!(!tap.gap_open("bybit", "BTCUSDT"));
        sink.stream_status(
            "bybit",
            "ETHUSDT",
            "book",
            StreamStatus::Stale { newest_data_timestamp_ms: 1, now_ms: 2 },
        );
        assert!(tap.gap_open("bybit", "ETHUSDT"), "Stale moves nothing");
        sink.stream_status(
            "bybit",
            "ETHUSDT",
            vike_venues::link_deadman::STRATEGY_LINK_STREAM,
            live,
        );
        assert!(tap.gap_open("bybit", "ETHUSDT"), "the strat link's Live closes no venue gap");
        sink.stream_status("bybit", "ETHUSDT", "book", live);
        assert!(!tap.gap_open("bybit", "ETHUSDT"));
    }

    /// Records every `stream_status` the core is handed, as `(venue, symbol, stream, status)`.
    #[derive(Default)]
    struct Statuses(Mutex<Vec<(String, String, String, StreamStatus)>>);
    impl DataClientSink for Statuses {
        fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
        fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
        fn quote(&self, _: &str, _: &str, _: TickQuote) {}
        fn trade(&self, _: &str, _: &str, _: TickTrade) {}
        fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
        fn stream_status(&self, v: &str, s: &str, stream: &str, status: StreamStatus) {
            self.0.lock().unwrap().push((v.into(), s.into(), stream.into(), status));
        }
    }

    /// **Ending a gap tells the core what the closed stream can no longer say** (A55): the core's link dead-man
    /// keeps a window per `(venue, symbol, source)` from a venue `GapStart` until a venue `Live`, so a pump
    /// closed under an open gap sends that `Live` itself, with the stream word and gap time the `GapStart`
    /// carried, once; and a symbol with no gap open (or a tap with no sink bound) sends nothing.
    #[test]
    fn ending_a_gap_says_live_for_the_stream_that_opened_it() {
        let seen = Arc::new(Statuses::default());
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(
            Arc::clone(&seen) as Arc<dyn DataClientSink>,
            Arc::clone(&tap),
        );

        sink.stream_status(
            "bybit",
            "ETHUSDT",
            "quotes",
            StreamStatus::GapStart { at_timestamp_ms: 7 },
        );
        // a repeat of the outage moves neither the word nor the time the core's window keeps
        sink.stream_status(
            "bybit",
            "ETHUSDT",
            "trades",
            StreamStatus::GapStart { at_timestamp_ms: 9 },
        );
        sink.stream_status(
            "bybit",
            "SOLUSDT",
            "quotes",
            StreamStatus::GapStart { at_timestamp_ms: 8 },
        );
        seen.0.lock().unwrap().clear();

        tap.end_gap("bybit", "ETHUSDT");
        assert!(!tap.gap_open("bybit", "ETHUSDT"));
        assert!(tap.gap_open("bybit", "SOLUSDT"), "another symbol's window is left alone");
        assert_eq!(
            *seen.0.lock().unwrap(),
            [(
                "bybit".to_string(),
                "ETHUSDT".to_string(),
                "quotes".to_string(),
                StreamStatus::Live { gap_started_timestamp_ms: Some(7) }
            )],
            "one Live, on the stream and for the gap time the first GapStart gave"
        );

        tap.end_gap("bybit", "ETHUSDT");
        tap.end_gap("bybit", "XRPUSDT");
        assert_eq!(seen.0.lock().unwrap().len(), 1, "no gap, nothing to close");

        // A tap nobody wrapped has no sink: the gap is forgotten and nothing else happens.
        let bare = PriceTap::default();
        bare.note_status(
            "bybit",
            "ETHUSDT",
            "quotes",
            StreamStatus::GapStart { at_timestamp_ms: 1 },
        );
        bare.end_gap("bybit", "ETHUSDT");
        assert!(!bare.gap_open("bybit", "ETHUSDT"));
    }

    /// One hand-over to the core's mark lane: `(symbol, bid, ask, the tap's price at that moment)`.
    type Handed = (String, f64, f64, Option<f64>);

    /// Records what the core's mark lane is handed: a top of book as its two sides, a venue's own mark passed
    /// through as the one price twice.
    struct Marks {
        tap: Arc<PriceTap>,
        seen: Mutex<Vec<Handed>>,
    }
    impl DataClientSink for Marks {
        fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
        fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn mark_tick(&self, v: &str, s: &str, px: f64, _: i64) {
            self.seen.lock().unwrap().push((s.to_string(), px, px, self.tap.price(v, s)));
        }
        fn top_of_book_tick(&self, v: &str, s: &str, bid: f64, ask: f64, _: i64) {
            self.seen.lock().unwrap().push((s.to_string(), bid, ask, self.tap.price(v, s)));
        }
        fn quote(&self, _: &str, _: &str, _: TickQuote) {}
        fn trade(&self, _: &str, _: &str, _: TickTrade) {}
        fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
    }

    fn marking() -> (Arc<PriceTap>, Arc<Marks>, DataClientSinkPriceTap) {
        let tap = Arc::new(PriceTap::default());
        let marks = Arc::new(Marks { tap: Arc::clone(&tap), seen: Mutex::default() });
        let sink = DataClientSinkPriceTap::new(
            Arc::clone(&marks) as Arc<dyn DataClientSink>,
            Arc::clone(&tap),
        );
        (tap, marks, sink)
    }

    /// **The lane twin's own symbol is priced in the core by its BID and ASK; no other symbol is.** The core
    /// gives a venue's quote, trade and book to the venue's default engine only, so the twin engine's own symbol
    /// is priced there by the throttled top of book (the lane the core mirrors to every engine of the venue). A
    /// quote and a book carry one, never as a mid (owner, 2026-11-15); a trade print carries none; and the core
    /// has it BEFORE the tap records its own price (a price the door reads is behind it).
    #[test]
    fn the_lane_twins_own_symbol_is_marked_in_the_core_and_no_other_symbol_is() {
        crate::testutil::note_the_binance_lane_twin();
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(1, &[L2BookLevel::new(99.0, 1.0)], &[L2BookLevel::new(101.0, 1.0)]);
        let book = Arc::new(book);
        for (verb, expected, price) in [
            ("quote", Some((100.0, 102.0)), 101.0),
            ("trade", None, 2500.0),
            ("book", Some((99.0, 101.0)), 100.0),
        ] {
            let (tap, marks, sink) = marking();
            let tick = |symbol: &str| match verb {
                "quote" => sink.quote("binance", symbol, quote_of(100.0, 102.0)),
                "trade" => sink.trade("binance", symbol, print_at(2500.0)),
                _ => sink.book("binance", symbol, Arc::clone(&book)),
            };
            tick("ETHUSDT.P");
            tick("BTCUSDT");
            tick("BTCUSDT.P");
            let want: Vec<_> = expected
                .map(|(bid, ask)| ("BTCUSDT.P".to_string(), bid, ask, None))
                .into_iter()
                .collect();
            assert_eq!(
                *marks.seen.lock().unwrap(),
                want,
                "{verb}: at most one top, for the twin's symbol only, ahead of the tap's own price"
            );
            assert_eq!(tap.price("binance", "BTCUSDT.P"), Some(price), "{verb}");
        }
        // Another venue's symbol of the same spelling is not a twin's: aster has no lane twin.
        let (_, marks, sink) = marking();
        sink.quote("aster", "BTCUSDT.P", quote_of(100.0, 102.0));
        assert!(marks.seen.lock().unwrap().is_empty());
    }

    /// **A labelled engine's own symbol is marked in the core exactly while its lane is held** (decision 0198):
    /// the same mark lane as the lane twin's (the core mirrors a venue's mark to every engine of the venue), for
    /// the `(venue, symbol)` the registry switched on and for nothing else: not before it, not another symbol or
    /// venue, and not after it is switched off. The door's price is the plain one (only the twin's symbol waits
    /// for its mark), and a twin's symbol stays marked by the table whatever this switch says.
    #[test]
    fn a_labelled_engines_held_symbol_is_marked_in_the_core_only_while_it_is_held() {
        crate::testutil::note_the_binance_lane_twin();
        let (tap, marks, sink) = marking();
        sink.quote("hyperliquid", "BTC", quote_of(100.0, 102.0));
        assert!(marks.seen.lock().unwrap().is_empty(), "nobody holds it: not marked");

        tap.set_labelled_mark("hyperliquid", "BTC", true);
        tap.forget("hyperliquid", "BTC");
        sink.quote("hyperliquid", "BTC", quote_of(100.0, 102.0));
        sink.quote("hyperliquid", "ETH", quote_of(100.0, 102.0));
        sink.quote("bybit", "BTC", quote_of(100.0, 102.0));
        assert_eq!(
            *marks.seen.lock().unwrap(),
            [("BTC".to_string(), 100.0, 102.0, None)],
            "one mark, for the held symbol of that venue only, ahead of the tap's own price"
        );
        assert_eq!(
            tap.door_price("hyperliquid", "BTC"),
            Ok(Some(101.0)),
            "the door reads the plain price"
        );

        tap.set_labelled_mark("hyperliquid", "BTC", false);
        tap.forget("hyperliquid", "BTC");
        sink.quote("hyperliquid", "BTC", quote_of(100.0, 102.0));
        assert_eq!(marks.seen.lock().unwrap().len(), 1, "switched off: no more marks");

        tap.set_labelled_mark("binance", "BTCUSDT.P", false);
        sink.quote("binance", "BTCUSDT.P", quote_of(100.0, 102.0));
        assert_eq!(marks.seen.lock().unwrap().len(), 2, "a twin's symbol is marked by the table");
    }

    /// **A mount on a labelled account is marked in the core, except while its venue states a mark of its own**
    /// (decision 0245). The core gives a venue's quote to the DEFAULT engine only, so a mount on a labelled
    /// account had a price board written at its bar closes and nothing between them. The switch is per
    /// `(venue, symbol)`; the quote's mid stands aside for [`NATIVE_MARK_FRESH`] after the venue's own mark and
    /// takes over again when that mark goes quiet; switched off, nothing is marked.
    #[test]
    fn a_mount_on_a_labelled_account_is_marked_until_the_venue_states_its_own_mark() {
        crate::testutil::note_the_binance_lane_twin();
        let (tap, marks, sink) = marking();
        sink.quote("oanda", "EURUSD", quote_of(100.0, 102.0));
        assert!(
            marks.seen.lock().unwrap().is_empty(),
            "no mount on a labelled account: not marked"
        );

        tap.set_mount_mark("oanda", "EURUSD", true);
        tap.forget("oanda", "EURUSD");
        sink.quote("oanda", "EURUSD", quote_of(100.0, 102.0));
        sink.quote("oanda", "GBPUSD", quote_of(100.0, 102.0));
        sink.quote("ig", "EURUSD", quote_of(100.0, 102.0));
        assert_eq!(
            *marks.seen.lock().unwrap(),
            [("EURUSD".to_string(), 100.0, 102.0, None)],
            "one mark, for the mounted symbol of that venue only, ahead of the tap's own price"
        );

        // The venue states its own mark: the top stands aside, the venue's mark passes through untouched.
        tap.set_mount_mark("bybit", "BTCUSDT.P", true);
        sink.mark_tick("bybit", "BTCUSDT.P", 99.5, 1);
        sink.quote("bybit", "BTCUSDT.P", quote_of(100.0, 102.0));
        assert_eq!(
            *marks.seen.lock().unwrap(),
            [
                ("EURUSD".to_string(), 100.0, 102.0, None),
                ("BTCUSDT.P".to_string(), 99.5, 99.5, None)
            ],
            "only the venue's own mark reached the core while it is fresh"
        );

        // …and the top takes over again once that mark has been quiet for longer than the bound.
        let stale = Instant::now().checked_sub(NATIVE_MARK_FRESH + Duration::from_secs(1)).unwrap();
        tap.native.lock().unwrap().insert(("bybit".to_string(), "BTCUSDT.P".to_string()), stale);
        sink.quote("bybit", "BTCUSDT.P", quote_of(100.0, 102.0));
        assert_eq!(marks.seen.lock().unwrap().len(), 3, "a quiet venue mark: the mid marks it");

        tap.set_mount_mark("oanda", "EURUSD", false);
        tap.forget("oanda", "EURUSD");
        sink.quote("oanda", "EURUSD", quote_of(100.0, 102.0));
        assert_eq!(marks.seen.lock().unwrap().len(), 3, "switched off: no more marks");
    }

    /// The mark is throttled: a pump that ticks hundreds of times a second hands the core a mark every
    /// [`TWIN_MARK_EVERY`], and a forgotten symbol (its pump closed) is marked again at once.
    #[test]
    fn the_twins_mark_is_throttled_and_a_closed_pump_starts_again() {
        crate::testutil::note_the_binance_lane_twin();
        let (tap, marks, sink) = marking();
        for _ in 0..50 {
            sink.quote("binance", "BTCUSDT.P", quote_of(100.0, 102.0));
        }
        assert_eq!(marks.seen.lock().unwrap().len(), 1, "fifty ticks, one mark");
        std::thread::sleep(TWIN_MARK_EVERY + Duration::from_millis(50));
        sink.quote("binance", "BTCUSDT.P", quote_of(110.0, 112.0));
        assert_eq!(marks.seen.lock().unwrap().len(), 2, "the next interval marks again");
        tap.forget("binance", "BTCUSDT.P");
        sink.quote("binance", "BTCUSDT.P", quote_of(110.0, 112.0));
        assert_eq!(marks.seen.lock().unwrap().len(), 3, "a re-subscribed symbol starts from none");
    }

    /// **Only a usable price is ever handed to the core as the twin's mark, and a bad frame spends nothing**
    /// (re-review): a one-sided quote (half a price), a zero, a negative and a NaN, in a quote, a trade print or a
    /// book, reach neither the core's mark lane, nor the tap's price, nor the throttle; the next good frame is marked
    /// at once. Another symbol's one-sided quote is recorded as it always was.
    #[test]
    fn a_bad_frame_is_never_the_twins_mark_and_spends_no_throttle() {
        crate::testutil::note_the_binance_lane_twin();
        let (tap, marks, sink) = marking();
        for (bid, ask) in [
            (0.0, 102.0),
            (100.0, 0.0),
            (-1.0, -2.0),
            (102.0, 100.0),
            (f64::NAN, 102.0),
            (100.0, f64::INFINITY),
        ] {
            sink.quote("binance", "BTCUSDT.P", quote_of(bid, ask));
        }
        for price in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            sink.trade("binance", "BTCUSDT.P", print_at(price));
        }
        assert!(marks.seen.lock().unwrap().is_empty(), "no bad frame reached the core's mark lane");
        assert_eq!(tap.price("binance", "BTCUSDT.P"), None, "…nor the tap's price");
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(None));

        sink.quote("binance", "BTCUSDT.P", quote_of(100.0, 102.0));
        assert_eq!(
            *marks.seen.lock().unwrap(),
            [("BTCUSDT.P".to_string(), 100.0, 102.0, None)],
            "the next good frame is marked at once: a bad one spent no throttle"
        );
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(Some(101.0)));

        // Any other symbol records a one-sided quote as before.
        sink.quote("binance", "ETHUSDT.P", quote_of(0.0, 2000.0));
        assert_eq!(
            tap.price("binance", "ETHUSDT.P"),
            Some(1000.0),
            "unchanged for a non-twin symbol"
        );
    }

    /// **The door's price for the twin's own symbol is "none yet" until the core was handed its mark.** A price
    /// the tap holds with no mark behind it (a tap that predates the rule, a mark the pump never delivered) is
    /// not one the twin engine can be judged at, and every other symbol keeps pricing as it always did.
    #[test]
    fn the_door_prices_the_twins_own_symbol_only_once_the_core_has_its_mark() {
        crate::testutil::note_the_binance_lane_twin();
        let (tap, _marks, sink) = marking();
        tap.note("binance", "BTCUSDT.P", 74_300.0);
        tap.note("binance", "ETHUSDT.P", 2_500.0);
        assert_eq!(tap.price("binance", "BTCUSDT.P"), Some(74_300.0), "the tap holds a price");
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(None), "but the engine has no mark");
        assert_eq!(tap.door_price("binance", "ETHUSDT.P"), Ok(Some(2_500.0)), "any other symbol");
        sink.quote("binance", "BTCUSDT.P", quote_of(74_299.0, 74_301.0));
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(Some(74_300.0)));
        tap.forget("binance", "BTCUSDT.P");
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(None));
    }

    /// **A twin whose feed died is "none yet" at the door, whatever mark the engine still holds** (decision 0208).
    /// The engine's mark floor asks whether a mark was EVER seen; the door is what notices a dead feed: its price
    /// for the twin's own symbol needs BOTH a tap price and a handed-over mark no older than [`MAX_PRICE_AGE`], so
    /// an opening order through the door is held for a fresh price (and then not sent) once either has aged out.
    #[test]
    fn the_door_reads_none_yet_for_the_twins_symbol_once_its_feed_is_older_than_the_bound() {
        crate::testutil::note_the_binance_lane_twin();
        let key = || ("binance".to_string(), "BTCUSDT.P".to_string());
        let old = Instant::now()
            .checked_sub(MAX_PRICE_AGE + Duration::from_secs(1))
            .expect("an old instant");
        let (tap, _marks, sink) = marking();
        sink.quote("binance", "BTCUSDT.P", quote_of(74_299.0, 74_301.0));
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(Some(74_300.0)), "a live feed");

        // The mark is old, the tap's price is fresh: the core's mark is not one the engine can be judged at.
        tap.marked.lock().unwrap().insert(key(), old);
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(None), "an old mark");

        // The price is old, the mark is fresh: no price, whatever was handed over a moment ago.
        tap.marked.lock().unwrap().insert(key(), Instant::now());
        tap.last.lock().unwrap().insert(key(), (74_300.0, old, None));
        assert_eq!(tap.door_price("binance", "BTCUSDT.P"), Ok(None), "an old price");
    }

    /// Files each top of book on a real [`PriceBoard`], the way the core's `drain_market` files the tap's top.
    #[derive(Default)]
    struct Board(Mutex<vike_execution::price_board::PriceBoard>);
    impl DataClientSink for Board {
        fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
        fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn mark_tick(&self, v: &str, s: &str, px: f64, ts: i64) {
            self.0.lock().unwrap().set_mark(v, s, px, ts);
        }
        fn top_of_book_tick(&self, v: &str, s: &str, bid: f64, ask: f64, ts: i64) {
            self.0.lock().unwrap().set_quote(v, s, bid, ask, ts);
        }
        fn quote(&self, _: &str, _: &str, _: TickQuote) {}
        fn trade(&self, _: &str, _: &str, _: TickTrade) {}
        fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
    }

    /// **The twin's top of book is fresh to the price-only sweep when the venue gave its quote no time.**
    /// Binance's raw `@bookTicker` frame carries no event time (`decode_book_ticker`: `ts: 0`), and the mark lane
    /// used to hand that 0 to the core as the price's time: the board filed a price stamped 1970, the core's
    /// price-only sweep (`price_only_basis`: every rung capped at [`MAX_PRICE_AGE`], aged by the stamp) read the
    /// live price of a held position as stale for good and skipped the engine (and, node-wide, the drawdown
    /// latch), while the door, which ages by arrival, called the same price fresh. A top with no venue time is
    /// stamped at its arrival; one WITH a venue time keeps it. It lands in the QUOTE slots: a long reads the bid.
    #[test]
    fn a_twin_mark_from_a_quote_the_venue_did_not_time_is_fresh_to_the_price_only_sweep() {
        crate::testutil::note_the_binance_lane_twin();
        use vike_execution::price_board::{MarkStatus, PriceBoardConfig, PriceSource};
        let board = Arc::new(Board::default());
        let sink = DataClientSinkPriceTap::new(
            Arc::clone(&board) as Arc<dyn DataClientSink>,
            Arc::new(PriceTap::default()),
        );
        sink.quote("binance", "BTCUSDT.P", TickQuote { timestamp: 0, ..quote_of(100.0, 102.0) });
        let cap = Some(i64::try_from(MAX_PRICE_AGE.as_millis()).unwrap());
        let config = PriceBoardConfig {
            use_mark: true,
            mark_max_age_ms: cap,
            quote_max_age_ms: cap,
            trade_max_age_ms: cap,
            bar_max_age_ms: cap,
            stale_fallback: false,
            last_known_by_recency: false,
        };
        let status = board.0.lock().unwrap().classify(
            "binance",
            "BTCUSDT.P",
            true,
            vike_model::now_ms(),
            &config,
        );
        assert!(
            matches!(status, MarkStatus::Fresh { price: px, source: PriceSource::Bid, .. } if px == 100.0),
            "a live quote with no venue time must be a fresh bid, got {status:?}"
        );

        // A venue time is kept, not replaced by the arrival time; no mark slot is written.
        let board = Arc::new(Board::default());
        let sink = DataClientSinkPriceTap::new(
            Arc::clone(&board) as Arc<dyn DataClientSink>,
            Arc::new(PriceTap::default()),
        );
        sink.quote(
            "binance",
            "BTCUSDT.P",
            TickQuote { timestamp: 1_700_000_000_000, ..quote_of(100.0, 102.0) },
        );
        let cell = *board.0.lock().unwrap().cell("binance", "BTCUSDT.P").expect("one top");
        assert_eq!(cell.bid, Some((100.0, 1_700_000_000_000)));
        assert_eq!(cell.ask, Some((102.0, 1_700_000_000_000)));
        assert_eq!(cell.mark, None, "the tap's top is never a mark");
    }

    /// **The tap keeps the last trade PRINT apart from its price** (a `Price` alert's fallback, `crate::alerts`): a
    /// quote moves the price and leaves the print, a labelled-quotes tap (which never prices from a print) keeps it
    /// too, a bad print is not kept, and forgetting the symbol drops it.
    #[test]
    fn the_tap_keeps_the_last_trade_print_apart_from_its_price() {
        for tap in [PriceTap::default(), PriceTap::labelled_quotes()] {
            let tap = Arc::new(tap);
            let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
            assert_eq!(tap.last_trade("bybit", "ETHUSDT"), None);
            sink.trade("bybit", "ETHUSDT", print_at(3000.0));
            sink.trade("bybit", "ETHUSDT", print_at(f64::NAN));
            sink.quote("bybit", "ETHUSDT", quote_of(3100.0, 3102.0));
            assert_eq!(
                tap.last_trade("bybit", "ETHUSDT"),
                Some(3000.0),
                "the print, not the quote"
            );
            tap.forget("bybit", "ETHUSDT");
            assert_eq!(tap.last_trade("bybit", "ETHUSDT"), None);
        }
    }

    /// **The top of book the tap hands the door is the venue's own best bid and best ask, never a mid** (the door's
    /// post-only check): a two-sided quote and a two-sided book state one, a trade print or a one-sided quote
    /// states none, a print moves the price and leaves the top, and forgetting the symbol drops both.
    #[test]
    fn the_tap_keeps_the_venues_best_bid_and_ask_and_never_turns_a_mid_into_one() {
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        let top = |s: &str| tap.door_top("bybit", s);
        assert_eq!(top("ETHUSDT"), None, "nothing seen");

        sink.trade("bybit", "ETHUSDT", print_at(3000.0));
        assert_eq!(tap.door_price("bybit", "ETHUSDT"), Ok(Some(3000.0)), "a last price is held");
        assert_eq!(top("ETHUSDT"), None, "a last price is no book");

        sink.quote("bybit", "ETHUSDT", quote_of(0.0, 3002.0));
        assert_eq!(top("ETHUSDT"), None, "a one-sided quote states no top, whatever its mid is");

        sink.quote("bybit", "ETHUSDT", quote_of(2999.5, 3000.5));
        assert_eq!(top("ETHUSDT"), Some((2999.5, 3000.5)));
        assert_eq!(tap.price("bybit", "ETHUSDT"), Some(3000.0), "the price is still the mid");

        sink.trade("bybit", "ETHUSDT", print_at(3010.0));
        assert_eq!(tap.price("bybit", "ETHUSDT"), Some(3010.0));
        assert_eq!(top("ETHUSDT"), Some((2999.5, 3000.5)), "a print moves the price, not the top");

        let mut book = L2Book::new(0.5);
        book.apply_snapshot(1, &[L2BookLevel::new(99.0, 1.0)], &[L2BookLevel::new(101.0, 1.0)]);
        sink.book("bybit", "SOLUSDT", Arc::new(book));
        assert_eq!(
            top("SOLUSDT"),
            Some((99.0, 101.0)),
            "a two-sided book states its best bid and ask"
        );

        tap.forget("bybit", "ETHUSDT");
        assert_eq!(top("ETHUSDT"), None, "a forgotten symbol holds neither");
    }

    /// **A top older than the price bound is no top**, and a stream the venue has not labelled real-time is not a
    /// book an order meets (ibkr: a delayed or frozen quote is information, never a bid and ask to cross).
    #[test]
    fn an_old_top_and_a_delayed_one_are_no_top() {
        let old = Instant::now()
            .checked_sub(MAX_PRICE_AGE + Duration::from_secs(1))
            .expect("an old instant");
        let tap = Arc::new(PriceTap::default());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        sink.quote("bybit", "ETHUSDT", quote_of(2999.5, 3000.5));
        assert_eq!(tap.door_top("bybit", "ETHUSDT"), Some((2999.5, 3000.5)));
        let key = ("bybit".to_string(), "ETHUSDT".to_string());
        tap.last.lock().unwrap().get_mut(&key).expect("one entry").2 = Some((2999.5, 3000.5, old));
        assert_eq!(tap.door_top("bybit", "ETHUSDT"), None, "an old top");

        let tap = Arc::new(PriceTap::labelled_quotes());
        let sink = DataClientSinkPriceTap::new(Arc::new(Null), Arc::clone(&tap));
        sink.quote("ibkr", AAPL, quote_of(149.98, 150.02));
        assert_eq!(tap.door_top("ibkr", AAPL), None, "a stream that stated no label");
        label(&sink, StreamDelay::DELAYED);
        sink.quote("ibkr", AAPL, quote_of(149.98, 150.02));
        assert_eq!(tap.door_top("ibkr", AAPL), None, "a delayed stream");
        label(&sink, StreamDelay::RealTime);
        sink.quote("ibkr", AAPL, quote_of(149.98, 150.02));
        assert_eq!(tap.door_top("ibkr", AAPL), Some((149.98, 150.02)), "a real-time stream");
    }
}
