//! Pure divergence detection: reports × local view → `Vec<Divergence>`. Deterministic ordering
//! (fills, then orders, then positions; input order within each) so golden fixtures are stable.
//! Both the order leg and the position leg are TWO-SIDED: a venue-anchored pass (what the venue
//! reports that local disagrees with) followed by a LOCAL-side sweep (what local holds that the
//! venue's report never mentions) — `OrphanLocalOrder` for orders, `OrphanLocalPosition` for
//! positions. The position sweep is the newer of the two: before it, a locally-open position whose
//! venue row was entirely ABSENT raised nothing at all, because the position leg iterated venue
//! rows only (#830 corrected a venue doc that claimed the diff engine already did this; this is
//! the engine actually doing it).
//!
//! An optional third leg — a `JournalView` (edge 2, Task 14) — upgrades the fill AND order checks
//! from a two-way (local vs venue) comparison to a three-way one: `journal: None` reproduces the
//! original two-way behavior byte-for-byte; `Some` distinguishes a plain `MissingFill` (venue
//! trade_id in neither local nor journal) from a `JournalDivergence` (venue trade_id the journal
//! recorded but live local state has since lost — a restore/persistence bug, not an ordinary
//! catch-up fill). The SAME split applies to orders: a venue order absent from local is a plain
//! `UnknownOrder` unless the journal records it as still LIVE (non-terminal), in which case local
//! lost a live order both the venue and the journal know — a `JournalDivergence`. Anchoring the
//! order check on the venue report (only orders the venue currently lists) keeps it free of
//! materializer-lag / terminal-reap false positives.

use std::collections::HashSet;

use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

use super::journal_view::JournalView;
use super::types::{BalanceTol, Divergence, LocalView};
use crate::account::BalanceMode;
use crate::order::OrderStatus;

