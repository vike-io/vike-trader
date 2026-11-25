//! The lock on a complete outcome set, read off the LIVE per-leg books: a pure, venue-neutral DETECTOR that WALKS each
//! leg's [`vike_marketdata::L2Book`] for size and nets the venue's taker fee and the walk-the-book slippage. The
//! live-book twin of [`super::cached_marks::buy_all_yes_edge`], which screens the venue's cached marks and ignores
//! depth.
//!
//! ## The two locks (both from the set's invariant: exactly one outcome resolves YES = $1; see [`super`])
//! - **BUY-ALL-YES.** Buy one YES of every outcome; the certain winner pays $1/share, the losers $0. So `size` shares
//!   of each of the `N` YES legs pays exactly `size` at resolution. Locked iff `Σ_i (YES ask_i) · size + fees < size`,
//!   i.e. per share `Σ_i (YES ask_i) + fee/size < 1`. Realised at resolution, whatever the venue.
//! - **BUY-ALL-NO.** Buy one NO of every outcome. Owning all `N` NOs is worth exactly `N − 1` at resolution (`N − 1`
//!   outcomes resolve NO = $1, the winner's NO resolves $0). Locked iff `Σ_i (NO ask_i) · size + fees < (N − 1) · size`,
//!   i.e. per share `Σ_i (NO ask_i) + fee/size < N − 1`. A venue that can CONVERT a full NO set realises that at
//!   once ([`PayoutTiming::Immediately`]); otherwise it is paid at resolution ([`PayoutTiming::AtResolution`]) and the
//!   capital is locked until then. The profit's arithmetic is the same either way.
//!
//! These are exactly the two gross triggers `Σ best YES asks < $1` and `Σ best NO asks < N − 1`. This module makes
//! them SIZED and NET.
//!
//! ## Fee
//! The per-leg taker fee is [`vike_model::FeeSchedule::commission`]`(false, size, vwap)`: for a prediction market the
//! `qty · rate · p·(1−p)` curve ([`vike_model::FeeSchedule::ProbabilityScaled`]), `FeeSchedule::Free` for a fee-less
//! market. The rate is the VENUE's and PER-MARKET (the caller passes it in [`OutcomeSetConfig`]). The fee is charged
//! at the leg's VWAP fill price; because `p·(1−p)` is concave, `fee(VWAP)` is an UPPER bound on the exact per-level
//! fee (Jensen), so the netted edge is CONSERVATIVE, never a phantom arb from under-charging. A venue's own
//! conversion or merge fee, if it has one, is NOT modelled here: stated, not silently swallowed.
//!
//! ## Completeness is the CALLER's, and load-bearing
//! Evaluating an INCOMPLETE set is a phantom-arb generator (see [`super`]). [`detect`] takes the set's outcome count
//! and its YES and NO token lists on trust, refuses fewer than two outcomes and a side whose token list is not one
//! per outcome, and treats a leg whose live book is missing (or has no ask) as that lock being unavailable this tick,
//! never a false one.
//!
//! Determinism: the per-leg Σ is a naive left-to-right fold in the legs' own order, so the result is reproducible
//! frame to frame.
//!
//! READ-ONLY / NO AUTO-EXECUTION. This emits sized, net opportunities; nothing here signs, sizes down, or sends.

use vike_marketdata::{L2Book, L2BookLevel};
use vike_model::{ContractValue, FeeSchedule};

/// How [`detect`] and [`evaluate_lock`] size a candidate lock across the `N` legs. The same size is bought on every
/// leg (a lock needs one unit of each outcome), so the size is bounded by the THINNEST leg.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizePolicy {
    /// The largest size fillable at EVERY leg's best ask with no walking: the minimum best-ask displayed qty across
    /// the legs. VWAP then equals the best ask on every leg, so this sized edge carries ZERO slippage: the
    /// conservative "how much can I lock at the quoted top of book?".
    TopOfBook,
    /// Probe a fixed shares-per-leg size, WALKING each leg's ask book for its VWAP (the taker price of an outcome
    /// share, a linear one-unit contract: [`ContractValue::taker_price`] of the default value,
    /// `L2Book::average_price_for_quantity` bit for bit). A size past a leg's best-level depth genuinely walks and pays
    /// slippage; a size past a leg's TOTAL displayed ask depth is unlockable (that leg cannot complete the set), so
    /// the whole lock is refused at that size.
    Fixed(f64),
}

