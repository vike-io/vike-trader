//! Pure divergence detection: reports × local view → `Vec<ReconcileDivergence>`. Deterministic ordering
//! (fills, then orders, then positions; input order within each) so golden fixtures are stable.
//! Both the order leg and the position leg are TWO-SIDED: a venue-anchored pass (what the venue
//! reports that local disagrees with) followed by a LOCAL-side sweep (what local holds that the
//! venue's report never mentions) — `OrphanLocalOrder` for orders, `OrphanLocalPosition` for
//! positions.
//!
//! An optional third leg — a `ReconcileJournalView` — upgrades the fill AND order checks from a two-way
//! (local vs venue) comparison to a three-way one; `journal: None` is the plain two-way behavior.
//! With `Some`, a venue trade_id the journal recorded but live local state has since lost is a
//! `JournalDivergence` (a restore/persistence bug, not an ordinary catch-up fill) rather than a
//! `MissingFill`; likewise a venue order absent from local is a `JournalDivergence` rather than an
//! `UnknownOrder` when the journal records it as still LIVE (non-terminal). Anchoring the order
//! check on the venue report (only orders the venue currently lists) keeps it free of
//! materializer-lag / terminal-reap false positives.
//!
//! **The two LOCAL-side sweeps speak only for the symbols the pass's client READ** ([`diff_scoped`]).
//! The venue's silence about a local order or position is an answer only for a symbol the client
//! fetched; for an admitted symbol it was never told about (decision 0216) the silence says nothing,
//! so the sweep skips it instead of raising an `OrphanLocal*` row the client could not have known.
//! [`diff`] is the unscoped form (an empty `unread`): every caller that has no per-pass list, and
//! every fixture written before the scope existed, reads it exactly as it always did.

use std::collections::HashSet;

use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

use super::journal_view::ReconcileJournalView;
use super::types::{ReconcileBalanceTolerance, ReconcileDivergence, ReconcileLocalView};
use crate::account::BalanceMode;
use vike_model::events::PositionSide;

/// [`diff_scoped`] with nothing unread and no symbol's size step known (so no local spot dust is rounded flat):
/// the venue's report is taken to span every symbol the engine holds.
pub fn diff(
    orders: &[OrderStatusReport],
    fills: &[FillReport],
    positions: &[PositionStatusReport],
    local: &ReconcileLocalView,
    journal: Option<&ReconcileJournalView>,
) -> Vec<ReconcileDivergence> {
    diff_scoped(orders, fills, positions, local, journal, &[], &|_| None)
}