pub fn diff(
    orders: &[OrderStatusReport],
    fills: &[FillReport],
    positions: &[PositionStatusReport],
    local: &LocalView,
    journal: Option<&JournalView>,
) -> Vec<Divergence> {
    let mut out = Vec::new();

    // 1. Missing fills — venue trade_id we have never folded. When a journal view is supplied,
    // split this into two cases: the journal also never saw it (plain catch-up MissingFill) vs
    // the journal DID record it (the local in-memory state lost a fill the journal has —
    // JournalDivergence, the three-way persistence-bug signal).
    for f in fills {
        if local.seen_trade_ids.contains(f.trade_id.as_str()) {
            continue;
        }
        let journal_has_it =
            journal.is_some_and(|j| j.seen_trade_ids.contains(f.trade_id.as_str()));
        if journal_has_it {
            out.push(Divergence::JournalDivergence {
                detail: format!(
                    "trade_id {} ({} {}) is recorded in the journal but missing from live local \
                     state — possible restore/persistence bug",
                    f.trade_id, f.venue, f.symbol
                ),
                recover_order: None, // fill-loss: nothing to re-register (the fill is at the venue)
            });
        } else {
            out.push(Divergence::MissingFill(f.clone()));
        }
    }

    // 2. Orders: missing-terminal, unknown-order. When the venue reports an order local does not
    // have, split it like the fill check above: if the journal records that order as still LIVE
    // (non-terminal), local lost a live order both the venue and the journal know — a
    // JournalDivergence (restore/persistence bug); otherwise it is an ordinary UnknownOrder (an
    // order local never tracked, or one the journal only has as already-terminal → reaped locally,
    // not a bug).
    for o in orders {
        match o.client_order_id.as_ref().and_then(|c| local.orders.get(c)) {
            Some(mo) => {
                let venue_terminal =
                    OrderStatus::parse(&o.status).map(|s| s.is_terminal()).unwrap_or(false);
                if venue_terminal && !mo.status.is_terminal() {
                    out.push(Divergence::MissingTerminal { order: o.clone() });
                }
            }
            None => {
                let journal_has_it_live = o
                    .client_order_id
                    .as_deref()
                    .zip(journal)
                    .and_then(|(c, j)| j.orders.get(c))
                    .is_some_and(|s| !s.is_terminal());
                if journal_has_it_live {
                    out.push(Divergence::JournalDivergence {
                        detail: format!(
                            "order {} ({} {}) is recorded live in the journal but missing from live \
                             local state — possible restore/persistence bug",
                            o.client_order_id.as_deref().unwrap_or(""),
                            o.venue,
                            o.symbol
                        ),
                        // order-loss: carry the venue report so an operator confirm can re-register it.
                        recover_order: Some(Box::new(o.clone())),
                    });
                } else {
                    out.push(Divergence::UnknownOrder(o.clone()));
                }
            }
        }
    }

    // 3. Orphan local orders — a live local order the venue no longer reports.
    let venue_coids: HashSet<&str> =
        orders.iter().filter_map(|o| o.client_order_id.as_deref()).collect();
    for (coid, mo) in local.orders {
        if !mo.status.is_terminal() && !venue_coids.contains(coid.as_str()) {
            out.push(Divergence::OrphanLocalOrder { client_order_id: coid.clone() });
        }
    }

    // 4. Positions: drift and external-only.
    for p in positions {
        let key = (p.symbol.clone(), position_side_str(p.position_side).to_string());
        match local.positions.get(&key) {
            Some(&local_qty) => {
                if (local_qty - p.qty).abs() > local.qty_tol {
                    out.push(Divergence::PositionDrift { report: p.clone(), local_qty });
                }
            }
            None if p.qty.abs() > local.qty_tol => {
                out.push(Divergence::PositionOnlyExternal(p.clone()));
            }
            None => {}
        }
    }

    // 5. Orphan local positions — a live LOCAL position this pass's venue report does not mention
    // AT ALL. The mirror of step 3's order sweep, and the leg that was missing: step 4 iterates
    // VENUE rows, so an absent row produced no divergence and the pass reported "reconciled" while
    // local still believed it held risk the venue never confirmed. A venue row that IS present —
    // including an explicitly FLAT (`qty == 0`) one, which is why several `ReconClient`s
    // deliberately keep zero rows instead of filtering them — stays entirely on step 4's
    // `PositionDrift` path, so the same disagreement is never reported twice.
    //
    // GATED ON A NON-EMPTY POSITION REPORT. An empty `positions` slice is indistinguishable from
    // "this venue has no position concept / the fetch is not implemented" — binance SPOT's
    // `fetch_position_status_reports` returns `Ok(Vec::new())` unconditionally, and every venue
    // whose `ReconClient` is orders-only does the same — so an empty report sweeps NOTHING. Without
    // that gate every spot inventory row would raise a permanent, un-healable divergence on every
    // pass. Residual (documented, not hidden): a venue whose position fetch is SYMBOL-SCOPED (most
    // of them: binance perp / bybit / deribit / alpaca / ig all filter to the mounted symbol) can
    // still omit a row for a local position in a DIFFERENT symbol, which surfaces here as a
    // false-positive orphan. That is precisely why the kind is quarantine-under-hybrid and folds
    // nothing (`resolve`'s module doc is the authority).
    if !positions.is_empty() {
        let venue_pos_keys: HashSet<(&str, &str)> = positions
            .iter()
            .map(|p| (p.symbol.as_str(), position_side_str(p.position_side)))
            .collect();
        // Same shape as step 3's sweep: genuinely-open local risk (beyond `qty_tol` — the local
        // book keeps a key after it closes) whose (symbol, side) the venue did not report at all.
        for ((symbol, side), &local_qty) in local.positions {
            if local_qty.abs() > local.qty_tol
                && !venue_pos_keys.contains(&(symbol.as_str(), side.as_str()))
            {
                out.push(Divergence::OrphanLocalPosition {
                    venue: local.venue.to_string(),
                    symbol: symbol.clone(),
                    position_side: side.clone(),
                    local_qty,
                });
            }
        }
    }

    out
}

