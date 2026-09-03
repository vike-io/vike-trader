//! Feed-lifecycle — **the whole start/stop story of a live market-data subscription**, moved out
//! of `vike-app`'s CI-excluded `main.rs`.
//!
//! Two halves:
//!
//! 1. **The pure gates/diffs** that decide *whether* something should happen:
//!    [`should_spawn_backfill`] (the orderflow-backfill spawn gate), [`backfill_earliest_ts`] (how
//!    far back that backfill may page), [`orphaned_feed_keys`] / [`live_window_keys`] /
//!    [`orphaned_trade_feed_keys`] (which live feeds no window references anymore). These landed
//!    here first; they depend only on `HashSet`/`String` and the [`workspace`](crate::workspace)
//!    window model.
//! 2. **The imperative half** that *performs* it — [`ensure_feed_on`], [`ensure_trade_feed_on`],
//!    [`ensure_depth`], [`ensure_poly_book`], [`reap_orphaned_feeds`] and its DOM/cockpit sibling
//!    [`reap_orphaned_dom_cockpit_streams`]. These were `App` methods
//!    in `main.rs`, a file NO gate compiles (`justfile`'s `ci_crates` omits `vike-app`, and
//!    `xtask/src/ci/tables.rs` lists it in `EXCLUDE_FROM_CI`), so the diffs above were tested while
//!    the code that called them — the code that actually starts and stops sockets — was not. That
//!    asymmetry is the reason for this module: the reaper's teardown ordering (which unsubscribe
//!    routes to which venue feed, and which `spawned` slot is freed so a later `ensure_*` genuinely
//!    restarts the feed) is the exact shape whose sibling bug — a mid-backfill teardown eating the
//!    remaining pages — is bug 4 in [`core_sync`](crate::core_sync)'s module doc.
//!
//! **Signature note.** `App` is an eframe type this crate cannot name, so each moved method's
//! `self.*` fields become explicit parameters, grouped the way [`core_sync`](crate::core_sync)
//! already groups them: the venue-subscription bookkeeping in [`FeedSlots`], the render-side slots
//! a feed owns in [`SeriesSlots`], and the four "which series" strings in [`SeriesSpec`]. `vike-app`
//! keeps a one-line method per function that builds the bundles, so every call site there is
//! unchanged. Two other forced changes, both mechanical:
//!
//! - `ensure_feed_on` took a `_ctx: &egui::Context` it never used (and `ensure_feed`/
//!   `restore_workspace` only forwarded one) — dropped here; `vike-app`'s wrapper still takes it.
//! - `ensure_feed_on` read `self.shutdown.load(Relaxed)` inline; it now takes the already-loaded
//!   `shutting_down: bool`, so the atomic (and the `Arc` around it) stays in `vike-app`. The load
//!   happens at exactly the same point in the same frame.
//!
//! Bodies were otherwise unchanged by that move, statement for statement.
//!
//! **Two bodies have changed since.** [`ensure_feed_on`] and [`ensure_trade_feed_on`] gated their
//! subscribe on `spawned` alone, marking the key *before* looking its venue's client up — so
//! ensuring a venue with no registered feed suppressed the subscribe permanently, with one `warn!`
//! as the only trace. That is the "permanent, silent-ish loss" class this module was created to put
//! under a gate, and the first thing the gate caught. The miss is now tracked in
//! [`FeedSlots::unroutable`], which reopens the attempt and bounds the logging; `spawned`'s own
//! population is deliberately unchanged, because it doubles as
//! [`sync_from_core`](crate::core_sync)'s fold filter. See `ensure_feed_on`'s doc for that argument
//! and for why a *missing* venue is treated differently from a *failed* subscribe.
//!
//! **And the four bodies that burned a slot on a FAILED subscribe.** #946 fixed the *missing-venue*
//! half and explicitly left the other half open — "a venue that keeps rejecting a symbol is
//! therefore still a one-warn-then-quiet failure; that is a known remaining gap … [retrying] needs
//! backoff to be safe — real machinery, deliberately not smuggled in here". That machinery is
//! [`FeedRetries`], and with it the remaining four burns are closed: a failed `subscribe_bars`
//! ([`ensure_feed_on`]) or `subscribe_trades` ([`ensure_trade_feed_on`]) no longer holds its
//! `spawned` slot shut forever, a failed 1m-bar leg no longer vanishes under [`ensure_depth`]'s
//! `dom_depth` gate, and a failed trade leg no longer vanishes under [`ensure_poly_book`]'s
//! `poly_subs` gate. One policy for all of them, stated once on [`FeedRetries`]: **the retry rate
//! matches the cost of the attempt, and the log rate is one line per state transition.**
//!
//! **And the fifth burn, one lane further out: the aggTrades BACKFILL.** #952 made an errored walk
//! *detectable* (`BackfillStop::is_retryable()` separates a REST failure from the `max_pages` cap)
//! and stopped there, because acting on it was this crate's to do. [`BackfillRetries`] is that
//! wiring: the same ladder ([`retry_backoff`]) and the same one-line-per-transition logging,
//! reopening [`should_spawn_backfill`]'s run-once `bf_spawned` guard for a symbol whose backfill
//! errored. It differs from [`FeedRetries`] in the two ways the lane forces — the attempt is
//! ASYNCHRONOUS (a worker thread reports back, so there is an in-flight state and a report type),
//! and it is BOUNDED ([`BACKFILL_MAX_RETRIES`]), because a re-walk is up to 300 REST pages rather
//! than one `subscribe_*` call. Its safety rule is stated once on [`BackfillRetries`]: **a symbol's
//! backfill chain may deliver ticks from at most one walk**, because `OrderflowAgg::ingest` has no
//! per-trade dedup.

use crate::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::venue_routing::{venue_bar_instrument, venue_inst, venue_of_key, venue_str};
use crate::workspace::{self, DEFAULT_VENUE};
use crate::{orderflow, tickvol};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use vike_chart::{model, DisplayTz};
use vike_data::{DataClient, LiveDataError, SubscriptionId};
use vike_panels::dom;

/// SP3 Task 3: the pure "should a backfill thread spawn for `symbol` right now" gate, factored
/// out of `App::maybe_spawn_backfill` purely so it's unit-testable in isolation (see that
/// method's doc). Three rules, in order: `of_backfill_hours` not being a finite positive number —
/// `<= 0.0`, OR NaN/±inf (a bare `<= 0.0` lets NaN slip through, since every `<=` comparison
/// against NaN is false, which would have wrongly spawned a backfill thread for a corrupt
/// hand-edited `workspace.json`) — is the master off-switch (`global-constraints.md`'s "default
/// = SP2 behavior" — byte-identical, no thread ever spawned); otherwise `bf_spawned` is the
/// run-once-per-symbol gate — `HashSet::insert` both checks AND records membership in one call,
/// so a symbol is recorded in `bf_spawned` on EXACTLY the one call that returns `true` for it.
///
/// **`bf_spawned` is still insert-only, and this function still never removes from it.** The one
/// thing that can reopen an already-recorded symbol is `retries` — a [`BackfillRetries`] record,
/// which exists only for a symbol whose worker thread reported a RETRYABLE stop and whose cooldown
/// has since elapsed. `||` short-circuits, so a first ensure for a symbol never even consults it;
/// on every later frame the claim is `BackfillRetries::take_retry`, which is check-and-record in one
/// call exactly like the `HashSet::insert` above it, so the same walk can never be claimed twice.
///
/// That reopening is deliberately NARROW, and [`BackfillRetries::note_report`] is the authority on
/// why: a walk that already delivered pages is never re-walked, because `OrderflowAgg::ingest` has
/// no per-trade dedup and re-emitting a delivered band would inflate those bars' volume/delta/CVD
/// forever. So the consumer side ([`core_sync::sync_from_core`](crate::core_sync)) still STAGES a
/// batch it cannot deliver (`CoreSyncState::bf_pending`) rather than discarding it: recovery from a
/// mid-backfill window teardown comes from the staging buffer, never from this gate reopening —
/// this gate reopens only walks that delivered NOTHING, which is a disjoint case.
pub fn should_spawn_backfill(
    of_backfill_hours: f64,
    bf_spawned: &mut HashSet<String>,
    retries: &mut BackfillRetries,
    symbol: &str,
) -> bool {
    if !of_backfill_hours.is_finite() || of_backfill_hours <= 0.0 {
        return false;
    }
    bf_spawned.insert(symbol.to_string()) || retries.take_retry(symbol)
}

/// How far back an orderflow aggTrades backfill may page, in epoch-ms — the bound computation
/// lifted verbatim out of `App::maybe_spawn_backfill` (whose remaining body names
/// `vike_binance::agg_trades_backfill_reported` behind the `fat` feature, so the method itself
/// stays in `vike-app`; this arithmetic does not, and it was the only part with an interesting
/// edge case).
///
/// The bound is the more-restrictive (**newer**) of (a) `oldest_bar_ot`, this chart's oldest
/// ALREADY-LOADED bar, and (b) `of_backfill_hours` back from `now_ms` — `.max()`, not `.min()`, so
/// whichever bound is closer to "now" wins: (a) stops backfill from paging further back than there
/// are loaded bars to attach footprints to, while (b) is the hard cap that keeps a long-loaded
/// chart's backfill from paging all the way back to the venue's listing date just because more
/// history happens to be loaded. `oldest_bar_ot` is `None` before any bar has synced yet for this
/// chart key — falls back to the hours-only floor.
///
/// `saturating_sub` (not `-`): `of_backfill_hours` is config-controlled (a hand-edited
/// `workspace.json` isn't range-checked on load) — an extreme value would still saturate the
/// float→int cast (Rust's `as i64` is a saturating cast) but could then overflow a plain `-` (a
/// debug-build panic, a release-build wraparound). `saturating_sub` degrades to "as far back as
/// representable" instead, never crashes.
pub fn backfill_earliest_ts(
    now_ms: i64,
    of_backfill_hours: f64,
    oldest_bar_ot: Option<i64>,
) -> i64 {
    let hours_floor_ts = now_ms.saturating_sub((of_backfill_hours * 3_600_000.0) as i64);
    oldest_bar_ot.map_or(hours_floor_ts, |ot| ot.max(hours_floor_ts))
}

/// C2 tidy (FIX 3): the pure "which `spawned` feeds does no window reference anymore" diff,
/// factored out of `App::reap_orphaned_feeds` for the same reason [`should_spawn_backfill`] is
/// standalone — unit-testable without a full `App`/`eframe::CreationContext` (see that
/// function's doc; `dom_position_tests`/`backfill_gate_tests` are the established pattern).
///
/// The live set is every CHART window's primary key (`WinState::key()`) UNION every chart
/// window's Compare symbols (`"{sym}@{w.interval}"` for each `w.compare` entry) UNION every
/// chart window's foreign-source study symbols (`series_key(src.venue, src.symbol, w.interval)`
/// for each indicator whose `source_symbol` is set — TradingView's "symbol" input) UNION every
/// DOM window's own `"{w.symbol}@1m"` — over ALL windows regardless of `open`/`minimized`,
/// matching how a closed window's OWN primary feed already survives being hidden off-desktop
/// (`WinState::open`'s doc: "false => hidden off-desktop (rail can unhide)" — nothing ever
/// tears down ITS feed just because the window is closed, so an orphan-reaper for compare
/// feeds shouldn't treat them any differently).
///
/// DOM review finding: a DOM window ALSO drives an `ensure_feed`/`spawned` entry for its own
/// symbol — a fixed `"1m"` bar feed so its mark + PAPER-engine fills flow (2 call sites in
/// `update`/menu handling that open a DOM window; `WinState::interval` is unused/empty for
/// DOM, unlike Chart, so `"1m"` is hardcoded here to match, NOT read off `w.interval`). Unlike
/// a Chart window's compare list, that ensure is a ONE-SHOT at window-creation time, never
/// re-asserted per frame — so it MUST be counted here, or this reaper would tear down a DOM's
/// own feed the very next frame after it opens (whenever no Chart window happens to already
/// cover the same symbol), and nothing would ever notice or re-request it: `ensure_depth`'s own
/// idempotency gate is `dom_depth`, keyed by `(venue, symbol)`, not `spawned` — once it's
/// satisfied once, it never checks whether the SEPARATE bar subscription this reaper could tear
/// down is still alive. Every OTHER non-chart window kind (Trade/Options/News/Calendar/Data)
/// genuinely never drives `ensure_feed` for its own symbol at all, so they're excluded.
///
/// `spawned` also mixes in a second key shape entirely (see `ensure_feed`/`ensure_trade_feed`):
/// the shared raw-trade-tape key (`"SYM@trades"`, per-SYMBOL, feeding both Tick/Volume charts'
/// `aggs` AND SP2 orderflow's `of_aggs` — never per-window, so it can't be diffed against a
/// per-window live set this way). `"@trades"` keys are explicitly excluded here so a symbol's
/// shared trade tape is never mistaken for an orphaned chart feed; a Tick/Volume-interval
/// compare's `aggs` entry is a pre-existing, documented gap this doesn't touch either (see
/// `reap_orphaned_feeds`'s doc).
pub fn orphaned_feed_keys(wins: &[workspace::WinState], spawned: &HashSet<String>) -> Vec<String> {
    let live = live_window_keys(wins);
    spawned
        .iter()
        .filter(|k| !k.ends_with("@trades") && !live.contains(k.as_str()))
        .cloned()
        .collect()
}

/// The pure live-window-key set that [`orphaned_feed_keys`] diffs `spawned` against, split out so
/// the same "which chart keys does a window still back" notion can also drive reaping orphaned
/// tick/volume (`aggs`) and orderflow (`of_aggs`) AGGREGATOR entries in `reap_orphaned_feeds` — an
/// `aggs`/`of_aggs` key IS a window's own [`WinState::key`], so a map entry whose key isn't in this
/// set has no window backing it and must be dropped (the coupled half of the trade-feed leak; see
/// [`orphaned_trade_feed_keys`]). Membership rules are exactly as documented on [`orphaned_feed_keys`]:
/// every Chart window's primary key UNION its Compare symbols UNION its foreign-source study symbols,
/// plus every DOM window's own `"{symbol}@1m"`, over ALL windows regardless of `open`/`minimized`.
pub fn live_window_keys(wins: &[workspace::WinState]) -> HashSet<String> {
    let mut live: HashSet<String> = HashSet::new();
    for w in wins {
        match w.kind {
            workspace::WinKind::Chart => {
                live.insert(w.key());
                for sym in &w.compare {
                    live.insert(format!("{sym}@{}", w.interval));
                }
                // Foreign-source studies (TradingView "symbol" input): an indicator
                // computing off ANOTHER symbol drives a live feed for it (subscribed
                // per-frame in the window loop, same interval as this chart), so it
                // must count as a live consumer — else this reaper would tear its feed
                // down the next frame. Keyed via `series_key` (venue-aware), matching
                // the `ensure_feed_on(src.venue, src.symbol, w.interval)` the loop
                // issues. Clearing the source or removing the study drops the ref, so
                // the key falls out of the live set and the feed is reaped normally.
                for a in &w.indicators {
                    if let Some(src) = &a.source_symbol {
                        live.insert(workspace::series_key(&src.venue, &src.symbol, &w.interval));
                    }
                }
            }
            workspace::WinKind::Dom => {
                live.insert(format!("{}@1m", w.symbol));
            }
            _ => {}
        }
    }
    live
}

/// The trade-feed half of the feed-leak fix: which spawned `"…@trades"` raw-trade-tape feeds does
/// NO remaining tick/volume (`aggs`) or orderflow (`of_aggs`) aggregator still need. A trade feed is
/// **shared** per `(venue, symbol)` across every aggregator on that pair (a Tick chart, a Volume
/// chart, and an orderflow overlay on the same symbol all ride one `subscribe_trades` stream), so it
/// may only be stopped once NO aggregator references it — hence `needed` is the union of the
/// `(venue, symbol)` of every REMAINING aggregator, computed by the caller AFTER it has already
/// dropped dead aggregator entries. Any spawned `@trades` key whose parsed `(venue, symbol)` is not
/// in `needed` is returned for teardown; every non-`@trades` (kline) key is ignored (that's
/// [`orphaned_feed_keys`]'s job).
///
/// The `(venue, symbol)` is parsed with the SAME convention `ensure_trade_feed_on` writes the key
/// with: `"{symbol}@trades"` ⇒ `(default_venue, symbol)`; `"{venue}:{symbol}@trades"` ⇒
/// `(venue, symbol)`. `default_venue` is passed in (rather than read from `workspace::DEFAULT_VENUE`)
/// purely to keep this helper pure/unit-testable in isolation; `main.rs` passes the real constant.
/// Venue/symbol never contain a `:` (venues are lower-case words, symbols are concatenated or
/// dashed — see `venue_of_key`), so the first-`:` split is unambiguous.
pub fn orphaned_trade_feed_keys(
    needed: &HashSet<(String, String)>,
    spawned: &HashSet<String>,
    default_venue: &str,
) -> Vec<String> {
    spawned
        .iter()
        .filter_map(|k| {
            let pair = trade_key_pair(k, default_venue)?;
            if needed.contains(&pair) {
                None
            } else {
                Some(k.clone())
            }
        })
        .collect()
}

/// Parse a raw-trade-tape key back into the `(venue, symbol)` [`ensure_trade_feed_on`] built it
/// from, or `None` for any key that is not a trade key (a kline key). The one place that
/// convention is decoded, shared by [`orphaned_trade_feed_keys`] and [`reap_orphaned_feeds`]'s
/// `unroutable` sweep so the two can never disagree about which feed a key names: `"{symbol}@trades"`
/// ⇒ `(default_venue, symbol)`; `"{venue}:{symbol}@trades"` ⇒ `(venue, symbol)`, split on the FIRST
/// `:` only so a dashed symbol (OKX `BTC-USDT`) round-trips.
fn trade_key_pair(key: &str, default_venue: &str) -> Option<(String, String)> {
    let inner = key.strip_suffix("@trades")?;
    Some(match inner.split_once(':') {
        Some((v, s)) => (v.to_string(), s.to_string()),
        None => (default_venue.to_string(), inner.to_string()),
    })
}

// ---------------------------------------------------------------------------------------------
// The imperative half: the five `App` methods that actually start and stop feeds.
// ---------------------------------------------------------------------------------------------

/// The GUI's venue-keyed live [`DataClient`] map (`"binance"`, `"bybit"`, `"okx"`,
/// `"polymarket"`, …). `+ Send` because `vike-app` moves the whole set onto a throwaway
/// shutdown thread and fans each venue's teardown out in parallel (see its `on_exit`).
pub type FeedMap = HashMap<&'static str, Box<dyn DataClient + Send>>;

// ---------------------------------------------------------------------------------------------
// The one retry policy every `subscribe_*` in this module shares.
// ---------------------------------------------------------------------------------------------

/// The first cooldown after a failed subscribe; doubles per consecutive failure up to
/// [`RETRY_MAX`]. One second is chosen so a blip recovers within a delay a human reads as "it came
/// back", not as "it never worked".
pub const RETRY_BASE: Duration = Duration::from_secs(1);

/// The cooldown ceiling. Deliberately 60s — the same steady-state re-check cadence
/// `VIKE_RECONCILE_INTERVAL_MS` defaults to, so this workspace has ONE answer to "how often does a
/// background thing re-ask a venue that is currently saying no".
pub const RETRY_MAX: Duration = Duration::from_secs(60);

/// The pure backoff schedule: `attempts` (`1` = the failure that just happened) ⇒ how long to wait
/// before re-attempting. `1s, 2s, 4s, 8s, 16s, 32s, 60s, 60s, …` — exponential until it reaches
/// the [`RETRY_MAX`] ceiling, then flat **forever**: there is no attempt limit, because a venue
/// that is down for an hour must still be able to come back on its own without the operator
/// restarting the GUI. Saturating throughout, so a pathological attempt count can neither overflow
/// the shift nor the multiply.
pub fn retry_backoff(attempts: u32) -> Duration {
    let steps = attempts.saturating_sub(1).min(20);
    RETRY_MAX.min(RETRY_BASE.saturating_mul(1u32 << steps))
}

/// Which gate a [`FeedRetries`] record shadows.
///
/// The retry logic is identical across lanes; the lane exists so [`reap_orphaned_feeds`] can sweep
/// the SERIES records — the only ones whose "is this series still wanted?" question it can answer —
/// without touching the DOM/cockpit records, whose keyspaces (`dom_depth`'s `(venue, symbol)`
/// pairs, `poly_subs`' token ids) it knows nothing about. A lane-tagged key rather than a string
/// prefix precisely because a series key already contains a `"venue:"` prefix of its own, and a
/// sweep that guessed wrong would silently drop a live record.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RetryLane {
    /// [`ensure_feed_on`]'s kline subscribe and [`ensure_trade_feed_on`]'s trade tape, keyed by the
    /// `spawned` key — so the reaper sweeps it under exactly the rules it sweeps `unroutable` with.
    Series,
    /// [`ensure_depth`]'s `subscribe_depth` leg, keyed `"{venue}:{canonical}"` (the `dom_depth`
    /// key). Its gate set already records the leg only on success, so this lane does not *create*
    /// the retry — it THROTTLES one that existed but ran on every frame, i.e. ~60 Hz of re-dialling
    /// (and warning) against a live venue socket.
    DomDepth,
    /// [`ensure_depth`]'s SECOND, non-Binance `subscribe_bars(inst, "1m")` leg, keyed
    /// `"{venue}:{inst}"`. It gets its own lane because it has no gate set at all: `dom_depth`
    /// records the DEPTH leg, and this leg's failure used to vanish underneath it — that venue's
    /// paper engine then never received a bar, permanently and with only one `warn!`.
    DomBars,
    /// [`ensure_poly_book`]'s `subscribe_book` leg, keyed by token id (the `poly_subs` key). Same
    /// throttle-only role as [`RetryLane::DomDepth`].
    PolyBook,
    /// [`ensure_poly_book`]'s SECOND `subscribe_trades` leg, keyed by token id. Same shape as
    /// [`RetryLane::DomBars`]: `poly_subs` deliberately records the token once the BOOK is live
    /// (the ladder paints), so a failed trade leg had nothing left that could reopen it and that
    /// token's trade tape was permanently absent.
    PolyTrades,
}

/// One failing subscribe leg: its [`RetryLane`] plus the lane-local id. Built through the five
/// named constructors so no call site has to remember which string a lane is keyed by.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RetryKey {
    lane: RetryLane,
    id: String,
}

impl RetryKey {
    /// A kline or `@trades` series key — the same string `spawned`/`unroutable` hold.
    pub fn series(key: &str) -> RetryKey {
        RetryKey { lane: RetryLane::Series, id: key.to_string() }
    }
    /// A DOM depth leg, keyed exactly like its `dom_depth` entry (venue + CANONICAL symbol).
    pub fn dom_depth(venue: &str, canonical: &str) -> RetryKey {
        RetryKey { lane: RetryLane::DomDepth, id: format!("{venue}:{canonical}") }
    }
    /// A DOM 1m-bar leg, keyed by the venue-NATIVE instrument the subscribe actually names.
    pub fn dom_bars(venue: &str, inst: &str) -> RetryKey {
        RetryKey { lane: RetryLane::DomBars, id: format!("{venue}:{inst}") }
    }
    /// A cockpit book leg, keyed by the Polymarket token id.
    pub fn poly_book(token: &str) -> RetryKey {
        RetryKey { lane: RetryLane::PolyBook, id: token.to_string() }
    }
    /// A cockpit trade-tape leg, keyed by the Polymarket token id.
    pub fn poly_trades(token: &str) -> RetryKey {
        RetryKey { lane: RetryLane::PolyTrades, id: token.to_string() }
    }
}