/// The pure divergence detection of [`diff`], told which symbols the pass's client did NOT read.
///
/// `unread` are symbols (in the engine's spelling, exact match, as `ReconcileReports::admitted_symbols`
/// is spelled) whose LOCAL orders and positions the two local-side sweeps (orphan order, orphan position)
/// leave alone: the client never fetched them, so the venue's report not mentioning them proves nothing.
/// Nothing venue-anchored changes (a fill, an order or a position the venue DOES report is judged for any
/// symbol), and an empty `unread` is [`diff`].
///
/// `lot_of` is a symbol's OWN size step in the engine's spelling (`vike_model::RiskLimits::own_lot_size`, never
/// the mounted symbol's lot lent to another), read only for a SPOT (`Cash`) row: a local position under one step
/// there is flat, as `PositionStatusReport::spot_holding` reads the wallet (decision 0276). `None` (no step
/// known) compares the local figure unrounded.
pub fn diff_scoped(
    orders: &[OrderStatusReport],
    fills: &[FillReport],
    positions: &[PositionStatusReport],
    local: &ReconcileLocalView,
    journal: Option<&ReconcileJournalView>,
    unread: &[String],
    lot_of: &dyn Fn(&str) -> Option<f64>,
) -> Vec<ReconcileDivergence> {
    let mut out = Vec::new();

    // The symbols this pass reports as a SPOT holding (a `Cash` row): a fill there that names no order is a
    // trade placed by hand in the venue's app, the user's and never the strategy's (the spot rule; the fold
    // keeps it out of the book), so it is no missing fill. Empty, and so no test below, on every other pass.
    let spot: HashSet<&str> = positions
        .iter()
        .filter(|p| p.margin_mode == vike_model::MarginMode::Cash)
        .map(|p| p.symbol.as_str())
        .collect();

    // 1. Missing fills — a venue trade_id we have never folded. With a journal view, one the
    // journal DID record is a JournalDivergence instead (local lost a fill the journal has).
    for f in fills {
        if local.seen_trade_ids.contains(f.trade_id.as_str()) {
            continue;
        }
        if !spot.is_empty()
            && f.order_client_id.as_deref().is_none_or(str::is_empty)
            && spot.contains(f.symbol.as_str())
        {
            continue;
        }
        let journal_has_it =
            journal.is_some_and(|j| j.seen_trade_ids.contains(f.trade_id.as_str()));
        if journal_has_it {
            out.push(ReconcileDivergence::JournalDivergence {
                detail: format!(
                    "trade_id {} ({} {}) is recorded in the journal but missing from live local \
                     state — possible restore/persistence bug",
                    f.trade_id, f.venue, f.symbol
                ),
                recover_order: None, // fill-loss: nothing to re-register (the fill is at the venue)
            });
        } else {
            out.push(ReconcileDivergence::MissingFill(f.clone()));
        }
    }

    // 2. Orders: missing-terminal, unknown-order. A venue order local does not have is a
    // JournalDivergence when the journal records it as still LIVE (local lost an order both the
    // venue and the journal know); otherwise an ordinary UnknownOrder (never tracked, or
    // journal-terminal and so reaped locally — not a bug).
    for o in orders {
        match o.order_client_id.as_ref().and_then(|c| local.orders.get(c)) {
            Some(mo) => {
                if o.status.is_terminal() && !mo.status.is_terminal() {
                    out.push(ReconcileDivergence::MissingTerminal { order: o.clone() });
                }
            }
            None => {
                let journal_has_it_live = o
                    .order_client_id
                    .as_deref()
                    .zip(journal)
                    .and_then(|(c, j)| j.orders.get(c))
                    .is_some_and(|s| !s.is_terminal());
                if journal_has_it_live {
                    out.push(ReconcileDivergence::JournalDivergence {
                        detail: format!(
                            "order {} ({} {}) is recorded live in the journal but missing from live \
                             local state — possible restore/persistence bug",
                            o.order_client_id.as_deref().unwrap_or(""),
                            o.venue,
                            o.symbol
                        ),
                        // order-loss: carry the venue report so an operator confirm can re-register it.
                        recover_order: Some(Box::new(o.clone())),
                    });
                } else {
                    out.push(ReconcileDivergence::UnknownOrder(o.clone()));
                }
            }
        }
    }

    // 3. Orphan local orders — a live local order the venue no longer reports.
    let venue_order_client_ids: HashSet<&str> =
        orders.iter().filter_map(|o| o.order_client_id.as_deref()).collect();
    for (order_client_id, mo) in local.orders {
        if !mo.status.is_terminal()
            && !venue_order_client_ids.contains(order_client_id.as_str())
            && !unread.contains(&mo.request.symbol)
        {
            out.push(ReconcileDivergence::OrphanLocalOrder {
                order_client_id: order_client_id.clone(),
            });
        }
    }

    // 4. Positions: drift and external-only. A SPOT row (`Cash`: a wallet's holding of the pair's base coin)
    // is a FLOOR (the spot rule, owner 2026-11-11): coins beyond what the engine bought are the user's, so a
    // wallet holding at least the local position agrees and a coin the engine never bought raises nothing;
    // only a wallet holding LESS (coins sold or withdrawn by hand) is a drift. Both sides are flat under one size
    // step: the wallet by `spot_holding`, the local figure here (fee dust the pair cannot sell is no drift).
    for p in positions {
        let key = (p.symbol.clone(), p.position_side);
        let floor = p.margin_mode == vike_model::MarginMode::Cash;
        match local.positions.get(&key) {
            Some(&local_quantity) => {
                let drifted = if floor {
                    let dust = lot_of(&p.symbol).is_some_and(|lot| local_quantity.abs() < lot);
                    let held = if dust { 0.0 } else { local_quantity };
                    p.qty < held - local.quantity_tolerance
                } else {
                    (local_quantity - p.qty).abs() > local.quantity_tolerance
                };
                if drifted {
                    out.push(ReconcileDivergence::PositionDrift {
                        report: p.clone(),
                        local_quantity,
                    });
                }
            }
            None if !floor && p.qty.abs() > local.quantity_tolerance => {
                out.push(ReconcileDivergence::PositionOnlyExternal(p.clone()));
            }
            None => {}
        }
    }

    // 5. Orphan local positions — a live LOCAL position this pass's venue report does not mention
    // AT ALL; the mirror of step 3's order sweep (step 4 iterates VENUE rows, so an absent row
    // raises nothing there). A venue row that IS present — including an explicitly FLAT
    // (`qty == 0`) one, which is why several `ReconcileClient`s deliberately keep zero rows — stays on
    // step 4's `PositionDrift` path, so the same disagreement is never reported twice.
    //
    // GATED ON A NON-EMPTY POSITION REPORT. An empty `positions` slice is indistinguishable from
    // "this venue has no position concept / the fetch is not implemented" — every orders-only
    // `ReconcileClient` returns `Ok(Vec::new())` — so an empty report sweeps NOTHING; otherwise every
    // such book would raise a permanent, un-healable divergence on every pass. (A SPOT pair is no
    // longer that case: every spot client reports each pair's wallet as a `Cash` row, step 4's
    // floor.) Residual: a venue whose
    // position fetch is SYMBOL-SCOPED (binance perp / bybit / deribit / alpaca / ig) can still omit
    // a row for a local position in a DIFFERENT symbol — a false-positive orphan, which is why the
    // kind is quarantine-under-hybrid and folds nothing (`resolve`'s module doc is the authority).
    // The ADMITTED symbols such a client was not told about are no longer part of that residual:
    // `unread` skips them (`diff_scoped`).
    if !positions.is_empty() {
        let venue_pos_keys: HashSet<(&str, PositionSide)> =
            positions.iter().map(|p| (p.symbol.as_str(), p.position_side)).collect();
        // Same shape as step 3's sweep: genuinely-open local risk (beyond `quantity_tolerance` — the local
        // book keeps a key after it closes) whose (symbol, side) the venue did not report at all.
        for ((symbol, side), &local_quantity) in local.positions {
            if local_quantity.abs() > local.quantity_tolerance
                && !venue_pos_keys.contains(&(symbol.as_str(), *side))
                && !unread.contains(symbol)
            {
                out.push(ReconcileDivergence::OrphanLocalPosition {
                    venue: local.venue.to_string(),
                    symbol: symbol.clone(),
                    position_side: *side,
                    local_quantity,
                });
            }
        }
    }

    out
}

