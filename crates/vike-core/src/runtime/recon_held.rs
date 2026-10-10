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
//! `crates/vike-core/CLAUDE.md`. The state is one `BTreeMap` keyed by ACCOUNT that is EMPTY (and
//! allocation-free) on any box with no held divergences, and self-prunes the moment an account's
//! held set empties, so it is bounded by the LIVE divergence set rather than by uptime.
//!
//! ## ⚠ The key is the ACCOUNT, not the venue — and at fifty accounts that is the difference
//! between an announcer and an eraser
//!
//! A reconcile pass runs PER ACCOUNT (`vike_core::ReconLeg`), so one exchange's pass arrives here
//! as N separate calls to [`HeldAnnouncer::observe`], one per account, each carrying only THAT
//! account's raised set. `observe` REPLACES its key's whole held set with the set it was handed.
//! Keyed by venue, account 2's call therefore diffs account 2's set against account 1's:
//!
//! * everything account 1 holds is reported CLEARED — "no longer reported", the good-news line —
//!   while it is in fact still held and still awaiting an operator;
//! * everything account 2 holds is reported NEWLY HELD, on every pass, forever, because account 3
//!   erased it again a microsecond later;
//! * the venue's held set can therefore NEVER go a full interval unchanged, so the periodic
//!   backlog summary this module exists to provide can never fire at all;
//! * and [`HeldTransitions::silent`] is false on every leg of every pass, which un-suppresses the
//!   per-pass `reconcile pass folded` INFO the same way.
//!
//! At the owner's fifty that is ~50 spurious WARNs and ~50 spurious INFOs per venue per pass — one
//! a minute, forever, for a backlog nobody touched. It is the exact 396-lines-a-day defect above,
//! multiplied by the account count and wearing the fix's own clothes. So the key is the ROUTE KEY,
//! falling back to the venue: a single-account box's route key IS its venue id, so that box is
//! unchanged BY CONSTRUCTION rather than by care.

use std::collections::{BTreeMap, BTreeSet};

use vike_exec::recon::DivergenceKind;
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
/// ## Why the key is (venue, account, kind, instance) and not the alert id
/// The held-alert id is minted fresh by every pass for an un-keyed alert (that is the very reason
/// the CI box accumulated one row and one WARN per minute), so it identifies a RAISE, never a
/// divergence. `vike_exec::recon::ReconAlert` carries no identity field of its own beyond
/// `dedup_key`, which only SOME kinds set. So the identity is assembled here, from exactly what the
/// alert makes available at this site:
///
/// - **`venue`** — the canonical exchange id, so two venues can never collide. It is the LABEL
///   half: every log line and ring note an operator reads keys on it.
/// - **`account`** — WHICH account of that venue, i.e. the pass's route key, or the venue itself
///   for a venue with one account. ⚠ This is the half that was missing, and its absence was not a
///   cosmetic gap: two accounts of one exchange holding the same divergence produce the same
///   `dedup_key` (`position:BTCUSDT:Both` names an instrument, not a book), so without it their
///   identities are EQUAL — the store's `h.identity == ident` match then finds the FIRST account's
///   row, refreshes it with the SECOND account's payload, and leaves its `route_key` pointing at
///   the first account. An operator confirm folds account B's synthesized fills into account A's
///   book: the exact misroute this branch's diff half removed, re-entering through the held path.
///   The route key is the only thing at this site that can tell the two apart.
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
    /// The ROUTE KEY of the account whose pass raised this — or [`Self::venue`] itself for a venue
    /// with one account, which is every venue on a box with no `[accounts]` table. Ordered second
    /// so a `BTreeSet` of these still groups by exchange first.
    pub(crate) account: String,
    pub(crate) kind: DivergenceKind,
    pub(crate) instance: String,
}