/// When a lock's payout reaches the holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayoutTiming {
    /// On the spot: the venue converts the full set into collateral (Polymarket's neg-risk `convertPositions` turns
    /// a full NO set into `(N − 1)` collateral).
    Immediately,
    /// At resolution: the capital is locked until the event resolves. A buy-all-YES lock is always this.
    AtResolution,
}

/// Inputs to the netting: the venue's per-leg taker fee, the size policy, and how the venue pays a full NO set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutcomeSetConfig {
    /// The per-leg taker fee: the venue's `FeeSchedule` (a [`FeeSchedule::ProbabilityScaled`] curve, or
    /// [`FeeSchedule::Free`] for a fee-less market or the gross screen).
    pub fee: FeeSchedule,
    /// How to size the lock (see [`SizePolicy`]).
    pub size: SizePolicy,
    /// When a full NO set pays its `N − 1`: [`PayoutTiming::Immediately`] where the venue can convert it,
    /// [`PayoutTiming::AtResolution`] where it cannot.
    pub no_set_payout: PayoutTiming,
}

impl OutcomeSetConfig {
    /// Construct with an explicit fee curve, size policy and NO-set payout timing.
    pub fn new(fee: FeeSchedule, size: SizePolicy, no_set_payout: PayoutTiming) -> Self {
        OutcomeSetConfig { fee, size, no_set_payout }
    }
}

/// Which lock an [`OutcomeSetLock`] realises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockKind {
    /// Buy one YES of every outcome; the certain winner pays $1/share, at resolution.
    BuyAllYes,
    /// Buy one NO of every outcome; the set pays `(N − 1)/share`, when the venue's [`PayoutTiming`] says.
    BuyAllNo,
}

/// The pure per-lock netting result: the numbers [`evaluate_lock`] produces for one side, before any set context is
/// attached. All figures are in collateral, for the whole `size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizedLock {
    /// Shares bought on EACH leg.
    pub size: f64,
    /// Σ_i (VWAP_i · size) over the legs: the pre-fee buy cost.
    pub gross_cost: f64,
    /// Σ_i taker fee: the venue's fee at each leg's VWAP.
    pub fee: f64,
    /// What the lock pays: `size` for buy-all-YES, `(N − 1) · size` for buy-all-NO.
    pub payout: f64,
    /// `payout − gross_cost − fee`. A [`SizedLock`] is only ever returned with `net_profit > 0`.
    pub net_profit: f64,
}

/// A sized, net-of-fee lock over one complete outcome set: [`SizedLock`] plus the context a caller needs to act (or
/// log). READ-ONLY: emitting one triggers nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutcomeSetLock {
    /// Which lock this is.
    pub kind: LockKind,
    /// When it pays: always [`PayoutTiming::AtResolution`] for [`LockKind::BuyAllYes`], the config's
    /// `no_set_payout` for [`LockKind::BuyAllNo`].
    pub payout_timing: PayoutTiming,
    /// `N`, the number of mutually exclusive outcomes in the set.
    pub outcomes: usize,
    /// Shares per leg.
    pub size: f64,
    /// Pre-fee buy cost.
    pub gross_cost: f64,
    /// Taker fee.
    pub fee: f64,
    /// The payout: `size` or `(N − 1) · size`.
    pub payout: f64,
    /// Locked profit net of fee and walk cost; always `> 0`.
    pub net_profit: f64,
}

/// A COMPLETE outcome set as [`detect`] reads it: how many outcomes it has and each outcome's two token ids, in the
/// set's own order. The caller proved the set complete (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutcomeSet<'a> {
    /// `N`, the number of mutually exclusive outcomes.
    pub outcomes: usize,
    /// Every outcome's YES token id, in order. A short list (an outcome with no token) makes the YES side unavailable.
    pub yes_tokens: &'a [&'a str],
    /// Every outcome's NO token id, in order.
    pub no_tokens: &'a [&'a str],
}

/// Σ of the best ASK across `legs` (top of book): the raw gross screen (`Σ best YES asks`, `Σ best NO asks`). `None`
/// if any leg has no ask quoted (an unpriceable set side). Naive left-to-right fold in the given order.
pub fn sum_best_asks(legs: &[&L2Book]) -> Option<f64> {
    let mut sum = 0.0;
    for b in legs {
        let L2BookLevel { price: ask, .. } = b.best_ask()?;
        sum += ask;
    }
    Some(sum)
}

