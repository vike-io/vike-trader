//! `runtime` — the venue-free daemon tick: resolve each subscription's current symbols, publish
//! their family membership, and drive the venue client to exactly that set.
//!
//! One [`VenueFeed`] per profile subscription. The trait is the ONLY venue-aware surface in this
//! crate; everything here is tested against a scripted feed with no network, and the binary picks
//! the concrete venue behind a Cargo feature.
//!
//! ## Two orderings this file exists to get right
//!
//! **Membership is published BEFORE the subscription is made.** The [`Membership`] map is what the
//! store's `GroupResolver` consults at flush time, so a token that starts streaming before its
//! family is published resolves to `None` and its first rows are committed PER-SYMBOL — silently
//! splitting one family into a grouped series plus a scatter of single-symbol ones, which is
//! exactly the layout grouping exists to avoid. Publishing first costs nothing and cannot race.
//!
//! **A resolution failure changes NOTHING.** If a family's symbols cannot be resolved this tick (a
//! Gamma outage, a DNS blip), the desired set is UNKNOWN, not empty — and reconciling against an
//! empty set would unsubscribe every live book and punch a hole in the tape at the exact moment the
//! venue is already unhappy. The tick skips that feed entirely and retries next time. This mirrors
//! `vike_polymarket::discovery::RollingWindowPlanner::plan`, which aborts its whole tick on a
//! resolver error for the same reason.

use std::collections::BTreeSet;

use vike_data::live::DataClient;

use crate::membership::Membership;
use crate::session::{ReconcileReport, Stream, SubscriptionSet};

/// One profile subscription's venue side: what should be recorded right now, and the client to
/// record it from.
///
/// Implemented per venue in the binary. A rotating family (Polymarket) recomputes
/// [`desired`](VenueFeed::desired) from the clock; a static one (every USDT perp) returns the same
/// set forever — spec §8.2's "families are general" claim is exactly that one trait covers both.
pub trait VenueFeed {
    /// The venue key, as it appears in the store's `venue=` partition.
    fn venue(&self) -> &str;

    /// The family this subscription records as ONE grouped series, or `None` for an explicit symbol
    /// list (which stays per-symbol — grouping only pays across a wide subscription).
    fn family(&self) -> Option<&str>;

    /// The symbols that should be streaming at `now_ms`.
    ///
    /// `Err` means UNKNOWN, never empty: the tick leaves this feed's subscriptions untouched. A
    /// legitimately empty family — between Polymarket windows there may be no live token — is
    /// `Ok(empty)`, and that DOES unsubscribe.
    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String>;

    /// The live client. Held by the feed so that a venue needing a handshake owns its own lifetime.
    fn client(&mut self) -> &mut dyn DataClient;

    /// Narrow the profile's REQUESTED streams to what this venue must actually subscribe in order to
    /// deliver them. Default: exactly what was requested.
    ///
    /// This hook exists because subscribing the requested set literally writes duplicate rows on a
    /// real venue. Polymarket's `subscribe_book` already emits derived L1 quotes (`PumpMode::Book`
    /// → `sink.book()` **and** `sink.quote()`), so also subscribing `Quotes` opens a SECOND socket
    /// deriving the same L1 from its own copy of the book, and every quote row is written twice —
    /// silently, into the customer's tape, where it later reads as double volume. A venue knows this
    /// about itself; the profile that says "record quotes and book" does not, and should not have to.
    fn narrow(&self, requested: &[Stream]) -> Vec<Stream> {
        requested.to_vec()
    }
}

/// What one [`RecorderRuntime::tick`] did to one feed.
///
/// ⚠ Every variant carries `live` and `last_nonempty_ms`, and BOTH exist because a tick that
/// produced nothing is otherwise indistinguishable from a tick that produced nothing *this once*.
/// See [`FeedTick::last_nonempty_ms`] — the field is the whole of that distinction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedTick {
    /// Resolved and reconciled.
    Reconciled {
        venue: String,
        symbols: usize,
        report: ReconcileReport,
        /// Live subscriptions on this feed AFTER the reconcile.
        live: usize,
        /// See [`FeedTick::last_nonempty_ms`].
        last_nonempty_ms: Option<i64>,
    },
    /// Resolution failed — subscriptions left exactly as they were, retried next tick.
    ResolveFailed {
        venue: String,
        error: String,
        /// Live subscriptions on this feed, UNCHANGED by the failure (that is the whole point of
        /// the failure arm — see the module doc).
        live: usize,
        /// See [`FeedTick::last_nonempty_ms`].
        last_nonempty_ms: Option<i64>,
    },
}