impl HeldId {
    /// Build the identity from the raw pieces of a held alert. Takes the pieces rather than a
    /// `&ReconAlert` so `confirm_recon` can rebuild the identical key from its stored
    /// `HeldReconAlert` — the two must agree or a confirmed-then-recurring divergence would go
    /// unannounced.
    ///
    /// `account` is the pass's route key (`vike_exec::ReconcileReports::route_key`), or the venue
    /// for a venue's sole account — `CoreThread::reconcile_reports` resolves that fallback ONCE
    /// into the same `held_route_key` it stores on the alert row, so the raise and the confirm
    /// cannot disagree about which book a divergence belongs to.
    pub(crate) fn new(
        venue: &str,
        account: &str,
        kind: DivergenceKind,
        dedup_key: Option<&str>,
        detail: &str,
        proposed: &[Event],
    ) -> Self {
        // A `dedup_key` is `resolve`'s OWN declared identity for a by-design recurring divergence,
        // so it answers alone — see this type's doc. Only an un-keyed alert needs legs.
        //
        // ⚠ It answers alone WITHIN ONE ACCOUNT. A dedup key names an instrument and a side, never
        // a book, so it is `account` above it that keeps two accounts' `position:BTCUSDT:Both`
        // apart — appending anything to the key itself would split a row `resolve` meant to be one.
        if let Some(key) = dedup_key {
            return Self {
                venue: venue.to_string(),
                account: account.to_string(),
                kind,
                instance: key.to_string(),
            };
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
                Event::OrderSubmitted(_)
                | Event::OrderAccepted(_)
                | Event::OrderRejected(_)
                | Event::OrderDenied(_)
                | Event::OrderTriggered(_)
                | Event::OrderPartiallyFilled(_)
                | Event::OrderFilled(_)
                | Event::OrderCanceled(_)
                | Event::OrderExpired(_)
                | Event::OrderLiquidated(_)
                | Event::OrderModified(_)
                | Event::PositionOpened(_)
                | Event::PositionChanged(_)
                | Event::PositionClosed(_)
                | Event::AccountState(_)
                | Event::Funding(_)
                | Event::PositionLiquidated(_)
                | Event::OrderCancelRejected(_)
                | Event::OrderModifyRejected(_) => None,
            })
            .collect();
        legs.sort();
        legs.dedup();
        if !legs.is_empty() {
            instance.push(' ');
            instance.push_str(&legs.join(","));
        }
        Self { venue: venue.to_string(), account: account.to_string(), kind, instance }
    }
}

/// What one ACCOUNT's pass did to the held set. Everything the caller logs comes from here — the
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
    /// Distinct held divergences for this ACCOUNT (see [`HeldId`]'s residual note).
    pub(crate) held: usize,
    /// `Kind xN` per kind, comma separated, kind-ordered — so the line says WHAT is waiting, not
    /// only how much.
    pub(crate) kinds: String,
    /// How long this account has been silent, in core-clock ms. Lets the line say "unchanged for
    /// 1h" instead of implying something just happened.
    pub(crate) quiet_ms: i64,
}

/// Per-ACCOUNT held state. Removed from the map entirely once its set empties, so a box with
/// nothing held carries no rows and a later re-raise announces from a clean slate.
#[derive(Debug, Default)]
struct AccountHeld {
    held: BTreeSet<HeldId>,
    /// Core-clock ms of the last thing this account SAID (a transition or a summary). `None` only
    /// between `or_default()` and the first `observe` decision.
    last_announce_ms: Option<i64>,
}

/// The announcement state for every ACCOUNT, owned by `CoreThread` and touched only on the
/// reconcile boundary. `Default` is empty and allocates nothing.
///
/// ⚠ **Keyed by route key, not by venue** — the module doc works through what a venue key does to
/// fifty accounts of one exchange. A venue with one account keys on its own venue id, so that
/// shape is byte-identical to the per-venue map this replaced.
#[derive(Debug, Default)]
pub(crate) struct HeldAnnouncer {
    accounts: BTreeMap<String, AccountHeld>,
}