/// What one cash-reconcile pass concluded about the venue's balance figure — the WHOLE verdict, so
/// a caller cannot collapse two different conclusions into one action.
///
/// ⚠ **Why three verdicts and not `Option<ReconcileDivergence>`.** A `None` there meant TWO things — "no
/// venue-anchored baseline yet, adopt the venue's figure" and "the figures agree, within
/// tolerance" — and both landed on one adopt arm, which overwrote `Account::balance` with the
/// venue's number and RE-ANCHORED `Account::realized_pnl_at_balance_sync` onto it on EVERY
/// within-tolerance pass. Re-anchoring made the tolerance unbounded IN AGGREGATE: each adopt moved
/// the baseline to the venue's figure, so a third party moving cash by a sub-threshold amount every
/// pass was absorbed forever with nothing ever raised (with the default [`ReconcileBalanceTolerance`] on a 250k
/// account, `max(1.0, 25.0)` = 25 a pass, at the shipped 60 s `VIKE_RECONCILE_INTERVAL_MS`
/// default). The three verdicts are three DIFFERENT writes (or none), and
/// `crates/vike-core/src/runtime/reconcile.rs`'s `apply_reconcile_balance` is where each is applied.
///
/// # ⚠ WHAT THIS TYPE CLAIMS, AND THE CLAIM THAT IS DELIBERATELY ABSENT
///
/// The split states a property of THIS PASS and needs no qualifier: the `Anchored` arm rolls the
/// LOCAL figure forward instead of adopting the venue's, so a reconcile pass no longer re-anchors
/// `Account::realized_pnl_at_balance_sync` onto venue truth — on every venue, under every policy.
///
/// ⚠ It deliberately states NO CONSEQUENCE derived from that: not that the absorption is bounded,
/// not that a slow bleed accumulates until it crosses the band, not that on some venue this pass is
/// the sole adopter. Each silently assumes nothing ELSE re-anchors, and that set was stated wrongly
/// five times running, each time by the fix for the previous one; the claim is CUT rather than
/// re-scoped. The test for anything added here: **could this sentence become false if a bridge
/// changed a subscription?** If yes it belongs in a derived gate, not in a comment —
/// `crates/vike-ops/tests/venues/wallet_frame_venues_gate.rs` re-derives from the bridge tree, on
/// every run, which venues' private socket actually receives a live wallet frame (the fold
/// `Account::apply_account_state` performs, which writes the anchor absolutely). Read the answer
/// there; do not copy it back into this doc.
///
/// Two facts about the OTHER anchor writers are stable enough to state, because neither depends on
/// a venue or a subscription: `Account::restore` restores the anchor as `None` (it is deliberately
/// absent from `vike_execution::EngineSnapshotAccount`), so the first pass after a process start is an
/// [`ReconcileBalanceCheck::Adopt`]; and `Engine::apply_snapshot` writes it too but no runtime
/// path calls it (tests and the frozen `fixtures/r5/` replay only).
// large_enum_variant: `Anchored` carries a whole `ReconcileDivergence`, which grew with the order report's terms
// (price, trigger, trail). One value per pass, at reconcile cadence and never on the fold; boxing it would
// respell every construction and match, the bridges' tests included.
#[derive(Debug, Clone, PartialEq)]
#[expect(clippy::large_enum_variant)]
pub enum ReconcileBalanceCheck {
    /// The venue reported no balance at all (not fetched, fetch failed, no row for the quote
    /// asset). Touch nothing — this is evidence of neither agreement NOR disagreement.
    NotReported,
    /// **FIRST SYNC.** Local is still [`BalanceMode::Delta`] (`balance` is the arbitrary
    /// `seed_cash`, not venue truth) or carries no `realized_at_sync` baseline, so there is nothing
    /// to diff against. The caller ADOPTS the venue's figure whole and anchors the realized-PnL
    /// baseline beside it. Mirrors LEAN's `PerformCashSync` waiting for a settled state before
    /// comparing, and it is what keeps a COLD START usable: without it a fresh live mount judges
    /// every order against the arbitrary seed.
    Adopt(f64),
    /// A venue-anchored baseline already exists, so the pass could DIFF.
    ///
    /// `expected` is the LOCAL figure ROLLED FORWARD by this daemon's own realized PnL since the
    /// anchor — see [`diff_balance`]'s money-confound note. The caller writes it back on every
    /// diffed pass, drift or no drift, because the diff is INVARIANT under it: writing
    /// `balance = expected` and `realized_at_sync = realized_pnl` leaves the next pass's `expected`
    /// bit-unchanged (both spellings are `balance₀ + realized − baseline₀`, however the two terms
    /// are split), and `Account::equity_all` with it, whose `Authoritative` arm adds the same term
    /// (`Account::realized_since_balance_sync`). **It adopts NOTHING from the venue**: the venue's
    /// number appears in no term of it.
    ///
    /// `drift` is `Some` iff the venue's figure differs from `expected` by more than
    /// `max(tolerance.abs_floor, tolerance.rel_frac·|venue|)`, measured from an anchor THIS PASS does not move
    /// onto the venue's number. ⚠ What follows from that for a deployment is NOT stated here; this
    /// type's own doc says why.
    ///
    /// ⚠ The caller writing `expected` back on a DRIFT pass is what makes local `balance` FREEZE
    /// once a drift is standing and the policy HOLDS it. **Which policies hold is
    /// [`crate::recon::mode_applies`]'s answer, not this comment's.** The local figure keeps
    /// rolling forward on this daemon's own realized PnL and never converges on the venue, so a
    /// downward bleed leaves it HIGH. Disclosed rather than bounded —
    /// `crates/vike-core/src/runtime/reconcile.rs`'s `balance_roll` arm carries the argument.
    Anchored { expected: f64, drift: Option<ReconcileDivergence> },
}