/// What is outstanding for one leg. Three states, because a subscribe can fail in three
/// meaningfully different ways and collapsing them is what produced either a permanent silence or
/// a ~60 Hz retry loop.
#[derive(Clone, Debug)]
enum RetryState {
    /// The venue has **no registered client** at all (the DOM/cockpit lanes only — the series lane
    /// records this in [`FeedSlots::unroutable`], which the reaper owns). Re-checking costs a
    /// hashmap lookup, so this state is ALWAYS due; the record exists only to warn once.
    Missing,
    /// The venue answered [`LiveDataError::Subscribe`]. Retried when `next_at` passes.
    Backoff { attempts: u32, next_at: Instant },
    /// The venue answered [`LiveDataError::Unsupported`] — a DECLARED-capability refusal, derived
    /// from the static `VenueCaps.live_data` matrix by `vike_data::require_live_verb`. It cannot
    /// become true by waiting, so it is never retried; the record is kept so the loss stays
    /// visible rather than being forgotten.
    Refused,
}

impl RetryState {
    fn due_now(&self) -> bool {
        match self {
            RetryState::Missing => true,
            RetryState::Backoff { next_at, .. } => Instant::now() >= *next_at,
            RetryState::Refused => false,
        }
    }
}

/// **The record of every subscribe leg that is currently failing** — one map, one policy, shared by
/// all four `ensure_*` functions.
///
/// # Why this exists
///
/// Four sites claimed their idempotency slot on a path where nothing was actually subscribed:
/// [`ensure_feed_on`] and [`ensure_trade_feed_on`] held `spawned` after a failed `subscribe_*`,
/// [`ensure_depth`] inserted `dom_depth` even when its inner 1m `subscribe_bars` failed, and
/// [`ensure_poly_book`] inserted `poly_subs` even when its `subscribe_trades` failed. Each was a
/// permanent loss for the life of the process — a dead chart, a bar-less paper engine, an absent
/// trade tape — behind at most one `warn!`. #946 fixed the *missing-venue* half of the first two
/// and named this the remaining gap; this closes it, for all four, the same way.
///
/// # The two questions this had to answer
///
/// **Retry cadence.** #946's rule was "retry freely, warn once", and it was right *there* because
/// its retry costs a `HashMap::get` — no venue is touched. A failed subscribe is the opposite: the
/// venue was reached and answered, and every caller here is per-frame, so an unconditional retry is
/// a ~60 Hz socket loop plus the ~60 Hz log loop behind this workspace's 341 GB trace-file
/// incident. So the shared rule generalises both: **the retry rate matches the cost of the
/// attempt** — free re-checks stay per-frame (a missing-venue record here, and
/// [`FeedSlots::unroutable`] in the series lane), venue calls back off ([`retry_backoff`]: 1s →
/// 60s, then flat forever). And **the log rate is one line per state transition**, never per
/// frame: the first failure `warn!`s with the schedule, later failures are `debug!`, and the
/// recovery is one `info!`.
///
/// **Is a transient error distinguishable from a permanent one?** Partly, and the part that is
/// distinguishable is worth acting on. [`LiveDataError`] has exactly two variants:
/// [`LiveDataError::Unsupported`] is a DECLARED-capability refusal — `vike_data::require_live_verb`
/// derives it from the static `vike_model::caps_for(venue).live_data` matrix — so it is *provably*
/// permanent and retrying it can only ever be noise (recorded, warned once, never re-attempted).
/// [`LiveDataError::Subscribe`] genuinely mixes the transient (the OS
/// refused a feed thread; the socket is down) with the permanent (the venue rejected the symbol up
/// front) and its payload is an opaque `String` this layer must not pattern-match. So it is treated
/// as retryable — but bounded, which is exactly the "treat all as retryable, bound the noise"
/// answer: an eventually-hopeless symbol costs one warn plus one `debug!` per minute, forever,
/// while a genuine blip recovers in a second.
///
/// # Lifetime
///
/// A record is created on failure and removed on success, so the healthy path holds NOTHING
/// (`is_empty()`). The SERIES lane is additionally swept by [`reap_orphaned_feeds`] (step 2e)
/// under the same rules as [`FeedSlots::unroutable`], so it can never become the insert-only set
/// #946 exists to have removed. The DOM/cockpit lanes are swept by
/// [`reap_orphaned_dom_cockpit_streams`] under ITS live-window rules — they used to be unsweepable
/// because `dom_depth`/`poly_subs` had no teardown path at all, which is the gap that function
/// closed — so no lane can grow insert-only for the life of the process anymore.
#[derive(Default)]
pub struct FeedRetries {
    inner: HashMap<RetryKey, RetryState>,
}

impl FeedRetries {
    /// Is a record outstanding for this leg — did it fail and not since succeed? The observable
    /// question: `true` means that series/leg is currently delivering nothing.
    pub fn is_pending(&self, key: &RetryKey) -> bool {
        self.inner.contains_key(key)
    }

    /// A record exists **and** its cooldown has elapsed. Used at the sites whose own gate set
    /// (`spawned`, `dom_depth`, `poly_subs`) already says "done", so this is the only thing that
    /// can reopen the attempt.
    pub fn is_retry_due(&self, key: &RetryKey) -> bool {
        self.inner.get(key).is_some_and(RetryState::due_now)
    }

    /// A record exists and its cooldown has **not** elapsed — the exact complement of
    /// [`Self::is_retry_due`] over "a record exists". Used at the sites whose gate set already says
    /// "not done" (so the caller would retry anyway) and this only throttles it.
    pub fn is_blocked(&self, key: &RetryKey) -> bool {
        self.inner.get(key).is_some_and(|st| !st.due_now())
    }

    /// Record that this leg's venue has **no registered client**. Returns `true` the first time,
    /// which is the caller's warn-once gate. Always immediately due afterwards: re-checking a
    /// hashmap costs nothing, so the backoff rule deliberately does not apply (this is the
    /// DOM/cockpit twin of what [`FeedSlots::unroutable`] does in the series lane).
    pub fn note_missing(&mut self, key: &RetryKey) -> bool {
        if matches!(self.inner.get(key), Some(RetryState::Missing)) {
            return false;
        }
        self.inner.insert(key.clone(), RetryState::Missing);
        true
    }

    /// Record a subscribe the venue **answered no** to, log it at a bounded rate, and either
    /// schedule the retry or refuse it forever. `what` is the site label the log line opens with
    /// (e.g. `"ensure_feed(BTCUSDT@1m): subscribe_bars"`), built by the caller so the line names
    /// the real key rather than a generic one.
    pub fn note_error(&mut self, key: &RetryKey, err: &LiveDataError, what: &str) {
        if matches!(err, LiveDataError::Unsupported(_)) {
            // Provably permanent (see this type's doc): re-asking a declared-capability refusal can
            // only ever be noise. Recorded rather than forgotten, so the gap stays visible.
            if !matches!(self.inner.get(key), Some(RetryState::Refused)) {
                self.inner.insert(key.clone(), RetryState::Refused);
                tracing::warn!(
                    "{what} refused: {err} — the venue's DECLARED capabilities cannot serve this, \
                     so it is not retried; this series stays empty"
                );
            }
            return;
        }
        let attempts = match self.inner.get(key) {
            Some(RetryState::Backoff { attempts, .. }) => attempts.saturating_add(1),
            _ => 1,
        };
        let delay = retry_backoff(attempts);
        self.inner
            .insert(key.clone(), RetryState::Backoff { attempts, next_at: Instant::now() + delay });
        // One line per state transition: the FIRST failure is the operator-visible event; the
        // repeats are `debug!` so a permanently-broken symbol cannot flood the trace file.
        if attempts == 1 {
            tracing::warn!(
                "{what} failed: {err} — retrying in {}s (doubling, capped at {}s)",
                delay.as_secs(),
                RETRY_MAX.as_secs()
            );
        } else {
            tracing::debug!(
                "{what} failed again (attempt {attempts}): {err} — next retry in {}s",
                delay.as_secs()
            );
        }
    }

    /// Forget this leg's record because it just SUCCEEDED, returning the failed-attempt count it
    /// was holding (`0` for a missing-venue or refused record) so the caller can report the
    /// recovery. `None` = there was nothing outstanding, i.e. the ordinary first-time success.
    pub fn clear(&mut self, key: &RetryKey) -> Option<u32> {
        self.inner.remove(key).map(|st| match st {
            RetryState::Backoff { attempts, .. } => attempts,
            RetryState::Missing | RetryState::Refused => 0,
        })
    }

    /// [`reap_orphaned_feeds`]'s sweep (step 2e): drop every **series-lane** record whose key
    /// nothing wants anymore, leaving the DOM/cockpit lanes alone (see [`RetryLane`]). Without it
    /// this map would grow insert-only for the life of the process — the very shape #946 removed
    /// one layer down, re-introduced one layer up.
    pub fn retain_series(&mut self, keep: impl Fn(&str) -> bool) {
        self.inner.retain(|k, _| k.lane != RetryLane::Series || keep(&k.id));
    }

    /// [`reap_orphaned_dom_cockpit_streams`]'s sweep — the DOM/cockpit twin of
    /// [`Self::retain_series`], leaving the SERIES lane alone for the mirror reason: each reaper
    /// can answer "is this key still wanted?" only for the keyspaces it knows. `keep` receives the
    /// record's [`RetryLane`] plus its lane-local id (the [`RetryKey`] constructors' `id` strings),
    /// and is never consulted for [`RetryLane::Series`].
    pub fn retain_dom_cockpit(&mut self, keep: impl Fn(RetryLane, &str) -> bool) {
        self.inner.retain(|k, _| k.lane == RetryLane::Series || keep(k.lane, &k.id));
    }

    /// How many legs are currently failing. `0` on the healthy path — a non-zero count is the
    /// number of charts/ladders/tapes delivering nothing right now.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when nothing is failing.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Forget EVERY lane's records — the feed-plane teardown
    /// (`crate::backend_conn::teardown_feed_plane`; a backend SWITCH no longer resets this lane —
    /// the streams it schedules are venue truth and keep running, split-plane B2). A stale record
    /// surviving a teardown would carry the OLD plane's attempt count (and so an inflated
    /// backoff, or a permanent `Refused`) into a re-mounted plane's first subscribe — the per-key
    /// sibling of what [`Self::clear`] fixes on success.
    pub fn reset(&mut self) {
        self.inner.clear();
    }

    /// TEST ONLY: pretend every scheduled cooldown has already elapsed, so a test can drive the
    /// retry path without sleeping for real seconds. The schedule itself is covered separately by
    /// [`retry_backoff`]'s own unit test, and the "a retry does NOT happen before the cooldown"
    /// property is testable without this (a test runs in microseconds; the floor is one second).
    #[cfg(test)]
    fn expire_all(&mut self) {
        let now = Instant::now();
        for st in self.inner.values_mut() {
            if let RetryState::Backoff { next_at, .. } = st {
                *next_at = now;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The same policy, one lane further out: the aggTrades BACKFILL retry.
// ---------------------------------------------------------------------------------------------

/// How many times a symbol's aggTrades backfill may be re-walked before this session gives up on
/// it. **Six**, and the number is not arbitrary: it is exactly [`retry_backoff`]'s strictly
/// increasing run (`1s, 2s, 4s, 8s, 16s, 32s`). The 7th attempt would be the first one the ceiling
/// flattens — and flat-forever is precisely what [`FeedRetries`] wants and what this lane must not
/// become. So the two lanes share one ladder and diverge exactly where the ladder stops carrying
/// information.
///
/// [`FeedRetries`] deliberately has NO attempt limit, and is right not to: its attempt is a single
/// `subscribe_*` call, and its value does not decay — a subscribe that finally succeeds at minute 61
/// delivers the whole live tape from then on. **Both halves invert here.** The attempt is a walk of
/// up to 300 REST pages, each of which already carries #952's own 6-step 429/418 ladder inside it;
/// and its value decays, because the window is `of_backfill_hours` back from *now* while the live
/// tape keeps covering more of it — a backfill still failing after a minute is chasing history that
/// is progressively less missing. Nor is giving up a regression: on `main` EVERY failure is terminal
/// for the process, so a bounded ladder is strictly more recovery than exists today, while an
/// unbounded one would be strictly more venue load than exists today, forever, for a chart the
/// operator may have closed.
pub const BACKFILL_MAX_RETRIES: u32 = 6;

/// What one finished aggTrades-backfill worker thread hands back to the UI thread — the whole
/// vocabulary [`BackfillRetries`] folds.
///
/// Deliberately NOT `vike_binance::BackfillOutcome`: `vike-app-core` links no venue bridge (see
/// `App::maybe_spawn_backfill`'s doc), so the worker flattens the venue's `BackfillStop` into these
/// three fields at the crate boundary. `is_retryable()` is the only bit of it this layer needs, plus
/// the message for the log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackfillReport {
    /// The symbol whose walk this was — the `bf_spawned`/[`BackfillRetries`] key.
    pub symbol: String,
    /// Pages the walk kept, i.e. `BackfillOutcome::pages`. On this seam it is exactly the
    /// "did anything get DELIVERED?" question: the pager emits every kept page and only kept pages,
    /// so `pages == 0` ⟺ the aggregator received nothing from this walk. That equivalence is what
    /// makes a retry safe — see [`BackfillRetries::note_report`].
    pub pages: u32,
    /// `Some(reason)` = the walk stopped on a RETRYABLE error (`BackfillStop::is_retryable()`),
    /// carrying the venue's message verbatim. `None` = it ended for a reason re-running cannot
    /// improve: `Complete` (nothing is missing) or `Capped` (a budget decision the venue served in
    /// full — an identical re-run caps at the same page, which is why #952 made `Capped` explicitly
    /// NOT retryable).
    pub error: Option<String>,
}

impl BackfillReport {
    /// A walk that ended for a non-retryable reason — `Complete` or `Capped`. Clears any
    /// outstanding record for the symbol.
    pub fn finished(symbol: &str, pages: u32) -> BackfillReport {
        BackfillReport { symbol: symbol.to_string(), pages, error: None }
    }
    /// A walk that stopped on a retryable error, carrying the venue's message verbatim.
    pub fn failed(symbol: &str, pages: u32, reason: &str) -> BackfillReport {
        BackfillReport { symbol: symbol.to_string(), pages, error: Some(reason.to_string()) }
    }
}

/// One symbol's outstanding backfill state. Three states rather than [`FeedRetries`]' three
/// *different* ones, because this attempt is ASYNCHRONOUS: the walk that fails is a worker thread,
/// so "an attempt is running right now" is a state a synchronous `subscribe_*` never needed.
#[derive(Clone, Debug)]
enum BackfillRetry {
    /// The last walk failed having delivered nothing; `attempts` failures so far. Re-walked by the
    /// first [`should_spawn_backfill`] after `next_at` — which is to say by the first frame after
    /// `next_at` on which some chart still WANTS this symbol's orderflow.
    Waiting { attempts: u32, next_at: Instant },
    /// A re-walk has been claimed by [`should_spawn_backfill`] and its worker has not reported yet.
    /// Nothing may claim a second one: without this state the per-frame call site would spawn a new
    /// 300-page walk on every frame of the minutes a walk can take.
    InFlight { attempts: u32 },
    /// Never re-walked again this session, for one of the two reasons in
    /// [`BackfillRetries::note_report`] (the attempt bound, or a partial delivery that cannot be
    /// resumed). Kept rather than removed so the loss stays visible to `len()`/`is_pending`, exactly
    /// as [`FeedRetries`] keeps a `Refused`.
    Abandoned,
}

/// **The record of every symbol whose aggTrades backfill is currently failing** — the backfill twin
/// of [`FeedRetries`], sharing its ladder and its logging discipline.
///
/// # Why this exists
///
/// #937 fixed the CONSUMER half of the backfill lane (a page the GUI could not place is staged, not
/// dropped) and deliberately left `bf_spawned` closed, because there the guarded work — a 300-page
/// REST walk — really had run. #952 then found the PRODUCER half: `unwrap_or_default()` turned any
/// REST error into an empty page, an empty page ended the walk, and the walk reported success, so a
/// 429 on page 2 of 300 was indistinguishable from "this symbol has no older history". It fixed the
/// detection (`BackfillStop::Failed` vs `Capped`, `is_retryable()`) and stopped there, noting that
/// nothing acted on it: the run-once guard still meant an error-truncated symbol never refetched.
/// This is the acting-on-it. The shared rule both halves obey is #946's — *the gate must reflect
/// what actually happened* — and it now points the third way in this lane: nothing ran when the
/// walk delivered nothing, so that gate reopens; the walk really did run when it delivered pages, so
/// that one stays shut.
///
/// # The safety rule: at most one walk in a chain may deliver
///
/// `OrderflowAgg::ingest` has **no per-trade dedup** — it accumulates into per-bar cells and relies
/// entirely on the caller for disjointness (its own
/// `backfill_then_live_ingest_equals_one_ingest_of_the_union` test says so in as many words). A
/// re-walk starts from the same `before_id`, so every page the failed walk already delivered would
/// be delivered a SECOND time, permanently inflating those bars' volume, delta and CVD. A
/// double-counted footprint is worse than a missing one: a missing one is visible (the CVD simply
/// starts later), a double-counted one is not.
///
/// A non-overlapping resume would need the aggTrade **id** of the oldest delivered trade as the next
/// walk's exclusive upper bound — and that id is not reachable here. The pager knows it, but
/// `agg_trades_backfill_reported` emits `vike_model::TradeTick`, which carries `ts`/`price`/`size`
/// and no id, and `BackfillOutcome` reports `pages`/`stop` and no floor. Timestamps cannot stand in:
/// several aggTrades share a millisecond, so a ts-keyed resume either drops or duplicates the ties.
/// So the rule is enforced by the only means available at this seam — **a walk that delivered
/// anything ends the chain** — and lifting it is a one-field change (`BackfillOutcome` gaining the
/// walk's floor id) in `crates/bridges/binance`, which this round does not own.
///
/// Two useful consequences fall out. The chain never delivers more than one walk's worth of ticks,
/// so `core_sync::BF_PENDING_MAX_TICKS`' and `orderflow::PENDING_MAX_TICKS`' sizing arguments —
/// both of which reason from "≤ 300 pages × 1000 ticks, once per symbol" — hold unchanged. And the
/// batch lane and the report lane cannot race: a retried walk is by construction one that put
/// nothing on `bf_rx`.
///
/// # Cadence, bound, and logs
///
/// The ladder is [`retry_backoff`], shared verbatim with [`FeedRetries`] — one answer in this
/// workspace to "how often does a background thing re-ask a venue that is saying no". The bound is
/// [`BACKFILL_MAX_RETRIES`] (which that constant's doc argues for, since [`FeedRetries`] deliberately
/// has none). Logging is one line per state transition, never per frame: the first failure `warn!`s
/// with its schedule, repeats are `debug!`, giving up is one `warn!`, and a recovery is one `info!`.
///
/// The retry is **demand-driven, not a background poller**: it fires from
/// [`should_spawn_backfill`], which `vike-app` calls per frame only for charts that currently want
/// orderflow. Close the chart and nothing re-asks the venue for it.
///
/// # Lifetime
///
/// A record is created by a worker's report and removed by a successful one, so the healthy path
/// holds NOTHING (`is_empty()`). It is deliberately not swept by [`reap_orphaned_feeds`], for the
/// same reason its `RetryLane::DomDepth`/`PolyBook` siblings are not: this map shadows `bf_spawned`,
/// which is itself insert-only until process exit and bounded by the distinct symbols an operator
/// opens orderflow on. A retry record is bounded identically, and strictly more tightly — it also
/// disappears the moment a walk succeeds.
#[derive(Default)]
pub struct BackfillRetries {
    inner: HashMap<String, BackfillRetry>,
}

impl BackfillRetries {
    /// Fold one finished worker's report, log the transition, and either schedule the re-walk or end
    /// the chain. The whole decision, in the order it is made:
    ///
    /// 1. **Not retryable** (`Complete`/`Capped`) ⇒ forget the symbol. If a record was outstanding
    ///    this was a recovery, and it gets the one `info!`.
    /// 2. **Retryable, but `pages > 0`** ⇒ `Abandoned`, one `warn!`. Re-walking would re-deliver
    ///    those pages and double-count them; see this type's doc for why no resume point exists at
    ///    this seam.
    /// 3. **Retryable with `pages == 0` past the bound** ⇒ `Abandoned`, one `warn!` saying so.
    /// 4. **Retryable with `pages == 0`** ⇒ `Waiting`, re-walked after [`retry_backoff`].
    ///
    /// A report for a symbol with no record is the ordinary case — the FIRST walk is guarded by
    /// `bf_spawned` alone and records nothing, so failure 1 creates the record.
    pub fn note_report(&mut self, r: &BackfillReport) {
        let attempts = match self.inner.get(&r.symbol) {
            Some(
                BackfillRetry::InFlight { attempts } | BackfillRetry::Waiting { attempts, .. },
            ) => *attempts,
            Some(BackfillRetry::Abandoned) | None => 0,
        };
        let symbol = r.symbol.as_str();
        let Some(reason) = r.error.as_deref() else {
            // (1) The walk ended for a reason a re-run cannot improve. `Capped` lands here too, by
            // #952's rule: the venue served every page it was asked for, so an identical re-run caps
            // at the same page — it warns on the venue side and is NOT this lane's business.
            if self.inner.remove(symbol).is_some() && attempts > 0 {
                tracing::info!(
                    "of-backfill({symbol}): walk completed after {attempts} failed attempt(s)"
                );
            }
            return;
        };
        if r.pages > 0 {
            // (2) The failed walk DELIVERED. Its ticks are already folded into the aggregator and
            // this seam has no id to resume below them, so the chain ends here — see this type's
            // doc. The pages that arrived are kept: they are good data adjacent to `before_id`.
            if !matches!(self.inner.get(symbol), Some(BackfillRetry::Abandoned)) {
                tracing::warn!(
                    "of-backfill({symbol}): delivered {} page(s) then failed: {reason} — NOT \
                     re-walked, because a second walk would re-deliver those pages and the \
                     orderflow aggregator has no per-trade dedup. History older than the delivered \
                     pages is missing for this session",
                    r.pages
                );
            }
            self.inner.insert(r.symbol.clone(), BackfillRetry::Abandoned);
            return;
        }
        let attempts = attempts.saturating_add(1);
        if attempts > BACKFILL_MAX_RETRIES {
            // (3) The bound. Says what stopped it and what was lost, once.
            self.inner.insert(r.symbol.clone(), BackfillRetry::Abandoned);
            tracing::warn!(
                "of-backfill({symbol}): giving up after {BACKFILL_MAX_RETRIES} re-walks, last \
                 error: {reason} — this symbol has no historical orderflow for the rest of this \
                 session. LIVE orderflow is unaffected; only a restart re-attempts the history \
                 (reopening the chart will not — `bf_spawned` still holds this symbol)"
            );
            return;
        }
        // (4) Nothing was delivered, so a re-walk from the same `before_id` is exactly the same
        // request with nothing to double-count. Schedule it.
        let delay = retry_backoff(attempts);
        self.inner.insert(
            r.symbol.clone(),
            BackfillRetry::Waiting { attempts, next_at: Instant::now() + delay },
        );
        if attempts == 1 {
            tracing::warn!(
                "of-backfill({symbol}): failed with nothing delivered: {reason} — re-walking in \
                 {}s (doubling, at most {BACKFILL_MAX_RETRIES} re-walks)",
                delay.as_secs()
            );
        } else {
            tracing::debug!(
                "of-backfill({symbol}): failed again (attempt {attempts}/{BACKFILL_MAX_RETRIES}): \
                 {reason} — next re-walk in {}s",
                delay.as_secs()
            );
        }
    }

    /// Claim the pending re-walk for `symbol` if one is due, marking it in-flight — check and record
    /// in ONE call, the same idiom `HashSet::insert` gives [`should_spawn_backfill`]'s run-once
    /// gate. `false` for every other state: no record, a cooldown that has not elapsed, a walk
    /// already in flight, or an abandoned chain.
    ///
    /// Private because claiming a walk without spawning one would strand the record in
    /// `InFlight` — [`should_spawn_backfill`] is the only correct caller, and `vike-app` reports a
    /// failed `thread::Builder::spawn` back through [`Self::note_report`] so even that path
    /// re-opens.
    fn take_retry(&mut self, symbol: &str) -> bool {
        let attempts = match self.inner.get(symbol) {
            Some(BackfillRetry::Waiting { attempts, next_at }) if Instant::now() >= *next_at => {
                *attempts
            }
            _ => return false,
        };
        self.inner.insert(symbol.to_string(), BackfillRetry::InFlight { attempts });
        true
    }

    /// Is anything outstanding for this symbol — has a walk failed and not since succeeded? The
    /// observable question: `true` means that symbol's historical orderflow is currently incomplete.
    pub fn is_pending(&self, symbol: &str) -> bool {
        self.inner.contains_key(symbol)
    }

    /// `true` once this symbol's chain has ENDED unsuccessfully — the bound was reached, or a
    /// partial delivery made a re-walk unsafe. Nothing will re-walk it this session.
    pub fn is_abandoned(&self, symbol: &str) -> bool {
        matches!(self.inner.get(symbol), Some(BackfillRetry::Abandoned))
    }

    /// Failed walks recorded for this symbol so far (`0` when there is no record, and for an
    /// abandoned chain, which no longer schedules anything).
    pub fn attempts(&self, symbol: &str) -> u32 {
        match self.inner.get(symbol) {
            Some(
                BackfillRetry::InFlight { attempts } | BackfillRetry::Waiting { attempts, .. },
            ) => *attempts,
            Some(BackfillRetry::Abandoned) | None => 0,
        }
    }

    /// How many symbols have an incomplete backfill right now. `0` on the healthy path.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when every symbol's backfill is whole (or was never attempted).
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Forget every symbol's record — the feed-plane teardown
    /// (`crate::backend_conn::teardown_feed_plane`), which also clears `bf_spawned`: a re-mounted
    /// plane re-walks from scratch, so neither an `Abandoned` verdict nor a scheduled re-walk may
    /// survive the teardown. (A backend SWITCH deliberately leaves this lane alone since
    /// split-plane B2 — the walks page Binance venue truth, valid under any backend.)
    pub fn reset(&mut self) {
        self.inner.clear();
    }

    /// TEST ONLY: pretend every scheduled cooldown has elapsed, so a test can drive the re-walk path
    /// without sleeping real seconds — the same device (and the same reason) as
    /// [`FeedRetries::expire_all`]. The schedule itself is covered by [`retry_backoff`]'s own unit
    /// test, and "a re-walk does NOT happen before the cooldown" is provable without it: the floor
    /// is one second and a test runs in microseconds.
    #[cfg(test)]
    fn expire_all(&mut self) {
        let now = Instant::now();
        for st in self.inner.values_mut() {
            if let BackfillRetry::Waiting { next_at, .. } = st {
                *next_at = now;
            }
        }
    }
}

/// The venue-subscription bookkeeping every feed start/stop touches, borrowed as one bundle —
/// the [`CoreSyncState`](crate::core_sync::CoreSyncState) pattern applied to the write side of
/// the feed layer. The five are inseparable in practice: a feed is started at most once per key
/// (`spawned`), through its venue's client (`feeds`), and the returned id must be remembered
/// (`subs`) or the stream can never be stopped again — while `unroutable` records the keys that
/// got none of that because their venue has no client at all, and `retries` the keys that got none
/// of it because the venue was reached and said no.
pub struct FeedSlots<'a> {
    /// The venue-keyed live clients (`App::feeds`).
    pub feeds: &'a mut FeedMap,
    /// `chart/feed key -> live subscription id` (`App::subs`), the handle `unsubscribe` needs.
    pub subs: &'a mut HashMap<String, SubscriptionId>,
    /// Every key the GUI has ensured (`App::spawned`) — "this series is wanted".
    ///
    /// Deliberately NOT narrowed to "successfully subscribed": this set is also
    /// [`sync_from_core`](crate::core_sync)'s fold filter, and `--observe` mode has no feeds at all
    /// yet must still render remote bars. See [`ensure_feed_on`] for the full argument.
    pub spawned: &'a mut HashSet<String>,
    /// The subset of `spawned` whose venue had **no registered feed** at the last attempt
    /// (`App::unroutable`) — i.e. wanted, but never actually subscribed.
    ///
    /// Two jobs: it reopens the subscribe (a key in here is retried on the next `ensure_*` instead
    /// of being suppressed by `spawned`), and it is the warn-once gate that keeps a permanently
    /// missing venue from logging on every frame. Membership is *current* state, not a
    /// warned-once-ever tombstone — cleared the moment the venue's client is found, and swept by
    /// [`reap_orphaned_feeds`] when nothing wants the key anymore, so it can never become the
    /// insert-only set this whole fix exists to remove. Empty on the normal path; a non-empty entry
    /// means that chart is painting nothing.
    pub unroutable: &'a mut HashSet<String>,
    /// Every subscribe leg the venue **answered no** to and that has not since succeeded
    /// (`App::feed_retries`) — the sibling of `unroutable` for the other half of "wanted, but not
    /// actually subscribed". Shared with [`ensure_depth`]/[`ensure_poly_book`], which take it as a
    /// bare parameter because they own no [`FeedSlots`]; see [`FeedRetries`] for the one retry
    /// policy all four sites follow, and [`RetryLane`] for why the reaper sweeps only part of it.
    pub retries: &'a mut FeedRetries,
}

/// The render-side slots a feed owns for as long as it runs: the per-key chart state it fills,
/// the Data-manager "deleted" set, and the two client-side aggregator maps a tick/volume or
/// orderflow series is built in. Separate from [`FeedSlots`] because `ensure_depth` /
/// `ensure_poly_book` touch none of it.
pub struct SeriesSlots<'a> {
    /// Per-chart-key render state (`App::charts`).
    pub charts: &'a mut HashMap<String, model::ChartState>,
    /// Data-manager-deleted keys: the feed may still run, the GUI stops syncing (`App::hidden`).
    pub hidden: &'a mut HashSet<String>,
    /// Tick/volume aggregators, `chart key -> (venue, symbol, agg)` (`App::aggs`).
    pub aggs: &'a mut HashMap<String, (String, String, tickvol::TickVolAgg)>,
    /// Orderflow (footprint/CVD) aggregators, same keyspace (`App::of_aggs`).
    pub of_aggs: &'a mut HashMap<String, (String, String, orderflow::OrderflowAgg)>,
}

/// Which series [`ensure_feed_on`] should bring up: the four values that used to be four loose
/// arguments on `App::ensure_feed_on`. Bundled so the function stays under clippy's
/// `too_many_arguments` threshold once the two state bundles are also parameters.
pub struct SeriesSpec<'a> {
    /// Venue slug the subscription routes to (`"binance"`, `"okx"`, …).
    pub venue: &'a str,
    /// The window's symbol, BEFORE venue-native instrument resolution.
    pub symbol: &'a str,
    /// Kline interval (`"1m"`), or a client-side tick/volume interval (`"100t"`/`"10v"`).
    pub interval: &'a str,
    /// The catalog asset class, when the symbol came from a catalog hit — drives
    /// [`venue_bar_instrument`]'s derivative allowlist. `None` = the spot/quick-pick path.
    pub asset_class: Option<vike_catalog::AssetClass>,
}

