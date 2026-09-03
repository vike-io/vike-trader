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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};
    use vike_data::live::{LiveDataError, SubscriptionId};

    /// `(symbol, family as membership reported it AT subscribe time)` — the ordering evidence.
    type SubscribeLog = Rc<RefCell<Vec<(String, Option<String>)>>>;

    /// Records, at the moment of each subscribe, what [`Membership`] said about that symbol — which
    /// is exactly how the publish-before-subscribe ordering is pinned: the store's resolver reads
    /// the same map at flush time, and the first frame can arrive the instant this returns.
    struct CountingClient {
        next: u64,
        venue: String,
        membership: Membership,
        /// Shared so a test reads it after the feed is moved into the runtime.
        subscribed: SubscribeLog,
    }

    impl CountingClient {
        fn issue(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
            self.next += 1;
            let fam = self.membership.family_of(&self.venue, symbol);
            self.subscribed.borrow_mut().push((symbol.to_string(), fam));
            Ok(SubscriptionId(self.next))
        }
    }

    impl DataClient for CountingClient {
        fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
            Err(LiveDataError::Unsupported("no bars"))
        }
        fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(s)
        }
        fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(s)
        }
        fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
            self.issue(s)
        }
        fn unsubscribe(&mut self, _id: SubscriptionId) {}
        fn shutdown(&mut self) {}
    }

    /// A scripted feed: `script` is consulted per tick, so a test drives rotation and outages by
    /// wall-clock value.
    struct ScriptedFeed {
        venue: String,
        family: Option<String>,
        client: CountingClient,
        script: Rc<dyn Fn(i64) -> Result<Vec<&'static str>, String>>,
        /// A stream this venue delivers as a side effect of another, so subscribing it separately
        /// would duplicate rows — the Polymarket `Book`-implies-`Quotes` shape.
        redundant: Option<Stream>,
    }

    impl VenueFeed for ScriptedFeed {
        fn venue(&self) -> &str {
            &self.venue
        }
        fn family(&self) -> Option<&str> {
            self.family.as_deref()
        }
        fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
            (self.script)(now_ms).map(|v| v.into_iter().map(String::from).collect())
        }
        fn client(&mut self) -> &mut dyn DataClient {
            &mut self.client
        }
        fn narrow(&self, requested: &[Stream]) -> Vec<Stream> {
            requested.iter().copied().filter(|s| Some(*s) != self.redundant).collect()
        }
    }

    /// Returns the feed plus the shared `(symbol, family-at-subscribe-time)` log.
    fn feed(
        membership: &Membership,
        family: Option<&str>,
        script: impl Fn(i64) -> Result<Vec<&'static str>, String> + 'static,
    ) -> (Box<ScriptedFeed>, SubscribeLog) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let f = Box::new(ScriptedFeed {
            venue: "polymarket".into(),
            family: family.map(String::from),
            client: CountingClient {
                next: 0,
                venue: "polymarket".into(),
                membership: membership.clone(),
                subscribed: seen.clone(),
            },
            script: Rc::new(script),
            redundant: None,
        });
        (f, seen)
    }

    #[test]
    fn a_family_is_published_to_membership_before_its_symbols_are_subscribed() {
        let m = Membership::new();
        let (f, seen) = feed(&m, Some("btc-5m"), |_| Ok(vec!["UP", "DOWN"]));
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
        rt.add_feed(f);

        rt.tick(1_000);

        // The client logged what membership said AT each subscribe. Both tokens were already mapped,
        // so a frame arriving on the very first read commits into the group rather than per-symbol.
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(
            seen.iter().all(|(_, f)| f.as_deref() == Some("btc-5m")),
            "membership was not published before subscribing: {seen:?}"
        );
    }

    /// The outage case. An empty desired set and an unresolvable one look the same to a naive loop
    /// and mean opposite things: one is "this family has no live market", the other is "I do not
    /// know". Reconciling the second would unsubscribe every live book.
    #[test]
    fn a_resolution_failure_leaves_every_subscription_untouched() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |now| {
            if now < 2_000 {
                Ok(vec!["UP", "DOWN"])
            } else {
                Err("gamma: connection reset".into())
            }
        });
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
        rt.add_feed(f);

        rt.tick(1_000);
        assert_eq!(rt.subscription_count(), 2);

        let out = rt.tick(3_000);

        assert!(matches!(out[0], FeedTick::ResolveFailed { .. }), "{out:?}");
        assert_eq!(rt.subscription_count(), 2, "the live books survived the outage");
        assert_eq!(
            m.family_of("polymarket", "UP").as_deref(),
            Some("btc-5m"),
            "membership must not be cleared either — the rows still arriving belong to the family"
        );
    }

    /// The distinction the previous test depends on: a family that genuinely resolves to nothing
    /// DOES release its subscriptions.
    #[test]
    fn a_family_that_resolves_to_nothing_does_unsubscribe() {
        let m = Membership::new();
        let (f, _) =
            feed(
                &m,
                Some("btc-5m"),
                |now| {
                    if now < 2_000 {
                        Ok(vec!["UP", "DOWN"])
                    } else {
                        Ok(vec![])
                    }
                },
            );
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
        rt.add_feed(f);

        rt.tick(1_000);
        rt.tick(3_000);

        assert_eq!(rt.subscription_count(), 0);
        assert!(m.is_empty(), "and the membership map is cleared, not left routing dead tokens");
    }

    #[test]
    fn a_rotation_republishes_membership_and_moves_the_subscriptions() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |now| {
            if now < 2_000 {
                Ok(vec!["UP", "DOWN"])
            } else {
                Ok(vec!["NEW"])
            }
        });
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
        rt.add_feed(f);

        rt.tick(1_000);
        let out = rt.tick(3_000);

        match &out[0] {
            FeedTick::Reconciled { report, symbols, .. } => {
                assert_eq!(*symbols, 1);
                assert_eq!(report.stopped.len(), 2);
                assert_eq!(report.started.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.family_of("polymarket", "NEW").as_deref(), Some("btc-5m"));
        // INVERTED (2026-08-02): this asserted `None` for the departed token, and that assertion was
        // the bug, pinned — a rotation must not strand rows still buffered for the old window. See
        // `Membership::set_family`'s retire note. "Moved" is now `len()`: UP is no longer LIVE.
        assert_eq!(
            m.family_of("polymarket", "UP").as_deref(),
            Some("btc-5m"),
            "retired, so its last flush still lands in the family"
        );
        assert_eq!(m.len(), 1, "only NEW is live");
    }

    /// An explicit symbol list has no family, so nothing is published and its rows stay per-symbol —
    /// the `config` module's documented split, enforced here rather than only described.
    #[test]
    fn an_explicit_symbol_subscription_publishes_no_membership() {
        let m = Membership::new();
        let (f, _) = feed(&m, None, |_| Ok(vec!["BTCUSDT"]));
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
        rt.add_feed(f);

        rt.tick(1_000);

        assert_eq!(rt.subscription_count(), 1);
        assert!(m.is_empty(), "no family ⇒ no group ⇒ per-symbol series");
    }

    /// One feed's outage must not stall the others — a Gamma blip cannot stop a Binance recording.
    #[test]
    fn one_feeds_failure_does_not_block_the_others() {
        let m = Membership::new();
        let (bad, _) = feed(&m, Some("bad"), |_| Err("down".into()));
        let (good, _) = feed(&m, Some("good"), |_| Ok(vec!["UP"]));
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Book]);
        rt.add_feed(bad);
        rt.add_feed(good);

        let out = rt.tick(1_000);

        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], FeedTick::ResolveFailed { .. }));
        assert!(matches!(out[1], FeedTick::Reconciled { .. }));
        assert_eq!(rt.subscription_count(), 1);
    }

    /// The duplicate-rows guard. On Polymarket `subscribe_book` already emits derived L1 quotes, so
    /// a profile asking for quotes AND book must NOT open a second quote pump — it would derive the
    /// same L1 from its own copy of the book and write every quote row twice, into the customer's
    /// tape, where it later reads as double volume.
    #[test]
    fn a_venue_can_drop_a_stream_it_already_delivers_via_another() {
        let m = Membership::new();
        let (mut f, seen) = feed(&m, Some("btc-5m"), |_| Ok(vec!["UP"]));
        f.redundant = Some(Stream::Quotes);
        let mut rt = RecorderRuntime::new(m, vec![Stream::Quotes, Stream::Trades, Stream::Book]);
        rt.add_feed(f);

        rt.tick(1_000);

        assert_eq!(rt.subscription_count(), 2, "trades + book, no separate quote pump");
        assert_eq!(seen.borrow().len(), 2);
    }

    // -- the never-resolved distinction, and the dry-run predicate ---------------------------------

    /// **The measured failure, as data.** A proxy nothing is listening behind: every tick fails to
    /// resolve, and the feed has never once produced a symbol. `last_nonempty_ms` is `None`, which
    /// is what separates this from a mid-run blip — and the daemon ran forever in this state
    /// logging one `warn!` a tick while recording nothing.
    #[test]
    fn a_feed_that_never_resolved_reports_no_last_nonempty() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |_| Err("network: io: Connection refused".into()));
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(f);

        for now in [1_000, 31_000, 61_000] {
            let out = rt.tick(now);
            assert_eq!(out[0].last_nonempty_ms(), None, "never resolved, at t={now}");
            assert_eq!(out[0].live(), 0);
        }
    }

    /// …and the mirror image, which must NOT escalate: a feed that resolved and then hit an outage
    /// carries the timestamp of when it last worked. `runtime`'s module doc and
    /// `a_resolution_failure_leaves_every_subscription_untouched` are why that is a retry.
    #[test]
    fn a_feed_that_resolved_then_failed_keeps_its_last_nonempty() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |now| {
            if now < 2_000 {
                Ok(vec!["UP", "DOWN"])
            } else {
                Err("gamma: connection reset".into())
            }
        });
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(f);

        assert_eq!(rt.tick(1_000)[0].last_nonempty_ms(), Some(1_000));
        let out = rt.tick(3_000);
        assert!(matches!(out[0], FeedTick::ResolveFailed { .. }));
        assert_eq!(
            out[0].last_nonempty_ms(),
            Some(1_000),
            "it worked at t=1_000 — a retry, not a gap"
        );
        assert_eq!(out[0].live(), 2, "…and its books are still live");
    }

    /// ⚠ **The QUIETER sibling, and the reason the field tracks NON-EMPTY rather than `Ok`.** A
    /// family that resolves to zero symbols returns `Ok`, reconciles to nothing, and produces a
    /// quiet report the daemon does not even log. Tracking `Ok` would leave it invisible.
    #[test]
    fn a_family_that_only_ever_resolves_to_nothing_reports_no_last_nonempty() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("typo-5m"), |_| Ok(vec![]));
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(f);

        let out = rt.tick(1_000);
        assert!(
            matches!(out[0], FeedTick::Reconciled { .. }),
            "it is an Ok, not an error: {out:?}"
        );
        assert_eq!(out[0].last_nonempty_ms(), None, "…and it has still never produced a symbol");
        assert_eq!(out[0].live(), 0);
        match &out[0] {
            FeedTick::Reconciled { report, .. } => {
                assert!(report.is_quiet(), "the daemon prints NOTHING for this tick: {report:?}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// A healthy tick proves the dry run can pass at all — without this the predicate below could
    /// be "always fail" and every other assertion would still hold.
    #[test]
    fn a_dry_run_over_a_working_feed_reports_nothing() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |_| Ok(vec!["UP", "DOWN"]));
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(f);

        let failures = dry_run_failures(&rt.tick(1_000));
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// **The false green this whole change exists to remove.** The exact the CI box measurement: the
    /// resolver refuses, `--once` printed one WARN and exited 0, and an operator scripting
    /// `--once && systemctl enable --now` got a green light on a total resolve failure.
    #[test]
    fn a_dry_run_over_an_unresolvable_feed_reports_the_venue_and_the_reason() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |_| Err("network: io: Connection refused".into()));
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(f);

        let failures = dry_run_failures(&rt.tick(1_000));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("polymarket"), "the VENUE is named: {}", failures[0]);
        assert!(
            failures[0].contains("Connection refused"),
            "…and the resolver's own reason, so a three-venue box is diagnosable from this one \
             line: {}",
            failures[0]
        );
    }

    /// The zero-symbol case fails the dry run too, and the message must carry the rotation caveat —
    /// the one accepted false positive, named rather than papered over.
    #[test]
    fn a_dry_run_over_a_family_that_resolves_to_nothing_fails_with_the_rotation_caveat() {
        let m = Membership::new();
        let (f, _) = feed(&m, Some("btc-5m"), |_| Ok(vec![]));
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(f);

        let failures = dry_run_failures(&rt.tick(1_000));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("NO live subscription"), "{}", failures[0]);
        assert!(failures[0].contains("re-run"), "the caveat is in the MESSAGE: {}", failures[0]);
    }

    /// The third arm: the venue accepted the resolve and then REFUSED the subscribe. Built as data
    /// rather than driven through a client, because [`dry_run_failures`] is pure and the arm is
    /// about the REPORT, not about how the report was produced.
    ///
    /// `ReconcileReport::failed`'s own doc says a non-empty list is not yet a fault for a running
    /// daemon — it is retried next pass — and that is exactly why a DRY RUN must judge it: a dry
    /// run has one pass, and the operator is standing there.
    #[test]
    fn a_dry_run_fails_when_the_venue_refused_a_subscribe() {
        let tick = FeedTick::Reconciled {
            venue: "binance".into(),
            symbols: 1,
            report: ReconcileReport {
                started: vec![("BTCUSDT".into(), Stream::Trades)],
                failed: vec![("BTCUSDT".into(), Stream::Book, "socket reconnecting".into())],
                ..Default::default()
            },
            live: 1,
            last_nonempty_ms: Some(1_000),
        };

        let failures = dry_run_failures(&[tick]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("BTCUSDT"), "{}", failures[0]);
        assert!(failures[0].contains("socket reconnecting"), "{}", failures[0]);
    }

    /// A three-venue profile with one dead venue must FAIL, and the passing venues must not hide
    /// it — "any venue resolved" is the weaker predicate that would pass this exact profile.
    #[test]
    fn a_dry_run_fails_on_one_dead_venue_among_healthy_ones() {
        let m = Membership::new();
        let (good, _) = feed(&m, Some("good"), |_| Ok(vec!["UP"]));
        let (bad, _) = feed(&m, Some("bad"), |_| Err("down".into()));
        let (also_good, _) = feed(&m, Some("also"), |_| Ok(vec!["DOWN"]));
        let mut rt = RecorderRuntime::new(m, vec![Stream::Book]);
        rt.add_feed(good);
        rt.add_feed(bad);
        rt.add_feed(also_good);

        let failures = dry_run_failures(&rt.tick(1_000));
        assert_eq!(failures.len(), 1, "exactly the dead one: {failures:?}");
    }

    #[test]
    fn stop_all_releases_every_feed() {
        let m = Membership::new();
        let (a, _) = feed(&m, Some("f1"), |_| Ok(vec!["UP", "DOWN"]));
        let mut rt = RecorderRuntime::new(m.clone(), vec![Stream::Quotes, Stream::Book]);
        rt.add_feed(a);
        rt.tick(1_000);
        assert_eq!(rt.subscription_count(), 4);

        rt.stop_all();

        assert_eq!(rt.subscription_count(), 0);
    }

    // -- the two-phase stop ------------------------------------------------------------------------

    /// How long one simulated socket takes to notice a raised stop flag — the fake's stand-in for a
    /// venue's read timeout (2 s in production; `crates/vike-bridge-core/src/pump_spec.rs`'s
    /// `market_pump_spec`). Small enough to keep the test quick, large enough that twelve of them
    /// SERIALLY (1.8 s) is unmistakably distinct from one (150 ms) on a loaded runner.
    const WIND_DOWN: Duration = Duration::from_millis(150);

    /// A client that models the one property that makes the teardown cost what it costs: a socket
    /// starts winding down when its flag is RAISED, and a join blocks until that wind-down is over.
    ///
    /// So `unsubscribe` sleeps until `raised_at + WIND_DOWN` — no longer. Raise every flag first and
    /// the whole set costs ONE wind-down; raise-and-join one at a time and it costs N. That is the
    /// entire finding, expressed as something a clock can measure.
    struct WindDownClient {
        next: u64,
        raised_at: Option<Instant>,
        /// Shared teardown trace: `"raise"` / `"join"`, in the order they happened, across ALL feeds.
        trace: Rc<RefCell<Vec<&'static str>>>,
    }

    impl DataClient for WindDownClient {
        fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
            Err(LiveDataError::Unsupported("no bars"))
        }
        fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
            self.next += 1;
            Ok(SubscriptionId(self.next))
        }
        fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
            self.subscribe_quotes(s)
        }
        fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
            self.subscribe_quotes(s)
        }
        fn begin_shutdown(&mut self) {
            self.trace.borrow_mut().push("raise");
            self.raised_at.get_or_insert_with(Instant::now);
        }
        fn unsubscribe(&mut self, _id: SubscriptionId) {
            self.trace.borrow_mut().push("join");
            // A join on a socket whose flag was never raised waits the FULL wind-down starting now —
            // exactly what the one-loop teardown paid, once per socket.
            let raised = *self.raised_at.get_or_insert_with(Instant::now);
            let ready = raised + WIND_DOWN;
            let now = Instant::now();
            if now < ready {
                std::thread::sleep(ready - now);
            }
        }
        fn shutdown(&mut self) {
            self.begin_shutdown();
        }
    }

    struct WindDownFeed {
        venue: String,
        client: WindDownClient,
    }

    impl VenueFeed for WindDownFeed {
        fn venue(&self) -> &str {
            &self.venue
        }
        fn family(&self) -> Option<&str> {
            None
        }
        fn desired(&mut self, _now_ms: i64) -> Result<BTreeSet<String>, String> {
            Ok(["A", "B", "C"].iter().map(|s| s.to_string()).collect())
        }
        fn client(&mut self) -> &mut dyn DataClient {
            &mut self.client
        }
    }

    /// ⚠ **The teardown-cost finding, as a measurement.** Twelve sockets across two venues: the stop
    /// must cost about ONE wind-down, not twelve.
    ///
    /// The one-loop `stop_all` (unsubscribe-and-join, one subscription at a time) paid a read
    /// timeout PER SOCKET — 2 s each in production — outside the daemon's bounded region, so a
    /// growing profile walked the recorder's total stop time through its unit's `TimeoutStopSec=`
    /// and SIGKILL landed on the final Parquet flush.
    ///
    /// MUTATION PROOF: delete the phase-1 loop in [`RecorderRuntime::stop_all`] and this goes red on
    /// the elapsed assertion (12 wind-downs ≈ 1.8 s against a 450 ms ceiling); the ordering
    /// assertion below goes red at the same time, and it is the one that cannot be flaky.
    #[test]
    fn stop_all_raises_every_flag_before_it_joins_anything() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut rt = RecorderRuntime::new(Membership::new(), vec![Stream::Quotes, Stream::Trades]);
        for venue in ["venue-a", "venue-b"] {
            rt.add_feed(Box::new(WindDownFeed {
                venue: venue.into(),
                client: WindDownClient { next: 0, raised_at: None, trace: Rc::clone(&trace) },
            }));
        }
        rt.tick(1_000);
        assert_eq!(rt.subscription_count(), 12, "3 symbols x 2 streams x 2 venues");

        let started = Instant::now();
        rt.stop_all();
        let elapsed = started.elapsed();

        assert_eq!(rt.subscription_count(), 0, "every subscription must still be released");

        // The ordering is the property; the clock is the consequence. Every raise must precede
        // every join, ACROSS feeds — a per-feed raise-then-join would give the parallelism back one
        // venue at a time and this assertion is what notices.
        let trace = trace.borrow();
        let first_join = trace.iter().position(|s| *s == "join").expect("joins must have happened");
        let last_raise =
            trace.iter().rposition(|s| *s == "raise").expect("raises must have happened");
        assert!(
            last_raise < first_join,
            "every feed's stop flag must be RAISED before the first join — trace: {trace:?}"
        );

        assert!(
            elapsed < WIND_DOWN * 3,
            "twelve sockets must cost about ONE wind-down ({WIND_DOWN:?}), not twelve: took \
             {elapsed:?}. In production that timeout is 2 s per socket, and the difference is \
             whether the recorder's stop fits inside its unit's TimeoutStopSec= before SIGKILL cuts \
             the flush."
        );
        assert!(
            elapsed >= WIND_DOWN,
            "…and the fake must actually have simulated a wind-down: took {elapsed:?}"
        );
    }
}