impl FeedTick {
    pub fn venue(&self) -> &str {
        match self {
            FeedTick::Reconciled { venue, .. } | FeedTick::ResolveFailed { venue, .. } => venue,
        }
    }

    /// Live subscriptions on this feed after the tick.
    pub fn live(&self) -> usize {
        match self {
            FeedTick::Reconciled { live, .. } | FeedTick::ResolveFailed { live, .. } => *live,
        }
    }

    /// When this feed last resolved a NON-EMPTY desired set — `None` = it never has, since startup.
    ///
    /// ⚠ **`None` and `Some` are two different faults, and collapsing them is how the measured
    /// failure stayed silent.** A venue that resolved and then stopped is a retry (a Gamma blip, a
    /// DNS wobble) and the daemon's per-tick `warn!` is the right reaction. A venue that has NEVER
    /// resolved is a MISCONFIGURATION — a proxy nothing is listening behind, a family name that
    /// matches no market — and it will not fix itself: the daemon runs forever recording nothing
    /// while logging one warning a tick. That is exactly
    /// [`crate::liveness::Silent::silent_for_ms`]'s never-vs-stopped distinction, lifted from a
    /// SERIES to a FEED.
    ///
    /// ⚠ It tracks the last NON-EMPTY resolve, not the last `Ok`. A family that resolves to zero
    /// symbols returns `Ok(empty)`, reconciles to nothing, and produces a
    /// [`ReconcileReport::is_quiet`] report the daemon does not even log — strictly quieter than
    /// the failure this field was added for, and reachable from a typo in a family name (on binance
    /// `Target::Family` caches the empty resolution for the process lifetime).
    pub fn last_nonempty_ms(&self) -> Option<i64> {
        match self {
            FeedTick::Reconciled { last_nonempty_ms, .. }
            | FeedTick::ResolveFailed { last_nonempty_ms, .. } => *last_nonempty_ms,
        }
    }
}

/// Why a `--once` DRY RUN proved nothing — one line per feed that ended the tick with no live
/// subscription or with a failed subscribe. Empty ⇒ every subscribed feed is really recording.
///
/// **Why the dry run is stricter than the daemon.** `crate`'s binary deliberately keeps a running
/// daemon lenient: a quiet or unresolvable venue must not kill a process recording five healthy
/// ones (that is `--exit-on-silence`'s whole argument, and it is off by default). `--once` is the
/// opposite situation — a hand-run commissioning check of the operator's OWN profile, with the
/// operator standing there, where every line in the profile is something they asserted they want
/// recorded. So a partially-working profile is a misconfiguration to fix now, and the verdict has
/// to reach the EXIT CODE: `docs/ops/recorder-deploy.md` tells an operator `--once` "proves … each
/// family resolves to live symbols, and the venue accepted the subscriptions", and a run that
/// resolved NOTHING exited 0 (measured on the CI box, with a proxy nothing was listening behind).
///
/// Evaluated on the TICK'S CONTENT, never through [`crate::liveness::ResolveWatch`]: that watch's
/// grace is `--silent-secs` (300 s by default) and a dry run is ONE tick, so routing this through
/// it would make every `--once` pass green again for a new reason.
///
/// ⚠ **One accepted false positive, and it is named in the message rather than papered over.** A
/// rotating family genuinely between windows resolves `Ok(empty)` and fails this check. It is rare
/// rather than routine — the Polymarket planner targets the current PLUS next window — and the
/// remedy is to re-run, which is what the text says. Weakening the predicate instead would hand
/// back the false green this exists to remove.
pub fn dry_run_failures(ticks: &[FeedTick]) -> Vec<String> {
    let mut out = Vec::new();
    for t in ticks {
        match t {
            FeedTick::ResolveFailed { venue, error, .. } => out.push(format!(
                "{venue}: could not resolve any symbol — {error}. Nothing is subscribed, so nothing \
                 would be recorded for this venue."
            )),
            FeedTick::Reconciled { venue, live: 0, .. } => out.push(format!(
                "{venue}: resolved to NO live subscription. For a ROTATING family this can be a \
                 genuine gap between windows — re-run. Otherwise the family name matches no market \
                 on this venue, or every stream it asked for was refused."
            )),
            FeedTick::Reconciled { venue, report, .. } if !report.failed.is_empty() => {
                let what: Vec<String> = report
                    .failed
                    .iter()
                    .map(|(sym, stream, err)| format!("{sym}/{}: {err}", stream.as_str()))
                    .collect();
                out.push(format!(
                    "{venue}: {} subscribe(s) the venue refused — {}",
                    report.failed.len(),
                    what.join("; ")
                ));
            }
            FeedTick::Reconciled { .. } => {}
        }
    }
    out
}