/// Subscribe `venue`'s raw trade tape for `symbol`, once (idempotent via `spawned`'s
/// trade-key entry). Shared by the tick/volume branch of [`ensure_feed_on`] (Task B5, now
/// venue-aware) and SP2 orderflow's `of_wanted` processing (Task 7, now venue-aware too —
/// any venue with a `subscribe_trades` feed drives orderflow; only the aggTrades REST
/// backfill stays Binance-only, see the `of_wanted` apply site) — both need the same
/// underlying feed, just different per-(venue,symbol) consumers (`aggs` vs `of_aggs`). Trade key mirrors
/// [`workspace::series_key`]'s convention so two venues' trade feeds for the same symbol never
/// collide: `"{symbol}@trades"` for [`DEFAULT_VENUE`] (byte-identical to the pre-venue-aware
/// behavior), else `"{venue}:{symbol}@trades"` — the exact convention
/// [`orphaned_trade_feed_keys`] parses back out.
///
/// **Venue lookup first, `spawned` second** — the same ordering fix [`ensure_feed_on`] documents,
/// applied here because this function had the identical defect in a *worse* form: its missing-feed
/// path had no `else` arm at all, so a tick/volume or orderflow chart on a venue with no registered
/// client burned its trade-tape slot in complete silence. Every caller of this function is
/// per-frame, so the miss is now recorded in `unroutable` (warn once, retry freely) rather than
/// logged per frame.
///
/// **A FAILED `subscribe_trades` no longer burns the slot either** — the twin of the same change in
/// [`ensure_feed_on`], and the gap #946 named. The tape a tick/volume chart and every orderflow
/// overlay on this `(venue, symbol)` share is the *only* source those aggregators have, so one
/// transient error used to leave every one of them permanently empty. It is now recorded in
/// [`FeedSlots::retries`], which reopens the attempt on that leg's own backoff; see [`FeedRetries`]
/// for the cadence and for the one error kind that is deliberately never retried.
pub fn ensure_trade_feed_on(f: &mut FeedSlots<'_>, venue: &str, symbol: &str) {
    let trade_key = if venue == DEFAULT_VENUE {
        format!("{symbol}@trades")
    } else {
        format!("{venue}:{symbol}@trades")
    };
    // Same three-set shape as `ensure_feed_on`'s kline arm: `spawned` is recorded unconditionally
    // (its population is unchanged by either fix), while `unroutable` (venue absent) and `retries`
    // (venue said no) mark the ones that never subscribed and are what make the attempt repeatable.
    let retry_key = RetryKey::series(&trade_key);
    let first_ensure = f.spawned.insert(trade_key.clone());
    if !first_ensure && !f.unroutable.contains(&trade_key) && !f.retries.is_retry_due(&retry_key) {
        return;
    }
    match f.feeds.get_mut(venue) {
        Some(feed) => {
            f.unroutable.remove(&trade_key);
            match feed.subscribe_trades(symbol) {
                Ok(id) => {
                    if let Some(n) = f.retries.clear(&retry_key) {
                        tracing::info!(
                            "ensure_trade_feed_on({trade_key}): subscribe_trades recovered after \
                             {n} failed attempt(s)"
                        );
                    }
                    f.subs.insert(trade_key.clone(), id);
                }
                Err(e) => f.retries.note_error(
                    &retry_key,
                    &e,
                    &format!("ensure_trade_feed_on({trade_key}): subscribe_trades"),
                ),
            }
        }
        None => {
            if f.unroutable.insert(trade_key.clone()) {
                tracing::warn!(
                    "ensure_trade_feed_on({trade_key}): no feed registered for venue {venue:?} — \
                     no trades will arrive for it; will retry once one is registered"
                );
            }
        }
    }
}

/// Spawn a feed for `(venue, symbol, interval)` if not already running, routing the bar
/// subscription to `venue`'s [`vike_data::DataClient`] (cross-exchange symbol search). Kline
/// intervals subscribe that venue's live klines; the `charts`/`subs`/`spawned` slots are keyed
/// by [`workspace::series_key`] so Binance stays byte-identical (`"SYMBOL@interval"`) while
/// other venues are namespaced (`"venue:SYMBOL@interval"`) and never collide on a shared symbol.
/// Tick/volume intervals (Task B5) have no venue kline feed of their own — they're built
/// client-side from `venue`'s raw trade tape (venue-aware: any venue whose feed implements
/// `subscribe_trades` can drive tick/vol charts), so those spawn `venue`'s own trade feed
/// ([`ensure_trade_feed_on`]) plus a per-chart-key aggregator in `aggs`.
///
/// `shutting_down` is `App::shutdown`'s already-loaded value (see the module doc): once the
/// window-close is observed, no NEW live feed may start — a fresh subscription would open another
/// blocking socket read the bounded teardown would then have to wait on.
///
/// # A missing venue no longer permanently suppresses the subscribe
///
/// This used to run `spawned.insert(key)` **before** `feeds.get_mut(venue)`, and `spawned` was the
/// *only* gate — so ensuring a venue with no registered client suppressed the subscribe forever:
/// no retry ever, registering the feed afterwards did not help, and a single `warn!` was the only
/// trace. The reaper could not recover it either — [`orphaned_feed_keys`] only frees a slot whose
/// window is *gone*, and this window is very much alive, so the key never became orphaned. The
/// chart simply painted nothing, forever.
///
/// **This is reachable from the UI, not hypothetical.** `vike-app` registers exactly six live
/// clients (binance/bybit/okx/aster/hyperliquid/polymarket) while its Symbol picker searches a
/// TWELVE-venue [`vike_catalog`]. Five of the six extra venues reach this function with an asset
/// class [`venue_bar_instrument`] passes straight through — alpaca (`Equity`/`CryptoSpot`) and
/// oanda/dukascopy/fxcm/ctrader (`Fx`/`Cfd`) — so picking one of their instruments lands here with
/// a venue that has no feed. (Deribit, the one catalog present in *every* build, is NOT among them:
/// its rows are all `Option`/`CryptoFuture`/`CryptoPerp`, which `venue_bar_instrument` already
/// rejects for deribit, so those return before reaching the slot at all. The five above are
/// `fat`-only — and `restore_workspace` replays whatever venue a persisted `workspace.json` holds
/// on any build.)
///
/// **The obvious fix — insert into `spawned` only after the lookup succeeds — is wrong here, and
/// the reason is worth stating.** `spawned` is not only this function's idempotency gate: it is
/// also the filter [`sync_from_core`](crate::core_sync) folds core snapshots through
/// (`if !spawned.contains(&key) { continue }`). In `--observe` mode `vike-app` builds an **empty**
/// `feeds` map — no local core, no venue clients — and renders charts purely from the remote
/// daemon's streamed bars. Making `spawned` conditional on a feed lookup would leave it empty
/// there, and every observed chart would silently stop rendering. So `spawned` keeps its exact
/// former population ("the GUI wants this series"), and the *subset that never actually
/// subscribed* is tracked separately in [`FeedSlots::unroutable`]; the subscribe is attempted when
/// the key is new **or** is currently unroutable. Two deliberate choices around that, both about
/// *not* trading a silent failure for a noisy one:
///
/// - **The miss is warned once, not once per frame.** Every caller here is per-frame (the window
///   loop's compare + foreign-source-study fan-out), and `App::feeds` is built once in `App::new`
///   and never mutated, so today's unregistered venue is a *permanent* condition — a bare `warn!`
///   on the retry path would be a ~60 Hz log loop that can never succeed, which is the failure mode
///   behind this workspace's 341 GB trace-file incident. `unroutable` is the bounded record; see
///   [`FeedSlots::unroutable`]. This mirrors #937, whose whole point was that the recovered lossy
///   path warns per *event*, "not per frame".
/// - **A failed `subscribe_bars` is retryable too — but on a backoff, not per frame.** #946 left
///   this one open on purpose ("retrying *that* would resubscribe against a live venue socket every
///   frame, which needs backoff to be safe — real machinery, deliberately not smuggled in here"),
///   and named it the remaining gap. The machinery now exists: [`FeedRetries`]. The distinction it
///   preserves is the one #946 drew — the venue was reached and *answered*, so the attempt really
///   happened and the re-attempt costs a venue call — which is why the cadence differs from the
///   missing-venue case rather than the outcome differing. A blip recovers in a second; a venue
///   that keeps rejecting the symbol settles at one re-ask a minute, with its record visible in
///   `retries` the whole time instead of a single warn scrolling away. The one exception is a
///   DECLARED-capability refusal (`LiveDataError::Unsupported`), which is provably permanent and is
///   recorded-but-never-retried; see [`FeedRetries`].
///
/// Consistency note with #937, which fixed this same insert-only shape in the aggTrades backfill
/// lane: that fix deliberately left `bf_spawned` closed and recovered the data elsewhere, because
/// there the guarded work (a 300-page REST backfill) really had run. The shared rule is *the gate
/// must reflect what actually happened* — and it points opposite ways in the two lanes precisely
/// because in this one, nothing happened. #937's other rule is honoured literally: its recovered
/// lossy path warns per *event*, "not per frame", which is why the miss here is warned once per key
/// rather than on every one of the ~60 ensure calls a second each window makes.
pub fn ensure_feed_on(
    f: &mut FeedSlots<'_>,
    s: &mut SeriesSlots<'_>,
    spec: SeriesSpec<'_>,
    display_tz: DisplayTz,
    shutting_down: bool,
) {
    let SeriesSpec { venue, symbol, interval, asset_class } = spec;
    // Shutting down (window-close requested / `on_exit` running): never start a NEW live feed —
    // a fresh subscription would open another blocking socket read the bounded teardown would
    // then have to wait on. Inert on the running path (a single Relaxed load; see `on_exit`).
    if shutting_down {
        return;
    }
    // Feed-routing slice 1: resolve the venue-native bar instrument for this product, or
    // skip entirely when the venue has no bar feed for it yet (no charting the wrong/spot
    // series). For every currently-working case (spot everywhere, OKX derivatives) `inst`
    // is identical to `symbol` — OKX's `raw_symbol` IS already the dashed instId — so the
    // key below is unchanged from before this feature for every case that used to work.
    let Some(inst) = venue_bar_instrument(venue, symbol, asset_class) else {
        return;
    };
    let key = workspace::series_key(venue, &inst, interval);
    s.hidden.remove(&key); // re-adding a deleted series just unhides it
    match tickvol::BarKind::parse(interval) {
        tickvol::BarKind::Kline(_) => {
            // `spawned` is recorded FIRST and unconditionally, exactly as before — it doubles as
            // `sync_from_core`'s fold filter, and `--observe` mode registers NO feeds at all, so
            // gating it on the venue lookup would stop every remote-snapshot chart from rendering
            // (see this function's doc). `unroutable` (venue absent) and `retries` (venue said no)
            // are the two subsets that never actually subscribed, and they are what reopen the
            // attempt on a later frame — the first per frame because re-checking is free, the
            // second only once its own backoff has elapsed.
            let retry_key = RetryKey::series(&key);
            let first_ensure = f.spawned.insert(key.clone());
            if first_ensure || f.unroutable.contains(&key) || f.retries.is_retry_due(&retry_key) {
                match f.feeds.get_mut(venue) {
                    Some(feed) => {
                        f.unroutable.remove(&key);
                        match feed.subscribe_bars(&inst, interval) {
                            Ok(id) => {
                                if let Some(n) = f.retries.clear(&retry_key) {
                                    tracing::info!(
                                        "ensure_feed({key}): subscribe_bars recovered after {n} \
                                         failed attempt(s)"
                                    );
                                }
                                f.subs.insert(key.clone(), id);
                            }
                            // A FAILED subscribe stays retryable, but on ITS OWN cadence: the venue
                            // was reached and answered, so the re-attempt costs a venue call, and
                            // every caller here is per-frame — hence the backoff rather than the
                            // free per-frame re-check the missing-venue arm below gets.
                            Err(e) => f.retries.note_error(
                                &retry_key,
                                &e,
                                &format!("ensure_feed({key}): subscribe_bars"),
                            ),
                        }
                    }
                    None => {
                        if f.unroutable.insert(key.clone()) {
                            // ⚠ States the FACT and not a consequence. This used to end "this chart
                            // will stay empty until one is", which is false in the configuration
                            // that produces it most often: `vike-app --observe` links no venue
                            // bridge at all, so every chart reaches this arm — while the chart
                            // itself fills perfectly from `WireSnapshot::bars` over the node
                            // protocol (proto v2's "bounded bar tails for the observer chart").
                            // MEASURED 2026-08-31: a thin client logged this line on repeat while
                            // drawing five hours of live BTCUSDT candles with volume, RSI and MACD.
                            // A warning that contradicts what the operator can see teaches them to
                            // ignore warnings.
                            tracing::warn!(
                                "ensure_feed({key}): no LOCAL feed registered for venue \
                                 {venue:?} — bars for this chart can only arrive from a backend \
                                 node (`--observe`); nothing local will produce them. Will retry"
                            );
                        }
                    }
                }
            }
        }
        kind => {
            // Tick/Volume: the underlying feed is the raw trade tape, spawned ONCE per
            // (venue, symbol) (a second tick/volume interval on the same symbol reuses it) —
            // the aggregator, not the feed, is per chart key. Venue-aware (any venue whose
            // feed implements `subscribe_trades` can drive tick/vol charts, not just Binance).
            ensure_trade_feed_on(f, venue, &inst);
            // `kind` is guaranteed non-Kline on this match arm (the Kline arm above is the
            // only other variant), so `TickVolAgg::new` never returns `None` here.
            s.aggs.entry(key.clone()).or_insert_with(|| {
                (venue.to_string(), inst.clone(), tickvol::TickVolAgg::new(&kind).unwrap())
            });
        }
    }
    s.charts.entry(key.clone()).or_default().set_tz(display_tz);
}

/// **Stop ONE series, completely** — the whole per-key teardown, in the one place both callers
/// reach it.
///
/// Two sites tear a series down and they must not disagree: [`reap_orphaned_feeds`] applies this to
/// every key no window references anymore, and the Data-manager "Delete" button
/// (`crates/vike-app/src/app_ui.rs`'s `draw_windows`) applies it to the one row an operator picked.
/// Four steps, each of which some later frame depends on: drop the render series (`charts`), mark
/// the key `hidden` (belt and braces against a same-frame resurrection), stop the live venue
/// subscription (`subs` + `unsubscribe`), and free the `spawned` slot so a later [`ensure_feed_on`]
/// genuinely restarts the feed instead of finding it already "spawned".
///
/// # Why this is a function and not a rule
///
/// Because it was a rule, and the rule was broken. [`reap_orphaned_feeds`]'s own doc said its
/// teardown mirrored the Data-manager path "exactly", while that path carried a HAND COPY that had
/// drifted on the one line that matters here: it looked the feed up as `feeds["binance"]` outright,
/// under a comment claiming both key kinds were binance-only. That claim stopped being true the
/// moment [`workspace::series_key`] began namespacing every non-[`DEFAULT_VENUE`] series as
/// `"venue:SYMBOL@interval"`, so EVERY venue but the default one was exposed. Deleting an
/// `"okx:BTC-USDT@1m"` row removed the id from `subs`, missed the binance lookup, dropped the id on
/// the floor, and freed `spawned` anyway — the stream ran until process exit, and the next
/// [`ensure_feed_on`] opened a SECOND one on top of it. Nothing could recover that: the key had
/// already left `subs`, and a Data-manager Delete leaves the window in place (`open = false`), so
/// [`reap_orphaned_feeds`] still counts the key as LIVE and never looks at it.
///
/// So the routing is stated once, here: the unsubscribe goes to the SAME venue feed the key was
/// subscribed on, recovered from the key's own prefix by [`venue_of_key`] — the inverse of the
/// builder that put it there. A venue with no registered client (an `--observe` session has none at
/// all) is tolerated exactly as every `ensure_*` tolerates it: the slots are freed and nothing is
/// dialled.
///
/// # What it deliberately does NOT touch
///
/// Any aggregator. The two callers need OPPOSITE things there, so folding an `aggs`/`of_aggs`
/// removal in would put two contradictory preconditions under one name: [`reap_orphaned_feeds`]
/// drops dead aggregators in BULK afterwards (step 2a) precisely because it must then recompute
/// which SHARED trade tapes are still needed, while the Data-manager caller removes its own key's
/// entry by hand because that key IS still live — its window is still in `wins`, so step 2a would
/// keep the aggregator. See that function's doc for the shared-tape argument.
pub fn stop_series(f: &mut FeedSlots<'_>, s: &mut SeriesSlots<'_>, key: &str) {
    s.charts.remove(key);
    s.hidden.insert(key.to_string());
    if let Some(id) = f.subs.remove(key) {
        // Route the unsubscribe to the SAME venue feed the key was subscribed on — a non-Binance
        // key is namespaced `"venue:SYMBOL@interval"` (see `workspace::series_key`), so its venue
        // is recoverable from the key prefix.
        if let Some(feed) = f.feeds.get_mut(venue_of_key(key)) {
            feed.unsubscribe(id);
        }
    }
    f.spawned.remove(key);
}

