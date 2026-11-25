//! Feed-lifecycle — **the whole start/stop story of a live market-data subscription**, moved out
//! of `vike-app`'s CI-excluded `main.rs`.
//!
//! Two halves:
//!
//! 1. **The pure diffs** that decide *whether* something should happen:
//!    `orphaned_feed_keys` / `live_window_keys` / `orphaned_trade_feed_keys` (which live
//!    feeds no window references anymore). These landed here first; they depend only on
//!    `HashSet`/`String` and the [`workspace`](crate::ui::window) window model.
//! 2. **The imperative half** that *performs* it — [`ensure_feed_on`], [`ensure_trade_feed_on`],
//!    [`ensure_depth`], [`ensure_polymarket_book`], [`reap_orphaned_feeds`] and its depth/cockpit sibling
//!    [`reap_orphaned_trade_cockpit_streams`]. These were `App` methods
//!    in `main.rs`, a file NO gate compiled then (`justfile`'s `ci_crates` omitted `vike-app`, and
//!    `xtask/src/ci/tables/roster.rs` listed it in `EXCLUDE_FROM_CI`; the `app-check` job has compiled the
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
//! a feed owns in [`SeriesSlots`], and the four "which series" strings in [`crate::ui::startup::SeriesSpec`]. The shell
//! (`vike-desktop`; `vike-app` when this moved) keeps a one-line method per function that builds
//! the bundles, so every call site there is unchanged. Two other forced changes, both mechanical:
//!
//! - `ensure_feed_on` took a `_context: &egui::Context` it never used (and `ensure_feed`/
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
//! [`DatahubFeedRetries`], and with it the remaining four burns are closed: a failed `subscribe_bars`
//! ([`ensure_feed_on`]) or `subscribe_trades` ([`ensure_trade_feed_on`]) no longer holds its
//! `spawned` slot shut forever, the DOM's 1m-bar leg no longer vanished under [`ensure_depth`]'s
//! `trade_depth` gate (the leg itself is gone since: the desktop runs no paper engine to feed, M-4
//! of the Trade window's final review), and a failed trade leg no longer vanishes under
//! [`ensure_polymarket_book`]'s `polymarket_subscriptions` gate. One policy for all of them, stated once on
//! [`DatahubFeedRetries`]: **the retry rate
//! matches the cost of the attempt, and the log rate is one line per state transition.**
//!
//! **And the kline subscribe is gone** (2026-11-16): the datahub wire, this desktop's only
//! transport, serves no bar lane, so [`ensure_feed_on`] asks no feed for a kline series at all and
//! only records it in `spawned` (its doc says where the bars come from instead).

use crate::data::chart_key;
use crate::data::datahub::feed_retry::{
    DatahubFeedRetries, DatahubFeedRetryKey, DatahubFeedRetryLane,
};
use crate::data::market_data_session::DataClientMap;
use crate::ui::window;
use std::collections::{HashMap, HashSet};
use vike_data::DataClient;

mod ensure;
mod reap;
mod streams;

pub use ensure::{
    FeedSlots, SeriesSlots, ensure_feed_on, ensure_trade_feed_on, retry_unroutable, stop_series,
};
#[cfg(test)]
pub(crate) use reap::live_polymarket_tokens;
pub use reap::{reap_orphaned_feeds, reap_orphaned_trade_cockpit_streams};
pub use streams::{L2BookSubscriptions, ensure_depth, ensure_polymarket_book};

/// C2 tidy (FIX 3): the pure "which `spawned` feeds does no window reference anymore" diff,
/// factored out of `App::reap_orphaned_feeds` so it is unit-testable without a full
/// `App`/`eframe::CreationContext` (`dom_position_tests` is the established pattern).
///
/// The live set is every CHART window's primary key (`WindowState::key()`) UNION every chart
/// window's Compare overlays (`series_key(c.venue, c.symbol, w.interval)` for each `w.compare` entry) UNION every
/// chart window's foreign-source study symbols (`series_key(src.venue, src.symbol, w.interval)`
/// for each indicator whose `source_symbol` is set — TradingView's "symbol" input) — over ALL
/// windows regardless of `open`/`minimized`, matching how a closed window's OWN primary feed
/// already survives being hidden off-desktop (`WindowState::open`'s doc: "false => hidden
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
/// the shared raw-trade-tape key (`"venue:SYM@trades"`, per-(venue, symbol), feeding both Tick/Volume charts'
/// `tick_volume_aggregators` AND SP2 orderflow's `orderflow_aggregators` — never per-window, so it can't be diffed against a
/// per-window live set this way). `"@trades"` keys are explicitly excluded here so a symbol's
/// shared trade tape is never mistaken for an orphaned chart feed; a Tick/Volume-interval
/// compare's `tick_volume_aggregators` entry is a pre-existing, documented gap this doesn't touch either (see
/// `reap_orphaned_feeds`'s doc).
pub(crate) fn orphaned_feed_keys(
    wins: &[window::WindowState],
    spawned: &HashSet<String>,
) -> Vec<String> {
    let live = live_window_keys(wins);
    spawned
        .iter()
        .filter(|k| !k.ends_with("@trades") && !live.contains(k.as_str()))
        .cloned()
        .collect()
}