/// What one cash-reconcile pass concluded about the venue's balance figure — the WHOLE verdict, so
/// a caller cannot collapse two different conclusions into one action.
///
/// ⚠ **This type exists because [`diff_balance`] used to answer `Option<Divergence>` and every
/// caller treated its `None` as one thing.** `None` was TWO things: "no venue-anchored baseline
/// yet, adopt the venue's figure" and "the figures agree, within tolerance". Both landed on the
/// same adopt arm, which overwrote `Account::balance` with the venue's number and RE-ANCHORED
/// `Account::realized_pnl_at_balance_sync` onto it on EVERY within-tolerance pass. Re-anchoring is
/// what made the tolerance unbounded IN AGGREGATE: each adopt moved the baseline to the venue's
/// figure, so the next pass measured drift from there, and a third party moving cash by a
/// sub-threshold amount every pass was absorbed forever with nothing ever raised. With the default
/// [`BalanceTol`] on a 250k account that is `max(1.0, 25.0)` = 25 a pass, and the shipped
/// `VIKE_RECONCILE_INTERVAL_MS` default is 60 s.
///
/// The three verdicts are three DIFFERENT writes (or none), and `crates/vike-core/src/runtime/
/// mod.rs`'s `reconcile_reports` is where each is applied.
///
/// # ⚠ WHAT THIS TYPE CLAIMS, AND THE CLAIM THAT IS DELIBERATELY ABSENT
///
/// What the split states is a property of THIS PASS and needs no qualifier: the `Anchored` arm
/// rolls the LOCAL figure forward instead of adopting the venue's, so a reconcile pass no longer
/// re-anchors `Account::realized_pnl_at_balance_sync` onto venue truth. That is the whole of the
/// mechanism, and it is true on every venue, under every policy.
///
/// ⚠ **What this doc does NOT state — and the omission is deliberate — is any CONSEQUENCE derived
/// from it**: that the absorption is therefore bounded, that a slow bleed therefore accumulates
/// and eventually crosses the band, or that on some venue this pass is therefore the sole adopter.
/// Every one of those silently assumes nothing ELSE re-anchors, and the set of "nothing else" has
/// now been stated wrongly five times running, each time by the fix for the previous one: the
/// balance seed bypassing the policy; "under every policy"; "the absorption is BOUNDED"; "four
/// wallet-frame venues" (the wrong four, welded into a gate); and "on every other roster venue the
/// pass is the sole adopter" (false for ctrader, where something else also adopts). Round six is
/// not worth writing. The claim is CUT rather than re-scoped.
///
/// The test for anything added here: **could this sentence become false if a bridge changed a
/// subscription?** If yes it belongs in a derived gate, not in a comment.
/// `crates/vike-ops/tests/wallet_frame_venues_gate.rs` is that gate — it re-derives from the bridge
/// tree, on every run, which venues' private socket actually receives a live wallet frame (the fold
/// `Account::apply_account_state` performs, which writes the anchor absolutely). Read the answer
/// there; do not copy it back into this doc.
///
/// Two facts about the OTHER anchor writers are stable enough to state, because neither depends on
/// a venue or a subscription: `Account::restore` restores the anchor as `None` (it is deliberately
/// absent from `vike_exec::AccountSnapshot`), so the first pass after a process start is an
/// [`BalanceCheck::Adopt`]; and `ExecutionEngine::apply_snapshot` writes it too but is reachable
/// only through `Command::ApplySnapshot`, which nothing sends outside
/// `crates/vike-core/tests/runtime_smoke.rs`.
#[derive(Debug, Clone, PartialEq)]
pub enum BalanceCheck {
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
    /// diffed pass, drift or no drift, because it is a re-parameterization the diff is INVARIANT
    /// under: writing `balance = expected` and `realized_at_sync = realized_pnl` leaves the next
    /// pass's `expected` bit-unchanged (both spellings are `balance₀ + realized − baseline₀`,
    /// however the two terms are split), while keeping `Account::equity_all` — which in
    /// `Authoritative` mode is `balance + unrealized` and reads no realized-PnL term at all — from
    /// going stale by Σ realized-since-anchor, which is the whole reason the seed used to re-run
    /// every pass. **It adopts NOTHING from the venue**: the venue's number appears in no term of
    /// it.
    ///
    /// `drift` is `Some` iff the venue's figure differs from `expected` by more than
    /// `max(tol.abs_floor, tol.rel_frac·|venue|)`. `expected` is measured from an anchor THIS PASS
    /// does not move onto the venue's number — which is the property the every-pass re-adopt
    /// destroyed. ⚠ What follows from that for a deployment is NOT stated here; see this type's own
    /// doc for why the derived consequence was cut rather than re-scoped.
    ///
    /// ⚠ The caller writing `expected` back on a DRIFT pass is what makes local `balance` FREEZE
    /// once a drift is standing and the policy HOLDS it. **Which policies hold is
    /// [`crate::recon::mode_applies`]'s answer, not this comment's** — enumerating them here is how
    /// the previous spelling came to name `hybrid`, which does not fold. The local figure keeps
    /// rolling forward on this daemon's own realized PnL and never converges on the venue, so a
    /// downward bleed leaves it HIGH. Disclosed rather than bounded —
    /// `crates/vike-core/src/runtime/mod.rs`'s `balance_roll` arm carries the argument.
    Anchored { expected: f64, drift: Option<Divergence> },
}

