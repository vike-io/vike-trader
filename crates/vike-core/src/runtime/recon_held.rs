//! Held-divergence ANNOUNCEMENT state — the pure half of "a HELD reconcile divergence is a STATE,
//! not an EVENT".
//!
//! ## The defect this exists for
//! `crate::runtime`'s `CoreThread::reconcile_reports` used to `tracing::warn!` once per HELD alert
//! per PASS. Under `VIKE_RECONCILE_POLICY=quarantine` (what the the CI box daemon runs) a divergence
//! that nothing heals — an externally-opened venue position, say — is re-diffed, re-resolved and
//! re-held on every interval, forever. Measured on the live the CI box box: **396 identical WARN lines
//! on 2026-08-25 and 345 on 2026-08-24, every one of them the SAME bybit `PositionOnlyExternal`**,
//! one a minute, indefinitely. That is not an alert, it is a screen saver — and the one line an
//! operator actually needs (a NEW divergence appearing) lands in the middle of it looking exactly
//! like its 396 neighbours.
//!
//! So the fix is not "log less"; it is to log the TRANSITIONS of a set instead of its contents:
//!
//! - a divergence ENTERING the held set warns once, on the pass it appears;
//! - a divergence LEAVING it (the venue stopped reporting it) is announced once, as news;
//! - an UNCHANGED held set says nothing at all, except a periodic backlog summary on
//!   [`HELD_SUMMARY_INTERVAL_MS`] — much longer than the reconcile interval — so "still held" stays
//!   visible without being repeated at pass cadence.
//!
//! Nothing here changes what folds, what holds, or the policy that decides: the held-alert store
//! (`recon_alerts`), its confirm ids, and the published `CoreSnapshot` projection are untouched.
//! This module decides only what reaches the LOG.
//!
//! ## Off the fold
//! Every entry point is reached from `reconcile_reports` (interval cadence, per venue) and
//! `confirm_recon` (an operator command). Neither is the per-message fold the `p99 < 10µs` core-hop
//! gate measures, and nothing here is touched from `dispatch`/`publish_to` — see
//! `crates/vike-core/CLAUDE.md`. The state is one `BTreeMap` keyed by venue that is EMPTY (and
//! allocation-free) on any box with no held divergences, and self-prunes the moment a venue's held
//! set empties, so it is bounded by the LIVE divergence set rather than by uptime.

use std::collections::{BTreeMap, BTreeSet};

use vike_exec::DivergenceKind;
use vike_model::events::Event;

/// How long a venue's held set may sit UNCHANGED before one summary line re-states the backlog.
///
/// One hour, against a reconcile interval whose default is 60s (`VIKE_RECONCILE_INTERVAL_MS`): a
/// steady, already-announced divergence therefore costs 24 lines a day instead of ~1440, and the
/// operator still cannot lose track of a non-empty backlog by walking away from the terminal. It is
/// deliberately NOT derived from the reconcile interval — the point is a cadence the reader
/// experiences as occasional, and an interval-relative multiple would go back to per-minute noise
/// the moment somebody set a fast interval for a debugging session.
pub(crate) const HELD_SUMMARY_INTERVAL_MS: i64 = 3_600_000;