/// The pure live-window-key set that [`orphaned_feed_keys`] diffs `spawned` against, split out so
/// the same "which chart keys does a window still back" notion can also drive reaping orphaned
/// tick/volume (`tick_volume_aggregators`) and orderflow (`orderflow_aggregators`) AGGREGATOR entries in `reap_orphaned_feeds` — an
/// `tick_volume_aggregators`/`orderflow_aggregators` key IS a window's own [`crate::ui::window::WindowState::key`], so a map entry whose key isn't in this
/// set has no window backing it and must be dropped (the coupled half of the trade-feed leak; see
/// [`orphaned_trade_feed_keys`]). Membership rules are exactly as documented on [`orphaned_feed_keys`]:
/// every Chart window's primary key UNION its Compare symbols UNION its foreign-source study symbols,
/// over ALL windows regardless of `open`/`minimized`.
pub(crate) fn live_window_keys(wins: &[window::WindowState]) -> HashSet<String> {
    let mut live: HashSet<String> = HashSet::new();
    // Chart windows only: every other kind drives no `ensure_feed` of its own (see
    // [`orphaned_feed_keys`]).
    for w in wins.iter().filter(|w| w.kind == window::WindowKind::Chart) {
        live.insert(w.key());
        for c in &w.compare {
            live.insert(chart_key::series_key(&c.venue, &c.symbol, &w.interval));
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
                live.insert(chart_key::series_key(&src.venue, &src.symbol, &w.interval));
            }
        }
    }
    live
}

/// The trade-feed half of the feed-leak fix: which spawned `"…@trades"` raw-trade-tape feeds does
/// NO remaining tick/volume (`tick_volume_aggregators`) or orderflow (`orderflow_aggregators`) aggregator still need. A trade feed is
/// **shared** per `(venue, symbol)` across every aggregator on that pair (a Tick chart, a Volume
/// chart, and an orderflow overlay on the same symbol all ride one `subscribe_trades` stream), so it
/// may only be stopped once NO aggregator references it — hence `needed` is the union of the
/// `(venue, symbol)` of every REMAINING aggregator, computed by the caller AFTER it has already
/// dropped dead aggregator entries. Any spawned `@trades` key whose parsed `(venue, symbol)` is not
/// in `needed` is returned for teardown; every non-`@trades` (kline) key is ignored (that's
/// [`orphaned_feed_keys`]'s job).
///
/// The `(venue, symbol)` is parsed by [`chart_key::split_series_key`], the inverse of the
/// [`chart_key::series_key`] `ensure_trade_feed_on` writes the key with:
/// `"{venue}:{symbol}@trades"` ⇒ `(venue, symbol)`.
pub(crate) fn orphaned_trade_feed_keys(
    needed: &HashSet<(String, String)>,
    spawned: &HashSet<String>,
) -> Vec<String> {
    spawned
        .iter()
        .filter_map(|k| {
            let pair = trade_key_pair(k)?;
            if needed.contains(&pair) { None } else { Some(k.clone()) }
        })
        .collect()
}

/// Parse a raw-trade-tape key back into the `(venue, symbol)` [`ensure_trade_feed_on`] built it
/// from, or `None` for any key that is not a trade key (a kline key, or a string naming no venue).
/// Shared by [`orphaned_trade_feed_keys`] and [`reap_orphaned_feeds`]'s `unroutable` sweep so the
/// two can never disagree about which feed a key names; the split itself is
/// [`chart_key::split_series_key`]'s, the one parse of the key.
fn trade_key_pair(key: &str) -> Option<(String, String)> {
    match chart_key::split_series_key(key)? {
        (venue, symbol, chart_key::TRADE_TAPE_INTERVAL) => {
            Some((venue.to_string(), symbol.to_string()))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The imperative half: the five `App` methods that actually start and stop feeds.
// ---------------------------------------------------------------------------------------------

/// The venue the cockpit's book/trade streams live on — [`ensure_polymarket_book`]'s feed key, and
/// therefore the ONE venue [`reap_orphaned_trade_cockpit_streams`] routes a cockpit unsubscribe to.
const POLYMARKET_VENUE: &str = "polymarket";

/// C2 tidy FIX 3: unit tests for the pure [`orphaned_feed_keys`] diff — see its doc for why
/// this is tested standalone rather than through a full `App` (`WindowState` itself needs no
/// `eframe::CreationContext`, just
/// plain `egui::Rect` data, same as `workspace`'s own `WindowState` tests).
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

/// Tests for the IMPERATIVE half — [`ensure_feed_on`], [`ensure_trade_feed_on`], [`ensure_depth`],
/// [`ensure_polymarket_book`] and [`reap_orphaned_feeds`]. None of these ever ran in any gate while they
/// were `App` methods in `vike-app`'s `main.rs`; they are exercised here against a recording
/// `FakeFeed` that implements the real [`DataClient`] trait, so a change to *which* venue a
/// subscribe/unsubscribe is routed to, or to the order the `spawned`/`subscriptions`/`charts` slots are
/// freed in, fails the merge gate.
///
/// The priority scenario is **teardown → re-add**
/// (`teardown_then_re_add_genuinely_restarts_the_feed`): the reaper must free every slot a later
/// `ensure_*` checks, or a re-added chart comes back permanently dead — the same class of silent
/// loss as bug 4 in [`core_sync`](crate::ui::core_sync)'s module doc.
#[path = "feed_slot_tests.rs"]
#[cfg(test)]
mod feed_slot_tests;