/// The daemon's subscription state: one [`VenueFeed`] + its [`SubscriptionSet`] per profile
/// subscription, over a shared [`Membership`].
///
/// Owns NO clock and NO thread — [`tick`](RecorderRuntime::tick) takes `now_ms` — so the binary owns
/// the cadence and these tests own the clock.
pub struct RecorderRuntime {
    feeds: Vec<Feed>,
    membership: Membership,
    streams: Vec<Stream>,
}

/// One profile subscription's runtime state.
///
/// `last_nonempty_ms` is the only field that is not obvious: it is the per-feed clock the
/// never-resolved escalation is built on, and nothing else in this daemon could answer the question
/// (`SubscriptionSet::is_empty()` is equally true for a venue that never resolved, a family
/// legitimately between windows, and the first tick of a healthy startup).
struct Feed {
    feed: Box<dyn VenueFeed>,
    subs: SubscriptionSet,
    /// See [`FeedTick::last_nonempty_ms`].
    last_nonempty_ms: Option<i64>,
}

impl RecorderRuntime {
    /// `membership` is the SAME map the store's `GroupResolver` was built from — that shared handle
    /// is what makes a rotation visible to the next flush.
    pub fn new(membership: Membership, streams: Vec<Stream>) -> Self {
        Self { feeds: Vec::new(), membership, streams }
    }

    pub fn add_feed(&mut self, feed: Box<dyn VenueFeed>) {
        self.feeds.push(Feed { feed, subs: SubscriptionSet::new(), last_nonempty_ms: None });
    }

    pub fn feed_count(&self) -> usize {
        self.feeds.len()
    }

    /// Total live subscriptions across every feed — the daemon's status line.
    pub fn subscription_count(&self) -> usize {
        self.feeds.iter().map(|f| f.subs.len()).sum()
    }

    /// Re-evaluate every feed at `now_ms`.
    pub fn tick(&mut self, now_ms: i64) -> Vec<FeedTick> {
        let mut out = Vec::with_capacity(self.feeds.len());
        for f in &mut self.feeds {
            let venue = f.feed.venue().to_string();
            let desired = match f.feed.desired(now_ms) {
                Ok(d) => d,
                // UNKNOWN, not empty. Leave this feed exactly as it is.
                Err(error) => {
                    out.push(FeedTick::ResolveFailed {
                        venue,
                        error,
                        live: f.subs.len(),
                        last_nonempty_ms: f.last_nonempty_ms,
                    });
                    continue;
                }
            };

            // ⚠ NON-EMPTY, not `Ok`. A family that resolves to zero symbols is the quieter of the
            // two failures this field exists for — see `FeedTick::last_nonempty_ms`.
            if !desired.is_empty() {
                f.last_nonempty_ms = Some(now_ms);
            }

            // Publish membership BEFORE subscribing, so no row can arrive while its family is
            // unresolved and be committed per-symbol.
            if let Some(family) = f.feed.family() {
                let symbols: Vec<String> = desired.iter().cloned().collect();
                self.membership.set_family(&venue, family, &symbols);
            }

            let streams = f.feed.narrow(&self.streams);
            let report = f.subs.reconcile(f.feed.client(), &desired, &streams);
            out.push(FeedTick::Reconciled {
                venue,
                symbols: desired.len(),
                report,
                live: f.subs.len(),
                last_nonempty_ms: f.last_nonempty_ms,
            });
        }
        out
    }

    /// Every live subscription across every feed, as store-series keys — the "expected" half of the
    /// silence watchdog ([`crate::liveness::silent_series`]). See
    /// [`SubscriptionSet::series_keys`](crate::session::SubscriptionSet::series_keys).
    pub fn expected_series(&self) -> Vec<String> {
        self.feeds.iter().flat_map(|f| f.subs.series_keys(f.feed.venue())).collect()
    }