/// The IDENTITY of one held divergence — what makes two held divergences the SAME one recurring
/// rather than two different ones.
///
/// ## Why the key is (venue, kind, instance) and not the alert id
/// The held-alert id is minted fresh by every pass for an un-keyed alert (that is the very reason
/// the CI box accumulated one row and one WARN per minute), so it identifies a RAISE, never a
/// divergence. `vike_exec::recon::ReconAlert` carries no identity field of its own beyond
/// `dedup_key`, which only SOME kinds set. So the identity is assembled here, from exactly what the
/// alert makes available at this site:
///
/// - **`venue`** — a reconcile pass is per-venue and the held set is diffed per venue, so two
///   venues can never collide.
/// - **`kind`** — `DivergenceKind`, the coarsest thing an operator reads off the line.
/// - **`instance`** — the discriminator, built by [`HeldId::new`]:
///   1. `dedup_key` ALONE when `resolve` supplied one (`position:{symbol}:{side}`,
///      `balance:{asset}`, an `UnknownOrder`'s venue order id, the aggregated orphan-order key).
///      That field IS `vike_exec`'s own declared "this recurs by design, here is its stable name",
///      so where it exists it is complete and nothing is appended to it — appending would let a
///      changing payload SPLIT a row `resolve` intended to be one, and it is what makes this key a
///      strict generalization of the `(venue, kind, dedup_key)` match the held-alert store used
///      before it existed.
///   2. otherwise the alert `detail` — which for the generic held path is only the kind name
///      (`format!("{:?}", d.kind())`), i.e. carries no instance at all — PLUS the instrument legs
///      of the alert's `proposed_events`: each `Event::Fill` rendered as
///      `[trade_id/]symbol/position_side/side/last_qty`, sorted and deduped.
///
/// Those legs are what keeps two genuinely different divergences apart. A `PositionOnlyExternal` on
/// BTCUSDT and one on ETHUSDT produce byte-identical `detail`s ("PositionOnlyExternal") and no
/// `dedup_key`, so without them they would collapse into one announcement and the second symbol
/// would never be reported. The legs are also carried onto the log line, so the two announcements
/// are distinguishable to a reader and not just to this map.
///
/// ## What is deliberately EXCLUDED, and why
/// `ts`, `client_order_id`, `last_px` — and `trade_id` for SOME kinds, which is the subtle half.
///
/// `ts`/`client_order_id` because `vike_exec::recon::resolve`'s `synth_position_legs` mints them
/// from the PASS CLOCK, so an unchanged external position produces brand-new ones every single
/// pass; keying on them reproduces the exact defect this module exists to remove. `last_px` because
/// a venue may re-derive an average entry price (funding, fee accrual, rounding) without anything
/// about the divergence having changed, and a price wobble is not news. `last_qty` is KEPT: an
/// external position that changes SIZE is a real state change.
///
/// ⚠ **`trade_id` is per-KIND, and getting it wrong loses money in one direction and spams in the
/// other** — see [`trade_ids_are_stable`], which is an exhaustive match precisely so a new kind
/// cannot silently inherit either answer. A `MissingFill`'s synthesized fill carries the VENUE's
/// own trade id, so two genuinely different missed fills that agree on symbol/side/size are told
/// apart ONLY by it — collapsing those would leave an operator able to confirm one of them and
/// silently lose the other's fill, which
/// `crates/vike-core/tests/recon/recon_quarantine.rs`'s
/// `recurring_unknown_order_alert_dedupes_and_clears_after_confirm` caught when this key first
/// excluded it wholesale. A position leg's id is the pass clock in disguise and must stay out.
///
/// ## The accepted residual, named
/// Two POSITION-kind divergences that agree on venue, kind, symbol, side and size collapse into
/// one — which is to say, the same position. Their log lines are byte-identical too, so the
/// collapse costs the reader nothing a second line would have told them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HeldId {
    pub(crate) venue: String,
    pub(crate) kind: DivergenceKind,
    pub(crate) instance: String,
}

impl HeldId {
    /// Build the identity from the raw pieces of a held alert. Takes the pieces rather than a
    /// `&ReconAlert` so `confirm_recon` can rebuild the identical key from its stored
    /// `HeldReconAlert` — the two must agree or a confirmed-then-recurring divergence would go
    /// unannounced.
    pub(crate) fn new(
        venue: &str,
        kind: DivergenceKind,
        dedup_key: Option<&str>,
        detail: &str,
        proposed: &[Event],
    ) -> Self {
        // A `dedup_key` is `resolve`'s OWN declared identity for a by-design recurring divergence,
        // so it answers alone — see this type's doc. Only an un-keyed alert needs legs.
        if let Some(key) = dedup_key {
            return Self { venue: venue.to_string(), kind, instance: key.to_string() };
        }
        let mut instance = detail.to_string();
        // Instrument legs: the ONLY per-instance information an un-keyed alert carries. Fills are
        // the only event shape `resolve` proposes that names an instrument; anything else (an
        // adoption `OrderAccepted`) contributes nothing here.
        let stable_ids = trade_ids_are_stable(kind);
        let mut legs: Vec<String> = proposed
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => {
                    let id = if stable_ids { f.trade_id.as_str() } else { "" };
                    Some(format!(
                        "{}/{}/{:?}/{}/{}",
                        id, f.symbol, f.position_side, f.side, f.last_qty
                    ))
                }
                _ => None,
            })
            .collect();
        legs.sort();
        legs.dedup();
        if !legs.is_empty() {
            instance.push(' ');
            instance.push_str(&legs.join(","));
        }
        Self { venue: venue.to_string(), kind, instance }
    }
}

/// What one venue's pass did to the held set. Everything the caller logs comes from here — the
/// holder itself never touches `tracing`, which is what makes it testable without a subscriber.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct HeldTransitions {
    /// Divergences that were NOT held before this pass and are now. One WARN each.
    pub(crate) newly_held: BTreeSet<HeldId>,
    /// Divergences this pass no longer reports. One INFO each — a divergence going away is news.
    pub(crate) cleared: Vec<HeldId>,
    /// The periodic backlog re-statement, `Some` only on a pass where nothing changed AND
    /// [`HELD_SUMMARY_INTERVAL_MS`] has elapsed since the last thing this venue said.
    pub(crate) summary: Option<HeldSummary>,
}