/// C2 tidy (FIX 3): reap any `spawned` feed no window references anymore — removing a
/// Compare symbol (chip ✕, `WinState::remove_compare`) or switching a window's primary
/// symbol away from one (FIX 1's `remove_compare` cascade) can otherwise leave that
/// symbol's feed running forever, since nothing else ever un-spawns it. [`ensure_feed_on`] is
/// idempotent per `sym@interval` (`spawned.insert`), so this is bounded by DISTINCT
/// symbols, not unbounded — but it still leaks until a matching compare is re-added.
///
/// Called once every frame (see `vike-app`'s `update` call site) rather than threaded into every
/// removal call site — cheap (a handful of windows/keys) and it catches every orphaning
/// path uniformly (a compare chip removed, a window's primary symbol switched away) with
/// no per-call-site bookkeeping to keep in sync. A mere window *close* is deliberately NOT
/// one of them: a closed window stays in `wins` (`open = false`) and keeps its feed, so its
/// key stays in the live set (see [`orphaned_feed_keys`]'s "regardless of `open`/`minimized`"
/// note). The live-set computation itself is that pure, unit-tested function — see its doc
/// for exactly what counts as "live".
///
/// The actual teardown IS the Data-manager "Delete" path: both call [`stop_series`], applied per
/// orphan and automatically here, by hand and per row there. ⚠ This paragraph used to say the two
/// "mirror … exactly" while the other side carried a hand copy that had drifted into a
/// binance-only unsubscribe — [`stop_series`]'s doc carries that post-mortem, and it is the reason
/// there is one copy now rather than a claim about two.
///
/// Feed-leak fix: two coupled cleanups follow the Kline reap. First, drop any orphaned
/// tick/volume (`aggs`) or orderflow (`of_aggs`) AGGREGATOR entry — an aggregator's key IS its
/// window's `WinState::key()`, so an entry whose key is not in [`live_window_keys`] has no
/// window backing it (its window's interval/symbol changed, or the window was deleted) and
/// would otherwise sit in the map until app restart. Second, stop any now-orphaned raw trade
/// tape (`"…@trades"`): those feeds are SHARED per `(venue, symbol)` across every aggregator on
/// that pair, so a feed is stopped ONLY once no remaining `aggs`/`of_aggs` entry needs it —
/// hence `needed` is computed AFTER the dead aggregators are dropped, and only feeds whose
/// `(venue, symbol)` fell out of `needed` are torn down (via the pure
/// [`orphaned_trade_feed_keys`]). This closes the old documented gap (a deleted tick/vol or
/// orderflow chart used to leak both its aggregator entry AND its `subscribe_trades` thread).
///
/// A third, trivial cleanup closes the loop on [`FeedSlots::unroutable`]: an unroutable key is by
/// construction NOT in `spawned`, so neither reap above can ever reach it, and the set would grow
/// insert-only for the life of the process — the exact shape [`ensure_feed_on`] was just fixed for.
/// It is swept by the same two rules, so `unroutable` always describes series the GUI still wants.
pub fn reap_orphaned_feeds(
    f: &mut FeedSlots<'_>,
    s: &mut SeriesSlots<'_>,
    wins: &[workspace::WinState],
) {
    for k in orphaned_feed_keys(wins, f.spawned) {
        stop_series(f, s, &k);
    }
    // (2a) Reap orphaned tick/vol + orderflow aggregator entries: an `aggs`/`of_aggs` key IS a
    // window's `key()`, so any entry not backed by a live window is dead. Closed/minimized
    // windows still count as live (same notion the Kline reap above uses), so this never drops
    // an aggregator for a merely-hidden chart — only for one whose window is truly gone or
    // whose key changed (interval/symbol switch).
    let live = live_window_keys(wins);
    s.aggs.retain(|k, _| live.contains(k));
    s.of_aggs.retain(|k, _| live.contains(k));
    // (2b) `needed` = the (venue, symbol) of every REMAINING aggregator (read straight off the
    // stored tuple fields). Both maps are unioned because a Tick/Volume `aggs` entry and an
    // orderflow `of_aggs` entry on the same (venue, symbol) ride the SAME trade feed. This is
    // computed after (2a) so a just-dropped aggregator no longer holds its feed open. No race
    // with the per-frame `of_wanted`/backfill: those run earlier this frame and (re)insert
    // their `of_aggs` entries before we get here, so every still-wanted pair is in `needed`.
    let mut needed: HashSet<(String, String)> = HashSet::new();
    for (venue, symbol, _) in s.aggs.values() {
        needed.insert((venue.clone(), symbol.clone()));
    }
    for (venue, symbol, _) in s.of_aggs.values() {
        needed.insert((venue.clone(), symbol.clone()));
    }
    // (2c) Stop each trade feed no remaining aggregator needs, routing the unsubscribe to the
    // right venue (recovered from the `@trades` key the same way the Kline reap recovers it).
    for key in orphaned_trade_feed_keys(&needed, f.spawned, DEFAULT_VENUE) {
        if let Some(id) = f.subs.remove(&key) {
            if let Some(feed) = f.feeds.get_mut(venue_of_key(&key)) {
                feed.unsubscribe(id);
            }
        }
        f.spawned.remove(&key);
    }
    // (2d) Forget the `unroutable` record for any key nothing wants anymore. Without this the set
    // would be insert-only — precisely the shape this whole fix exists to remove, re-introduced one
    // layer up. An unroutable key is never in `spawned` (that is the fix), so neither reap above
    // can reach it; it is swept here by the SAME two rules they use — a kline key survives while a
    // window backs it, a `@trades` key while some remaining aggregator still needs its
    // (venue, symbol).
    let still_wanted = |k: &str| match trade_key_pair(k, DEFAULT_VENUE) {
        Some(pair) => needed.contains(&pair),
        None => live.contains(k),
    };
    f.unroutable.retain(|k| still_wanted(k.as_str()));
    // (2e) …and the same sweep for the SERIES lane of `retries`, for the same reason and under the
    // same two rules. A torn-down key falls out here even though the reaps above already freed its
    // `spawned` slot: leaving the record would carry a stale attempt count (and so an inflated
    // backoff) into the re-added series' first subscribe. The DOM/cockpit lanes are deliberately
    // untouched — this function knows nothing about `dom_depth`/`poly_subs`; their teardown (and
    // the matching lane sweep) is [`reap_orphaned_dom_cockpit_streams`]'s job, run by the same
    // per-frame caller.
    f.retries.retain_series(still_wanted);
}

/// The venue the cockpit's book/trade streams live on — [`ensure_poly_book`]'s feed key, and
/// therefore the ONE venue [`reap_orphaned_dom_cockpit_streams`] routes a cockpit unsubscribe to.
const POLY_VENUE: &str = "polymarket";

/// The canonical symbols of every DOM window in `wins` — the live set
/// [`reap_orphaned_dom_cockpit_streams`] diffs `dom_depth` against. Same membership rule as
/// [`live_window_keys`]: ALL windows count, regardless of `open`/`minimized` (a merely-closed
/// window keeps its streams exactly like it keeps its kline feed; only a DELETED window tears
/// down). Deliberately venue-BLIND: a DOM window's selected venue lives in its tool view, not its
/// [`workspace::WinState`], and [`ensure_depth`] keeps a previously-selected venue's stream warm
/// across a venue switch on purpose (see its doc) — so an entry dies only when NO DOM window shows
/// its symbol anymore, never merely because the selector moved on.
pub fn live_dom_symbols(wins: &[workspace::WinState]) -> HashSet<String> {
    wins.iter().filter(|w| w.kind == workspace::WinKind::Dom).map(|w| w.symbol.clone()).collect()
}

/// The token-ids of every Polymarket cockpit window in `wins` — [`live_dom_symbols`]'s cockpit
/// twin, under the same regardless-of-`open` rule. A window still carrying the placeholder token
/// (its Gamma resolve has not landed) or an empty symbol contributes nothing: neither can have
/// been subscribed ([`ensure_poly_book`] refuses both), so neither can hold a stream alive.
pub fn live_poly_tokens(wins: &[workspace::WinState]) -> HashSet<String> {
    wins.iter()
        .filter(|w| w.kind == workspace::WinKind::Polymarket)
        .filter(|w| !w.symbol.is_empty() && w.symbol != POLY_PLACEHOLDER_TOKEN)
        .map(|w| w.symbol.clone())
        .collect()
}

/// [`reap_orphaned_feeds`]'s DOM/cockpit sibling — the teardown path `dom_depth`/`poly_subs`
/// never had (the B1 review's flagged debt, and B2's prerequisite): stop every depth and cockpit
/// stream whose windows are GONE, at the same per-frame cadence and under the same live-set
/// notion. Before this existed, closing a DOM window left its L2 depth stream (and, non-Binance,
/// its 1m bar leg) running until process exit — harmless in observe mode (both maps empty), a real
/// socket leak in the fat build.
///
/// Split from [`reap_orphaned_feeds`] rather than folded into it because the two own disjoint
/// state: that reaper's whole surface is [`FeedSlots`]/[`SeriesSlots`], which deliberately carry
/// neither `dom_depth` nor `poly_subs` ([`ensure_depth`]/[`ensure_poly_book`] take them as bare
/// parameters for the same reason). The routing mirror-images the subscribe exactly:
/// [`ensure_depth`] subscribed through `feeds[venue_str(venue)]` and the entry's key REMEMBERS
/// that venue string, so the unsubscribe goes to `feeds[&key.0]` — the same
/// "route by the key's own venue" rule `reap_orphaned_feeds` applies via
/// [`venue_of_key`](crate::venue_routing::venue_of_key); a cockpit stream always routes to
/// [`POLY_VENUE`], the one feed [`ensure_poly_book`] ever subscribes on.
///
/// A venue whose client is no longer registered (or was never registered — a `--observe` session
/// has no feeds at all) is tolerated: the entry is dropped without an unsubscribe, matching the
/// venue-missing arms of every `ensure_*`. The venue-side tolerance is the trait's:
/// [`DataClient::unsubscribe`] on an id the client no longer knows is a documented no-op, so a
/// second teardown of the same id (impossible here — the entry is removed — but reachable through
/// the backend-switch clear running after this reaper) cannot panic either.
///
/// The closing sweep drops the DOM/cockpit [`FeedRetries`] records nothing can drive anymore —
/// [`reap_orphaned_feeds`]'s step (2e), applied to the lanes it deliberately leaves alone. A
/// depth/book record dies with its window (its per-frame `ensure_*` caller is gone, so it could
/// only ever sit idle); a bar/trade record dies with the map entry whose retry arm re-attempts it.
/// Without this, a torn-down window's records would be the insert-only residue this module keeps
/// having to remove, and a re-opened window would inherit a stale attempt count.
pub fn reap_orphaned_dom_cockpit_streams(
    feeds: &mut FeedMap,
    dom_depth: &mut HashMap<(String, String), DomDepthSubs>,
    poly_subs: &mut HashMap<String, PolyBookSubs>,
    retries: &mut FeedRetries,
    wins: &[workspace::WinState],
) {
    let dom_live = live_dom_symbols(wins);
    let poly_live = live_poly_tokens(wins);
    dom_depth.retain(|(venue, canonical), sub| {
        if dom_live.contains(canonical) {
            return true;
        }
        if let Some(feed) = feeds.get_mut(venue.as_str()) {
            feed.unsubscribe(sub.depth);
            if let Some(bars) = sub.bars {
                feed.unsubscribe(bars);
            }
        }
        false
    });
    poly_subs.retain(|token, sub| {
        if poly_live.contains(token) {
            return true;
        }
        if let Some(feed) = feeds.get_mut(POLY_VENUE) {
            feed.unsubscribe(sub.book);
            if let Some(trades) = sub.trades {
                feed.unsubscribe(trades);
            }
        }
        false
    });
    // The lane sweep. DomDepth/PolyBook records are keyed by what the live sets hold (canonical
    // symbol behind the venue prefix / token id), so they are judged against the window directly.
    // A DomBars record's id carries the venue-NATIVE inst, which only its `dom_depth` entry can
    // translate back — so it is kept exactly while a SURVIVING entry still names it (the entry's
    // retry arm is the only thing that can ever re-attempt it); PolyTrades records ride their
    // token like the book does.
    let live_bar_ids: HashSet<String> =
        dom_depth.iter().map(|((venue, _), sub)| format!("{venue}:{}", sub.inst)).collect();
    retries.retain_dom_cockpit(|lane, id| match lane {
        RetryLane::DomDepth => id.split_once(':').is_some_and(|(_, c)| dom_live.contains(c)),
        RetryLane::DomBars => live_bar_ids.contains(id),
        RetryLane::PolyBook | RetryLane::PolyTrades => poly_live.contains(id),
        // Never consulted for Series (`retain_dom_cockpit` retains that lane unconditionally);
        // named so this match stays exhaustive when a lane is added.
        RetryLane::Series => true,
    });
}

/// What one `dom_depth` entry OWNS: the live subscription ids [`ensure_depth`] minted for its
/// `(venue, canonical)` key, plus the venue-native instrument both legs subscribed with. Kept —
/// instead of the ids being dropped on the floor, which is what this map's `HashSet` predecessor
/// did — so [`reap_orphaned_dom_cockpit_streams`] can stop exactly these streams when the last
/// DOM window on the symbol is deleted, and `teardown_feed_plane` (the backend-switch clear's
/// feed-plane complement) can stop them against the clients that issued them before forgetting
/// them. (A backend SWITCH keeps these streams — venue truth, split-plane B2.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomDepthSubs {
    /// The venue-NATIVE instrument id both legs subscribe with ([`venue_inst`]) — remembered so
    /// teardown can name this entry's [`RetryKey::dom_bars`] record exactly (that key carries the
    /// inst, not the canonical symbol, and only this entry can translate between them).
    pub inst: String,
    /// `subscribe_depth`'s id — the handle `unsubscribe` needs to stop the L2 stream.
    pub depth: SubscriptionId,
    /// The non-Binance 1m `subscribe_bars` leg's id, once that leg has succeeded. `None` on
    /// Binance (no bar leg here at all — see [`ensure_depth`]) and while the leg is still
    /// failing/retrying under its [`RetryLane::DomBars`] record.
    pub bars: Option<SubscriptionId>,
}

/// What one `poly_subs` entry OWNS — [`DomDepthSubs`]'s cockpit twin, for the same two spenders.
/// No `inst` field: a cockpit stream is keyed by the token id on both the subscribe and the
/// [`RetryKey`], so there is nothing to translate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolyBookSubs {
    /// `subscribe_book`'s id — the leg whose success records the entry (the ladder paints).
    pub book: SubscriptionId,
    /// The `subscribe_trades` leg's id, once it has succeeded; `None` while it is still
    /// failing/retrying under its [`RetryLane::PolyTrades`] record.
    pub trades: Option<SubscriptionId>,
}

/// Start a live L2 depth stream for `(venue, canonical)` (once) so the DOM renders that venue's
/// real book. Routes to the venue's feed; dedups via `dom_depth`, whose entry KEEPS the minted
/// subscription ids ([`DomDepthSubs`]); the stream is stopped by
/// [`reap_orphaned_dom_cockpit_streams`] once no DOM window shows the symbol anymore (before that
/// existed, only the feed's `shutdown()` on exit ever stopped it). Called every frame for the
/// DOM's selected venue — idempotent, so a venue switch just starts the new stream while the old
/// one keeps its book warm (the reaper honours that: it is venue-blind, tearing down only when
/// the symbol's last DOM window is deleted).
///
/// The extra `"1m"` bar subscription it starts for a non-Binance venue is recorded in the entry's
/// [`DomDepthSubs::bars`] rather than in [`FeedSlots`] (it has no chart key of its own) — which is
/// exactly why `dom_depth`, not `spawned`, is this function's idempotency gate, and why
/// [`orphaned_feed_keys`] must count a DOM window's own `"{symbol}@1m"` as live (see its doc).
///
/// # The 1m bar leg no longer vanishes under the depth gate
///
/// That "not entered into [`FeedSlots`]" note used to be the whole story for the bar leg: it was
/// attempted exactly once, inside the depth leg's `Ok` arm, and `dom_depth.insert(key)` ran
/// **whether or not it succeeded** — so a single `subscribe_bars` error left that venue's PAPER
/// engine without a bar for the life of the process (the core's `on_bar` seam is what routes closed
/// bars to each venue's engine), behind one `warn!`, with nothing able to reopen it: `dom_depth` is
/// this function's only gate, and this function is its only writer. The leg now carries its own
/// [`RetryLane::DomBars`] record, so a later frame re-attempts *just* that leg — the depth stream,
/// which did succeed, is never re-subscribed.
///
/// The depth leg itself was already retryable (a failure records no `dom_depth` entry) but retried
/// on **every frame**, i.e. a ~60 Hz socket-and-log loop against a live venue. It is now throttled
/// by the same [`FeedRetries`] schedule, so this function has ONE retry cadence rather than two
/// contradictory ones. Same for the missing-venue arm's `warn!`, which was per frame and is now
/// once per `(venue, symbol)`.
pub fn ensure_depth(
    feeds: &mut FeedMap,
    dom_depth: &mut HashMap<(String, String), DomDepthSubs>,
    retries: &mut FeedRetries,
    venue: dom::DomVenue,
    canonical: &str,
) {
    let vs = venue_str(venue);
    let key = (vs.to_string(), canonical.to_string());
    let inst = venue_inst(venue, canonical);
    let depth_key = RetryKey::dom_depth(vs, canonical);
    let bars_key = RetryKey::dom_bars(vs, &inst);
    // Non-primary venues also need 1m bars so their PAPER engine fills (the core's `on_bar` seam
    // routes each venue's closed bars to its engine); Binance bars already flow via `ensure_feed`
    // (the chart/DOM open), so it has no bar leg here at all.
    let need_bars = venue != dom::DomVenue::Binance;

    if let Some(sub) = dom_depth.get_mut(&key) {
        // The depth stream is live and must NOT be re-subscribed. The only thing that can still be
        // outstanding is the bar leg — which used to be dropped on the floor right here.
        if need_bars && retries.is_retry_due(&bars_key) {
            if let Some(feed) = feeds.get_mut(vs) {
                if let Some(id) = try_dom_bars(&mut **feed, retries, vs, &inst, &bars_key) {
                    sub.bars = Some(id);
                }
            }
        }
        return;
    }
    // A previously-failed depth subscribe is retried, but only once its cooldown has elapsed —
    // this is the per-frame resubscribe loop the old code ran.
    if retries.is_blocked(&depth_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(vs) else {
        if retries.note_missing(&depth_key) {
            tracing::warn!(
                "ensure_depth({vs}, {inst}): no feed registered for venue — the DOM ladder will \
                 stay STALE; will retry once one is registered"
            );
        }
        return;
    };
    match feed.subscribe_depth(&inst) {
        Ok(depth_id) => {
            if let Some(n) = retries.clear(&depth_key) {
                tracing::info!(
                    "ensure_depth({vs}, {inst}): subscribe_depth recovered after {n} failed \
                     attempt(s)"
                );
            }
            let bars = if need_bars {
                try_dom_bars(&mut **feed, retries, vs, &inst, &bars_key)
            } else {
                None
            };
            dom_depth.insert(key, DomDepthSubs { inst, depth: depth_id, bars });
        }
        Err(e) => retries.note_error(
            &depth_key,
            &e,
            &format!("ensure_depth({vs}, {inst}): subscribe_depth"),
        ),
    }
}

/// The DOM's non-Binance 1m bar leg, in one place so its FIRST attempt (inside the depth `Ok` arm)
/// and its RETRIES (a later frame, once `dom_depth` is already recorded) can never diverge — the
/// divergence being exactly how the retry path came not to exist at all. Returns the minted id on
/// success so both callers record it in the entry's [`DomDepthSubs::bars`] (`None` = the failure
/// was recorded on `retries` and a later frame re-attempts).
fn try_dom_bars(
    feed: &mut (dyn DataClient + Send),
    retries: &mut FeedRetries,
    venue: &str,
    inst: &str,
    bars_key: &RetryKey,
) -> Option<SubscriptionId> {
    match feed.subscribe_bars(inst, "1m") {
        Ok(id) => {
            if let Some(n) = retries.clear(bars_key) {
                tracing::info!(
                    "ensure_depth({venue}, {inst}): the 1m bar leg recovered after {n} failed \
                     attempt(s) — this venue's paper engine receives bars again"
                );
            }
            Some(id)
        }
        Err(e) => {
            retries.note_error(
                bars_key,
                &e,
                &format!("ensure_depth({venue}, {inst}): subscribe_bars 1m"),
            );
            None
        }
    }
}

/// Start (once) a live Polymarket L2 book + trade stream for `token` (the YES-outcome token-id)
/// so a cockpit window renders that token's real book from the shared `BookStore` under venue
/// `"polymarket"`. The polymarket feed's `subscribe_book` emits `LiveDataSink::book`, which the
/// composition-root `CoreSinkAdapter::book` now also lands in the `BookStore` (see that method).
/// The cockpit's analog of [`ensure_depth`]; dedups via `poly_subs`, whose entry KEEPS the minted
/// ids ([`PolyBookSubs`]) so [`reap_orphaned_dom_cockpit_streams`] stops both streams once no
/// cockpit window shows the token anymore (before that existed, only the feed's `shutdown()` on
/// exit ever stopped them). A missing feed, an empty/placeholder token, or a subscribe error is
/// logged and skipped — feed failure never crashes the GUI (the cockpit just stays STALE).
///
/// The feed runs on its own thread and routes through the crate's proxy (`POLY_*` env, resolved
/// internally); it connects only when the WS proxy is enabled AND reachable (US-geo-block),
/// otherwise it retries with backoff and the book stays empty/STALE — graceful degradation.
///
/// # The trade leg no longer vanishes under the book gate
///
/// `poly_subs.insert(token)` is deliberately driven by the BOOK leg — the book is what the ladder
/// paints, so recording the token once it is live is correct and must stay (re-running
/// `subscribe_book` every frame would be the other bug). But the `subscribe_trades` leg ran inside
/// that same `Ok` arm and its failure only warned: `poly_subs` then suppressed every later call, so
/// that token's trade tape was permanently absent — the cockpit painted a book with no prints and
/// nothing could reopen it. The leg now carries its own [`RetryLane::PolyTrades`] record, so a
/// later frame re-attempts *just* the trades subscribe while the live book is left alone. The book
/// leg's own failure is unchanged in outcome (still retried, still records nothing) but is now
/// throttled by the same schedule instead of re-dialling on every frame.
pub fn ensure_poly_book(
    feeds: &mut FeedMap,
    poly_subs: &mut HashMap<String, PolyBookSubs>,
    retries: &mut FeedRetries,
    token: &str,
) {
    if token.is_empty() || token == POLY_PLACEHOLDER_TOKEN {
        return;
    }
    let book_key = RetryKey::poly_book(token);
    let trades_key = RetryKey::poly_trades(token);
    if let Some(sub) = poly_subs.get_mut(token) {
        // The book is live and must NOT be re-subscribed; only the trade leg can still be
        // outstanding — which is exactly what used to be unreachable from here.
        if retries.is_retry_due(&trades_key) {
            if let Some(feed) = feeds.get_mut(POLY_VENUE) {
                if let Some(id) = try_poly_trades(&mut **feed, retries, token, &trades_key) {
                    sub.trades = Some(id);
                }
            }
        }
        return;
    }
    if retries.is_blocked(&book_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(POLY_VENUE) else {
        if retries.note_missing(&book_key) {
            tracing::warn!(
                "ensure_poly_book({token}): no polymarket feed registered — the cockpit will stay \
                 STALE; will retry once one is registered"
            );
        }
        return;
    };
    match feed.subscribe_book(token) {
        Ok(book_id) => {
            if let Some(n) = retries.clear(&book_key) {
                tracing::info!(
                    "ensure_poly_book({token}): subscribe_book recovered after {n} failed \
                     attempt(s)"
                );
            }
            let trades = try_poly_trades(&mut **feed, retries, token, &trades_key);
            poly_subs.insert(token.to_string(), PolyBookSubs { book: book_id, trades });
        }
        Err(e) => {
            retries.note_error(&book_key, &e, &format!("ensure_poly_book({token}): subscribe_book"))
        }
    }
}

/// The cockpit's trade-tape leg, in one place for the same reason [`try_dom_bars`] is: its first
/// attempt and its retries must stay identical. Returns the minted id on success so both callers
/// record it in the entry's [`PolyBookSubs::trades`].
fn try_poly_trades(
    feed: &mut (dyn DataClient + Send),
    retries: &mut FeedRetries,
    token: &str,
    trades_key: &RetryKey,
) -> Option<SubscriptionId> {
    match feed.subscribe_trades(token) {
        Ok(id) => {
            if let Some(n) = retries.clear(trades_key) {
                tracing::info!(
                    "ensure_poly_book({token}): the trade leg recovered after {n} failed \
                     attempt(s) — prints resume"
                );
            }
            Some(id)
        }
        Err(e) => {
            retries.note_error(
                trades_key,
                &e,
                &format!("ensure_poly_book({token}): subscribe_trades"),
            );
            None
        }
    }
}

/// SP3 Task 3: unit tests for the pure [`should_spawn_backfill`] gate — see its doc for why this
/// is tested standalone rather than through a full `App` (no precedent for constructing one in
/// `cargo test`; `dom_position_tests` in `dom_math` is the established pattern of testing an
/// extracted pure helper instead).
#[cfg(test)]
mod backfill_gate_tests {
    use super::{should_spawn_backfill, BackfillRetries};
    use std::collections::HashSet;

    /// `of_backfill_hours <= 0.0` (the SP2-identical default-off floor) must never spawn, for
    /// ANY symbol, and must never even touch `bf_spawned` — `global-constraints.md`'s "default =
    /// SP2 behavior: `of_backfill_hours = 0.0` ⇒ no backfill thread spawned ⇒ byte-identical to
    /// SP2". The retry lane must not weaken that: the off-switch returns before either set is read.
    #[test]
    fn zero_or_negative_backfill_hours_never_spawns_and_never_touches_bf_spawned() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(!should_spawn_backfill(0.0, &mut bf_spawned, &mut retries, "BTCUSDT"));
        assert!(!should_spawn_backfill(-1.0, &mut bf_spawned, &mut retries, "ETHUSDT"));
        assert!(bf_spawned.is_empty(), "hours <= 0.0 must not record ANY symbol into bf_spawned");
        assert!(retries.is_empty(), "hours <= 0.0 must not record ANY symbol into the retry lane");
    }

    /// NaN/±inf must also hit the off-switch — a bare `<= 0.0` alone lets NaN slip through
    /// (every `<=` comparison against NaN is false), which would have wrongly spawned a
    /// backfill thread for a corrupt/hand-edited `workspace.json`'s `of_backfill_hours`. Covers
    /// the SP3-review NaN-guard follow-up.
    #[test]
    fn non_finite_backfill_hours_never_spawns_and_never_touches_bf_spawned() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(!should_spawn_backfill(f64::NAN, &mut bf_spawned, &mut retries, "BTCUSDT"));
        assert!(!should_spawn_backfill(f64::INFINITY, &mut bf_spawned, &mut retries, "ETHUSDT"));
        assert!(!should_spawn_backfill(
            f64::NEG_INFINITY,
            &mut bf_spawned,
            &mut retries,
            "SOLUSDT"
        ));
        assert!(
            bf_spawned.is_empty(),
            "non-finite hours must not record ANY symbol into bf_spawned"
        );
        assert!(
            retries.is_empty(),
            "non-finite hours must not record ANY symbol into the retry lane"
        );
    }

    /// A positive `of_backfill_hours` spawns exactly once per symbol (the run-once gate): the
    /// first call for a symbol returns `true` and records it; every later call for that SAME
    /// symbol returns `false` (re-enabling orderflow on an already-spawned symbol does not
    /// refetch); a DIFFERENT symbol is still independent. Unchanged by the retry lane, which is
    /// only ever consulted once `bf_spawned` has already said no AND a worker has reported a
    /// failure — neither of which happens here.
    #[test]
    fn positive_backfill_hours_spawns_exactly_once_per_symbol() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, "BTCUSDT"));
        assert!(bf_spawned.contains("BTCUSDT"));
        assert!(
            !should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, "BTCUSDT"),
            "a second call for the same symbol must not re-spawn"
        );
        assert!(
            should_spawn_backfill(2.0, &mut bf_spawned, &mut retries, "ETHUSDT"),
            "a different symbol must still spawn independently"
        );
        assert_eq!(bf_spawned.len(), 2);
        assert!(retries.is_empty(), "a healthy run-once spawn records nothing in the retry lane");
    }
}