impl HeldAnnouncer {
    /// Fold ONE ACCOUNT's pass: `raised` is the identity of every alert that pass held (both the
    /// newly-appended ones and the dedup-refreshed ones — a refreshed row is still held). Returns
    /// what changed, and re-arms the summary clock.
    ///
    /// ⚠ `account` is the pass's ROUTE KEY, or the venue for a venue with one account — never the
    /// bare venue on a box that runs several accounts of it. Handing this a venue there makes each
    /// leg of a pass erase the previous leg's backlog; the module doc works that through at fifty.
    ///
    /// `now_ms` is the core clock (`engine.now_ms`), which is event-driven and therefore not
    /// guaranteed monotonic across an account that goes quiet and resumes; a backwards step re-arms
    /// the clock rather than firing, so a clock jump can never turn the summary back into
    /// per-pass noise.
    pub(crate) fn observe(
        &mut self,
        account: &str,
        now_ms: i64,
        raised: BTreeSet<HeldId>,
    ) -> HeldTransitions {
        let mut summary = None;
        let entry = self.accounts.entry(account.to_string()).or_default();
        let newly_held: BTreeSet<HeldId> = raised.difference(&entry.held).cloned().collect();
        let cleared: Vec<HeldId> = entry.held.difference(&raised).cloned().collect();
        entry.held = raised;
        let changed = !newly_held.is_empty() || !cleared.is_empty();
        // Nothing left held ⇒ the `cleared` lines ARE this pass's announcement and the account row
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
                // account has never spoken, or the core clock ran BACKWARDS. All three re-arm from
                // this reading.
                _ => entry.last_announce_ms = Some(now_ms),
            }
        }
        if now_empty {
            self.accounts.remove(account);
        }
        HeldTransitions { newly_held, cleared, summary }
    }

    /// Drop one identity because an operator CONFIRMED its held alert. Without this, a confirm that
    /// does not actually resolve the divergence (the venue keeps reporting it) would re-raise a
    /// fresh alert row in silence — the operator would have acted, seen nothing, and have no way to
    /// learn their action did not take. Forgetting makes the next pass announce it again as news.
    ///
    /// ⚠ Keyed on the identity's OWN `account`, not on its venue: forgetting by venue would drop
    /// the confirming account's row out of a map fifty accounts share, and — worse — would then
    /// re-announce every OTHER account's still-held divergences as news on the next pass.
    pub(crate) fn forget(&mut self, id: &HeldId) {
        let Some(entry) = self.accounts.get_mut(&id.account) else { return };
        entry.held.remove(id);
        if entry.held.is_empty() {
            self.accounts.remove(&id.account);
        }
    }

    /// Held identities for one ACCOUNT (its route key, or the venue for a venue with one account)
    /// — test/inspection only. `pub(crate)` so the runtime-wiring suite can assert the store size
    /// and this count AGREE (they must: one identity, one row).
    #[cfg(test)]
    pub(crate) fn held(&self, account: &str) -> usize {
        self.accounts.get(account).map_or(0, |v| v.held.len())
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
///
/// ⚠ `account` rides beside `venue` on all three emitters below. On a box with one account per
/// venue the two fields carry the SAME string and the line says what it always said; on fifty
/// accounts of one exchange, `venue` alone cannot say which book an operator has to go and look
/// at, and fifty identical `venue=binance` WARNs are what the reader gets. Same choice, and the
/// same reason, as the `account` field `ReconManager`'s staleness ERRORs already carry.
pub(crate) fn warn_newly_held(alert_id: u64, id: &HeldId, detail: &str) {
    tracing::warn!(
        alert = alert_id,
        venue = %id.venue,
        account = %id.account,
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
        account = %id.account,
        kind = ?id.kind,
        instance = %id.instance,
        "reconcile divergence no longer reported — was HELD, now absent from this account's pass"
    );
}

/// The periodic backlog re-statement. WARN, because an unattended backlog still needs an operator.
///
/// Takes both strings because the summary is an ACCOUNT's backlog while the label an operator
/// reads is the exchange — `account` equals `venue` for a venue with one account.
pub(crate) fn warn_summary(venue: &str, account: &str, s: &HeldSummary) {
    tracing::warn!(
        venue = %venue,
        account = %account,
        held = s.held,
        kinds = %s.kinds,
        quiet_ms = s.quiet_ms,
        "reconcile divergences still HELD — unchanged since the last line, awaiting operator confirm"
    );
}

#[path = "tests/recon_held_unit.rs"]
#[cfg(test)]
mod recon_held_unit_tests;