impl HeldTransitions {
    /// True when this pass has nothing at all to say — the steady state, and the whole point.
    ///
    /// ⚠ **This verdict answers for the per-PASS line too, not only for the three lines above.**
    /// `crate::runtime`'s `CoreThread::reconcile_reports` closes with an INFO `reconcile pass
    /// folded venue=… events=… alerts=…` that fired on every pass carrying ANY alert — which
    /// under `quarantine` is every pass forever, one a minute per venue, `events=0 alerts=1`
    /// repeating identically (the CI box, 2026-08-25, 11:41:43 and 11:42:43). That is the same
    /// state-mistaken-for-an-event defect as the WARN flood, one severity down: it is INFO, so on
    /// a daemon running `VIKE_LOG_FILE_LEVEL=warn` it floods the journal rather than the log file,
    /// which is why it was deferred out of the WARN fix rather than shipped with it.
    ///
    /// It reuses THIS verdict rather than growing a second rate limiter beside it: there is one
    /// answer to "did this venue's held set do anything this pass", one clock behind it, and the
    /// periodic pass line therefore lands beside the summary that armed it instead of on a cadence
    /// of its own that could drift out of step. The caller ORs in its own folded-event count —
    /// folding is a fact about the pass, not about the held set, and a pass that changed the book
    /// is never suppressed.
    pub(crate) fn silent(&self) -> bool {
        self.newly_held.is_empty() && self.cleared.is_empty() && self.summary.is_none()
    }
}

/// The periodic "the backlog is still non-empty" line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeldSummary {
    /// Distinct held divergences for this venue (see [`HeldId`]'s residual note).
    pub(crate) held: usize,
    /// `Kind xN` per kind, comma separated, kind-ordered — so the line says WHAT is waiting, not
    /// only how much.
    pub(crate) kinds: String,
    /// How long this venue has been silent, in core-clock ms. Lets the line say "unchanged for 1h"
    /// instead of implying something just happened.
    pub(crate) quiet_ms: i64,
}

/// Per-venue held state. Removed from the map entirely once its set empties, so a box with nothing
/// held carries no rows and a later re-raise announces from a clean slate.
#[derive(Debug, Default)]
struct VenueHeld {
    held: BTreeSet<HeldId>,
    /// Core-clock ms of the last thing this venue SAID (a transition or a summary). `None` only
    /// between `or_default()` and the first `observe` decision.
    last_announce_ms: Option<i64>,
}

/// The announcement state for every venue, owned by `CoreThread` and touched only on the reconcile
/// boundary. `Default` is empty and allocates nothing.
#[derive(Debug, Default)]
pub(crate) struct HeldAnnouncer {
    venues: BTreeMap<String, VenueHeld>,
}

impl HeldAnnouncer {
    /// Fold ONE venue's pass: `raised` is the identity of every alert that pass held (both the
    /// newly-appended ones and the dedup-refreshed ones — a refreshed row is still held). Returns
    /// what changed, and re-arms the summary clock.
    ///
    /// `now_ms` is the core clock (`engine.now_ms`), which is event-driven and therefore not
    /// guaranteed monotonic across a venue that goes quiet and resumes; a backwards step re-arms
    /// the clock rather than firing, so a clock jump can never turn the summary back into
    /// per-pass noise.
    pub(crate) fn observe(
        &mut self,
        venue: &str,
        now_ms: i64,
        raised: BTreeSet<HeldId>,
    ) -> HeldTransitions {
        let mut summary = None;
        let entry = self.venues.entry(venue.to_string()).or_default();
        let newly_held: BTreeSet<HeldId> = raised.difference(&entry.held).cloned().collect();
        let cleared: Vec<HeldId> = entry.held.difference(&raised).cloned().collect();
        entry.held = raised;
        let changed = !newly_held.is_empty() || !cleared.is_empty();
        // Nothing left held ⇒ the `cleared` lines ARE this pass's announcement and the venue row
        // goes away with them (done below, once the borrow ends).
        let now_empty = entry.held.is_empty();
        if !now_empty {
            match entry.last_announce_ms {
                // Quiet pass on a known clock: the summary is the only thing that can fire, and
                // only once the interval has fully elapsed. Not re-arming below the interval is
                // what makes the cadence the SUMMARY's rather than the pass's.
                Some(last) if !changed && now_ms >= last => {
                    let quiet = now_ms.saturating_sub(last);
                    if quiet >= HELD_SUMMARY_INTERVAL_MS {
                        summary = Some(HeldSummary {
                            held: entry.held.len(),
                            kinds: render_kinds(&entry.held),
                            quiet_ms: quiet,
                        });
                        entry.last_announce_ms = Some(now_ms);
                    }
                }
                // Something changed (the transition IS the line — no summary behind it), or this
                // venue has never spoken, or the core clock ran BACKWARDS. All three re-arm from
                // this reading.
                _ => entry.last_announce_ms = Some(now_ms),
            }
        }
        if now_empty {
            self.venues.remove(venue);
        }
        HeldTransitions { newly_held, cleared, summary }
    }