/// The wiring #952 made possible: what an aggTrades backfill's reported stop reason actually
/// DOES. Same standalone-pure-helper shape as [`backfill_gate_tests`] above (no `App`, no worker
/// thread, no network): a report is a plain value, so the whole state machine is exercised by
/// handing [`BackfillRetries`] the reports a real worker would have sent.
///
/// **No test sleeps.** The cooldown floor is one second and these run in microseconds, so "not
/// re-walked yet" is proven directly, and "re-walked now" through
/// [`BackfillRetries::expire_all`] — the device [`FeedRetries`]' own tests already use.
#[cfg(test)]
mod backfill_retry_tests {
    use super::{
        should_spawn_backfill, BackfillReport, BackfillRetries, BACKFILL_MAX_RETRIES, RETRY_MAX,
    };
    use std::collections::HashSet;

    const SYM: &str = "BTCUSDT";
    const HOURS: f64 = 2.0;

    /// One frame's worth of the real gate.
    fn gate(bf_spawned: &mut HashSet<String>, retries: &mut BackfillRetries) -> bool {
        should_spawn_backfill(HOURS, bf_spawned, retries, SYM)
    }

    /// Drive the first spawn, then feed back whatever the worker reported.
    fn first_walk_then(report: BackfillReport) -> (HashSet<String>, BackfillRetries) {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(gate(&mut bf_spawned, &mut retries), "the first walk always spawns");
        retries.note_report(&report);
        (bf_spawned, retries)
    }

    /// **The reported bug.** A walk that stopped on a REST error having delivered nothing is
    /// re-walked: the run-once guard alone said no forever, and #952 made the difference
    /// detectable without anything acting on it. The re-walk is not immediate — it waits out the
    /// cooldown, which is what keeps a failing endpoint from being re-asked every frame.
    #[test]
    fn an_errored_walk_that_delivered_nothing_is_re_walked_after_its_cooldown() {
        let (mut bf_spawned, mut retries) =
            first_walk_then(BackfillReport::failed(SYM, 0, "429 Too Many Requests"));
        assert!(retries.is_pending(SYM), "the failure is recorded, so the loss is visible");
        assert_eq!(retries.attempts(SYM), 1);

        assert!(
            !gate(&mut bf_spawned, &mut retries),
            "a frame inside the cooldown must NOT re-walk — that is the per-frame abuse guard"
        );

        retries.expire_all();
        assert!(gate(&mut bf_spawned, &mut retries), "once the cooldown elapses, the walk re-runs");
        assert!(
            bf_spawned.contains(SYM),
            "the run-once guard itself is never cleared — the retry record is what reopens it"
        );
    }

    /// A claimed re-walk is IN FLIGHT: the per-frame call site must not launch a second 300-page
    /// walk on the very next frame (or on any of the thousands of frames the walk can take).
    /// Nothing reopens the gate again until that walk reports.
    #[test]
    fn a_claimed_re_walk_is_not_claimed_again_while_it_is_still_running() {
        let (mut bf_spawned, mut retries) = first_walk_then(BackfillReport::failed(SYM, 0, "boom"));
        retries.expire_all();
        assert!(gate(&mut bf_spawned, &mut retries), "the re-walk is claimed once");

        for _ in 0..1_000 {
            retries.expire_all(); // even a fully elapsed clock may not claim a second walk
            assert!(
                !gate(&mut bf_spawned, &mut retries),
                "an in-flight walk must never be re-claimed"
            );
        }
        assert_eq!(retries.attempts(SYM), 1, "an in-flight claim is not itself a failure");
    }

    /// **A `Capped` truncation is NOT retryable** (#952's own rule: the venue served every page it
    /// was asked for, so an identical re-run caps at the same page). It must leave the guard shut,
    /// exactly like a clean walk — the cure there is a bigger `max_pages`, not more requests.
    #[test]
    fn a_capped_walk_is_not_re_walked_and_records_nothing() {
        let (mut bf_spawned, mut retries) = first_walk_then(BackfillReport::finished(SYM, 300));
        assert!(retries.is_empty(), "a capped walk is complete-as-asked, not a failure to track");
        for _ in 0..10 {
            retries.expire_all();
            assert!(!gate(&mut bf_spawned, &mut retries), "a cap must never reopen the guard");
        }
    }

    /// A walk that simply succeeded is the same story with no truncation at all: one spawn, ever.
    #[test]
    fn a_successful_walk_is_not_re_walked() {
        let (mut bf_spawned, mut retries) = first_walk_then(BackfillReport::finished(SYM, 12));
        assert!(retries.is_empty());
        for _ in 0..10 {
            retries.expire_all();
            assert!(!gate(&mut bf_spawned, &mut retries), "success must never re-run the walk");
        }
    }

    /// **The safety rule.** A walk that delivered pages and THEN errored is not re-walked, however
    /// retryable its stop reason: those pages are already folded into an `OrderflowAgg`, which has
    /// no per-trade dedup, and this seam carries no aggTrade id to resume below them — so a second
    /// walk would re-deliver and double-count them. The loss is recorded and warned, not retried.
    #[test]
    fn an_errored_walk_that_already_delivered_pages_is_never_re_walked() {
        let (mut bf_spawned, mut retries) =
            first_walk_then(BackfillReport::failed(SYM, 7, "connection reset"));
        assert!(retries.is_abandoned(SYM), "the chain ends, visibly");
        for _ in 0..10 {
            retries.expire_all();
            assert!(
                !gate(&mut bf_spawned, &mut retries),
                "re-walking a partially delivered walk would double-count its pages"
            );
        }
    }

    /// The invariant the rule above buys, stated directly: across a whole retry chain, **at most
    /// one walk ever delivers ticks**. Every re-walk is claimed only after a report with
    /// `pages == 0`, and the first report that carries pages ends the chain either way (success
    /// clears it, failure abandons it).
    #[test]
    fn at_most_one_walk_in_a_chain_ever_delivers_ticks() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        let mut delivered = 0u32;
        let mut walks = 0u32;

        // Four walks that deliver nothing, then one that delivers and succeeds.
        let script = [0u32, 0, 0, 0, 9];
        for (i, pages) in script.iter().enumerate() {
            retries.expire_all();
            assert!(gate(&mut bf_spawned, &mut retries), "walk {i} must be claimable");
            walks += 1;
            delivered += *pages;
            if *pages == 0 {
                retries.note_report(&BackfillReport::failed(SYM, 0, "timeout"));
            } else {
                retries.note_report(&BackfillReport::finished(SYM, *pages));
            }
        }
        assert_eq!(walks, 5);
        assert_eq!(delivered, 9, "exactly one walk in the chain delivered anything");
        assert!(retries.is_empty(), "the successful walk closed the chain");
        retries.expire_all();
        assert!(!gate(&mut bf_spawned, &mut retries), "and nothing re-opens it afterwards");
    }

    /// **The bound.** A permanently broken endpoint gets `BACKFILL_MAX_RETRIES` re-walks and then
    /// stops for the session — it does not spin forever. What stops it is the attempt count; what
    /// is logged is one `warn!` naming the count and the last error (the `Abandoned` state below is
    /// that line's observable twin).
    #[test]
    fn a_permanently_failing_walk_stops_after_the_bound() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(gate(&mut bf_spawned, &mut retries), "the first walk");
        retries.note_report(&BackfillReport::failed(SYM, 0, "500 Internal Server Error"));

        let mut re_walks = 0u32;
        for _ in 0..(BACKFILL_MAX_RETRIES + 5) {
            retries.expire_all();
            if !gate(&mut bf_spawned, &mut retries) {
                break;
            }
            re_walks += 1;
            retries.note_report(&BackfillReport::failed(SYM, 0, "500 Internal Server Error"));
        }
        assert_eq!(re_walks, BACKFILL_MAX_RETRIES, "exactly the bounded number of re-walks");
        assert!(retries.is_abandoned(SYM), "and then the chain is over, visibly");

        for _ in 0..10 {
            retries.expire_all();
            assert!(!gate(&mut bf_spawned, &mut retries), "an abandoned chain never re-opens");
        }
    }

    /// The bound is the ladder's own strictly-increasing run: the last scheduled re-walk is the
    /// last one shorter than the ceiling, which is exactly where [`FeedRetries`]' unbounded lane
    /// would flatten into a fixed-rate poller. Pins the constant to that argument rather than to a
    /// number someone picked.
    #[test]
    fn the_bound_is_where_the_shared_ladder_stops_growing() {
        assert!(
            super::retry_backoff(BACKFILL_MAX_RETRIES) < RETRY_MAX,
            "the last re-walk must still be on the growing part of the ladder"
        );
        assert_eq!(
            super::retry_backoff(BACKFILL_MAX_RETRIES + 1),
            RETRY_MAX,
            "and the first attempt past the bound is the one the ceiling would flatten"
        );
    }

    /// A blip that heals: three failures, then the fourth walk completes. The record clears
    /// entirely (that is the one `info!` recovery line), so a LATER unrelated failure on the same
    /// symbol starts a fresh ladder rather than inheriting a stale, inflated attempt count.
    #[test]
    fn a_recovery_clears_the_record_so_a_later_failure_starts_a_fresh_ladder() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(gate(&mut bf_spawned, &mut retries));
        for _ in 0..3 {
            retries.note_report(&BackfillReport::failed(SYM, 0, "socket"));
            retries.expire_all();
            assert!(gate(&mut bf_spawned, &mut retries));
        }
        assert_eq!(retries.attempts(SYM), 3);

        retries.note_report(&BackfillReport::finished(SYM, 4));
        assert!(retries.is_empty(), "a completed walk forgets the whole chain");
        assert_eq!(retries.attempts(SYM), 0);
    }

    /// Two symbols fail independently: one symbol's exhausted chain must not close the other's,
    /// and one symbol's recovery must not clear the other's record.
    #[test]
    fn symbols_are_independent() {
        let mut retries = BackfillRetries::default();
        retries.note_report(&BackfillReport::failed("BTCUSDT", 0, "a"));
        retries.note_report(&BackfillReport::failed("ETHUSDT", 3, "b"));
        assert_eq!(retries.len(), 2);
        assert!(!retries.is_abandoned("BTCUSDT"));
        assert!(retries.is_abandoned("ETHUSDT"));

        retries.note_report(&BackfillReport::finished("BTCUSDT", 1));
        assert!(!retries.is_pending("BTCUSDT"));
        assert!(retries.is_pending("ETHUSDT"), "one symbol's recovery is not another's");
    }

    /// A report for a symbol the gate never recorded — `vike-app`'s failed
    /// `thread::Builder::spawn` path, which reports a zero-page failure so the symbol it just
    /// burned in `bf_spawned` is not lost for the session. It must open a normal ladder.
    #[test]
    fn a_failure_reported_for_an_unrecorded_symbol_opens_a_normal_ladder() {
        let mut bf_spawned: HashSet<String> = HashSet::new();
        let mut retries = BackfillRetries::default();
        assert!(gate(&mut bf_spawned, &mut retries), "the first walk records bf_spawned");
        retries.note_report(&BackfillReport::failed(SYM, 0, "thread spawn failed: too many open"));
        assert_eq!(retries.attempts(SYM), 1);
        retries.expire_all();
        assert!(gate(&mut bf_spawned, &mut retries), "so the spawn is retried, not lost");
    }
}

/// C2 tidy FIX 3: unit tests for the pure [`orphaned_feed_keys`] diff — see its doc for why
/// this is tested standalone rather than through a full `App` (same rationale as
/// `backfill_gate_tests` above; `WinState` itself needs no `eframe::CreationContext`, just
/// plain `egui::Rect` data, same as `workspace`'s own `WinState` tests).
#[cfg(test)]
mod orphaned_feed_tests {
    use super::{orphaned_feed_keys, workspace};
    use std::collections::HashSet;

    fn chart_win(symbol: &str, interval: &str) -> workspace::WinState {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        workspace::WinState::new("t", symbol, interval, workspace::WinKind::Chart, r)
    }

