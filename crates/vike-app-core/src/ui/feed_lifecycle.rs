//! Feed-lifecycle — **the whole start/stop story of a live market-data subscription**, moved out
//! of `vike-app`'s CI-excluded `main.rs`.
//!
//! Two halves:
//!
//! 1. **The pure gates/diffs** that decide *whether* something should happen:
//!    [`should_spawn_backfill`] (the orderflow-backfill spawn gate), [`backfill_earliest_ts`] (how
//!    far back that backfill may page), [`orphaned_feed_keys`] / [`live_window_keys`] /
//!    [`orphaned_trade_feed_keys`] (which live feeds no window references anymore). These landed
//!    here first; they depend only on `HashSet`/`String` and the [`workspace`](crate::ui::workspace)
//!    window model.
//! 2. **The imperative half** that *performs* it — [`ensure_feed_on`], [`ensure_trade_feed_on`],
//!    [`ensure_depth`], [`ensure_poly_book`], [`reap_orphaned_feeds`] and its depth/cockpit sibling
//!    [`reap_orphaned_trade_cockpit_streams`]. These were `App` methods
//!    in `main.rs`, a file NO gate compiled then (`justfile`'s `ci_crates` omitted `vike-app`, and
//!    `xtask/src/ci/tables.rs` listed it in `EXCLUDE_FROM_CI`; the `app-check` job has compiled the
//!    shell since, and still executes none of this), so the diffs above were tested while
//!    the code that called them — the code that actually starts and stops sockets — was not. That
//!    asymmetry is the reason for this module: the reaper's teardown ordering (which unsubscribe
//!    routes to which venue feed, and which `spawned` slot is freed so a later `ensure_*` genuinely
//!    restarts the feed) is the exact shape whose sibling bug — a mid-backfill teardown eating the
//!    remaining pages — is bug 4 in [`core_sync`](crate::ui::core_sync)'s module doc.
//!
//! **Signature note.** `App` is an eframe type this crate cannot name, so each moved method's
//! `self.*` fields become explicit parameters, grouped the way [`core_sync`](crate::ui::core_sync)
//! already groups them: the venue-subscription bookkeeping in [`FeedSlots`], the render-side slots
//! a feed owns in [`SeriesSlots`], and the four "which series" strings in [`SeriesSpec`]. The shell
//! (`vike-desktop`; `vike-app` when this moved) keeps a one-line method per function that builds
//! the bundles, so every call site there is unchanged. Two other forced changes, both mechanical:
//!
//! - `ensure_feed_on` took a `_ctx: &egui::Context` it never used (and `ensure_feed`/
//!   `restore_workspace` only forwarded one) — dropped here; the shell's wrapper still takes it.
//! - `ensure_feed_on` read `self.shutdown.load(Relaxed)` inline; it now takes the already-loaded
//!   `shutting_down: bool`, so the atomic (and the `Arc` around it) stays in the shell. The load
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
//! [`sync_from_core`](crate::ui::core_sync)'s fold filter. See `ensure_feed_on`'s doc for that argument
//! and for why a *missing* venue is treated differently from a *failed* subscribe.
//!
//! **And the four bodies that burned a slot on a FAILED subscribe.** #946 fixed the *missing-venue*
//! half and explicitly left the other half open — "a venue that keeps rejecting a symbol is
//! therefore still a one-warn-then-quiet failure; that is a known remaining gap … \[retrying\] needs
//! backoff to be safe — real machinery, deliberately not smuggled in here". That machinery is
//! [`FeedRetries`], and with it the remaining four burns are closed: a failed `subscribe_bars`
//! ([`ensure_feed_on`]) or `subscribe_trades` ([`ensure_trade_feed_on`]) no longer holds its
//! `spawned` slot shut forever, the DOM's 1m-bar leg no longer vanished under [`ensure_depth`]'s
//! `trade_depth` gate (the leg itself is gone since: the desktop runs no paper engine to feed, M-4
//! of the Trade window's final review), and a failed trade leg no longer vanishes under
//! [`ensure_poly_book`]'s `poly_subs` gate. One policy for all of them, stated once on
//! [`FeedRetries`]: **the retry rate
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