/// First-class cash/balance reconcile diff. DELIBERATELY SEPARATE from [`diff`], whose signature
/// and golden fixtures stay untouched: the caller pushes this verdict's `drift` into the same
/// `divergences` vec `diff` produced, before `resolve`.
///
/// THE MONEY CONFOUND (why this is NOT `|local.balance − venue| > ε`): local `Account::balance` is
/// not a clean mirror of venue cash. Realized PnL never enters `balance` in Authoritative mode
/// (`account.rs::equity_all` — the venue is relied on to re-seed it), while commissions/funding DO
/// adjust it. So between venue balance syncs, local `balance` legitimately diverges from venue cash
/// by Σ realized-PnL-since-sync (which can be large). A naive raw diff false-positives on EVERY
/// realized trade. We therefore compare venue cash against the REALIZED-CORRECTED expected value
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
/// ⚠ The three verdicts are [`BalanceCheck`]'s, and its doc carries why they may not be merged.
pub fn diff_balance(
    local: &LocalView,
    venue_balance: Option<f64>,
    quote_asset: &str,
    tol: BalanceTol,
    ts: i64,
) -> BalanceCheck {
    let Some(venue_bal) = venue_balance else {
        return BalanceCheck::NotReported;
    };
    let cash = &local.cash;
    // First-observation: no venue-anchored baseline yet → adopt, never flag.
    if cash.mode == BalanceMode::Delta {
        return BalanceCheck::Adopt(venue_bal);
    }
    let Some(baseline) = cash.realized_at_sync else {
        return BalanceCheck::Adopt(venue_bal); // never synced => adopt, never flag
    };
    let expected = cash.balance + (cash.realized_pnl - baseline);
    let threshold = tol.abs_floor.max(tol.rel_frac * venue_bal.abs());
    let drift = ((venue_bal - expected).abs() > threshold).then(|| Divergence::BalanceDrift {
        venue: local.venue.to_string(),
        asset: quote_asset.to_string(),
        local: expected,
        venue_bal,
        ts,
    });
    BalanceCheck::Anchored { expected, drift }
}

fn position_side_str(s: vike_model::events::PositionSide) -> &'static str {
    use vike_model::events::PositionSide::*;
    match s {
        Both => "BOTH",
        Long => "LONG",
        Short => "SHORT",
    }
}

#[path = "diff_tests.rs"]
#[cfg(test)]
mod diff_tests;
