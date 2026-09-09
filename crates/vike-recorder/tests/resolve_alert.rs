//! **A venue that never resolves a symbol is LOUD.** Driven through the real production tick
//! (`vike_recorder::alerts::resolve_tick`) over the crate's PUBLIC surface — the same call the
//! daemon binary makes, with the same arguments.
//!
//! ## What this exists to stop happening again
//!
//! A clean-install validation on the CI box followed `docs/ops/recorder-deploy.md` literally and got a
//! daemon that **recorded nothing and reported success**. The whole of its output was one line per
//! tick:
//!
//! ```text
//! WARN recorder: could not resolve the desired set — subscriptions unchanged venue=polymarket \
//!   error=network: io: Connection refused
//! ```
//!
//! `POLY_PROXY_ENABLED` defaults ON (Polymarket is geo-blocked; that decision is closed), so a
//! fresh install's most likely state is "proxy enabled, proxy not reachable" — and in that state
//! the daemon ran forever at `warn`, raising no alert. The silence watchdog could not help: it
//! diffs against what the runtime SUBSCRIBED, and this venue subscribed nothing, so its expected
//! set was empty and it reported nothing at 2 seconds or at 2 days.
//!
//! The one variable that changed in the A/B was `POLY_PROXY_ENABLED=false`, which produced
//! `subscriptions changed venue=polymarket symbols=4 started=8 stopped=0 failed=0`. Both states
//! exited 0; `crates/vike-recorder/src/runtime.rs`'s `dry_run_failures` is the other half of this
//! repair, and it is unit-tested there.
//!
//! Everything here is plain data: no store, no venue, no network, and `now_ms` is a parameter all
//! the way down.

use std::sync::{Arc, Mutex};

use vike_alerting::{AlertSink, FiredAlert};
use vike_recorder::Alerting;
use vike_recorder::alerts::{RecorderAlerts, resolve_tick};
use vike_recorder::liveness::ResolveWatch;
use vike_recorder::runtime::FeedTick;
use vike_recorder::session::ReconcileReport;

/// A sink that records what was delivered — the same double `tests/silence_alert.rs` uses, and
/// deliberately blind to `AlertTargets` (the recorder's rules set `in_process: false`, so an
/// `InProcessSink` would legitimately see nothing here and prove nothing about the wiring).
#[derive(Clone, Default)]
struct Pager(Arc<Mutex<Vec<FiredAlert>>>);