    /// Drop one identity because an operator CONFIRMED its held alert. Without this, a confirm that
    /// does not actually resolve the divergence (the venue keeps reporting it) would re-raise a
    /// fresh alert row in silence — the operator would have acted, seen nothing, and have no way to
    /// learn their action did not take. Forgetting makes the next pass announce it again as news.
    pub(crate) fn forget(&mut self, id: &HeldId) {
        let Some(entry) = self.venues.get_mut(&id.venue) else { return };
        entry.held.remove(id);
        if entry.held.is_empty() {
            self.venues.remove(&id.venue);
        }
    }

    /// Held identities for one venue — test/inspection only. `pub(crate)` so the runtime-wiring
    /// suite can assert the store size and this count AGREE (they must: one identity, one row).
    #[cfg(test)]
    pub(crate) fn held(&self, venue: &str) -> usize {
        self.venues.get(venue).map_or(0, |v| v.held.len())
    }
}

/// Does this kind's proposed `Event::Fill` carry a trade id that IDENTIFIES the divergence, or one
/// minted from the pass clock?
///
/// ⚠ **Exhaustive on purpose — do not add a `_` arm.** A new `DivergenceKind` must be classified
/// here, because both wrong answers are silent and neither is safe:
///
/// * saying `true` for a clock-minted id re-announces (and, since the held-alert store shares this
///   key, re-APPENDS a row) on every single pass, forever — the the CI box defect this module exists to
///   remove;
/// * saying `false` for a real venue id COLLAPSES two genuinely different divergences into one
///   row, so an operator confirms one of them and the other's events are silently lost.
///
/// The classification is a property of how `vike_exec::recon::resolve` SYNTHESIZES each kind's
/// events, so it is cited per arm rather than guessed from the kind's name.
fn trade_ids_are_stable(kind: DivergenceKind) -> bool {
    match kind {
        // `resolve`'s `events_for` copies the VENUE's own `FillReport::trade_id` onto the
        // synthesized fill. It is the venue's execution id — stable across passes, and the ONLY
        // thing telling two same-symbol/side/size missed fills apart.
        DivergenceKind::MissingFill => true,
        // `adoption_events` uses `adoption_trade_id`, a pure function of venue + venue order id
        // (its own doc says so, and says why). Stable. In practice this kind is always
        // dedup-keyed, so the answer never decides anything — classified anyway, because "it
        // cannot reach here today" is exactly the reasoning that rots.
        DivergenceKind::UnknownOrder => true,
        // ⚠ `synth_position_legs` bakes the report's `ts` into both the trade id and the coid
        // (`EXT-POS-{venue}-{symbol}-{ts}-{i}`) — a new id every pass for an UNCHANGED position.
        // Its own comment flags the instability as known and deliberately unchanged. Keep it out.
        DivergenceKind::PositionDrift | DivergenceKind::PositionOnlyExternal => false,
        // These propose no `Event::Fill` at all (empty event lists, or a `BalanceDrift`'s
        // `AccountState`), so no leg is ever rendered and the answer cannot matter. `false` is the
        // conservative side: it can only ever merge, never spam.
        DivergenceKind::MissingTerminal
        | DivergenceKind::OrphanLocalOrder
        | DivergenceKind::OrphanLocalPosition
        | DivergenceKind::BalanceDrift
        | DivergenceKind::JournalDivergence => false,
    }
}

/// `Kind xN` per kind, kind-ordered.
fn render_kinds(held: &BTreeSet<HeldId>) -> String {
    let mut counts: BTreeMap<DivergenceKind, usize> = BTreeMap::new();
    for id in held {
        *counts.entry(id.kind).or_insert(0) += 1;
    }
    counts.into_iter().map(|(k, n)| format!("{k:?} x{n}")).collect::<Vec<_>>().join(", ")
}

/// The ONE warn a newly-held divergence gets. The message text is unchanged from the pre-fix line
/// (operators and log queries key on it); `instance` is new, and is what tells two same-kind
/// divergences on one venue apart.
pub(crate) fn warn_newly_held(alert_id: u64, id: &HeldId, detail: &str) {
    tracing::warn!(
        alert = alert_id,
        venue = %id.venue,
        kind = ?id.kind,
        instance = %id.instance,
        detail = %detail,
        "reconcile divergence HELD — awaiting operator confirm"
    );
}