    /// [`expected_series`](Self::expected_series) folded through [`Membership`] — every live
    /// subscription as `(series key, FAMILY key)`, the input
    /// `crate::liveness::SilenceWatch::family_collapse` judges.
    ///
    /// **The family key is the whole idea.** A Polymarket instrument lives ~600 s and then dies ON
    /// PURPOSE — 576 legitimate deaths a day on the deployed profile — so no per-instrument rule
    /// can tell a death from a collapse, and the family key is the only subject with CONTINUOUS
    /// EXISTENCE across a rotation: a dying member's tail and its successor's birth land in one
    /// total, so the rotation is invisible by construction rather than by an exemption somebody
    /// maintains.
    ///
    /// `{kind}/{venue}/{family}` — which is also the store's own grouping, so the key in an alert
    /// is the partition path an operator goes and looks at.
    ///
    /// ⚠ Resolution is here, in the RUNTIME, and not inside the judgement: `crate::liveness` is the
    /// pure layer and must not read a `RwLock`. Two properties make the fold total rather than
    /// best-effort, and both belong to code above: [`tick`](Self::tick) publishes membership BEFORE
    /// subscribing, so a symbol is mapped before it can have a series key; and `Membership`'s
    /// `RETIRE_GRACE` keeps a departed symbol resolving for 90 s, so `family_of` still answers at
    /// the far end of a rotation. A symbol with no family at all — an explicit per-symbol
    /// subscription, which is how a static venue is usually spelled in a profile — maps to its OWN
    /// series key, which degenerates the rule to a one-member family with no special case anywhere.
    /// ⚠ That is a property of the SUBSCRIPTION and not of the venue —
    /// `crates/vike-recorder/src/resolve.rs`'s `CatalogGlob` declares one too, so a binance family
    /// subscription folds exactly like a Polymarket one. `Membership`'s own doc
    /// already frames rotation as the general case and static as the degenerate one; this is that
    /// claim being taken at its word.
    pub fn expected_families(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for f in &self.feeds {
            let venue = f.feed.venue();
            for (key, symbol) in f.subs.series_keys_with_symbols(venue) {
                let family = match self.membership.family_of(venue, &symbol) {
                    // The kind is the key's own first segment — a family is per (kind, venue,
                    // family), never per venue: a `book` collapse and a `trade` collapse on one
                    // Polymarket family are two different faults, and on 2026-08-05 they happened
                    // together, which is exactly the case the licence has to survive.
                    Some(fam) => {
                        let kind = key.split('/').next().unwrap_or_default();
                        format!("{kind}/{venue}/{fam}")
                    }
                    None => key.clone(),
                };
                out.push((key, family));
            }
        }
        out
    }

    /// Unsubscribe everything, on every feed — the daemon's stop path. The store sink is flushed
    /// separately by its own handle; this only stops new rows arriving.
    ///
    /// ⚠ **Two phases, and the split is the whole cost model.** A feed thread learns it should stop
    /// on its next socket read timeout — 2 s on every on-driver venue
    /// (`crates/vike-bridge-core/src/pump_spec.rs`'s `market_pump_spec`) — and an unsubscribe BLOCKS
    /// on that thread's join. The one-loop version of this method (unsubscribe-and-join, one
    /// subscription at a time) therefore cost **one read timeout per socket**, serially, and a
    /// binance family subscription is two threads per symbol: a handful of symbols walked the
    /// recorder's total stop time straight through its unit's `TimeoutStopSec=`, where SIGKILL lands
    /// on the final Parquet flush — the exact outcome the bounded teardown exists to prevent.
    ///
    /// Phase one raises every flag on every feed and joins nothing
    /// ([`DataClient::begin_shutdown`]); phase two then does the exact per-subscription release,
    /// whose joins collect threads that have all been winding down since phase one. The whole
    /// profile costs about ONE wind-down rather than one per socket. It is
    /// `vike_data::live::FeedRegistry::shutdown`'s own raise-all-then-join idiom, lifted one level
    /// so it spans the several clients a recorder holds.
    pub fn stop_all(&mut self) {
        // PHASE 1 — raise, join nothing. This must complete for EVERY feed before the first join
        // below, or the parallelism it exists for is given back one feed at a time.
        for f in &mut self.feeds {
            f.feed.client().begin_shutdown();
        }
        // PHASE 2 — the exact per-subscription release + bookkeeping, joining threads that are
        // already on their way out.
        for f in &mut self.feeds {
            f.subs.stop_all(f.feed.client());
        }
    }
}

#[path = "runtime_tests.rs"]
#[cfg(test)]
mod runtime_tests;