impl AlertSink for Pager {
    fn deliver(&self, alert: &FiredAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

impl Pager {
    fn delivered(&self) -> Vec<FiredAlert> {
        self.0.lock().unwrap().clone()
    }
}

fn mounted(cfg: &Alerting) -> (RecorderAlerts, Pager) {
    let pager = Pager::default();
    (RecorderAlerts::mount(cfg, Vec::new()).with_sink(Box::new(pager.clone())), pager)
}

/// The measured failure: the resolver refuses and the feed has never produced a symbol.
fn refused(venue: &str) -> FeedTick {
    FeedTick::ResolveFailed {
        venue: venue.to_string(),
        error: "network: io: Connection refused".into(),
        live: 0,
        last_nonempty_ms: None,
    }
}

/// The retry shape: the same refusal, on a feed that HAS resolved before and still holds live books.
fn refused_after_working(venue: &str, worked_at_ms: i64) -> FeedTick {
    FeedTick::ResolveFailed {
        venue: venue.to_string(),
        error: "gamma: connection reset".into(),
        live: 8,
        last_nonempty_ms: Some(worked_at_ms),
    }
}

fn healthy(venue: &str, now_ms: i64) -> FeedTick {
    FeedTick::Reconciled {
        venue: venue.to_string(),
        symbols: 4,
        report: ReconcileReport::default(),
        live: 8,
        last_nonempty_ms: Some(now_ms),
    }
}

/// The five-minute default, in seconds — what `--silent-secs` resolves to, and the grace this
/// watchdog shares with its series-level sibling.
const GRACE_SECS: u64 = 300;

/// **The deliverable.** A venue that has never resolved, past its grace, produces a delivered alert
/// NAMING it — not a `warn!` nothing can observe.
#[test]
fn a_venue_that_never_resolves_delivers_an_alert_naming_it() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    let ticks = [refused("polymarket")];

    // Inside the grace: a feed is unresolved BY DEFINITION on the tick it is mounted, so reporting
    // it at once would fire on every startup of every box.
    assert!(resolve_tick(&mut watch, &mut alerts, &ticks, 0, GRACE_SECS).is_empty());
    assert!(pager.delivered().is_empty(), "no false positive on startup");

    // Past it: this state does not fix itself.
    let unresolved = resolve_tick(&mut watch, &mut alerts, &ticks, 301_000, GRACE_SECS);
    assert_eq!(unresolved, vec!["polymarket".to_string()]);

    let delivered = pager.delivered();
    assert_eq!(delivered.len(), 1, "exactly one alert: {delivered:?}");
    assert_eq!(delivered[0].rule_id, "recorder-venue-unresolved");
    assert!(
        delivered[0].body.contains("polymarket"),
        "the alert must NAME the venue: {}",
        delivered[0].body
    );
}

/// ⚠ **The retry case, which must stay a `warn!` and must NOT page.** `FeedTick::ResolveFailed`'s
/// "this is a retry, not a gap" is correct for a venue that resolved before: the daemon leaves its
/// live books alone on purpose, and paging for a Gamma blip is how a pager gets muted.
///
/// MUTATION PROOF for the whole never-vs-stopped distinction: make `ResolveWatch::check` ignore
/// `last_nonempty_ms` (report every feed with no live subscription this tick) and this test goes
/// red while the one above stays green.
#[test]
fn a_venue_that_resolved_once_never_pages_however_long_it_has_been_failing() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    let ticks = [refused_after_working("polymarket", 1_000)];

    for tick in 0..10 {
        let now = 1_000 + tick * 3_600_000; // ten HOURS of failing resolves
        assert!(
            resolve_tick(&mut watch, &mut alerts, &ticks, now, GRACE_SECS).is_empty(),
            "a venue that worked once is on the retry path at t={now}"
        );
    }
    assert!(pager.delivered().is_empty(), "…and nothing paged: {:?}", pager.delivered());
}

/// ⚠ **The QUIETER sibling.** A family that resolves to zero symbols returns `Ok`, subscribes
/// nothing, and produces a `ReconcileReport::is_quiet` report the daemon does not even log — not
/// one WARN, ever. It must escalate exactly like the refusal above.
#[test]
fn a_family_that_only_ever_resolves_to_nothing_also_pages() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    let ticks = [FeedTick::Reconciled {
        venue: "binance".into(),
        symbols: 0,
        report: ReconcileReport::default(),
        live: 0,
        last_nonempty_ms: None,
    }];
    assert!(match &ticks[0] {
        FeedTick::Reconciled { report, .. } => report.is_quiet(),
        _ => false,
    });

    let _ = resolve_tick(&mut watch, &mut alerts, &ticks, 0, GRACE_SECS);
    assert_eq!(resolve_tick(&mut watch, &mut alerts, &ticks, 301_000, GRACE_SECS).len(), 1);
    assert_eq!(pager.delivered().len(), 1, "the quietest failure reaches the pager too");
}

/// A healthy venue produces nothing, ever — the steady state this must not be noisy in.
#[test]
fn a_healthy_venue_delivers_nothing() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    for tick in 0..20 {
        let now = tick * 30_000;
        assert!(
            resolve_tick(&mut watch, &mut alerts, &[healthy("polymarket", now)], now, GRACE_SECS)
                .is_empty()
        );
    }
    assert!(pager.delivered().is_empty());
}

