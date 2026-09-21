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
                _ => None,
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

    /// The the CI box alert: `PositionOnlyExternal`, no dedup key, detail = the kind name — on a venue
    /// with ONE account, so its account key is its venue id. That is the shape every test below
    /// except the per-account ones is about.
    fn ext_pos_id(venue: &str, symbol: &str, qty: f64, ts: i64) -> HeldId {
        ext_pos_id_at(venue, venue, symbol, qty, ts)
    }

    /// …and the same alert raised by a NAMED account of that venue.
    fn ext_pos_id_at(venue: &str, account: &str, symbol: &str, qty: f64, ts: i64) -> HeldId {
        HeldId::new(
            venue,
            account,
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
            assert!(
                a.observe("bybit", ts, set([ext_pos_id("bybit", "BTCUSDT", 0.5, ts)])).silent()
            );
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
            "bybit",
            DivergenceKind::OrphanLocalPosition,
            Some("position:BTCUSDT:BOTH"),
            "local bybit BTCUSDT BOTH position 0.5 has NO venue position row this pass",
            &[],
        );
        let b = HeldId::new(
            "bybit",
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
        let bare = HeldId::new(
            "bybit",
            "bybit",
            DivergenceKind::UnknownOrder,
            Some("v-9"),
            "unknown",
            &[],
        );
        let with_fill = HeldId::new(
            "bybit",
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
        HeldId::new("bybit", "bybit", DivergenceKind::MissingFill, None, "MissingFill", &[leg])
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
        let a = HeldId::new("bybit", "bybit", DivergenceKind::PositionOnlyExternal, None, "x", &[]);
        let b = HeldId::new("bybit", "bybit", DivergenceKind::PositionDrift, None, "x", &[]);
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------------------------------
    // FIFTY ACCOUNTS OF ONE EXCHANGE — the ruled scale. The integration twin (a real core, real
    // engines, a real driver) is `crates/vike-core/tests/recon/recon_per_account.rs`; these are
    // the pure ones, and they are where the erasure is visible as ARITHMETIC.
    // ---------------------------------------------------------------------------------------

    /// The number the owner ruled the design must hold at: *"it can be 50 accounts per venue."*
    const FIFTY: usize = 50;

    /// `binance#A00` … `binance#A49` — what `vike_mount::account_route_key` renders for a labelled
    /// account, and what that account's `ExecutionEngine::route_key` carries.
    fn acct(i: usize) -> String {
        format!("binance#A{i:02}")
    }

    /// **THE MAJOR, as arithmetic.** One pass over fifty accounts of one exchange, each holding
    /// its own divergence, and after the whole pass ALL FIFTY are still held — each under its own
    /// account key.
    ///
    /// ⚠ Two accounts cannot distinguish "merged" from "erased" and fifty can: keyed by venue,
    /// `observe` replaces the venue's whole set with each leg's, so the pass ends with ONE account
    /// held (the last) and 49 erased — and each leg's call reports the previous leg's set as
    /// CLEARED, i.e. as the good-news "no longer reported" line, for divergences that are still
    /// held and still waiting for somebody.
    #[test]
    fn fifty_accounts_of_one_venue_are_fifty_held_sets_not_one() {
        let mut a = HeldAnnouncer::default();
        let mut announced = 0usize;
        let mut cleared = 0usize;
        for i in 0..FIFTY {
            let t = a.observe(
                &acct(i),
                1_000,
                set([ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, 1_000)]),
            );
            announced += t.newly_held.len();
            cleared += t.cleared.len();
        }
        assert_eq!(announced, FIFTY, "each account's divergence is its own news");
        assert_eq!(
            cleared, 0,
            "NOTHING cleared — no account's backlog was erased by its neighbour"
        );
        for i in 0..FIFTY {
            assert_eq!(a.held(&acct(i)), 1, "account {i} must still hold its own divergence");
        }
    }

    /// …and it STAYS held and STAYS quiet: sixty passes over fifty accounts announce the fifty
    /// once and say nothing at all thereafter. Keyed by venue this is 50 WARNs + 50 "cleared"
    /// INFOs per pass, forever — the measured 396-lines-a-day defect multiplied by the account
    /// count, wearing the fix's own clothes.
    #[test]
    fn fifty_accounts_holding_a_steady_backlog_announce_once_each_and_then_go_quiet() {
        let mut a = HeldAnnouncer::default();
        let mut announced = 0usize;
        let mut cleared = 0usize;
        let mut noisy_legs = 0usize;
        for pass in 0..60i64 {
            let ts = 1_000 + pass * PASS_MS;
            for i in 0..FIFTY {
                let t = a.observe(
                    &acct(i),
                    ts,
                    set([ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, ts)]),
                );
                announced += t.newly_held.len();
                cleared += t.cleared.len();
                if !t.silent() {
                    noisy_legs += 1;
                }
            }
        }
        assert_eq!(announced, FIFTY, "fifty divergences, fifty announcements — not 50 per pass");
        assert_eq!(cleared, 0, "and not one spurious clear across 3,000 legs");
        assert_eq!(noisy_legs, FIFTY, "only the raising pass of each account says anything");
    }

    /// **The dedup key names an INSTRUMENT, never a book.** Fifty accounts all holding
    /// `position:BTCUSDT:Both` are fifty identities, because the account is above the key. Without
    /// it they are ONE, and the store's identity match then refreshes the first account's row with
    /// the fiftieth account's payload while its `route_key` still points at the first — which is
    /// how a confirm folds the wrong account's fills.
    #[test]
    fn fifty_accounts_sharing_one_dedup_key_are_fifty_identities() {
        let ids: BTreeSet<HeldId> = (0..FIFTY)
            .map(|i| {
                HeldId::new(
                    "binance",
                    &acct(i),
                    DivergenceKind::OrphanLocalPosition,
                    Some("position:BTCUSDT:BOTH"),
                    "local binance BTCUSDT BOTH position 0.5 has NO venue position row this pass",
                    &[],
                )
            })
            .collect();
        assert_eq!(ids.len(), FIFTY, "one identity per account, not one for the exchange");
        // …and each names its own account while every one of them names the same exchange.
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(id.venue, "binance", "the LABEL half stays canonical");
            assert_eq!(id.account, acct(i), "…and the KEY half names the book");
        }
    }

    /// A confirm on one of fifty accounts forgets THAT account's identity and leaves the other 49
    /// untouched — so the next pass re-announces the one that did not resolve, and stays silent
    /// about the rest.
    #[test]
    fn a_confirm_on_one_of_fifty_accounts_forgets_only_that_account() {
        let mut a = HeldAnnouncer::default();
        let id_of = |i: usize, ts: i64| ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, ts);
        for i in 0..FIFTY {
            a.observe(&acct(i), 1_000, set([id_of(i, 1_000)]));
        }
        a.forget(&id_of(7, 1_000));
        assert_eq!(a.held(&acct(7)), 0, "the confirmed account's row is gone");
        for i in (0..FIFTY).filter(|&i| i != 7) {
            assert_eq!(a.held(&acct(i)), 1, "account {i} is untouched by account 7's confirm");
        }

        let ts = 1_000 + PASS_MS;
        let mut reannounced = 0usize;
        for i in 0..FIFTY {
            reannounced += a.observe(&acct(i), ts, set([id_of(i, ts)])).newly_held.len();
        }
        assert_eq!(reannounced, 1, "only the account whose confirm did not take says anything");
    }

    /// The periodic backlog summary SURVIVES fifty accounts — it fires per account on its own
    /// cadence. Keyed by venue it can never fire at all, because every leg of every pass changes
    /// the venue's set and re-arms the clock.
    #[test]
    fn the_summary_still_fires_at_fifty_accounts() {
        let mut a = HeldAnnouncer::default();
        let mut summaries = 0usize;
        // Two summary intervals' worth of one-minute passes.
        let passes = (2 * HELD_SUMMARY_INTERVAL_MS) / PASS_MS;
        for pass in 0..passes {
            let ts = 1_000 + pass * PASS_MS;
            for i in 0..FIFTY {
                let t = a.observe(
                    &acct(i),
                    ts,
                    set([ext_pos_id_at("binance", &acct(i), "BTCUSDT", 0.5, ts)]),
                );
                if t.summary.is_some() {
                    summaries += 1;
                }
            }
        }
        assert_eq!(summaries, FIFTY, "one summary per account over the interval, and no more");
    }

    /// `render_kinds` counts per kind rather than listing rows.
    #[test]
    fn the_summary_names_what_is_waiting() {
        let held = set([
            ext_pos_id("bybit", "BTCUSDT", 0.5, 1),
            ext_pos_id("bybit", "ETHUSDT", 1.0, 1),
            HeldId::new("bybit", "bybit", DivergenceKind::MissingFill, None, "MissingFill", &[]),
        ]);
        assert_eq!(render_kinds(&held), "MissingFill x1, PositionOnlyExternal x2");
    }
}