/// Evaluate ONE lock given the per-leg ASK books already gathered in the set's order. `payout_per_share` is `1.0` for
/// buy-all-YES and `(N − 1)` for buy-all-NO. Returns the sized, net lock ONLY when it is net-positive; `None` when the
/// size cannot be filled on every leg (the set cannot be completed at that size) or the netted edge is not `> 0`.
///
/// Pure: walks each leg's asks with [`ContractValue::taker_price`] (side `+1` = buy consumes asks), and charges the
/// venue's fee at each leg's VWAP via [`FeeSchedule::commission`].
pub fn evaluate_lock(
    legs: &[&L2Book],
    payout_per_share: f64,
    config: &OutcomeSetConfig,
) -> Option<SizedLock> {
    if legs.is_empty() {
        return None;
    }
    let size = match config.size {
        SizePolicy::Fixed(q) => {
            if q.is_nan() || q <= 0.0 {
                return None; // non-positive / NaN size is not a lock
            }
            q
        }
        SizePolicy::TopOfBook => {
            // The thinnest leg's best-ask qty. Any leg with no ask ⇒ this side is unpriceable.
            let mut min_qty = f64::INFINITY;
            for b in legs {
                let L2BookLevel { qty, .. } = b.best_ask()?;
                if qty < min_qty {
                    min_qty = qty;
                }
            }
            if min_qty <= 0.0 {
                return None; // resting levels never carry 0 qty, but never lock a zero size
            }
            min_qty
        }
    };

    let mut gross_cost = 0.0;
    let mut fee = 0.0;
    for b in legs {
        // +1 buy walks asks low→high; None ⇒ displayed depth cannot cover `size` on this leg, so the
        // full set cannot be assembled at this size and there is no lock.
        let vwap = ContractValue::default().taker_price(b, 1, size, None)?;
        gross_cost += vwap * size;
        fee += config.fee.commission(false, size, vwap);
    }
    let payout = payout_per_share * size;
    let net_profit = payout - gross_cost - fee;
    (net_profit > 0.0).then_some(SizedLock { size, gross_cost, fee, payout, net_profit })
}

fn lock(
    outcomes: usize,
    kind: LockKind,
    payout_timing: PayoutTiming,
    sized: SizedLock,
) -> OutcomeSetLock {
    OutcomeSetLock {
        kind,
        payout_timing,
        outcomes,
        size: sized.size,
        gross_cost: sized.gross_cost,
        fee: sized.fee,
        payout: sized.payout,
        net_profit: sized.net_profit,
    }
}

/// Detect the net-positive locks on `set`, reading each leg's live book via `book_for(token_id)`. Returns the locks
/// found (0, 1, or 2: the YES and NO locks are independent, distinct trades, in that order). Emits NOTHING for:
///
/// - a set with fewer than 2 outcomes (not a real outcome set: the `(N − 1)` payout would be `≤ 0`);
/// - a side whose token list is not one token per outcome (an outcome with no YES/NO token);
/// - a side where any leg's book is missing (`book_for` returns `None`): that lock is simply unavailable this tick;
/// - a lock whose netted edge is not `> 0`.
///
/// The set's COMPLETENESS is the caller's (see the module doc). `book_for` is a lookup into whatever the caller holds
/// the live books in (a feed's per-token book, a snapshot map, ...); the returned reference's lifetime `'a` ties the
/// gathered legs to it.
pub fn detect<'a>(
    set: &OutcomeSet<'_>,
    book_for: impl Fn(&str) -> Option<&'a L2Book>,
    config: &OutcomeSetConfig,
) -> Vec<OutcomeSetLock> {
    let mut out = Vec::new();
    let n = set.outcomes;
    if n < 2 {
        return out;
    }

    // BUY-ALL-YES: buy one YES of every outcome; the certain winner pays $1/share → payout `size`.
    if set.yes_tokens.len() == n
        && let Some(legs) =
            set.yes_tokens.iter().map(|&id| book_for(id)).collect::<Option<Vec<_>>>()
        && let Some(sized) = evaluate_lock(&legs, 1.0, config)
    {
        out.push(lock(n, LockKind::BuyAllYes, PayoutTiming::AtResolution, sized));
    }

    // BUY-ALL-NO: buy one NO of every outcome → `(N − 1)·size`, paid when the venue's timing says.
    if set.no_tokens.len() == n
        && let Some(legs) = set.no_tokens.iter().map(|&id| book_for(id)).collect::<Option<Vec<_>>>()
        && let Some(sized) = evaluate_lock(&legs, (n - 1) as f64, config)
    {
        out.push(lock(n, LockKind::BuyAllNo, config.no_set_payout, sized));
    }

    out
}