/// A three-venue box with one dead venue pages for exactly that one, and the healthy venues do not
/// mask it — the strictness that "any venue resolved" would have thrown away.
#[test]
fn one_dead_venue_among_healthy_ones_still_pages_and_only_it_does() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    let ticks = [healthy("binance", 0), refused("polymarket"), healthy("okx", 0)];

    let _ = resolve_tick(&mut watch, &mut alerts, &ticks, 0, GRACE_SECS);
    assert_eq!(
        resolve_tick(&mut watch, &mut alerts, &ticks, 301_000, GRACE_SECS),
        vec!["polymarket".to_string()]
    );
    let delivered = pager.delivered();
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(delivered[0].body.contains("polymarket"), "{}", delivered[0].body);
}

/// THREE dead venues page THREE times. One rule covers every venue, and `AlertRule::cooldown_ms` is
/// per RULE — so without the producer-side gate in `ResolveWatch::alertable` a three-venue outage
/// would page once and read to an operator as a single-venue fault. That is the CI box's six-series
/// silence episode, one layer up.
#[test]
fn every_unresolved_venue_of_one_episode_reaches_the_pager() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    let ticks = [refused("polymarket"), refused("binance"), refused("aster")];

    let _ = resolve_tick(&mut watch, &mut alerts, &ticks, 0, GRACE_SECS);
    assert_eq!(resolve_tick(&mut watch, &mut alerts, &ticks, 301_000, GRACE_SECS).len(), 3);
    assert_eq!(pager.delivered().len(), 3, "all three, not just the first");
}

/// The pager is rate-limited per venue across ticks (the daemon ticks every 30 s and a
/// misconfiguration lasts until somebody fixes it), while the returned set is reported every tick.
#[test]
fn a_persisting_misconfiguration_pages_once_but_keeps_reporting_every_tick() {
    let (mut alerts, pager) = mounted(&Alerting::default()); // repeat_secs = 3600
    let mut watch = ResolveWatch::new();
    let ticks = [refused("polymarket")];

    // The mount tick: the grace runs from here, so it must exist before any of the assertions.
    assert!(resolve_tick(&mut watch, &mut alerts, &ticks, 0, GRACE_SECS).is_empty());

    for tick in 0..10 {
        let now = 301_000 + tick * 30_000;
        assert_eq!(
            resolve_tick(&mut watch, &mut alerts, &ticks, now, GRACE_SECS).len(),
            1,
            "still unresolved at tick {tick} — always reported"
        );
    }
    assert_eq!(pager.delivered().len(), 1, "…but paged ONCE inside the repeat window");
}

/// `--silent-secs 0` disables BOTH watchdogs: no judgement, no log, no alert. An operator who turns
/// the alarm off must get silence, not a quieter alarm. (The `--once` dry run is unaffected — it
/// judges the tick's content directly, never through this watch.)
#[test]
fn a_disabled_watchdog_delivers_nothing_at_all() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = ResolveWatch::new();
    assert!(
        resolve_tick(&mut watch, &mut alerts, &[refused("polymarket")], 86_400_000, 0).is_empty()
    );
    assert!(pager.delivered().is_empty());
}

/// ⚠ A profile that scoped its SERIES alerts to one family must not thereby silence the venue that
/// produced no family at all. `series_prefix` is a prefix on `{kind}/{venue}/{symbol}`, and a venue
/// that never resolved has no series to match — so the feed rule deliberately ignores it.
#[test]
fn a_series_prefix_scope_does_not_silence_the_venue_alert() {
    let cfg = Alerting { series_prefix: Some("book/polymarket/".into()), ..Default::default() };
    let (mut alerts, pager) = mounted(&cfg);
    let mut watch = ResolveWatch::new();
    let ticks = [refused("binance")]; // a venue the prefix does not even mention

    let _ = resolve_tick(&mut watch, &mut alerts, &ticks, 0, GRACE_SECS);
    assert_eq!(resolve_tick(&mut watch, &mut alerts, &ticks, 301_000, GRACE_SECS).len(), 1);
    assert_eq!(
        pager.delivered().len(),
        1,
        "the feed rule is not series-scoped: {:?}",
        pager.delivered()
    );
}