    fn spawned(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_inputs_reap_nothing() {
        assert!(orphaned_feed_keys(&[], &HashSet::new()).is_empty());
        let wins = vec![chart_win("BTCUSDT", "1m")];
        assert!(orphaned_feed_keys(&wins, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_primary_symbol_with_no_window_left_is_orphaned() {
        let wins = vec![chart_win("BTCUSDT", "1m")];
        let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m"]);
        assert_eq!(orphaned_feed_keys(&wins, &sp), vec!["ETHUSDT@1m".to_string()]);
    }

    #[test]
    fn a_compare_symbol_still_referenced_by_another_window_is_not_reaped() {
        let mut a = chart_win("BTCUSDT", "1m");
        a.compare.push("ETHUSDT".to_string());
        let b = chart_win("ETHUSDT", "1m"); // ETHUSDT is ALSO window b's own primary
        let wins = vec![a, b];
        let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m"]);
        assert!(orphaned_feed_keys(&wins, &sp).is_empty());
    }

    /// The scenario the fix targets: a Compare symbol backing a live feed loses its only
    /// referencing window (chip removed, or FIX 1's primary-switch cascade) — mutating the
    /// SAME `WinState` in place between assertions (it isn't `Clone`) stands in for "before"
    /// and "after" a `remove_compare` call.
    #[test]
    fn removing_the_only_reference_orphans_the_compare_feed() {
        let mut a = chart_win("BTCUSDT", "1m");
        a.compare.push("ETHUSDT".to_string());
        let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m"]);
        assert!(
            orphaned_feed_keys(std::slice::from_ref(&a), &sp).is_empty(),
            "still referenced by a's compare list"
        );

        a.compare.clear(); // simulates WinState::remove_compare
        assert_eq!(
            orphaned_feed_keys(std::slice::from_ref(&a), &sp),
            vec!["ETHUSDT@1m".to_string()],
            "no window references ETHUSDT@1m anymore"
        );
        // the window's OWN primary feed is never reaped by this
        assert!(
            !orphaned_feed_keys(std::slice::from_ref(&a), &sp).contains(&"BTCUSDT@1m".to_string())
        );
    }

    /// A foreign-source study (TradingView "symbol" input) keeps its source symbol's feed alive
    /// exactly like a Compare overlay does — the feed is subscribed per-frame for the study, so it
    /// must not be reaped while the study exists; clearing the source (or removing the study) drops
    /// the reference and lets the feed be reaped normally. Cross-venue: the key is `series_key`d, so
    /// a non-Binance source is namespaced (never collides with a same-symbol Binance feed).
    #[test]
    fn a_foreign_source_study_symbol_is_not_reaped_and_is_venue_keyed() {
        use vike_chart::indicators::SourceSymbol;
        let mut a = chart_win("BTCUSDT", "1m");
        a.add_indicator("rsi", &[]); // oscillator on this chart
        a.indicators[0].source_symbol =
            Some(SourceSymbol { venue: "bybit".into(), symbol: "ETHUSDT".into() });
        // the bybit-namespaced source feed AND the window's own primary are both live.
        let sp = spawned(&["BTCUSDT@1m", "bybit:ETHUSDT@1m"]);
        assert!(
            orphaned_feed_keys(std::slice::from_ref(&a), &sp).is_empty(),
            "a study's foreign-source feed must count as a live consumer"
        );
        // clearing the source orphans exactly that feed (the primary is untouched).
        a.indicators[0].source_symbol = None;
        assert_eq!(
            orphaned_feed_keys(std::slice::from_ref(&a), &sp),
            vec!["bybit:ETHUSDT@1m".to_string()],
            "clearing the source releases only the source feed"
        );
    }

    /// A closed (`open=false`, "hidden off-desktop, rail can unhide") or minimized window still
    /// counts as live — mirrors how nothing in `main.rs` ever tears down a closed window's OWN
    /// primary feed either; a compare feed must not be treated more aggressively.
    #[test]
    fn a_closed_or_minimized_window_still_counts_as_live() {
        let mut w = chart_win("BTCUSDT", "1m");
        let sp = spawned(&["BTCUSDT@1m"]);

        w.open = false;
        assert!(orphaned_feed_keys(std::slice::from_ref(&w), &sp).is_empty(), "closed ≠ gone");

        w.open = true;
        w.minimized = true;
        assert!(orphaned_feed_keys(std::slice::from_ref(&w), &sp).is_empty(), "minimized ≠ gone");
    }

    /// The shared raw-trade-tape key (`"SYM@trades"`, feeding Tick/Volume `aggs` AND SP2
    /// orderflow `of_aggs`, per-SYMBOL not per-window) must NEVER be reaped by this diff — it
    /// isn't part of the per-window live-set formula at all, so a naive diff would treat every
    /// one as orphaned on every call. This is the precise trap the fix's doc calls out.
    #[test]
    fn trade_tape_keys_are_never_reaped() {
        let wins = vec![chart_win("BTCUSDT", "1m")];
        let sp = spawned(&["BTCUSDT@1m", "BTCUSDT@trades", "ETHUSDT@trades"]);
        assert_eq!(orphaned_feed_keys(&wins, &sp), Vec::<String>::new());
    }

    /// Tool windows OTHER than DOM (Trade/Options/News/Calendar/Data) never contribute to the
    /// live set — and must not interfere with reaping a real orphaned chart key either. (DOM is
    /// the one exception — see the tests below; it drives a real `ensure_feed`/`spawned` entry
    /// for its own symbol.)
    #[test]
    fn non_dom_tool_windows_never_contribute_to_the_live_set() {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let trade = workspace::WinState::tool("t", workspace::WinKind::Trade, r);
        let wins = vec![trade];
        let sp = spawned(&["BTCUSDT@1m"]);
        assert_eq!(orphaned_feed_keys(&wins, &sp), vec!["BTCUSDT@1m".to_string()]);
    }

    fn dom_win(symbol: &str) -> workspace::WinState {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let mut w = workspace::WinState::tool("d", workspace::WinKind::Dom, r);
        w.symbol = symbol.to_string(); // mirrors `ws.symbol = "BTCUSDT".to_string()` at DOM open
        w
    }

    /// DOM review finding: a DOM window's OWN fixed `"1m"` bar feed (ensured once, at window
    /// creation, NOT re-asserted per frame the way a Chart window's compare list is) must be
    /// protected — this is the scenario that would otherwise regress a DOM's mark/PAPER-fill
    /// feed the very next frame after opening, with no path to recover it (`ensure_depth`'s own
    /// `dom_depth` idempotency gate never notices the underlying bar subscription died).
    #[test]
    fn a_dom_windows_own_1m_feed_is_protected_even_with_no_matching_chart() {
        let wins = vec![dom_win("BTCUSDT")];
        let sp = spawned(&["BTCUSDT@1m"]);
        assert!(orphaned_feed_keys(&wins, &sp).is_empty(), "DOM's own feed must survive");
    }

    /// A DOM window only protects its OWN symbol at the fixed `"1m"` — a DIFFERENT symbol's
    /// Kline feed (no Chart or DOM referencing it) is still correctly reaped alongside it, and
    /// DOM never protects a non-`"1m"` interval on its own symbol either (DOM has no interval
    /// of its own to widen the match with — `WinState::interval` stays empty for it).
    #[test]
    fn a_dom_window_does_not_protect_unrelated_keys() {
        let wins = vec![dom_win("BTCUSDT")];
        let sp = spawned(&["BTCUSDT@1m", "ETHUSDT@1m", "BTCUSDT@5m"]);
        let mut orphaned = orphaned_feed_keys(&wins, &sp);
        orphaned.sort();
        assert_eq!(orphaned, vec!["BTCUSDT@5m".to_string(), "ETHUSDT@1m".to_string()]);
    }
}

/// Unit tests for the pure [`orphaned_trade_feed_keys`] diff — the trade-feed half of the feed-leak
/// fix. Standalone for the same reason the modules above are (no `App`/`eframe::CreationContext`
/// needed): the helper is pure over plain `HashSet`s. Mirrors `orphaned_feed_tests`' `spawned`
/// helper style.
#[cfg(test)]
mod orphaned_trade_feed_tests {
    use super::orphaned_trade_feed_keys;
    use std::collections::HashSet;

    const DEFAULT_VENUE: &str = "binance";

    fn spawned(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    fn needed(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
        pairs.iter().map(|(v, s)| (v.to_string(), s.to_string())).collect()
    }

    /// The core rule: a trade feed whose `(venue, symbol)` is NOT needed by any remaining
    /// aggregator is orphaned and returned; one that IS needed is kept.
    #[test]
    fn a_trade_feed_no_aggregator_needs_is_orphaned_and_one_still_needed_is_kept() {
        let sp = spawned(&["BTCUSDT@trades", "ETHUSDT@trades"]);
        // Only BTCUSDT still has an aggregator; ETHUSDT's was the last one removed.
        let need = needed(&[(DEFAULT_VENUE, "BTCUSDT")]);
        assert_eq!(
            orphaned_trade_feed_keys(&need, &sp, DEFAULT_VENUE),
            vec!["ETHUSDT@trades".to_string()]
        );
    }

    /// Shared-consumer case (the invariant that guards a live feed): two aggregators on the SAME
    /// `(venue, symbol)` (e.g. a Tick chart AND a Volume chart, or a tick/vol `aggs` entry AND an
    /// orderflow `of_aggs` entry) both key the one trade feed. Removing ONE leaves the pair still
    /// in `needed`, so the shared feed is NOT torn down while the other aggregator lives.
    #[test]
    fn a_trade_feed_still_shared_by_another_aggregator_is_not_reaped() {
        let sp = spawned(&["BTCUSDT@trades"]);
        // One consumer removed, but another aggregator on the same (venue,symbol) remains.
        let need = needed(&[(DEFAULT_VENUE, "BTCUSDT")]);
        assert!(
            orphaned_trade_feed_keys(&need, &sp, DEFAULT_VENUE).is_empty(),
            "a trade feed still needed by any aggregator on its (venue,symbol) must survive"
        );
    }

    /// When NOTHING needs a feed anymore (its window/aggregator gone), it is reaped — the leak the
    /// fix closes: an empty `needed` orphans every spawned trade feed.
    #[test]
    fn no_remaining_aggregators_reaps_every_trade_feed() {
        let sp = spawned(&["BTCUSDT@trades", "okx:BTC-USDT@trades"]);
        let mut got = orphaned_trade_feed_keys(&HashSet::new(), &sp, DEFAULT_VENUE);
        got.sort();
        assert_eq!(got, vec!["BTCUSDT@trades".to_string(), "okx:BTC-USDT@trades".to_string()]);
    }

    /// Key parsing matches `ensure_trade_feed_on`'s convention exactly: a bare `"SYMBOL@trades"`
    /// is the DEFAULT_VENUE (Binance); a `"venue:SYMBOL@trades"` is namespaced. A dashed symbol
    /// (OKX `BTC-USDT`) round-trips because only the FIRST `:` is a venue separator.
    #[test]
    fn keys_parse_to_the_right_venue_and_symbol() {
        // Binance bare key: needed as (binance, BTCUSDT) → kept; needed under the WRONG venue → reaped.
        let sp = spawned(&["BTCUSDT@trades"]);
        assert!(
            orphaned_trade_feed_keys(&needed(&[("binance", "BTCUSDT")]), &sp, DEFAULT_VENUE)
                .is_empty(),
            "bare key parses to (binance, BTCUSDT)"
        );
        assert_eq!(
            orphaned_trade_feed_keys(&needed(&[("okx", "BTCUSDT")]), &sp, DEFAULT_VENUE),
            vec!["BTCUSDT@trades".to_string()],
            "a bare key is binance, so an okx need does not keep it alive"
        );

        // OKX namespaced key with a dashed symbol: kept only under (okx, BTC-USDT).
        let sp = spawned(&["okx:BTC-USDT@trades"]);
        assert!(
            orphaned_trade_feed_keys(&needed(&[("okx", "BTC-USDT")]), &sp, DEFAULT_VENUE)
                .is_empty(),
            "namespaced key parses to (okx, BTC-USDT), venue split on the first ':' only"
        );
        assert_eq!(
            orphaned_trade_feed_keys(&needed(&[(DEFAULT_VENUE, "BTC-USDT")]), &sp, DEFAULT_VENUE),
            vec!["okx:BTC-USDT@trades".to_string()],
            "an okx feed is not kept alive by a binance need for the same symbol"
        );
    }

    /// A kline key (no `@trades` suffix) is NEVER returned by the trade-feed reaper — those are
    /// [`super::orphaned_feed_keys`]'s domain. Even with an empty `needed`, kline keys are ignored.
    #[test]
    fn kline_keys_are_never_returned() {
        let sp = spawned(&["BTCUSDT@1m", "okx:BTC-USDT@5m", "ETHUSDT@trades"]);
        // Empty needed = reap everything reapable; only the @trades key qualifies.
        assert_eq!(
            orphaned_trade_feed_keys(&HashSet::new(), &sp, DEFAULT_VENUE),
            vec!["ETHUSDT@trades".to_string()]
        );
    }
}

/// Unit tests for [`backfill_earliest_ts`] — the paging-floor arithmetic lifted out of
/// `App::maybe_spawn_backfill`. Standalone for the same reason [`backfill_gate_tests`] is: the
/// helper is pure, and the method it came from names a `fat`-gated `vike_binance` entry point.
#[cfg(test)]
mod backfill_bound_tests {
    use super::backfill_earliest_ts;

    const HOUR_MS: i64 = 3_600_000;
    const NOW: i64 = 1_700_000_000_000;

    /// No bar has synced for this chart key yet: the bound is the hours-only floor.
    #[test]
    fn no_loaded_bars_falls_back_to_the_hours_floor() {
        assert_eq!(backfill_earliest_ts(NOW, 2.0, None), NOW - 2 * HOUR_MS);
    }

    /// `.max()`, NOT `.min()`: whichever bound is closer to "now" wins. A chart whose oldest
    /// loaded bar is NEWER than the hours floor stops at that bar (nothing older has bars to
    /// attach footprints to); one whose oldest bar is OLDER than the floor stops at the floor
    /// (the hard cap against paging back to the venue's listing date).
    #[test]
    fn the_newer_of_the_two_bounds_wins() {
        let floor = NOW - 6 * HOUR_MS;
        // oldest bar NEWER than the floor -> the bar wins
        assert_eq!(backfill_earliest_ts(NOW, 6.0, Some(NOW - HOUR_MS)), NOW - HOUR_MS);
        // oldest bar OLDER than the floor -> the floor wins
        assert_eq!(backfill_earliest_ts(NOW, 6.0, Some(NOW - 99 * HOUR_MS)), floor);
    }

    /// `of_backfill_hours` is config-controlled (a hand-edited `workspace.json` is not
    /// range-checked), so an absurd value must never panic or wrap. TWO independent guards, and
    /// they clamp at different places — this pins both.
    ///
    /// 1. The **cast**: `f64::MAX * 3.6e6` is `inf`, and `1e300 * 3.6e6` is merely enormous;
    ///    `as i64` is a saturating cast, so both become `i64::MAX` milliseconds of lookback. The
    ///    subtraction `NOW - i64::MAX` is then still perfectly representable (`NOW` is ~1.7e12,
    ///    nowhere near the ~9.2e18 range), so the answer is that finite, absurdly-old timestamp —
    ///    NOT `i64::MIN`. Without the saturating cast this would be UB-adjacent nonsense instead.
    /// 2. The **subtraction**: `saturating_sub` is what stops a debug-build panic / release-build
    ///    wraparound when `now_ms` itself is pathological. Proven at `i64::MIN`, the only input
    ///    that actually makes the subtraction overflow.
    #[test]
    fn an_absurd_hours_value_saturates_instead_of_overflowing() {
        // (1) the cast saturates; the subtraction does not need to.
        assert_eq!(backfill_earliest_ts(NOW, f64::MAX, None), NOW - i64::MAX);
        assert_eq!(backfill_earliest_ts(NOW, 1e300, None), NOW - i64::MAX);
        // (2) the subtraction saturates when it genuinely would overflow.
        assert_eq!(backfill_earliest_ts(i64::MIN, f64::MAX, None), i64::MIN);
        // …and a loaded bar still clamps the absurd floor back up to something usable.
        assert_eq!(backfill_earliest_ts(NOW, f64::MAX, Some(NOW - HOUR_MS)), NOW - HOUR_MS);
    }
}

/// Tests for the IMPERATIVE half — [`ensure_feed_on`], [`ensure_trade_feed_on`], [`ensure_depth`],
/// [`ensure_poly_book`] and [`reap_orphaned_feeds`]. None of these ever ran in any gate while they
/// were `App` methods in `vike-app`'s `main.rs`; they are exercised here against a recording
/// `FakeFeed` that implements the real [`DataClient`] trait, so a change to *which* venue a
/// subscribe/unsubscribe is routed to, or to the order the `spawned`/`subs`/`charts` slots are
/// freed in, fails the merge gate.
///
/// The priority scenario is **teardown → re-add**
/// (`teardown_then_re_add_genuinely_restarts_the_feed`): the reaper must free every slot a later
/// `ensure_*` checks, or a re-added chart comes back permanently dead — the same class of silent
/// loss as bug 4 in [`core_sync`](crate::core_sync)'s module doc.
#[cfg(test)]
mod feed_slot_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Shared call log: one `"venue:verb arg…"` line per `DataClient` call, in order.
    type Log = Arc<Mutex<Vec<String>>>;

    /// The verbs a [`FakeFeed`] refuses, held behind a shared handle so a test can HEAL the venue
    /// mid-run and prove a retry genuinely reaches a working socket (the property the whole
    /// [`FeedRetries`] change exists for).
    ///
    /// Two spellings, because [`FeedRetries`] treats them differently and a double that could only
    /// produce one of them could not test the other: a bare verb (`"bars"`) answers the RETRYABLE
    /// [`LiveDataError::Subscribe`] — the "OS refused a feed thread / socket is down" kind — while
    /// a trailing `!` (`"bars!"`) answers the permanent, declared-capability
    /// [`LiveDataError::Unsupported`] that `vike_data::require_live_verb` produces from the static
    /// `VenueCaps` matrix.
    type FailSet = Arc<Mutex<HashSet<String>>>;

    fn fail_set(verbs: &[&str]) -> FailSet {
        Arc::new(Mutex::new(verbs.iter().map(|v| v.to_string()).collect()))
    }

    /// A recording [`DataClient`] double. Hands out ids from a per-venue `base` so a logged
    /// `unsub` line names which venue's subscription was actually stopped, and can be told to
    /// fail a named verb (`fail`) to exercise the error arms.
    struct FakeFeed {
        venue: &'static str,
        log: Log,
        next: u64,
        fail: FailSet,
    }

    impl FakeFeed {
        fn boxed(
            venue: &'static str,
            log: &Log,
            base: u64,
            fail: &[&str],
        ) -> Box<dyn DataClient + Send> {
            Box::new(FakeFeed { venue, log: Arc::clone(log), next: base, fail: fail_set(fail) })
        }

        /// Same double, but sharing a [`FailSet`] the test keeps a handle to — so the venue can be
        /// healed between two `ensure_*` calls.
        fn boxed_flaky(
            venue: &'static str,
            log: &Log,
            base: u64,
            fail: &FailSet,
        ) -> Box<dyn DataClient + Send> {
            Box::new(FakeFeed { venue, log: Arc::clone(log), next: base, fail: Arc::clone(fail) })
        }

        fn issue(&mut self, verb: &str, args: &str) -> Result<SubscriptionId, LiveDataError> {
            self.log.lock().unwrap().push(format!("{}:{verb} {args}", self.venue));
            let (refused, failing) = {
                let f = self.fail.lock().unwrap();
                (f.contains(&format!("{verb}!")), f.contains(verb))
            };
            if refused {
                return Err(LiveDataError::Unsupported("fake feed: verb declared unsupported"));
            }
            if failing {
                return Err(LiveDataError::Subscribe(format!(
                    "fake feed: {verb} disabled for this test"
                )));
            }
            self.next += 1;
            Ok(SubscriptionId(self.next))
        }
    }

    impl DataClient for FakeFeed {
        fn subscribe_bars(
            &mut self,
            symbol: &str,
            interval: &str,
        ) -> Result<SubscriptionId, LiveDataError> {
            self.issue("bars", &format!("{symbol} {interval}"))
        }
        fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue("quotes", symbol)
        }
        fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue("trades", symbol)
        }
        fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue("book", symbol)
        }
        fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue("depth", symbol)
        }
        fn unsubscribe(&mut self, id: SubscriptionId) {
            self.log.lock().unwrap().push(format!("{}:unsub {}", self.venue, id.0));
        }
        fn shutdown(&mut self) {
            self.log.lock().unwrap().push(format!("{}:shutdown", self.venue));
        }
    }

    /// The `App` fields these functions borrow, owned in one place so a test can build both
    /// bundles from disjoint borrows (which is exactly how `vike-app`'s thin wrappers do it).
    struct Env {
        feeds: FeedMap,
        subs: HashMap<String, SubscriptionId>,
        spawned: HashSet<String>,
        unroutable: HashSet<String>,
        retries: FeedRetries,
        charts: HashMap<String, model::ChartState>,
        hidden: HashSet<String>,
        aggs: HashMap<String, (String, String, tickvol::TickVolAgg)>,
        of_aggs: HashMap<String, (String, String, orderflow::OrderflowAgg)>,
        dom_depth: HashMap<(String, String), DomDepthSubs>,
        poly_subs: HashMap<String, PolyBookSubs>,
        log: Log,
    }

    impl Env {
        /// Binance (ids 100+) and OKX (ids 200+) feeds registered; nothing subscribed yet.
        fn new() -> Env {
            Env::with_feeds(&[("binance", 100, &[]), ("okx", 200, &[])])
        }

        fn with_feeds(venues: &[(&'static str, u64, &'static [&'static str])]) -> Env {
            let log: Log = Arc::new(Mutex::new(Vec::new()));
            let mut feeds: FeedMap = HashMap::new();
            for &(venue, base, fail) in venues {
                feeds.insert(venue, FakeFeed::boxed(venue, &log, base, fail));
            }
            Env {
                feeds,
                subs: HashMap::new(),
                spawned: HashSet::new(),
                unroutable: HashSet::new(),
                retries: FeedRetries::default(),
                charts: HashMap::new(),
                hidden: HashSet::new(),
                aggs: HashMap::new(),
                of_aggs: HashMap::new(),
                dom_depth: HashMap::new(),
                poly_subs: HashMap::new(),
                log,
            }
        }

        fn both(&mut self) -> (FeedSlots<'_>, SeriesSlots<'_>) {
            (
                FeedSlots {
                    feeds: &mut self.feeds,
                    subs: &mut self.subs,
                    spawned: &mut self.spawned,
                    unroutable: &mut self.unroutable,
                    retries: &mut self.retries,
                },
                SeriesSlots {
                    charts: &mut self.charts,
                    hidden: &mut self.hidden,
                    aggs: &mut self.aggs,
                    of_aggs: &mut self.of_aggs,
                },
            )
        }

        fn ensure(&mut self, venue: &str, symbol: &str, interval: &str) {
            self.ensure_at(venue, symbol, interval, DisplayTz::Utc, false);
        }

        fn ensure_at(
            &mut self,
            venue: &str,
            symbol: &str,
            interval: &str,
            tz: DisplayTz,
            shutting_down: bool,
        ) {
            let (mut f, mut s) = self.both();
            let spec = SeriesSpec { venue, symbol, interval, asset_class: None };
            ensure_feed_on(&mut f, &mut s, spec, tz, shutting_down);
        }

        fn reap(&mut self, wins: &[workspace::WinState]) {
            let (mut f, mut s) = self.both();
            reap_orphaned_feeds(&mut f, &mut s, wins);
        }

        /// One Data-manager "Delete" on `key` — the same call `vike-app`'s `to_stop` loop makes,
        /// built from the same two bundles that loop builds.
        fn stop(&mut self, key: &str) {
            let (mut f, mut s) = self.both();
            stop_series(&mut f, &mut s, key);
        }

        fn depth(&mut self, venue: dom::DomVenue, canonical: &str) {
            ensure_depth(&mut self.feeds, &mut self.dom_depth, &mut self.retries, venue, canonical);
        }

        fn poly(&mut self, token: &str) {
            ensure_poly_book(&mut self.feeds, &mut self.poly_subs, &mut self.retries, token);
        }

        fn reap_streams(&mut self, wins: &[workspace::WinState]) {
            reap_orphaned_dom_cockpit_streams(
                &mut self.feeds,
                &mut self.dom_depth,
                &mut self.poly_subs,
                &mut self.retries,
                wins,
            );
        }

        fn calls(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        /// Simulate the backoff having elapsed, so a retry test never has to sleep for real
        /// seconds. See [`FeedRetries::expire_all`].
        fn elapse(&mut self) {
            self.retries.expire_all();
        }
    }

    fn chart_win(venue: &str, symbol: &str, interval: &str) -> workspace::WinState {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let mut w = workspace::WinState::new("t", symbol, interval, workspace::WinKind::Chart, r);
        w.venue = venue.to_string();
        w
    }

    fn dom_win(symbol: &str) -> workspace::WinState {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let mut w = workspace::WinState::tool("d", workspace::WinKind::Dom, r);
        w.symbol = symbol.to_string();
        w
    }

    fn poly_win(token: &str) -> workspace::WinState {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let mut w = workspace::WinState::tool("p", workspace::WinKind::Polymarket, r);
        w.symbol = token.to_string();
        w
    }

    // ----- ensure_feed_on ---------------------------------------------------------------------

    /// The happy path: ONE `subscribe_bars` on the named venue, the returned id remembered under
    /// the `series_key`, the key marked `spawned`, and a `ChartState` created with the passed
    /// display timezone applied.
    #[test]
    fn a_kline_feed_subscribes_once_and_fills_every_slot() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()]);
        assert_eq!(env.subs.get("BTCUSDT@1m"), Some(&SubscriptionId(101)));
        assert!(env.spawned.contains("BTCUSDT@1m"));
        assert_eq!(env.charts["BTCUSDT@1m"].tz(), DisplayTz::Utc);
        assert!(env.aggs.is_empty(), "a kline interval creates no client-side aggregator");
    }

    /// Idempotent per key (the `spawned.insert` gate): calling it every frame — which the window
    /// loop does — must never open a second socket. The chart's tz is still re-asserted, which is
    /// how a menu tz change propagates without a resubscribe.
    #[test]
    fn a_second_ensure_for_the_same_key_does_not_resubscribe() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure_at("binance", "BTCUSDT", "1m", DisplayTz::Local, false);
        assert_eq!(env.calls().len(), 1, "exactly one subscribe for a repeated ensure");
        assert_eq!(env.charts["BTCUSDT@1m"].tz(), DisplayTz::Local, "tz is still re-asserted");
    }

    /// A non-Binance venue is namespaced by `series_key` and routed to ITS OWN feed — the
    /// cross-exchange bug class this keying exists to prevent (a same-named Binance chart must
    /// never receive an OKX subscription, or vice versa).
    #[test]
    fn a_non_binance_venue_is_namespaced_and_routed_to_its_own_feed() {
        let mut env = Env::new();
        env.ensure("okx", "BTC-USDT", "1m");
        assert_eq!(env.calls(), vec!["okx:bars BTC-USDT 1m".to_string()]);
        assert_eq!(env.subs.get("okx:BTC-USDT@1m"), Some(&SubscriptionId(201)));
        assert!(env.charts.contains_key("okx:BTC-USDT@1m"));
        assert!(!env.charts.contains_key("BTC-USDT@1m"), "must not land in the Binance keyspace");
    }

    /// Teardown-safety: once `App::shutdown` is raised, no NEW live feed may start — a fresh
    /// subscription would open another blocking socket read the bounded `on_exit` teardown would
    /// then have to wait on (PR #589's >6s window-close hang). Nothing at all is touched.
    #[test]
    fn shutting_down_starts_no_new_feed_and_creates_no_chart() {
        let mut env = Env::new();
        env.ensure_at("binance", "BTCUSDT", "1m", DisplayTz::Utc, true);
        assert!(env.calls().is_empty());
        assert!(env.spawned.is_empty() && env.subs.is_empty() && env.charts.is_empty());
    }