use crate::backend::venue_routing::{venue_bar_instrument, venue_of_key};
use crate::ui::tool_views::POLY_PLACEHOLDER_TOKEN;
use crate::ui::workspace::{self, DEFAULT_VENUE};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use vike_chart::{DisplayTz, model};
use vike_data::{DataClient, LiveDataError, SubscriptionId};
use vike_orderflow::{bar_agg, tickvol};

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
/// forever. So the consumer side ([`core_sync::sync_from_core`](crate::ui::core_sync)) still STAGES a
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
/// lifted verbatim out of `App::maybe_spawn_backfill` (whose remaining body named
/// `vike_binance::agg_trades_backfill_reported` behind the `fat` feature, so the method itself
/// stayed in the shell; this arithmetic did not, and it was the only part with an interesting
/// edge case). ⚠ That method has been a no-op tombstone since the `fat` build went on 2026-09-09
/// (`crates/vike-desktop/src/app_methods.rs`'s `maybe_spawn_backfill`), so nothing in production
/// calls this today; the bound is kept, tested, for the next backfill that needs it.
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
/// for each indicator whose `source_symbol` is set — TradingView's "symbol" input) — over ALL
/// windows regardless of `open`/`minimized`, matching how a closed window's OWN primary feed
/// already survives being hidden off-desktop (`WinState::open`'s doc: "false => hidden
/// off-desktop (rail can unhide)" — nothing ever tears down ITS feed just because the window is
/// closed, so an orphan-reaper for compare feeds shouldn't treat them any differently).
///
/// Every non-chart window kind (Trade/Account/Options/News/Calendar/Data/…) genuinely never drives
/// `ensure_feed` for its own symbol at all, so they're excluded. (The DOM was the one exception — a
/// fixed `"1m"` bar feed ensured once at window creation; the Trade window that replaced it opens
/// no bar feed, Ruling R6, and its depth stream is [`ensure_depth`]'s, reaped by
/// [`reap_orphaned_trade_cockpit_streams`].)
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
/// `aggs`/`of_aggs` key IS a window's own [`crate::ui::workspace::WinState::key`], so a map entry whose key isn't in this
/// set has no window backing it and must be dropped (the coupled half of the trade-feed leak; see
/// [`orphaned_trade_feed_keys`]). Membership rules are exactly as documented on [`orphaned_feed_keys`]:
/// every Chart window's primary key UNION its Compare symbols UNION its foreign-source study symbols,
/// over ALL windows regardless of `open`/`minimized`.
pub fn live_window_keys(wins: &[workspace::WinState]) -> HashSet<String> {
    let mut live: HashSet<String> = HashSet::new();
    // Chart windows only: every other kind drives no `ensure_feed` of its own (see
    // [`orphaned_feed_keys`]).
    for w in wins.iter().filter(|w| w.kind == workspace::WinKind::Chart) {
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
            if needed.contains(&pair) { None } else { Some(k.clone()) }
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
/// `"polymarket"`, …). `+ Send` because the shell (`vike-desktop`) moves the whole set onto a
/// throwaway shutdown thread and fans each venue's teardown out in parallel (see its `on_exit`).
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
/// without touching the depth/cockpit records, whose keyspaces (`trade_depth`'s `(venue, symbol)`
/// pairs, `poly_subs`' token ids) it knows nothing about. A lane-tagged key rather than a string
/// prefix precisely because a series key already contains a `"venue:"` prefix of its own, and a
/// sweep that guessed wrong would silently drop a live record.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RetryLane {
    /// [`ensure_feed_on`]'s kline subscribe and [`ensure_trade_feed_on`]'s trade tape, keyed by the
    /// `spawned` key — so the reaper sweeps it under exactly the rules it sweeps `unroutable` with.
    Series,
    /// [`ensure_depth`]'s `subscribe_depth` leg, keyed `"{venue}:{inst}"` (the `trade_depth`
    /// key). Its gate set already records the leg only on success, so this lane does not *create*
    /// the retry — it THROTTLES one that existed but ran on every frame, i.e. ~60 Hz of re-dialling
    /// (and warning) against a live venue socket.
    TradeDepth,
    /// [`ensure_poly_book`]'s `subscribe_book` leg, keyed by token id (the `poly_subs` key). Same
    /// throttle-only role as [`RetryLane::TradeDepth`].
    PolyBook,
    /// [`ensure_poly_book`]'s SECOND `subscribe_trades` leg, keyed by token id. It gets its own lane
    /// because it has no gate set of its own: `poly_subs` deliberately records the token once the
    /// BOOK is live (the ladder paints), so a failed trade leg had nothing left that could reopen it
    /// and that token's trade tape was permanently absent.
    PolyTrades,
}

/// One failing subscribe leg: its [`RetryLane`] plus the lane-local id. Built through the four
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
    /// A depth leg, keyed exactly like its `trade_depth` entry (venue + venue-NATIVE symbol).
    pub fn trade_depth(venue: &str, inst: &str) -> RetryKey {
        RetryKey { lane: RetryLane::TradeDepth, id: format!("{venue}:{inst}") }
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
    /// The venue has **no registered client** at all (the depth/cockpit lanes only — the series lane
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
/// [`ensure_depth`] inserted `trade_depth` even when its inner 1m `subscribe_bars` failed, and
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
/// #946 exists to have removed. The depth/cockpit lanes are swept by
/// [`reap_orphaned_trade_cockpit_streams`] under ITS live-window rules — they used to be unsweepable
/// because `trade_depth`/`poly_subs` had no teardown path at all, which is the gap that function
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
    /// (`spawned`, `trade_depth`, `poly_subs`) already says "done", so this is the only thing that
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
    /// depth/cockpit twin of what [`FeedSlots::unroutable`] does in the series lane).
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
    /// nothing wants anymore, leaving the depth/cockpit lanes alone (see [`RetryLane`]). Without it
    /// this map would grow insert-only for the life of the process — the very shape #946 removed
    /// one layer down, re-introduced one layer up.
    pub fn retain_series(&mut self, keep: impl Fn(&str) -> bool) {
        self.inner.retain(|k, _| k.lane != RetryLane::Series || keep(&k.id));
    }

    /// [`reap_orphaned_trade_cockpit_streams`]'s sweep — the depth/cockpit twin of
    /// [`Self::retain_series`], leaving the SERIES lane alone for the mirror reason: each reaper
    /// can answer "is this key still wanted?" only for the keyspaces it knows. `keep` receives the
    /// record's [`RetryLane`] plus its lane-local id (the [`RetryKey`] constructors' `id` strings),
    /// and is never consulted for [`RetryLane::Series`].
    pub fn retain_trade_cockpit(&mut self, keep: impl Fn(RetryLane, &str) -> bool) {
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
    /// (`crate::backend::backend_conn::teardown_feed_plane`; a backend SWITCH no longer resets this lane —
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
/// so `core_sync::BF_PENDING_MAX_TICKS`' and `bar_agg::PENDING_MAX_TICKS`' sizing arguments —
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
/// [`should_spawn_backfill`], which the shell called per frame only for charts that currently
/// wanted orderflow. Close the chart and nothing re-asks the venue for it. (That caller was
/// `vike-app`'s backfill spawn, a tombstone since the `fat` build went; nothing in production calls
/// the gate today.)
///
/// # Lifetime
///
/// A record is created by a worker's report and removed by a successful one, so the healthy path
/// holds NOTHING (`is_empty()`). It is deliberately not swept by [`reap_orphaned_feeds`], for the
/// same reason its `RetryLane::TradeDepth`/`PolyBook` siblings are not: this map shadows `bf_spawned`,
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
    /// `InFlight` — [`should_spawn_backfill`] is the only correct caller, and the shell reported a
    /// failed `thread::Builder::spawn` back through [`Self::note_report`] so even that path
    /// re-opened (`vike-app`'s spawn; its body went with the `fat` build).
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
    /// (`crate::backend::backend_conn::teardown_feed_plane`), which also clears `bf_spawned`: a re-mounted
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
/// the [`CoreSyncState`](crate::ui::core_sync::CoreSyncState) pattern applied to the write side of
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
    /// [`sync_from_core`](crate::ui::core_sync)'s fold filter, and `--observe` mode has no feeds at all
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
    pub of_aggs: &'a mut HashMap<String, (String, String, bar_agg::OrderflowAgg)>,
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
    pub asset_class: Option<vike_model::AssetClass>,
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
/// **This was reachable from the UI, not hypothetical.** The shell (`vike-app` then) registered
/// exactly six live clients (binance/bybit/okx/aster/hyperliquid/polymarket) while its Symbol
/// picker searched a TWELVE-venue [`vike_catalog`] — and since the `fat` build went (2026-09-09)
/// the desktop registers NO live client at all, so every venue reaches it. When this was written,
/// five of the six extra venues reached this function with an asset
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
/// also the filter [`sync_from_core`](crate::ui::core_sync) folds core snapshots through
/// (`if !spawned.contains(&key) { continue }`). In `--observe` mode — every launch since the `fat`
/// build went — the shell builds an **empty** `feeds` map — no local core, no venue clients — and
/// renders charts purely from the remote
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
                            // that produces it most often — every launch now: the desktop
                            // registers no local venue feed (`vike-app --observe`, when this was
                            // written, linked no venue bridge), so every chart reaches this arm —
                            // while the chart itself fills perfectly from `WireSnapshot::bars` over
                            // the node
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
/// (`crates/vike-desktop/src/app_ui.rs`'s `draw_windows`) applies it to the one row an operator picked.
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
/// Called once every frame (see `crates/vike-desktop/src/app_ui.rs`'s `draw_windows` — it named
/// `vike-app`'s `update` call site until 2026-09-28) rather than threaded into every
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
        if let Some(id) = f.subs.remove(&key)
            && let Some(feed) = f.feeds.get_mut(venue_of_key(&key))
        {
            feed.unsubscribe(id);
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
    // backoff) into the re-added series' first subscribe. The depth/cockpit lanes are deliberately
    // untouched — this function knows nothing about `trade_depth`/`poly_subs`; their teardown (and
    // the matching lane sweep) is [`reap_orphaned_trade_cockpit_streams`]'s job, run by the same
    // per-frame caller.
    f.retries.retain_series(still_wanted);
}

/// The venue the cockpit's book/trade streams live on — [`ensure_poly_book`]'s feed key, and
/// therefore the ONE venue [`reap_orphaned_trade_cockpit_streams`] routes a cockpit unsubscribe to.
const POLY_VENUE: &str = "polymarket";

/// The `(venue, native symbol)` of every Trade window in `wins` — the live set
/// [`reap_orphaned_trade_cockpit_streams`] diffs `trade_depth` against. ALL windows count, regardless
/// of `open`/`minimized`, as before; only a DELETED window tears its stream down. Venue-SPECIFIC,
/// unlike the DOM's venue-blind set: a Trade window keeps its venue in its `WinState`, so a window
/// that moves to another venue releases the old venue's stream on the next reap. A window with no
/// symbol yet (the glue seeds it on the first frame) names no book, so it holds nothing alive.
pub fn live_trade_books(wins: &[workspace::WinState]) -> HashSet<(String, String)> {
    wins.iter()
        .filter(|w| w.kind == workspace::WinKind::Trade && !w.symbol.is_empty())
        .map(|w| (w.venue.clone(), w.symbol.clone()))
        .collect()
}

/// The token-ids of every Polymarket cockpit window in `wins` — [`live_trade_books`]'s cockpit
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

/// [`reap_orphaned_feeds`]'s depth/cockpit sibling — the teardown path `trade_depth`/`poly_subs`
/// never had (the B1 review's flagged debt, and B2's prerequisite): stop every depth and cockpit
/// stream whose windows are GONE, at the same per-frame cadence and under the same live-set
/// notion. Before this existed, closing a DOM window left its L2 depth stream (and, non-Binance,
/// its 1m bar leg) running until process exit — harmless in observe mode (both maps empty), a real
/// socket leak in the fat build (deleted 2026-09-09; every launch is the observe mode now).
///
/// Split from [`reap_orphaned_feeds`] rather than folded into it because the two own disjoint
/// state: that reaper's whole surface is [`FeedSlots`]/[`SeriesSlots`], which deliberately carry
/// neither `trade_depth` nor `poly_subs` ([`ensure_depth`]/[`ensure_poly_book`] take them as bare
/// parameters for the same reason). The routing mirror-images the subscribe exactly:
/// [`ensure_depth`] subscribed through `feeds[venue]` and the entry's key REMEMBERS
/// that venue string, so the unsubscribe goes to `feeds[&key.0]` — the same
/// "route by the key's own venue" rule `reap_orphaned_feeds` applies via
/// [`venue_of_key`](crate::backend::venue_routing::venue_of_key); a cockpit stream always routes to
/// [`POLY_VENUE`], the one feed [`ensure_poly_book`] ever subscribes on.
///
/// A venue whose client is no longer registered (or was never registered — a `--observe` session
/// has no feeds at all) is tolerated: the entry is dropped without an unsubscribe, matching the
/// venue-missing arms of every `ensure_*`. The venue-side tolerance is the trait's:
/// [`DataClient::unsubscribe`] on an id the client no longer knows is a documented no-op, so a
/// second teardown of the same id (impossible here — the entry is removed — but reachable through
/// the backend-switch clear running after this reaper) cannot panic either.
///
/// The closing sweep drops the depth/cockpit [`FeedRetries`] records nothing can drive anymore —
/// [`reap_orphaned_feeds`]'s step (2e), applied to the lanes it deliberately leaves alone. A
/// depth/book record dies with its window (its per-frame `ensure_*` caller is gone, so it could
/// only ever sit idle); a trade record dies with the token whose retry arm re-attempts it.
/// Without this, a torn-down window's records would be the insert-only residue this module keeps
/// having to remove, and a re-opened window would inherit a stale attempt count.
pub fn reap_orphaned_trade_cockpit_streams(
    feeds: &mut FeedMap,
    trade_depth: &mut HashMap<(String, String), TradeDepthSubs>,
    poly_subs: &mut HashMap<String, PolyBookSubs>,
    retries: &mut FeedRetries,
    wins: &[workspace::WinState],
) {
    let trade_live = live_trade_books(wins);
    let poly_live = live_poly_tokens(wins);
    trade_depth.retain(|(venue, inst), sub| {
        if trade_live.contains(&(venue.clone(), inst.clone())) {
            return true;
        }
        if let Some(feed) = feeds.get_mut(venue.as_str()) {
            feed.unsubscribe(sub.depth);
            if let Some(trades) = sub.trades {
                feed.unsubscribe(trades);
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
    // The lane sweep. TradeDepth/PolyBook records are keyed by what the live sets hold (the
    // `(venue, native symbol)` behind the id's `"venue:"` prefix / token id), so they are judged
    // against the window directly. PolyTrades records ride their token like the book does.
    retries.retain_trade_cockpit(|lane, id| match lane {
        RetryLane::TradeDepth => id
            .split_once(':')
            .is_some_and(|(v, s)| trade_live.contains(&(v.to_string(), s.to_string()))),
        RetryLane::PolyBook | RetryLane::PolyTrades => poly_live.contains(id),
        // Never consulted for Series (`retain_trade_cockpit` retains that lane unconditionally);
        // named so this match stays exhaustive when a lane is added.
        RetryLane::Series => true,
    });
}

/// What one `trade_depth` entry OWNS: the live subscription id [`ensure_depth`] minted for its
/// `(venue, native symbol)` key. Kept — instead of the id being dropped on the floor, which is what
/// this map's `HashSet` predecessor did — so [`reap_orphaned_trade_cockpit_streams`] can stop
/// exactly this stream when the last Trade window on the book is deleted, and `teardown_feed_plane`
/// (the backend-switch clear's feed-plane complement) can stop it against the client that issued it
/// before forgetting it. (A backend SWITCH keeps these streams — venue truth, split-plane B2.)
///
/// ⚠ It held a second id until the Trade window's final review (M-4): the DOM's non-Binance `"1m"`
/// `subscribe_bars` leg, opened "so that venue's PAPER engine fills". The desktop has run no engine
/// since it lost its local core, the datahub feed refuses the bar lane (`Unsupported`), and the leg
/// logged one warn per instrument a Trade window opened — so it is deleted with its retry lane, not
/// kept as a second subscription that feeds nothing (Ruling R6: the Trade window opens no bar feed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeDepthSubs {
    /// `subscribe_depth`'s id — the handle `unsubscribe` needs to stop the L2 stream.
    pub depth: SubscriptionId,
    /// `subscribe_trades`' id, once it has succeeded: the print stream the window's tick chart paints
    /// bubbles from. `None` for a venue that serves no trade stream, which costs the chart its Trades
    /// layer and nothing else — the depth leg above never waits on it.
    pub trades: Option<SubscriptionId>,
}

/// What one `poly_subs` entry OWNS — [`TradeDepthSubs`]'s cockpit twin, for the same two spenders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolyBookSubs {
    /// `subscribe_book`'s id — the leg whose success records the entry (the ladder paints).
    pub book: SubscriptionId,
    /// The `subscribe_trades` leg's id, once it has succeeded; `None` while it is still
    /// failing/retrying under its [`RetryLane::PolyTrades`] record.
    pub trades: Option<SubscriptionId>,
}

/// Start a live L2 depth stream for `(venue, inst)` (once) so the Trade window renders that
/// venue's real book. The key is the window's venue and the venue-NATIVE symbol it trades, so
/// there is nothing to translate. Routes to the venue's feed; dedups via `trade_depth`, whose entry
/// KEEPS the minted subscription id ([`TradeDepthSubs`]); the stream is stopped by
/// [`reap_orphaned_trade_cockpit_streams`] once no Trade window names the book anymore (before that
/// existed, only the feed's `shutdown()` on exit ever stopped it). Called every frame for the
/// window's book — idempotent. A Trade window that moves to another venue releases the old
/// venue's stream on the next reap ([`reap_orphaned_trade_cockpit_streams`] is venue-specific).
///
/// The depth stream is the ONLY one: a Trade window opens no bar feed on any venue (Ruling R6;
/// [`TradeDepthSubs`] says what the deleted 1m leg was).
///
/// A failed depth subscribe records no `trade_depth` entry, so it stays retryable — but it used to
/// be retried on **every frame**, i.e. a ~60 Hz socket-and-log loop against a live venue. It is
/// throttled by the [`FeedRetries`] schedule, and the missing-venue arm's `warn!`, which was per
/// frame, is once per `(venue, symbol)`.
pub fn ensure_depth(
    feeds: &mut FeedMap,
    trade_depth: &mut HashMap<(String, String), TradeDepthSubs>,
    retries: &mut FeedRetries,
    venue: &str,
    inst: &str,
) {
    let key = (venue.to_string(), inst.to_string());
    // The depth stream is live and must NOT be re-subscribed.
    if trade_depth.contains_key(&key) {
        return;
    }
    let depth_key = RetryKey::trade_depth(venue, inst);
    // A previously-failed depth subscribe is retried, but only once its cooldown has elapsed —
    // this is the per-frame resubscribe loop the old code ran.
    if retries.is_blocked(&depth_key) {
        return;
    }
    let Some(feed) = feeds.get_mut(venue) else {
        if retries.note_missing(&depth_key) {
            tracing::warn!(
                "ensure_depth({venue}, {inst}): no feed registered for venue — the Trade window's \
                 ladder will stay STALE; will retry once one is registered"
            );
        }
        return;
    };
    match feed.subscribe_depth(inst) {
        Ok(depth) => {
            if let Some(n) = retries.clear(&depth_key) {
                tracing::info!(
                    "ensure_depth({venue}, {inst}): subscribe_depth recovered after {n} failed \
                     attempt(s)"
                );
            }
            // The tick chart's print stream rides the same entry. A venue with no trade lane (or a
            // session that refuses it) is not an error for the window — its chart has no Trades
            // layer — so a refusal is noted once per window at debug and never retried.
            let trades = match feed.subscribe_trades(inst) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::debug!(
                        "ensure_depth({venue}, {inst}): no trade stream for the tick chart: {e}"
                    );
                    None
                }
            };
            trade_depth.insert(key, TradeDepthSubs { depth, trades });
        }
        Err(e) => retries.note_error(
            &depth_key,
            &e,
            &format!("ensure_depth({venue}, {inst}): subscribe_depth"),
        ),
    }
}

/// Start (once) a live Polymarket L2 book + trade stream for `token` (the YES-outcome token-id)
/// so a cockpit window renders that token's real book from the shared `BookStore` under venue
/// `"polymarket"`. The polymarket feed's `subscribe_book` emits `LiveDataSink::book`, which the
/// composition-root `CoreSinkAdapter::book` now also lands in the `BookStore` (see that method).
/// The cockpit's analog of [`ensure_depth`]; dedups via `poly_subs`, whose entry KEEPS the minted
/// ids ([`PolyBookSubs`]) so [`reap_orphaned_trade_cockpit_streams`] stops both streams once no
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
        if retries.is_retry_due(&trades_key)
            && let Some(feed) = feeds.get_mut(POLY_VENUE)
            && let Some(id) = try_poly_trades(&mut **feed, retries, token, &trades_key)
        {
            sub.trades = Some(id);
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

/// The cockpit's trade-tape leg, in one place so its FIRST attempt (inside the book's `Ok` arm) and
/// its RETRIES (a later frame, once `poly_subs` is already recorded) can never diverge — the
/// divergence being exactly how a retry path comes not to exist at all. Returns the minted id on
/// success so both callers record it in the entry's [`PolyBookSubs::trades`].
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
#[path = "backfill_gate_tests.rs"]
#[cfg(test)]
mod backfill_gate_tests;

/// The wiring #952 made possible: what an aggTrades backfill's reported stop reason actually
/// DOES. Same standalone-pure-helper shape as [`backfill_gate_tests`] above (no `App`, no worker
/// thread, no network): a report is a plain value, so the whole state machine is exercised by
/// handing [`BackfillRetries`] the reports a real worker would have sent.
///
/// **No test sleeps.** The cooldown floor is one second and these run in microseconds, so "not
/// re-walked yet" is proven directly, and "re-walked now" through
/// [`BackfillRetries::expire_all`] — the device [`FeedRetries`]' own tests already use.
#[path = "backfill_retry_tests.rs"]
#[cfg(test)]
mod backfill_retry_tests;

/// C2 tidy FIX 3: unit tests for the pure [`orphaned_feed_keys`] diff — see its doc for why
/// this is tested standalone rather than through a full `App` (same rationale as
/// `backfill_gate_tests` above; `WinState` itself needs no `eframe::CreationContext`, just
/// plain `egui::Rect` data, same as `workspace`'s own `WinState` tests).
#[path = "orphaned_feed_tests.rs"]
#[cfg(test)]
mod orphaned_feed_tests;

/// Unit tests for the pure [`orphaned_trade_feed_keys`] diff — the trade-feed half of the feed-leak
/// fix. Standalone for the same reason the modules above are (no `App`/`eframe::CreationContext`
/// needed): the helper is pure over plain `HashSet`s. Mirrors `orphaned_feed_tests`' `spawned`
/// helper style.
#[path = "orphaned_trade_feed_tests.rs"]
#[cfg(test)]
mod orphaned_trade_feed_tests;

/// Unit tests for [`backfill_earliest_ts`] — the paging-floor arithmetic lifted out of
/// `App::maybe_spawn_backfill`. Standalone for the same reason [`backfill_gate_tests`] is: the
/// helper is pure, and the method it came from named a `fat`-gated `vike_binance` entry point (a
/// no-op tombstone since that feature went).
#[path = "backfill_bound_tests.rs"]
#[cfg(test)]
mod backfill_bound_tests;

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
/// loss as bug 4 in [`core_sync`](crate::ui::core_sync)'s module doc.
#[path = "feed_slot_tests.rs"]
#[cfg(test)]
mod feed_slot_tests;