/// A held divergence the venue stopped reporting. INFO, not WARN: it is the good transition, and it
/// is the one an operator otherwise has no way to observe at all.
pub(crate) fn info_cleared(id: &HeldId) {
    tracing::info!(
        target: "vike_core::reconcile",
        venue = %id.venue,
        kind = ?id.kind,
        instance = %id.instance,
        "reconcile divergence no longer reported — was HELD, now absent from this venue's pass"
    );
}

/// The periodic backlog re-statement. WARN, because an unattended backlog still needs an operator.
pub(crate) fn warn_summary(venue: &str, s: &HeldSummary) {
    tracing::warn!(
        venue = %venue,
        held = s.held,
        kinds = %s.kinds,
        quiet_ms = s.quiet_ms,
        "reconcile divergences still HELD — unchanged since the last line, awaiting operator confirm"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::{FillEvent, LiquiditySide, PositionSide, TradeId};

    /// One reconcile pass's worth of clock (the `VIKE_RECONCILE_INTERVAL_MS` default).
    const PASS_MS: i64 = 60_000;

    /// A synthesized position leg exactly as `vike_exec::recon::resolve`'s `synth_position_legs`
    /// mints one: the `trade_id` and the `client_order_id` BAKE IN the pass clock, so every pass
    /// produces different ones for an unchanged position. That churn is the defect's engine and is
    /// reproduced here on purpose.
    fn ext_pos_leg(venue: &str, symbol: &str, qty: f64, ts: i64) -> Event {
        Event::Fill(FillEvent {
            trade_id: TradeId::prefixed("EXT-POS-", format_args!("{venue}-{symbol}-{ts}-0")),
            client_order_id: format!("EXT-{venue}-POS-{symbol}-{ts}-0"),
            venue: ustr::ustr(venue),
            symbol: ustr::ustr(symbol),
            side: 1,
            last_qty: qty,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: ustr::ustr(""),
            liquidity_side: LiquiditySide::Unknown,
            ts,
            mark_price: None,
            position_side: PositionSide::Both,
        })
    }

    /// The the CI box alert: `PositionOnlyExternal`, no dedup key, detail = the kind name.
    fn ext_pos_id(venue: &str, symbol: &str, qty: f64, ts: i64) -> HeldId {
        HeldId::new(
            venue,
            DivergenceKind::PositionOnlyExternal,
            None,
            "PositionOnlyExternal",
            &[ext_pos_leg(venue, symbol, qty, ts)],
        )
    }

    fn set(ids: impl IntoIterator<Item = HeldId>) -> BTreeSet<HeldId> {
        ids.into_iter().collect()
    }

    /// THE regression pin for the identity choice: the same unchanged external position, resolved
    /// on two different passes, is ONE identity — even though its synthesized `trade_id`, its
    /// `client_order_id` and its `ts` all differ. Keying on any of those would re-announce forever,
    /// which is the measured the CI box defect.
    #[test]
    fn an_unchanged_external_position_keeps_one_identity_across_passes() {
        let a = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000);
        let b = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000 + PASS_MS);
        assert_eq!(a, b, "ts/trade_id/coid churn must not create a second identity");
        assert!(a.instance.contains("BTCUSDT"), "the symbol must be IN the key: {}", a.instance);
        assert!(
            !a.instance.contains("EXT-POS-"),
            "the ts-baked trade id must NOT be: {}",
            a.instance
        );
    }

    /// The measured defect, as an assertion: 60 passes of the identical bybit
    /// `PositionOnlyExternal` produce ONE announcement, not 60.
    #[test]
    fn the_same_divergence_held_across_many_passes_announces_once() {
        let mut a = HeldAnnouncer::default();
        let mut announced = 0usize;
        let mut silent_passes = 0usize;
        for pass in 0..60 {
            let ts = 1_000 + pass * PASS_MS;
            let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]));
            announced += t.newly_held.len();
            if t.silent() {
                silent_passes += 1;
            }
            assert!(t.cleared.is_empty(), "nothing cleared on pass {pass}");
        }
        assert_eq!(announced, 1, "one divergence, one announcement");
        assert_eq!(silent_passes, 59, "every later pass says nothing");
        assert_eq!(a.held("bybit"), 1);
    }

    /// A NEW divergence is announced on the pass it appears, and only that one — the already-held
    /// neighbour stays quiet. This is the signal the old per-pass repetition buried.
    #[test]
    fn a_new_divergence_announces_immediately_and_alone() {
        let mut a = HeldAnnouncer::default();
        let first = a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]));
        assert_eq!(first.newly_held.len(), 1);

        // ...many quiet passes...
        for pass in 1..10 {
            let ts = 1_000 + pass * PASS_MS;
            assert!(a
                .observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]))
                .silent());
        }

        let ts = 1_000 + 10 * PASS_MS;
        let t = a.observe(
            "bybit",
            ts,
            set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 3.0, ts)]),
        );
        assert_eq!(t.newly_held.len(), 1, "only the newcomer");
        let new = t.newly_held.iter().next().expect("one");
        assert!(new.instance.contains("ETHUSDT"), "the ETH leg is the news: {}", new.instance);
        assert_eq!(a.held("bybit"), 2);
    }

    /// Two same-kind divergences that differ ONLY in symbol must not collapse: the un-keyed alert's
    /// `detail` is the bare kind name, so the instrument legs are the only thing keeping them apart.
    #[test]
    fn two_divergences_differing_only_in_symbol_do_not_collapse() {
        let btc = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000);
        let eth = ext_pos_id("bybit", "ETHUSDT", 0.5, 1_000);
        assert_ne!(btc, eth);
        let mut a = HeldAnnouncer::default();
        let t = a.observe("bybit", 1_000, set([btc, eth]));
        assert_eq!(t.newly_held.len(), 2, "both announced");
        assert_eq!(a.held("bybit"), 2);
    }

    /// An external position that changes SIZE is a real state change, so it announces again.
    #[test]
    fn a_changed_position_size_is_a_new_divergence() {
        let mut a = HeldAnnouncer::default();
        assert_eq!(
            a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]))
                .newly_held
                .len(),
            1
        );
        let ts = 1_000 + PASS_MS;
        let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.9, ts)]));
        assert_eq!(t.newly_held.len(), 1, "0.5 -> 0.9 is news");
        assert_eq!(t.cleared.len(), 1, "...and the old size is gone");
        assert_eq!(a.held("bybit"), 1, "one live divergence, not two");
    }

    /// A divergence the venue stops reporting is OBSERVABLE — the transition an operator had no way
    /// to see at all before, since the held-alert store never self-clears.
    #[test]
    fn a_divergence_that_clears_is_announced_once_and_then_forgotten() {
        let mut a = HeldAnnouncer::default();
        a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]));
        let t = a.observe("bybit", 1_000 + PASS_MS, BTreeSet::new());
        assert_eq!(t.cleared.len(), 1, "the clear is announced");
        assert!(t.newly_held.is_empty());
        assert_eq!(a.held("bybit"), 0, "and the row is pruned");
        let quiet = a.observe("bybit", 1_000 + 2 * PASS_MS, BTreeSet::new());
        assert!(quiet.silent(), "a clear is announced ONCE, not every pass thereafter");
    }

    /// The summary fires on its OWN cadence, not the pass cadence: 24h of one-minute passes over an
    /// unchanging held set produces one line per [`HELD_SUMMARY_INTERVAL_MS`], and the line names
    /// what is waiting.
    #[test]
    fn the_summary_fires_on_its_own_cadence_not_the_pass_cadence() {
        let mut a = HeldAnnouncer::default();
        let day = 24 * 60 * 60 * 1_000;
        let passes = day / PASS_MS; // 1440
        let mut summaries = Vec::new();
        for pass in 0..passes {
            let ts = 1_000 + pass * PASS_MS;
            let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]));
            if let Some(s) = t.summary {
                summaries.push(s);
            }
        }
        let expected = (day / HELD_SUMMARY_INTERVAL_MS) as usize;
        assert_eq!(expected, 24, "sanity: an hourly summary over a day");
        assert_eq!(summaries.len(), expected - 1, "one per interval after the raise re-armed it");
        assert!(!summaries.is_empty(), "a summary DID fire");
        let first = &summaries[0];
        assert_eq!(first.held, 1);
        assert_eq!(first.kinds, "PositionOnlyExternal x1");
        assert!(first.quiet_ms >= HELD_SUMMARY_INTERVAL_MS, "quiet_ms = {}", first.quiet_ms);
        // The whole point, as a rate: 1440 passes, 24 lines.
        assert!(
            summaries.len() * 50 < passes as usize,
            "the summary must be an order of magnitude quieter than the pass cadence"
        );
    }

    /// A transition RE-ARMS the summary clock — a summary must never land right behind a line that
    /// just said the same thing.
    #[test]
    fn a_transition_rearms_the_summary_clock() {
        let mut a = HeldAnnouncer::default();
        a.observe("bybit", 0, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 0)]));
        // One tick short of the interval: nothing.
        let ts = HELD_SUMMARY_INTERVAL_MS - 1;
        assert!(a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)])).silent());
        // A new divergence lands (a transition) — and that re-arms the clock...
        let ts = HELD_SUMMARY_INTERVAL_MS;
        let t = a.observe(
            "bybit",
            ts,
            set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 1.0, ts)]),
        );
        assert_eq!(t.newly_held.len(), 1);
        assert!(t.summary.is_none(), "the transition IS the line; no summary behind it");
        // ...so the next summary is an interval after the TRANSITION, not after the first raise.
        let ts = 2 * HELD_SUMMARY_INTERVAL_MS - 1;
        let t = a.observe(
            "bybit",
            ts,
            set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 1.0, ts)]),
        );
        assert!(t.silent(), "one tick short of the interval after the transition");
        let ts = 2 * HELD_SUMMARY_INTERVAL_MS;
        let t = a.observe(
            "bybit",
            ts,
            set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts), ext_pos_id("bybit", "ETHUSDT", 1.0, ts)]),
        );
        let s = t.summary.expect("the summary is due now");
        assert_eq!(s.held, 2);
        assert_eq!(s.kinds, "PositionOnlyExternal x2");
    }

    /// A backwards core clock (an event-driven `now_ms` on a venue that resumed behind its own last
    /// pass) re-arms rather than firing — a clock jump must not become per-pass noise.
    #[test]
    fn a_backwards_clock_rearms_instead_of_firing() {
        let mut a = HeldAnnouncer::default();
        let far = 10 * HELD_SUMMARY_INTERVAL_MS;
        a.observe("bybit", far, set([ext_pos_id("bybit", "BTCUSDT", 0.5, far)]));
        let t = a.observe("bybit", 0, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 0)]));
        assert!(t.silent(), "backwards clock says nothing");
        let t = a.observe("bybit", 1, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1)]));
        assert!(t.silent(), "...and the clock is re-armed from the new reading");
    }

    /// Venues do not share held state: a bybit divergence cannot silence a binance one, and a
    /// binance pass cannot clear bybit's set.
    #[test]
    fn two_venues_do_not_share_held_state() {
        let mut a = HeldAnnouncer::default();
        assert_eq!(
            a.observe("bybit", 1_000, set([ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000)]))
                .newly_held
                .len(),
            1
        );
        let t = a.observe("binance", 1_000, set([ext_pos_id("binance", "BTCUSDT", 0.5, 1_000)]));
        assert_eq!(t.newly_held.len(), 1, "a different venue's identical shape is its own news");
        assert!(t.cleared.is_empty(), "and it does not clear bybit's");
        assert_eq!(a.held("bybit"), 1);
        assert_eq!(a.held("binance"), 1);
    }

    /// An operator confirm forgets the identity, so a divergence the confirm did NOT resolve
    /// announces again on the next pass instead of re-appearing in silence.
    #[test]
    fn a_confirmed_divergence_reannounces_if_the_next_pass_still_reports_it() {
        let mut a = HeldAnnouncer::default();
        let id = ext_pos_id("bybit", "BTCUSDT", 0.5, 1_000);
        assert_eq!(a.observe("bybit", 1_000, set([id.clone()])).newly_held.len(), 1);
        let ts = 1_000 + PASS_MS;
        assert!(a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)])).silent());

        a.forget(&id);
        assert_eq!(a.held("bybit"), 0);

        let ts = 1_000 + 2 * PASS_MS;
        let t = a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)]));
        assert_eq!(t.newly_held.len(), 1, "the confirm did not take — say so");
    }

    /// A `dedup_key`, where `resolve` supplies one, IS the identity: `vike_exec` has already
    /// declared what makes that divergence the same one recurring.
    #[test]
    fn a_dedup_keyed_alert_keys_on_the_dedup_key() {
        let a = HeldId::new(
            "bybit",
            DivergenceKind::OrphanLocalPosition,
            Some("position:BTCUSDT:BOTH"),
            "local bybit BTCUSDT BOTH position 0.5 has NO venue position row this pass",
            &[],
        );
        let b = HeldId::new(
            "bybit",
            DivergenceKind::OrphanLocalPosition,
            Some("position:BTCUSDT:BOTH"),
            // A later pass phrases the detail differently — same divergence.
            "local bybit BTCUSDT BOTH position 0.6 has NO venue position row this pass",
            &[],
        );
        assert_eq!(a, b, "the dedup key decides, not the prose");
        let other = HeldId::new(
            "bybit",
            DivergenceKind::OrphanLocalPosition,
            Some("position:ETHUSDT:BOTH"),
            "local bybit ETHUSDT BOTH position 1.0 has NO venue position row this pass",
            &[],
        );
        assert_ne!(a, other, "a different keyed instance is a different divergence");
    }

    /// ...and a keyed alert whose PAYLOAD changes is still ONE identity. This is what makes the
    /// identity a strict generalization of the `(venue, kind, dedup_key)` match the held-alert
    /// store used before: appending legs to a supplied key could SPLIT a row `resolve` meant to be
    /// one (an `UnknownOrder`'s adoption fill, say), which would be a behaviour regression wearing
    /// a logging change's clothes.
    #[test]
    fn a_dedup_keyed_alert_ignores_its_legs() {
        let bare = HeldId::new("bybit", DivergenceKind::UnknownOrder, Some("v-9"), "unknown", &[]);
        let with_fill = HeldId::new(
            "bybit",
            DivergenceKind::UnknownOrder,
            Some("v-9"),
            "unknown",
            &[ext_pos_leg("bybit", "BTCUSDT", 4.0, 77)],
        );
        assert_eq!(bare, with_fill, "the supplied key answers alone");
        assert_eq!(bare.instance, "v-9", "...and is the whole instance");
    }

    /// A `MissingFill` leg carrying the VENUE's own trade id.
    fn missed_fill_id(trade_id: &'static str, qty: f64, ts: i64) -> HeldId {
        let leg = Event::Fill(FillEvent {
            trade_id: TradeId::from(trade_id),
            client_order_id: "EXT-bybit-v9".into(),
            venue: ustr::ustr("bybit"),
            symbol: ustr::ustr("BTCUSDT"),
            side: 1,
            last_qty: qty,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: ustr::ustr(""),
            liquidity_side: LiquiditySide::Taker,
            ts,
            mark_price: None,
            position_side: PositionSide::Both,
        });
        HeldId::new("bybit", DivergenceKind::MissingFill, None, "MissingFill", &[leg])
    }

    /// ⚠ THE OTHER DIRECTION of the identity choice, and the expensive one to get wrong. Two
    /// genuinely different missed fills that agree on symbol, side and SIZE are told apart only by
    /// the venue's trade id. Collapsing them would let an operator confirm one and silently lose
    /// the other's fill — caught for real by
    /// `crates/vike-core/tests/recon/recon_quarantine.rs`'s
    /// `recurring_unknown_order_alert_dedupes_and_clears_after_confirm`, whose `t1`/`t2` differ in
    /// nothing else.
    #[test]
    fn two_missed_fills_differing_only_in_trade_id_do_not_collapse() {
        assert_ne!(missed_fill_id("t1", 1.0, 5), missed_fill_id("t2", 1.0, 5));
    }

    /// ...while the SAME missed fill, re-diffed pass after pass because quarantine never folds it,
    /// stays one identity — its venue trade id does not move even though its pass does.
    #[test]
    fn the_same_missed_fill_recurring_keeps_one_identity() {
        assert_eq!(missed_fill_id("t1", 1.0, 5), missed_fill_id("t1", 1.0, 5));
    }

    /// The classification is per KIND and both answers are load-bearing — stated directly so the
    /// reason survives even if the two behavioural tests above are ever rewritten.
    #[test]
    fn trade_id_stability_is_classified_per_kind() {
        assert!(trade_ids_are_stable(DivergenceKind::MissingFill), "the venue's own execution id");
        assert!(trade_ids_are_stable(DivergenceKind::UnknownOrder), "pure fn of venue + order id");
        assert!(
            !trade_ids_are_stable(DivergenceKind::PositionOnlyExternal),
            "synth_position_legs bakes the pass clock into it"
        );
        assert!(!trade_ids_are_stable(DivergenceKind::PositionDrift), "…and into this one's too");
    }

    /// Two different KINDS on one venue never collapse, however similar their instance text.
    #[test]
    fn two_kinds_never_collapse() {
        let a = HeldId::new("bybit", DivergenceKind::PositionOnlyExternal, None, "x", &[]);
        let b = HeldId::new("bybit", DivergenceKind::PositionDrift, None, "x", &[]);
        assert_ne!(a, b);
    }

    /// `render_kinds` counts per kind rather than listing rows.
    #[test]
    fn the_summary_names_what_is_waiting() {
        let held = set([
            ext_pos_id("bybit", "BTCUSDT", 0.5, 1),
            ext_pos_id("bybit", "ETHUSDT", 1.0, 1),
            HeldId::new("bybit", DivergenceKind::MissingFill, None, "MissingFill", &[]),
        ]);
        assert_eq!(render_kinds(&held), "MissingFill x1, PositionOnlyExternal x2");
    }
}