    /// Re-adding a Data-manager-"deleted" series just unhides it (the `hidden.remove` at the top
    /// runs before every other decision, so it applies even on the idempotent repeat call).
    #[test]
    fn ensuring_a_hidden_key_unhides_it() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        env.hidden.insert("BTCUSDT@1m".to_string()); // Data manager "Delete"
        env.ensure("binance", "BTCUSDT", "1m");
        assert!(!env.hidden.contains("BTCUSDT@1m"));
    }

    /// A tick/volume interval has no venue kline feed: it subscribes the raw TRADE tape once per
    /// `(venue, symbol)` and builds a per-chart-key aggregator. A second tick/volume interval on
    /// the SAME symbol reuses the one trade feed and only adds an aggregator.
    #[test]
    fn a_tick_interval_subscribes_the_trade_tape_and_creates_an_aggregator() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "100t");
        assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()]);
        assert!(env.spawned.contains("BTCUSDT@trades"));
        assert_eq!(env.subs.get("BTCUSDT@trades"), Some(&SubscriptionId(101)));
        assert_eq!(env.aggs["BTCUSDT@100t"].0, "binance");
        assert_eq!(env.aggs["BTCUSDT@100t"].1, "BTCUSDT");
        assert!(env.charts.contains_key("BTCUSDT@100t"));

        env.ensure("binance", "BTCUSDT", "10v");
        assert_eq!(env.calls().len(), 1, "the trade tape is shared, not resubscribed");
        assert!(env.aggs.contains_key("BTCUSDT@10v"));
    }

    /// **The regression proof.** This test previously asserted the BUG (as
    /// `an_unregistered_venue_still_burns_the_spawned_slot`), whose closing assertion was literally
    /// `"the burned `spawned` slot suppresses the retry"`. Inverted here: the miss must be
    /// recorded, not suppressed, so a later ensure can still act on it.
    ///
    /// Note what is deliberately NOT inverted — the key IS still in `spawned`. That set doubles as
    /// `sync_from_core`'s fold filter, so narrowing it would break `--observe` mode (see
    /// [`ensure_feed_on`]'s doc, and `every_ensured_key_is_foldable_even_with_no_feeds_registered`
    /// below). `unroutable` is what carries the "never actually subscribed" fact.
    ///
    /// Reachable from the UI: the Symbol picker searches a twelve-venue catalog while only six live
    /// clients are registered, so this is what picking an Alpaca/OANDA/cTrader instrument does.
    #[test]
    fn an_unregistered_venue_does_not_permanently_suppress_the_subscribe() {
        let mut env = Env::new();
        env.ensure("bybit", "BTCUSDT", "1m");
        assert!(env.calls().is_empty(), "no feed to call");
        assert!(env.subs.is_empty(), "nothing was subscribed, so there is no id to remember");
        assert!(
            env.unroutable.contains("bybit:BTCUSDT@1m"),
            "the miss is RECORDED — this is what reopens the subscribe, and what makes it visible"
        );
        assert!(env.spawned.contains("bybit:BTCUSDT@1m"), "still wanted, so still foldable");
        // The chart is still created, so the window renders empty rather than not at all.
        assert!(env.charts.contains_key("bybit:BTCUSDT@1m"));
    }

    /// The recovery half, and the exact assertion the old pin made in the negative: registering the
    /// venue's feed afterwards and re-ensuring now genuinely subscribes. `unroutable` clears again,
    /// so it tracks CURRENT state rather than accumulating tombstones.
    #[test]
    fn registering_the_venue_later_lets_a_re_ensure_succeed() {
        let mut env = Env::new();
        env.ensure("bybit", "BTCUSDT", "1m");
        assert!(env.calls().is_empty());

        let log = Arc::clone(&env.log);
        env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
        env.ensure("bybit", "BTCUSDT", "1m");

        assert_eq!(env.calls(), vec!["bybit:bars BTCUSDT 1m".to_string()], "the retry really runs");
        assert_eq!(env.subs.get("bybit:BTCUSDT@1m"), Some(&SubscriptionId(301)));
        assert!(env.unroutable.is_empty(), "no longer unroutable, so the record clears");

        // …and it does not now resubscribe forever: the recovered key is a normal spawned key.
        env.ensure("bybit", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 1, "recovery is once, not once per frame");
    }

    /// The guard still guards: reopening the subscribe on a MISS must not weaken the once-per-key
    /// rule on the HIT. Every caller is per-frame, so a registered venue re-ensured many times must
    /// open exactly one socket — the property a careless "just move the insert" fix would break.
    #[test]
    fn a_registered_venue_still_subscribes_exactly_once_across_many_frames() {
        let mut env = Env::new();
        for _ in 0..5 {
            env.ensure("binance", "BTCUSDT", "1m");
        }
        assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()], "exactly one socket");
        assert_eq!(env.subs.len(), 1, "and exactly one id remembered");
        assert!(env.unroutable.is_empty(), "a routable key never enters the unroutable set");
    }

    /// **The `--observe` guard.** That mode builds an EMPTY `feeds` map and renders charts purely
    /// from the remote daemon's streamed bars, which `sync_from_core` folds only for keys present
    /// in `spawned`. So every ensured key must still land in `spawned` even when nothing can be
    /// subscribed — this is precisely what the "obvious" version of this fix (insert only after a
    /// successful lookup) would have broken, silently blanking every observed chart.
    #[test]
    fn every_ensured_key_is_foldable_even_with_no_feeds_registered() {
        let mut env = Env::with_feeds(&[]); // the --observe shape: no venue clients at all
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure("okx", "BTC-USDT", "5m");
        assert!(env.calls().is_empty(), "nothing to subscribe against");
        let mut got: Vec<String> = env.spawned.iter().cloned().collect();
        got.sort();
        assert_eq!(
            got,
            vec!["BTCUSDT@1m".to_string(), "okx:BTC-USDT@5m".to_string()],
            "sync_from_core's fold filter must still admit every wanted series"
        );
    }

    /// The unroutable record is per KEY, so a miss on one venue never suppresses the report for
    /// another — and repeating the miss (the per-frame call) neither re-warns nor grows the set.
    #[test]
    fn unroutable_is_keyed_per_series_and_repeats_do_not_accumulate() {
        let mut env = Env::new();
        for _ in 0..3 {
            env.ensure("bybit", "BTCUSDT", "1m");
        }
        env.ensure("alpaca", "AAPL", "1m");
        let mut got: Vec<String> = env.unroutable.iter().cloned().collect();
        got.sort();
        assert_eq!(
            got,
            vec!["alpaca:AAPL@1m".to_string(), "bybit:BTCUSDT@1m".to_string()],
            "one entry per key, no duplicates from the per-frame repeats"
        );
        assert!(env.calls().is_empty(), "and no venue was ever called");
    }

    /// The trade-tape twin: [`ensure_trade_feed_on`] had the same defect in a WORSE form — its
    /// missing-feed path had no `else` arm at all, so a tick/volume or orderflow chart on a
    /// feedless venue lost its tape in complete silence. Same cure, same recovery.
    #[test]
    fn an_unregistered_venue_does_not_permanently_suppress_the_trade_tape() {
        let mut env = Env::new();
        env.ensure("bybit", "BTCUSDT", "100t");
        assert!(env.calls().is_empty(), "no feed to call");
        assert!(
            env.unroutable.contains("bybit:BTCUSDT@trades"),
            "the miss is recorded, not silent"
        );
        assert!(env.aggs.contains_key("bybit:BTCUSDT@100t"), "the aggregator is still created");

        // Register the venue: the tape now genuinely starts on the next ensure.
        let log = Arc::clone(&env.log);
        env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
        env.ensure("bybit", "BTCUSDT", "100t");
        assert_eq!(env.calls(), vec!["bybit:trades BTCUSDT".to_string()]);
        assert_eq!(env.subs.get("bybit:BTCUSDT@trades"), Some(&SubscriptionId(301)));
        assert!(env.unroutable.is_empty());

        env.ensure("bybit", "BTCUSDT", "100t");
        assert_eq!(env.calls().len(), 1, "the shared tape is not resubscribed once recovered");
    }

    /// A `subscribe_bars` FAILURE leaves no `subs` entry (there is no id to stop) but still HOLDS
    /// the `spawned` slot — unchanged, because `spawned` doubles as `sync_from_core`'s fold filter
    /// (the `--observe` constraint). It is NOT recorded `unroutable` either: the venue is routable,
    /// it just said no. What IS now recorded is the [`FeedRetries`] entry, which is the whole point
    /// — before it, this exact state was permanent for the life of the process. The chart is still
    /// created, so the window renders empty rather than not at all.
    #[test]
    fn a_failed_bar_subscribe_records_no_subscription_id() {
        let mut env = Env::with_feeds(&[("binance", 100, &["bars"])]);
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()]);
        assert!(env.subs.is_empty());
        assert!(env.spawned.contains("BTCUSDT@1m"));
        assert!(env.charts.contains_key("BTCUSDT@1m"));
        assert!(env.unroutable.is_empty(), "a reachable venue is routable even when it says no");
        assert!(
            env.retries.is_pending(&RetryKey::series("BTCUSDT@1m")),
            "the failed attempt is RECORDED — this is what makes it retryable and visible"
        );
    }

    /// **The regression proof for the failed-subscribe half**, the twin of
    /// `an_unregistered_venue_does_not_permanently_suppress_the_subscribe`. A transient
    /// `subscribe_bars` error used to hold the `spawned` slot shut forever: the chart was dead for
    /// the life of the process behind one `warn!`. Now the failure is recorded, the per-frame
    /// callers are throttled by the backoff (so a live socket is not re-dialled ~60×/second), and
    /// once the cooldown elapses the retry runs and genuinely succeeds against a healed venue.
    #[test]
    fn a_transient_bar_subscribe_error_is_retried_and_eventually_succeeds() {
        let mut env = Env::with_feeds(&[]);
        let log = Arc::clone(&env.log);
        let fail = fail_set(&["bars"]);
        env.feeds.insert("binance", FakeFeed::boxed_flaky("binance", &log, 100, &fail));

        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 1, "the first attempt really happened");
        assert!(env.subs.is_empty(), "and really failed");

        // The per-frame callers must NOT re-dial while the cooldown holds. No sleeping needed:
        // the floor is one second and this test runs in microseconds.
        for _ in 0..10 {
            env.ensure("binance", "BTCUSDT", "1m");
        }
        assert_eq!(env.calls().len(), 1, "the backoff throttles the per-frame retry");

        // Cooldown elapsed, venue still broken: exactly ONE more attempt, then quiet again.
        env.elapse();
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 2, "one retry per elapsed cooldown, not a loop");

        // Heal the venue and let the next cooldown elapse: the chart comes back.
        fail.lock().unwrap().clear();
        env.elapse();
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 3);
        assert_eq!(env.subs.get("BTCUSDT@1m"), Some(&SubscriptionId(101)), "genuinely subscribed");
        assert!(env.retries.is_empty(), "the record clears on success");

        // …and having recovered, it is an ordinary spawned key again — not a resubscribe loop.
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 3, "recovery is once, not once per frame");
    }

    /// The trade-tape twin of the test above: the tape a tick/volume chart and every orderflow
    /// overlay on a `(venue, symbol)` share is their ONLY source, so one transient
    /// `subscribe_trades` error used to leave all of them permanently empty.
    #[test]
    fn a_transient_trade_subscribe_error_is_retried_and_eventually_succeeds() {
        let mut env = Env::with_feeds(&[]);
        let log = Arc::clone(&env.log);
        let fail = fail_set(&["trades"]);
        env.feeds.insert("binance", FakeFeed::boxed_flaky("binance", &log, 100, &fail));

        env.ensure("binance", "BTCUSDT", "100t");
        assert_eq!(env.calls().len(), 1);
        assert!(env.subs.is_empty(), "no tape");
        assert!(env.aggs.contains_key("BTCUSDT@100t"), "the aggregator exists, waiting for prints");
        assert!(env.retries.is_pending(&RetryKey::series("BTCUSDT@trades")));

        env.ensure("binance", "BTCUSDT", "100t");
        assert_eq!(env.calls().len(), 1, "throttled while the cooldown holds");

        fail.lock().unwrap().clear();
        env.elapse();
        env.ensure("binance", "BTCUSDT", "100t");
        assert_eq!(env.subs.get("BTCUSDT@trades"), Some(&SubscriptionId(101)), "the tape starts");
        assert!(env.retries.is_empty());
    }

    /// The one error kind that must NOT be retried. `LiveDataError::Unsupported` is a DECLARED-
    /// capability refusal — `vike_data::require_live_verb` derives it from the static
    /// `VenueCaps.live_data` matrix — so waiting cannot make it true and re-asking is pure noise.
    /// It is still RECORDED, so the resulting empty chart is visible rather than forgotten: that is
    /// the difference between "not retried" and the old "silently burnt".
    #[test]
    fn a_declared_capability_refusal_is_recorded_and_never_retried() {
        let mut env = Env::with_feeds(&[("binance", 100, &["bars!"])]);
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 1);
        let key = RetryKey::series("BTCUSDT@1m");
        assert!(env.retries.is_pending(&key), "recorded, so the gap stays visible");
        assert!(!env.retries.is_retry_due(&key), "…but never due");

        env.elapse(); // even letting every cooldown lapse changes nothing
        for _ in 0..5 {
            env.ensure("binance", "BTCUSDT", "1m");
        }
        assert_eq!(env.calls().len(), 1, "a provably-permanent refusal is asked exactly once");
    }

    /// A healthy venue must be untouched by all of the above: exactly one subscribe across many
    /// frames, and NOTHING recorded in either failure set.
    #[test]
    fn a_healthy_subscribe_still_happens_exactly_once_and_records_no_retry() {
        let mut env = Env::new();
        for _ in 0..10 {
            env.ensure("binance", "BTCUSDT", "1m");
            env.ensure("okx", "BTC-USDT", "100t");
        }
        assert_eq!(
            env.calls(),
            vec!["binance:bars BTCUSDT 1m".to_string(), "okx:trades BTC-USDT".to_string()],
            "exactly one socket each"
        );
        assert!(
            env.unroutable.is_empty() && env.retries.is_empty(),
            "healthy path records nothing"
        );
    }

    /// The backoff schedule itself: doubling from one second, capped at the 60s ceiling and flat
    /// forever after (there is no attempt limit — a venue down for an hour must still recover on
    /// its own), and saturating so an absurd attempt count can neither shift- nor multiply-overflow.
    #[test]
    fn retry_backoff_doubles_then_flattens_at_the_ceiling() {
        let secs = |n| retry_backoff(n).as_secs();
        assert_eq!([secs(1), secs(2), secs(3), secs(4)], [1, 2, 4, 8]);
        assert_eq!([secs(5), secs(6)], [16, 32]);
        assert_eq!(secs(7), 60, "capped, not 64");
        assert_eq!(secs(100), 60);
        assert_eq!(secs(u32::MAX), 60, "saturating, not a panic");
        assert_eq!(secs(0), 1, "a defensive 0 reads as the first failure");
    }

    // ----- ensure_trade_feed_on ---------------------------------------------------------------

    /// The trade-key convention [`orphaned_trade_feed_keys`] parses back out: bare for
    /// [`DEFAULT_VENUE`], `"venue:"`-namespaced otherwise. Idempotent per key.
    #[test]
    fn trade_feed_keys_follow_the_default_venue_convention() {
        let mut env = Env::new();
        {
            let (mut f, _) = env.both();
            ensure_trade_feed_on(&mut f, "binance", "BTCUSDT");
            ensure_trade_feed_on(&mut f, "binance", "BTCUSDT"); // idempotent
            ensure_trade_feed_on(&mut f, "okx", "BTC-USDT");
        }
        assert_eq!(
            env.calls(),
            vec!["binance:trades BTCUSDT".to_string(), "okx:trades BTC-USDT".to_string()]
        );
        assert_eq!(env.subs.get("BTCUSDT@trades"), Some(&SubscriptionId(101)));
        assert_eq!(env.subs.get("okx:BTC-USDT@trades"), Some(&SubscriptionId(201)));
    }

    // ----- stop_series (the Data-manager "Delete" path) ----------------------------------------

    /// **The Data-manager Delete leak, asserted instead of commented.** The GUI shell used to carry
    /// its own copy of this teardown and route every unsubscribe to `feeds["binance"]`, under a
    /// comment claiming both key kinds were binance-only. For a `series_key`-namespaced row the
    /// binance lookup misses, so the id minted by OKX was dropped on the floor while the `spawned`
    /// slot was freed anyway — a socket running with nothing left that names it. That comment was
    /// the only thing standing where this test now stands, and it was false: the id must be handed
    /// back to the venue that MINTED it.
    #[test]
    fn deleting_a_non_binance_series_unsubscribes_on_its_own_venue() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure("okx", "BTC-USDT", "1m");
        assert_eq!(env.subs["okx:BTC-USDT@1m"], SubscriptionId(201), "minted by the OKX feed");

        env.stop("okx:BTC-USDT@1m");
        assert_eq!(
            env.calls().last().unwrap(),
            "okx:unsub 201",
            "the unsubscribe must reach the venue the key names, not the default one"
        );
        assert!(
            !env.calls().iter().any(|c| c.starts_with("binance:unsub")),
            "the binance feed must never be offered another venue's subscription id"
        );
        assert!(!env.subs.contains_key("okx:BTC-USDT@1m"), "the id is forgotten");
        assert!(!env.spawned.contains("okx:BTC-USDT@1m"), "the spawned slot is freed");
        assert!(!env.charts.contains_key("okx:BTC-USDT@1m"), "the render series is dropped");
        assert!(env.hidden.contains("okx:BTC-USDT@1m"), "and the key is marked hidden");
        assert!(env.spawned.contains("BTCUSDT@1m"), "the untouched binance series survives");
        assert_eq!(env.subs["BTCUSDT@1m"], SubscriptionId(101));
    }

    /// The other half, and the reason a misroute here is PERMANENT rather than merely late: a
    /// Data-manager Delete does not remove the window (it stays with `open = false`), so the key is
    /// still in [`live_window_keys`] and [`reap_orphaned_feeds`] never considers it — the stop
    /// above is the only chance that stream ever gets. Driven as the real sequence — delete, the
    /// per-frame reap, then the operator re-adding the chart — and asserted on the venue's own call
    /// log: one subscribe, one unsubscribe, one FRESH subscribe. Under the old hand copy the middle
    /// line was absent and the last one stacked a second live stream on a first nothing had
    /// stopped.
    #[test]
    fn a_deleted_non_binance_series_leaves_no_orphan_and_re_adds_as_one_stream() {
        let mut env = Env::new();
        env.ensure("okx", "BTC-USDT", "1m");
        let mut w = chart_win("okx", "BTC-USDT", "1m");
        w.open = false; // exactly what a Data-manager Delete leaves behind

        env.stop("okx:BTC-USDT@1m");
        let after_stop = env.calls();
        env.reap(std::slice::from_ref(&w));
        assert_eq!(
            env.calls(),
            after_stop,
            "the per-frame reaper adds nothing: the key has left `subs`/`spawned` and its window is \
             still live, so a stop that failed to unsubscribe could never be repaired"
        );

        env.ensure("okx", "BTC-USDT", "1m");
        assert_eq!(
            env.calls(),
            vec![
                "okx:bars BTC-USDT 1m".to_string(),
                "okx:unsub 201".to_string(),
                "okx:bars BTC-USDT 1m".to_string(),
            ],
            "one stream at a time on the venue's own socket, never two"
        );
        assert_eq!(env.subs["okx:BTC-USDT@1m"], SubscriptionId(202), "a FRESH subscription id");
        assert!(!env.hidden.contains("okx:BTC-USDT@1m"), "unhidden again, so the fold resumes");
    }

    // ----- reap_orphaned_feeds ----------------------------------------------------------------

    /// A window still backing its key keeps its feed; nothing is unsubscribed and no slot moves.
    #[test]
    fn a_live_window_keeps_its_feed() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
        assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()]);
        assert!(env.spawned.contains("BTCUSDT@1m"));
        assert!(env.charts.contains_key("BTCUSDT@1m"));
        assert!(!env.hidden.contains("BTCUSDT@1m"));
    }

    /// **The priority scenario.** A key whose last window is gone must be torn down completely —
    /// chart dropped, key marked hidden, the live subscription STOPPED on its own venue feed, and
    /// the `spawned` slot freed — and then a later [`ensure_feed_on`] must genuinely restart it (a
    /// NEW subscription id, unhidden again). Leaving any one slot behind is how a re-added chart
    /// comes back permanently dead: `spawned` still set means no resubscribe, `hidden` still set
    /// means `sync_from_core` skips it forever.
    #[test]
    fn teardown_then_re_add_genuinely_restarts_the_feed() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.subs["BTCUSDT@1m"], SubscriptionId(101));

        env.reap(&[]); // every window closed/deleted
        assert_eq!(
            env.calls(),
            vec!["binance:bars BTCUSDT 1m".to_string(), "binance:unsub 101".to_string()],
            "the live subscription is stopped on the venue it was started on"
        );
        assert!(!env.charts.contains_key("BTCUSDT@1m"), "render series dropped");
        assert!(env.hidden.contains("BTCUSDT@1m"), "marked hidden against a same-frame revival");
        assert!(!env.spawned.contains("BTCUSDT@1m"), "spawned slot freed");
        assert!(!env.subs.contains_key("BTCUSDT@1m"), "subscription id forgotten");

        // Re-add the very same series: it must actually resubscribe, not silently no-op.
        env.ensure("binance", "BTCUSDT", "1m");
        assert_eq!(env.calls().len(), 3, "a real, second subscribe_bars");
        assert_eq!(env.subs["BTCUSDT@1m"], SubscriptionId(102), "a FRESH subscription id");
        assert!(!env.hidden.contains("BTCUSDT@1m"), "unhidden again, so the fold resumes");
    }

    /// The unsubscribe is routed by the KEY's venue prefix, not to whichever feed happens to be
    /// first: reaping an OKX key must call `unsubscribe` on the OKX feed with the OKX id, leaving
    /// the live Binance series untouched.
    #[test]
    fn the_unsubscribe_is_routed_to_the_keys_own_venue() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure("okx", "BTC-USDT", "1m");
        env.reap(&[chart_win("binance", "BTCUSDT", "1m")]); // only the Binance window survives
        assert_eq!(env.calls().last().unwrap(), "okx:unsub 201");
        assert!(env.spawned.contains("BTCUSDT@1m"), "the surviving Binance feed is untouched");
        assert!(!env.spawned.contains("okx:BTC-USDT@1m"));
    }

    /// A closed (`open = false`) window still counts as live — nothing tears down a merely-hidden
    /// window's feed. The GUI-level twin of `orphaned_feed_tests`' pure assertion.
    #[test]
    fn a_closed_window_does_not_trigger_teardown() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        let mut w = chart_win("binance", "BTCUSDT", "1m");
        w.open = false;
        env.reap(std::slice::from_ref(&w));
        assert_eq!(env.calls().len(), 1, "no unsubscribe");
        assert!(env.spawned.contains("BTCUSDT@1m"));
    }

    /// The trade-tape half. Two tick charts on the SAME `(venue, symbol)` share ONE trade feed:
    /// closing one drops only its aggregator, and the shared feed survives; closing the second
    /// drops the last aggregator and only THEN is the feed stopped — exactly once.
    #[test]
    fn a_shared_trade_feed_is_stopped_only_when_the_last_aggregator_goes() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "100t");
        env.ensure("binance", "BTCUSDT", "10v");
        assert_eq!(env.calls().len(), 1, "one trade feed for both charts");

        // Close the 10v chart: its aggregator dies, the shared feed lives on.
        env.reap(&[chart_win("binance", "BTCUSDT", "100t")]);
        assert!(!env.aggs.contains_key("BTCUSDT@10v"), "dead aggregator dropped");
        assert!(env.aggs.contains_key("BTCUSDT@100t"));
        assert_eq!(env.calls().len(), 1, "the trade feed is still needed");
        assert!(env.spawned.contains("BTCUSDT@trades"));

        // Close the last one: now the feed is orphaned and stopped.
        env.reap(&[]);
        assert!(env.aggs.is_empty());
        assert_eq!(
            env.calls(),
            vec!["binance:trades BTCUSDT".to_string(), "binance:unsub 101".to_string()]
        );
        assert!(!env.spawned.contains("BTCUSDT@trades"));
        assert!(!env.subs.contains_key("BTCUSDT@trades"));
    }

    /// An ORDERFLOW aggregator on the same `(venue, symbol)` holds the shared trade feed open just
    /// like a tick/volume one does — the union in step (2b). Here the tick chart is closed but an
    /// orderflow overlay on a still-open kline window remains, so the tape must NOT be stopped.
    #[test]
    fn an_orderflow_aggregator_alone_keeps_the_trade_feed_alive() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "100t"); // starts the tape
        env.of_aggs.insert(
            "BTCUSDT@1m".to_string(),
            ("binance".to_string(), "BTCUSDT".to_string(), orderflow::OrderflowAgg::new(1.0)),
        );
        // Only the 1m kline window (with the orderflow overlay) survives.
        env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
        assert!(env.aggs.is_empty(), "the tick chart's aggregator is gone");
        assert!(env.of_aggs.contains_key("BTCUSDT@1m"), "the orderflow aggregator survives");
        assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()], "tape NOT stopped");
        assert!(env.spawned.contains("BTCUSDT@trades"));
    }

    /// The `unroutable` record is swept by the reaper like every other slot, so it cannot become a
    /// new insert-only set. A key whose window is gone is forgotten; a key whose window is still
    /// open stays (the venue is still missing, so the report is still true).
    #[test]
    fn the_reaper_forgets_unroutable_keys_whose_window_is_gone() {
        let mut env = Env::new();
        env.ensure("bybit", "BTCUSDT", "1m"); // no bybit feed -> unroutable
        env.ensure("bybit", "ETHUSDT", "1m");
        assert_eq!(env.unroutable.len(), 2);

        // Only the BTCUSDT window survives.
        env.reap(&[chart_win("bybit", "BTCUSDT", "1m")]);
        assert_eq!(
            env.unroutable.iter().cloned().collect::<Vec<_>>(),
            vec!["bybit:BTCUSDT@1m".to_string()],
            "the live window's key stays reported; the closed one is forgotten"
        );

        env.reap(&[]);
        assert!(env.unroutable.is_empty(), "no windows left, nothing to report");
    }

    /// The trade-key half of that sweep: an unroutable `@trades` key is kept only while some
    /// remaining aggregator still needs its `(venue, symbol)` — the same rule the trade-feed reap
    /// uses, which is why both decode the key through the one shared `trade_key_pair`.
    #[test]
    fn the_reaper_sweeps_unroutable_trade_keys_by_remaining_aggregators() {
        let mut env = Env::new();
        env.ensure("bybit", "BTCUSDT", "100t"); // no bybit feed -> unroutable trade key
        assert!(env.unroutable.contains("bybit:BTCUSDT@trades"));

        // The window still exists, so its aggregator survives and the report stays.
        env.reap(&[chart_win("bybit", "BTCUSDT", "100t")]);
        assert!(env.unroutable.contains("bybit:BTCUSDT@trades"), "aggregator still needs it");

        // Close it: the aggregator dies, so the trade-key report is dropped too.
        env.reap(&[]);
        assert!(env.unroutable.is_empty());
    }

    /// The SERIES lane of [`FeedRetries`] is swept by exactly the same two rules (step 2e), for
    /// exactly the same reason: otherwise the map grows insert-only for the life of the process.
    /// Two properties beyond that: a still-wanted key keeps its record (the venue is still saying
    /// no, so the report is still true), and a torn-down key loses it — so a re-added series does
    /// NOT inherit a stale attempt count and the inflated backoff that comes with it.
    #[test]
    fn the_reaper_forgets_retry_records_whose_window_is_gone() {
        let mut env = Env::with_feeds(&[("binance", 100, &["bars", "trades"])]);
        env.ensure("binance", "BTCUSDT", "1m"); // subscribe_bars fails -> a series record
        env.ensure("binance", "ETHUSDT", "100t"); // subscribe_trades fails -> another
        assert_eq!(env.retries.len(), 2);

        // Only the BTCUSDT kline window survives; the tick window (and its aggregator) is gone.
        env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
        assert!(env.retries.is_pending(&RetryKey::series("BTCUSDT@1m")), "still wanted");
        assert!(
            !env.retries.is_pending(&RetryKey::series("ETHUSDT@trades")),
            "no aggregator needs that tape anymore, so its record goes too"
        );

        env.reap(&[]);
        assert!(env.retries.is_empty(), "nothing wanted, nothing recorded");
    }

    /// The other half of that sweep: the DOM/cockpit lanes are deliberately NOT swept by THIS
    /// reaper, which knows nothing about `dom_depth`/`poly_subs` — their teardown (and lane sweep)
    /// is [`reap_orphaned_dom_cockpit_streams`]'s job, under its own window rules. Sweeping them
    /// by the SERIES rules here would silently drop a live report the very next frame (no window
    /// ever backs a `"binance:BTCUSDT"` DOM key).
    #[test]
    fn the_reaper_leaves_the_dom_and_cockpit_retry_lanes_alone() {
        let mut env = Env::with_feeds(&[("polymarket", 400, &["trades"])]);
        env.poly("tok-1"); // book live, trade leg failed -> a PolyTrades record
        let mut dom_env = Env::with_feeds(&[("okx", 200, &["bars"])]);
        dom_env.depth(dom::DomVenue::Okx, "BTCUSDT"); // depth live, bar leg failed

        env.reap(&[]);
        dom_env.reap(&[]);
        assert!(
            env.retries.is_pending(&RetryKey::poly_trades("tok-1")),
            "a cockpit token is not a window key; the reaper must not judge it"
        );
        assert!(dom_env.retries.is_pending(&RetryKey::dom_bars("okx", "BTC-USDT-SWAP")));
    }

    /// A DOM window's own fixed `"1m"` bar feed is protected even with no matching chart window —
    /// the reaper would otherwise tear it down the frame after the DOM opens, and nothing would
    /// ever re-request it ([`ensure_depth`]'s idempotency gate is `dom_depth`, not `spawned`).
    #[test]
    fn a_dom_windows_own_bar_feed_survives_the_reaper() {
        let mut env = Env::new();
        env.ensure("binance", "BTCUSDT", "1m");
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
        let mut dom_win = workspace::WinState::tool("d", workspace::WinKind::Dom, r);
        dom_win.symbol = "BTCUSDT".to_string();
        env.reap(&[dom_win]);
        assert_eq!(env.calls().len(), 1, "no unsubscribe");
        assert!(env.spawned.contains("BTCUSDT@1m"));
    }

    // ----- ensure_depth -----------------------------------------------------------------------

    /// Binance: depth only (its 1m bars already flow from the chart/DOM open). Idempotent via
    /// `dom_depth`, so the per-frame call never opens a second stream.
    #[test]
    fn ensure_depth_on_binance_subscribes_depth_only_and_dedups() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        assert_eq!(env.calls(), vec!["binance:depth BTCUSDT".to_string()]);
        let sub = &env.dom_depth[&("binance".to_string(), "BTCUSDT".to_string())];
        assert_eq!(
            (sub.depth, sub.bars),
            (SubscriptionId(101), None),
            "the minted id is KEPT (not dropped on the floor); Binance has no bar leg"
        );
        assert!(env.retries.is_empty(), "the healthy path records nothing");
    }

    /// A non-Binance DOM venue ALSO needs 1m bars so its PAPER engine fills, and the canonical
    /// symbol is translated to that venue's native instrument id ([`venue_inst`]) first — OKX
    /// subscribes the dashed SWAP-perp inst, while the `dom_depth` dedup key keeps the CANONICAL
    /// symbol. That asymmetry is deliberate and load-bearing: the dedup key is what the caller
    /// (the DOM's per-frame `ensure_depth`) passes, not the venue-native id.
    #[test]
    fn ensure_depth_on_a_non_binance_venue_also_subscribes_1m_bars() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Okx, "BTCUSDT");
        assert_eq!(
            env.calls(),
            vec!["okx:depth BTC-USDT-SWAP".to_string(), "okx:bars BTC-USDT-SWAP 1m".to_string(),]
        );
        let sub = &env.dom_depth[&("okx".to_string(), "BTCUSDT".to_string())];
        assert_eq!(
            (sub.inst.as_str(), sub.depth, sub.bars),
            ("BTC-USDT-SWAP", SubscriptionId(201), Some(SubscriptionId(202))),
            "the entry keeps what teardown needs: the native inst and BOTH minted ids"
        );
        assert!(env.retries.is_empty());
    }

    /// A FAILED depth subscribe still records no `dom_depth` entry — that is what keeps it
    /// retryable at all — but the retry is now THROTTLED rather than fired on the very next frame:
    /// `ensure_depth` is a per-frame call, so the old shape re-dialled a live venue socket (and
    /// wrote a `warn!`) ~60×/second for as long as the DOM window stayed open.
    #[test]
    fn a_failed_depth_subscribe_is_retried_after_its_cooldown_not_next_frame() {
        let mut env = Env::with_feeds(&[("binance", 100, &["depth"])]);
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        assert!(env.dom_depth.is_empty(), "nothing recorded, so it stays retryable");
        for _ in 0..10 {
            env.depth(dom::DomVenue::Binance, "BTCUSDT");
        }
        assert_eq!(env.calls().len(), 1, "no ~60 Hz re-dial while the cooldown holds");

        env.elapse();
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        assert_eq!(env.calls().len(), 2, "the retry really happens, one per elapsed cooldown");
    }

    /// **The `dom_depth` burn.** The inner, non-Binance 1m bar leg used to be attempted exactly
    /// once — inside the depth leg's `Ok` arm — while `dom_depth.insert(key)` ran regardless, so a
    /// single `subscribe_bars` error left that venue's PAPER engine with no bars for the life of
    /// the process and nothing could reopen it. Now the leg is recorded and re-attempted ALONE:
    /// the depth stream, which did succeed, must never be re-subscribed.
    #[test]
    fn a_failed_dom_bar_leg_is_retried_without_resubscribing_the_live_depth_stream() {
        let mut env = Env::with_feeds(&[]);
        let log = Arc::clone(&env.log);
        let fail = fail_set(&["bars"]);
        env.feeds.insert("okx", FakeFeed::boxed_flaky("okx", &log, 200, &fail));

        env.depth(dom::DomVenue::Okx, "BTCUSDT");
        assert_eq!(
            env.calls(),
            vec!["okx:depth BTC-USDT-SWAP".to_string(), "okx:bars BTC-USDT-SWAP 1m".to_string()],
        );
        assert!(
            env.dom_depth.contains_key(&("okx".to_string(), "BTCUSDT".to_string())),
            "book is live"
        );
        let bars_key = RetryKey::dom_bars("okx", "BTC-USDT-SWAP");
        assert!(env.retries.is_pending(&bars_key), "the bar leg's loss is recorded, not dropped");

        for _ in 0..10 {
            env.depth(dom::DomVenue::Okx, "BTCUSDT");
        }
        assert_eq!(env.calls().len(), 2, "throttled, and the live depth stream is left alone");

        fail.lock().unwrap().clear();
        env.elapse();
        env.depth(dom::DomVenue::Okx, "BTCUSDT");
        assert_eq!(
            env.calls(),
            vec![
                "okx:depth BTC-USDT-SWAP".to_string(),
                "okx:bars BTC-USDT-SWAP 1m".to_string(),
                "okx:bars BTC-USDT-SWAP 1m".to_string(),
            ],
            "ONLY the bar leg is re-attempted — depth is never re-subscribed"
        );
        assert!(env.retries.is_empty(), "recovered: this venue's paper engine gets bars again");
        assert_eq!(
            env.dom_depth[&("okx".to_string(), "BTCUSDT".to_string())].bars,
            Some(SubscriptionId(202)),
            "the recovered leg's id lands on the entry, so teardown can stop it too"
        );

        env.depth(dom::DomVenue::Okx, "BTCUSDT");
        assert_eq!(env.calls().len(), 3, "and it is once, not once per frame");
    }

    /// An unregistered DOM venue is logged and skipped — never a panic, never a `dom_depth` entry.
    /// The miss is now RECORDED instead of re-warned on every frame (the same warn-once treatment
    /// `unroutable` gives the series lane), and stays immediately retryable because re-checking a
    /// hashmap costs nothing.
    #[test]
    fn a_missing_dom_feed_is_recorded_once_and_stays_immediately_retryable() {
        let mut env = Env::with_feeds(&[]);
        env.depth(dom::DomVenue::Bybit, "BTCUSDT");
        assert!(env.calls().is_empty() && env.dom_depth.is_empty());
        let key = RetryKey::dom_depth("bybit", "BTCUSDT");
        assert!(env.retries.is_pending(&key), "the miss is recorded, so the warn is once");
        assert!(env.retries.is_retry_due(&key), "…and free to re-check every frame");

        // Register the venue: the very next call subscribes, with no cooldown to wait out.
        let log = Arc::clone(&env.log);
        env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
        env.depth(dom::DomVenue::Bybit, "BTCUSDT");
        assert_eq!(env.calls()[0], "bybit:depth BTCUSDT");
        assert!(env.dom_depth.contains_key(&("bybit".to_string(), "BTCUSDT".to_string())));
    }

    // ----- ensure_poly_book -------------------------------------------------------------------

    /// The happy path: book + trades on the polymarket feed, token recorded, then deduped.
    #[test]
    fn ensure_poly_book_subscribes_book_and_trades_once() {
        let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
        env.poly("tok-1");
        env.poly("tok-1");
        assert_eq!(
            env.calls(),
            vec!["polymarket:book tok-1".to_string(), "polymarket:trades tok-1".to_string()]
        );
        assert!(env.poly_subs.contains_key("tok-1"));
        assert_eq!(
            (env.poly_subs["tok-1"].book, env.poly_subs["tok-1"].trades),
            (SubscriptionId(401), Some(SubscriptionId(402))),
            "both minted ids are KEPT for teardown"
        );
        assert!(env.retries.is_empty(), "the healthy path records nothing");
    }

    /// An empty or placeholder token is skipped BEFORE any feed lookup — a cockpit window whose
    /// Gamma resolve has not landed yet must never subscribe the literal placeholder id.
    #[test]
    fn an_empty_or_placeholder_token_never_subscribes() {
        let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
        env.poly("");
        env.poly(POLY_PLACEHOLDER_TOKEN);
        assert!(env.calls().is_empty() && env.poly_subs.is_empty());
        assert!(env.retries.is_empty(), "a skipped token is not a failure");
    }

    /// A failed BOOK subscribe records no `poly_subs` entry (so it stays retryable), but is now
    /// throttled rather than re-dialled on every frame.
    #[test]
    fn a_failed_poly_book_subscribe_is_retried_after_its_cooldown() {
        let mut env = Env::with_feeds(&[("polymarket", 400, &["book"])]);
        env.poly("tok-1");
        assert!(env.poly_subs.is_empty(), "book failed -> still retryable");
        for _ in 0..10 {
            env.poly("tok-1");
        }
        assert_eq!(env.calls().len(), 1, "no ~60 Hz re-dial while the cooldown holds");
        env.elapse();
        env.poly("tok-1");
        assert_eq!(env.calls().len(), 2, "the retry really happens");
    }

    /// **The `poly_subs` burn.** A failed TRADES subscribe still records the token — the book is
    /// live and the ladder paints, so re-running `subscribe_book` every frame would be the other
    /// bug — but that record used to suppress every later call, leaving the token's trade tape
    /// permanently absent behind one `warn!`. The trade leg now carries its own record and is
    /// re-attempted ALONE, without touching the live book.
    #[test]
    fn a_failed_poly_trades_leg_is_retried_without_resubscribing_the_live_book() {
        let mut env = Env::with_feeds(&[]);
        let log = Arc::clone(&env.log);
        let fail = fail_set(&["trades"]);
        env.feeds.insert("polymarket", FakeFeed::boxed_flaky("polymarket", &log, 400, &fail));

        env.poly("tok-1");
        assert!(env.poly_subs.contains_key("tok-1"), "book is live, so the token is recorded");
        let trades_key = RetryKey::poly_trades("tok-1");
        assert!(env.retries.is_pending(&trades_key), "the missing tape is recorded, not dropped");

        for _ in 0..10 {
            env.poly("tok-1");
        }
        assert_eq!(env.calls().len(), 2, "throttled, and the live book is left alone");

        fail.lock().unwrap().clear();
        env.elapse();
        env.poly("tok-1");
        assert_eq!(
            env.calls(),
            vec![
                "polymarket:book tok-1".to_string(),
                "polymarket:trades tok-1".to_string(),
                "polymarket:trades tok-1".to_string(),
            ],
            "ONLY the trade leg is re-attempted — the book is never re-subscribed"
        );
        assert!(env.retries.is_empty(), "recovered: prints resume");
        assert_eq!(
            env.poly_subs["tok-1"].trades,
            Some(SubscriptionId(402)),
            "the recovered leg's id lands on the entry, so teardown can stop it too"
        );

        env.poly("tok-1");
        assert_eq!(env.calls().len(), 3, "and it is once, not once per frame");
    }

    /// No polymarket feed registered (the default/CI build, where the bridge is feature-gated
    /// away): logged and skipped, never a panic — and, like the DOM twin, recorded so the warn is
    /// once per token rather than once per frame.
    #[test]
    fn ensure_poly_book_without_a_polymarket_feed_is_a_no_op() {
        let mut env = Env::new();
        env.poly("tok-1");
        assert!(env.calls().is_empty() && env.poly_subs.is_empty());
        let key = RetryKey::poly_book("tok-1");
        assert!(env.retries.is_pending(&key) && env.retries.is_retry_due(&key));
    }

    // ----- reap_orphaned_dom_cockpit_streams --------------------------------------------------

    /// **The B1-flagged leak, closed.** Deleting the last DOM window on a symbol stops exactly
    /// that symbol's streams — the depth leg AND the non-Binance 1m bar leg, unsubscribed on the
    /// entry's OWN venue — while another symbol's live DOM streams are untouched.
    #[test]
    fn deleting_a_dom_window_unsubscribes_exactly_its_depth_streams() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Binance, "BTCUSDT"); // depth id 101
        env.depth(dom::DomVenue::Okx, "ETHUSDT"); // depth id 201 + bars id 202
        assert_eq!(env.calls().len(), 3);

        env.reap_streams(&[dom_win("BTCUSDT")]); // the ETHUSDT window is gone
        assert_eq!(
            env.calls()[3..].to_vec(),
            vec!["okx:unsub 201".to_string(), "okx:unsub 202".to_string()],
            "both ETHUSDT legs stopped on OKX; the live BTCUSDT stream is untouched"
        );
        assert!(env.dom_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
        assert!(!env.dom_depth.contains_key(&("okx".to_string(), "ETHUSDT".to_string())));
    }

    /// A merely-closed (`open = false`) DOM window keeps its streams — the same rule the kline
    /// reaper applies ([`orphaned_feed_keys`]'s "regardless of `open`/`minimized`" note): close
    /// hides the window off-desktop and the rail can unhide it; only DELETION tears down.
    #[test]
    fn a_closed_dom_window_keeps_its_depth_stream() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        let mut w = dom_win("BTCUSDT");
        w.open = false;
        env.reap_streams(std::slice::from_ref(&w));
        assert_eq!(env.calls().len(), 1, "no unsubscribe");
        assert!(env.dom_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
    }

    /// A DOM venue switch keeps the previous venue's stream warm while the window lives — the
    /// reaper is venue-BLIND on purpose ([`ensure_depth`]'s documented warm-book behavior; the
    /// selected venue lives in the tool view, which `wins` cannot see). Deleting the window then
    /// takes every venue's streams with it.
    #[test]
    fn a_venue_switch_keeps_the_old_stream_warm_until_the_window_is_deleted() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Binance, "BTCUSDT"); // depth id 101
        env.depth(dom::DomVenue::Okx, "BTCUSDT"); // depth id 201 + bars id 202
        env.reap_streams(&[dom_win("BTCUSDT")]);
        assert_eq!(env.calls().len(), 3, "window still live: nothing unsubscribed");
        assert_eq!(env.dom_depth.len(), 2, "both venues' books stay warm");

        env.reap_streams(&[]);
        let mut unsubs = env.calls()[3..].to_vec();
        unsubs.sort();
        assert_eq!(
            unsubs,
            vec![
                "binance:unsub 101".to_string(),
                "okx:unsub 201".to_string(),
                "okx:unsub 202".to_string(),
            ],
            "the deleted window takes BOTH venues' streams with it"
        );
        assert!(env.dom_depth.is_empty());
    }

    /// Teardown then re-open genuinely restarts the stream — a FRESH subscription id, the DOM
    /// twin of `teardown_then_re_add_genuinely_restarts_the_feed`. The idempotency entry must
    /// have been freed, or a re-opened DOM would paint a book nothing feeds.
    #[test]
    fn teardown_then_reopen_genuinely_restarts_the_depth_stream() {
        let mut env = Env::new();
        let key = ("binance".to_string(), "BTCUSDT".to_string());
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        assert_eq!(env.dom_depth[&key].depth, SubscriptionId(101));

        env.reap_streams(&[]);
        assert!(env.dom_depth.is_empty());

        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        env.depth(dom::DomVenue::Binance, "BTCUSDT"); // still idempotent after the round trip
        assert_eq!(
            env.calls(),
            vec![
                "binance:depth BTCUSDT".to_string(),
                "binance:unsub 101".to_string(),
                "binance:depth BTCUSDT".to_string(),
            ],
            "a real second subscribe — and exactly one"
        );
        assert_eq!(env.dom_depth[&key].depth, SubscriptionId(102), "a FRESH id");
    }

    /// Teardown with the venue's client gone (a de-registered venue; every `--observe` session)
    /// is tolerated: the entry is dropped with no unsubscribe and no panic, and a later re-reap
    /// finds nothing left to stop — an id is never unsubscribed twice.
    #[test]
    fn teardown_without_the_venues_client_is_tolerated_and_never_double_unsubscribes() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        env.poly("tok-1"); // no polymarket feed registered -> a retry record, no entry

        env.feeds.remove("binance");
        env.reap_streams(&[]);
        assert!(env.dom_depth.is_empty() && env.poly_subs.is_empty());
        assert!(!env.calls().iter().any(|c| c.contains("unsub")), "no client, no unsubscribe");

        // Re-register the venue and re-reap: the torn-down entry stays down.
        let log = Arc::clone(&env.log);
        env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 500, &[]));
        env.reap_streams(&[]);
        assert!(!env.calls().iter().any(|c| c.contains("unsub")), "nothing left to stop");
    }

    /// An unsubscribe for an id the CLIENT no longer knows is tolerated — the venue client was
    /// replaced mid-session (a reconnect), so the teardown's id was never issued by the client
    /// that receives it. [`DataClient::unsubscribe`]'s contract pins unknown ids as a no-op
    /// ("never panics"); this proves the teardown path leans on exactly that and nothing more.
    #[test]
    fn an_unsubscribe_for_an_id_the_client_no_longer_knows_is_tolerated() {
        let mut env = Env::new();
        env.depth(dom::DomVenue::Binance, "BTCUSDT"); // id 101, minted by the ORIGINAL client
        let log = Arc::clone(&env.log);
        env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 500, &[]));
        env.reap_streams(&[]); // must not panic
        assert_eq!(env.calls().last().unwrap(), "binance:unsub 101");
        assert!(env.dom_depth.is_empty());
    }

    /// The cockpit twin of the DOM teardown: deleting the last window on a token stops its book
    /// AND trade streams on the polymarket feed, another token's cockpit is untouched, and a
    /// re-opened window genuinely resubscribes with fresh ids.
    #[test]
    fn deleting_a_cockpit_window_unsubscribes_its_book_and_trade_streams() {
        let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
        env.poly("tok-1"); // book 401 + trades 402
        env.poly("tok-2"); // book 403 + trades 404

        env.reap_streams(&[poly_win("tok-1")]); // tok-2's window is gone
        assert_eq!(
            env.calls()[4..].to_vec(),
            vec!["polymarket:unsub 403".to_string(), "polymarket:unsub 404".to_string()],
            "both tok-2 legs stopped; the live tok-1 streams are untouched"
        );
        assert!(env.poly_subs.contains_key("tok-1") && !env.poly_subs.contains_key("tok-2"));

        env.reap_streams(&[]);
        assert!(env.poly_subs.is_empty());

        env.poly("tok-1");
        assert_eq!(
            env.calls()[8..].to_vec(),
            vec!["polymarket:book tok-1".to_string(), "polymarket:trades tok-1".to_string()],
            "a re-opened cockpit genuinely resubscribes"
        );
        assert_eq!(env.poly_subs["tok-1"].book, SubscriptionId(405), "a FRESH id");
    }

    /// A still-resolving cockpit window (placeholder token) holds nothing alive: it never
    /// subscribed anything ([`ensure_poly_book`] refuses the placeholder), so it cannot pin
    /// another token's stream either.
    #[test]
    fn a_placeholder_cockpit_window_pins_no_stream() {
        let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
        env.poly("tok-1");
        env.reap_streams(&[poly_win(POLY_PLACEHOLDER_TOKEN)]);
        assert!(env.poly_subs.is_empty(), "tok-1's window is gone; the placeholder is not it");
    }

    /// The lane sweep — [`reap_orphaned_feeds`]'s step (2e), applied to the DOM/cockpit lanes
    /// this teardown owns: a torn-down entry's failing BAR-leg record goes with it (the retry arm
    /// lives behind the entry, so nothing could ever re-attempt it), while a live window's record
    /// stays (the venue is still saying no, so the report is still true).
    #[test]
    fn the_stream_reaper_sweeps_the_dom_and_cockpit_retry_lanes() {
        let mut env = Env::with_feeds(&[("okx", 200, &["bars"])]);
        env.depth(dom::DomVenue::Okx, "BTCUSDT"); // depth live, bar leg failing
        let bars_key = RetryKey::dom_bars("okx", "BTC-USDT-SWAP");
        env.reap_streams(&[dom_win("BTCUSDT")]);
        assert!(env.retries.is_pending(&bars_key), "window live: the record is still true");

        env.reap_streams(&[]);
        assert!(!env.retries.is_pending(&bars_key), "torn down: nothing can re-attempt the leg");
        assert!(env.retries.is_empty());
    }

    /// The entry-LESS half of that sweep: a failing depth/book subscribe records no entry (that
    /// is what keeps it retryable), so when its window is deleted only the retry record remains —
    /// with no per-frame `ensure_*` caller left, it could only sit idle forever. It is swept;
    /// a still-open window's record survives.
    #[test]
    fn the_stream_reaper_sweeps_failing_subscribes_whose_window_is_gone() {
        let mut env =
            Env::with_feeds(&[("binance", 100, &["depth"]), ("polymarket", 400, &["book"])]);
        env.depth(dom::DomVenue::Binance, "BTCUSDT");
        env.poly("tok-1");
        assert_eq!(env.retries.len(), 2);

        env.reap_streams(&[dom_win("BTCUSDT"), poly_win("tok-1")]);
        assert_eq!(env.retries.len(), 2, "both windows live: both records stay");

        env.reap_streams(&[dom_win("BTCUSDT")]);
        assert!(env.retries.is_pending(&RetryKey::dom_depth("binance", "BTCUSDT")));
        assert!(!env.retries.is_pending(&RetryKey::poly_book("tok-1")), "cockpit gone");

        env.reap_streams(&[]);
        assert!(env.retries.is_empty());
    }
}