/// First-class cash/balance reconcile diff. DELIBERATELY SEPARATE from [`diff`], whose signature
/// and golden fixtures stay untouched: the caller pushes this verdict's `drift` into the same
/// `divergences` vec `diff` produced, before `resolve`.
///
/// THE MONEY CONFOUND (why this is NOT `|local.balance − venue| > ε`): local `Account::balance` is
/// not a clean mirror of venue cash. Realized PnL does not enter `balance` at the fill in Authoritative
/// mode (the pass rolls it in, the venue re-seeds it), so between venue balance
/// syncs local `balance` legitimately diverges from venue cash by Σ realized-PnL-since-sync (which
/// can be large), and a naive raw diff false-positives on EVERY realized trade. We therefore
/// compare venue cash against the REALIZED-CORRECTED expected value
///
/// ```text
/// expected = balance + (realized_pnl − realized_at_sync)
/// ```
///
/// which cancels that legitimate drift; the residual is the genuinely-unexplained cash move
/// (withdrawal / deposit / liquidation haircut / funding mis-track) — exactly what we want to catch.
/// Commissions and funding need no term of their own: `Account::apply_fill`/`apply_funding` move
/// `balance` by them in BOTH modes, so they are already inside the first term.
///
/// ⚠ The three verdicts are [`ReconcileBalanceCheck`]'s, and its doc carries why they may not be merged.
pub fn diff_balance(
    local: &ReconcileLocalView,
    venue_balance: Option<f64>,
    quote_asset: &str,
    tolerance: ReconcileBalanceTolerance,
    ts: i64,
) -> ReconcileBalanceCheck {
    let Some(venue_balance_value) = venue_balance else {
        return ReconcileBalanceCheck::NotReported;
    };
    let cash = &local.cash;
    // First-observation: no venue-anchored baseline yet → adopt, never flag.
    if cash.mode == BalanceMode::Delta {
        return ReconcileBalanceCheck::Adopt(venue_balance_value);
    }
    let Some(baseline) = cash.realized_at_sync else {
        return ReconcileBalanceCheck::Adopt(venue_balance_value); // never synced => adopt, never flag
    };
    let expected = cash.balance + (cash.realized_pnl - baseline);
    let threshold = tolerance.abs_floor.max(tolerance.rel_frac * venue_balance_value.abs());
    let drift = ((venue_balance_value - expected).abs() > threshold).then(|| {
        ReconcileDivergence::BalanceDrift {
            venue: local.venue.to_string(),
            asset: quote_asset.to_string(),
            local: expected,
            venue_balance: venue_balance_value,
            timestamp: ts,
        }
    });
    ReconcileBalanceCheck::Anchored { expected, drift }
}

#[path = "diff_tests.rs"]
#[cfg(test)]
mod diff_tests;
