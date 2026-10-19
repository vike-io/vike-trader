//! The multi-currency half of [`Account`]: a symbol quoted in a currency other than the account's
//! has its money booked in the ACCOUNT's currency through one factor per quote currency.
//!
//! # The model
//!
//! * **The ledger stays ONE currency**, the account's (`ExecutionEngine::quote_asset`). A symbol
//!   quoted in another currency ("JPY" for USDJPY on a USD account) carries that currency, and each
//!   such currency has ONE standing RATE `r`: account-currency units per ONE unit of it (`1/150`,
//!   never `150`) and the instant it was observed.
//! * **The conversion lives inside the contract value.** [`Account::contract_value_of`] returns
//!   `ContractValue::in_account(r)` of the venue's own (BASE) unit, written into the same per-symbol
//!   grid the fold already reads. Every money product on the order path (the gate's notional and
//!   exposure caps, margin, both PnLs, the margin call, the mount ledgers, the published block)
//!   goes through that one lookup, so the fold stays one lookup returning one 16-byte `Copy` value
//!   and no consumer is edited. What this module owns is COLD: the BASE values, the quote
//!   currencies, the rates, and the rewrite that keeps the grid's rows equal to
//!   `base.in_account(rate)` whenever a rate moves (copy-on-write through the `Arc`, like
//!   [`Account::admit_contract_value`]).
//! * **Realized PnL converts ONCE, at the rate in force when the fill folds**: quote-currency PnL
//!   times the close-time rate. That is the margin-account convention; no entry rate is stored and
//!   `PositionEntry` is unchanged. Unrealized PnL moves with today's rate. A rate that moves
//!   between two fills never changes what the first one booked.
//! * **The rate is a journaled input**: it rides `Command::AdmitSymbol`
//!   (`SymbolAdmission::quote_rate`), so a replay prices every fill with the rate that preceded it
//!   and needs no venue. A rate is refreshed by admitting the symbol again.
//!
//! # Fail closed, never par
//!
//! * No usable rate: the symbol is not admitted (`no-quote-rate`); a rate older than
//!   [`DEFAULT_QUOTE_RATE_TTL_MS`] on arrival is refused too (`quote-rate-stale`), and so is one
//!   stamped more than [`QUOTE_RATE_FUTURE_SKEW_MS`] AHEAD of the engine's clock
//!   (`quote-rate-future`): a stamp from the future would pin the rate, because every real refresh
//!   is older than it and the newer-wins rule would keep it for ever.
//! * A rate that stands and then ages past the window denies OPENING orders on that currency's
//!   symbols (`quote-rate-stale`, the gate's lane); a covered reduce is never held up and the
//!   valuation keeps the last rate. A standing stamp that is ahead of the clock by more than the
//!   skew (the clock stepped back) is stale too. [`Account::quote_leg`] is the one answer both read.
//! * A rate that moves more than [`MAX_QUOTE_RATE_STEP`] in one update is refused
//!   (`quote-rate-jump`) and remembered WITH ITS STAMP: it is accepted as a real move only when the
//!   same rate is offered again, stamped at least [`MIN_QUOTE_RATE_CONFIRM_GAP_MS`] later, while the
//!   memory is no older than [`DEFAULT_QUOTE_RATE_TTL_MS`]. Two offers with one stamp (a re-delivered
//!   command; two symbols of one currency in one refresh batch) are one observation and never
//!   confirm each other.
//! * **Currency codes are compared trimmed and upper-cased** (`ccy_code`, `same_ccy`): `usd` and
//!   `USD` are one currency, never two rows of a rate book.
//! * A value is never built from a degenerate rate: `ContractValue::in_account` answers `None`
//!   rather than fold it to 1.0.
//! * A fee, a funding amount or a forced-close fee whose currency the account cannot convert (a
//!   THIRD currency; `FundingEvent` and `PositionLiquidated` name none) is not booked at par: it is
//!   left out of the ledger, counted ([`Account::fees_unconverted`]) and said once. Converting them
//!   by the currency the venue states is phase 2.
//!
//! # What persists
//!
//! `AccountSnapshot::mult` / `inverse` keep the BASE value of every symbol, and `AccountSnapshot::fx`
//! (absent while the book is empty, so every existing engine hashes byte-for-byte as before) names
//! the foreign-quoted symbols and the rate of each currency. [`Account::restore`] rebuilds the
//! effective grid from them with the same function the admission used.

use std::sync::Arc;

use indexmap::IndexMap;
use ustr::Ustr;
use vike_model::ContractValue;
use vike_model::events::FillEvent;

use super::{
    Account, DEFAULT_QUOTE_RATE_TTL_MS, FxRestore, MAX_QUOTE_RATE_STEP,
    MIN_QUOTE_RATE_CONFIRM_GAP_MS, QUOTE_RATE_FUTURE_SKEW_MS,
};
use crate::engine_snapshot::{FxRateSnapshot, FxSnapshot};
use crate::execution_engine::AppliedFx;
use crate::{Interned, QuoteRateView};

/// The rates this engine believes at all: no currency is worth less than a trillionth or more than
/// a trillion of another. It exists so the product `base · rate` of every symbol stays representable
/// (`ContractValue::in_account` re-checks), not as a sanity bound on markets.
const MIN_QUOTE_RATE: f64 = 1e-12;
const MAX_QUOTE_RATE: f64 = 1e12;

/// Two refused jumps within this relative distance are "the same rate sent again".
const RESEND_REL_TOL: f64 = 1e-3;

/// Whether the `n`th occurrence of a left-out amount is one worth a log line: the 1st, 10th, 100th,
/// ... So a standing fault is repeated (it does not go quiet after the first line) at a volume that
/// grows with the LOGARITHM of the count, and the engine reads no clock for it (the fold may not:
/// `crates/vike-ops/tests/architecture/clock_pin.rs`).
fn worth_saying(n: u64) -> bool {
    let mut p: u64 = 1;
    while p < n {
        match p.checked_mul(10) {
            Some(next) => p = next,
            None => return false,
        }
    }
    p == n
}

fn valid_rate(r: f64) -> bool {
    r.is_finite() && (MIN_QUOTE_RATE..=MAX_QUOTE_RATE).contains(&r)
}

/// A currency code as the engine keeps it: trimmed and upper-cased, so `usd` and ` USD ` are the
/// currency `USD`. `None` for a code with nothing in it. Applied where a code ENTERS the engine (an
/// admission's `quote_ccy`, the account currency the node states); the comparisons against codes a
/// venue wrote use [`same_ccy`].
pub(crate) fn ccy_code(raw: &str) -> Option<String> {
    let code = raw.trim();
    (!code.is_empty()).then(|| code.to_ascii_uppercase())
}

/// Whether two currency codes name one currency (trim + ASCII case folded).
pub(crate) fn same_ccy(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Whether a rate observed at `as_of_ms` cannot be relied on at `now_ms`: older than the staleness
/// window, or stamped further ahead of the clock than the skew allows (a stamp the clock has not
/// reached is not a measurement of the present). `saturating_sub` on both sides: a stamp of
/// `i64::MAX` or `i64::MIN` is unreliable, not an overflow.
fn unreliable_at(as_of_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(as_of_ms) > DEFAULT_QUOTE_RATE_TTL_MS
        || as_of_ms.saturating_sub(now_ms) > QUOTE_RATE_FUTURE_SKEW_MS
}

/// The cold structures behind [`Account`]'s foreign-quoted symbols. Present (as `Some(Box)`) exactly
/// while at least one symbol is quoted in a currency other than the account's.
#[derive(Debug, Clone)]
pub(super) struct FxBook {
    /// The currency every rate is expressed in: the engine's `quote_asset` when the first foreign
    /// symbol was admitted.
    pub(super) account_ccy: Ustr,
    /// Foreign-quoted symbols, in admission order (their effective rows live in `Account::values`).
    pub(super) symbols: IndexMap<String, FxSymbol>,
    /// One standing rate per quote currency in use, in first-use order.
    pub(super) rates: IndexMap<String, CcyRate>,
    /// Fees, funding and forced-close fees left out because their currency cannot be converted.
    /// A live health signal like `Account::duplicate_fills_refused`: not snapshotted.
    unconverted: u64,
    /// `(what, key, times seen)`; a line is said on the 1st, 10th, 100th... time ([`worth_saying`]).
    /// Not snapshotted.
    said: Vec<(&'static str, String, u64)>,
}

#[derive(Debug, Clone)]
pub(super) struct FxSymbol {
    pub(super) ccy: String,
    /// The venue's own unit, before any rate.
    pub(super) base: ContractValue,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CcyRate {
    /// The currency's code, interned once at admission so a publish copies it instead of
    /// allocating it (the published block is built per event on a sporadic feed).
    pub(super) name: Ustr,
    pub(super) rate: f64,
    pub(super) as_of_ms: i64,
    /// A rate refused as a jump, waiting to be offered again (see [`MAX_QUOTE_RATE_STEP`]).
    pub(super) pending: Option<Pending>,
}

/// A refused jump and the stamp of the offer that carried it: what a CONFIRMING offer is compared
/// with. The stamp is what makes "offered twice" mean two observations.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pending {
    pub(super) rate: f64,
    pub(super) as_of_ms: i64,
}

impl Pending {
    /// `offered` is this memory's rate again (inside the resend tolerance).
    fn same_rate(&self, offered: f64) -> bool {
        (offered - self.rate).abs() <= RESEND_REL_TOL * self.rate
    }

    /// The memory is older than the staleness window at `now_ms`: it confirms nothing any more.
    fn expired(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) > DEFAULT_QUOTE_RATE_TTL_MS
    }

    /// Whether an offer of `offered` stamped `as_of_ms` and judged at `now_ms` confirms this refused
    /// jump: the same rate, a LATER observation (at least the minimum gap after this one's stamp, so
    /// a re-delivered command or a second symbol of the same refresh batch can never confirm what
    /// its twin was refused for), and a memory that has not expired.
    fn confirmed_by(&self, offered: f64, as_of_ms: i64, now_ms: i64) -> bool {
        self.same_rate(offered)
            && as_of_ms.saturating_sub(self.as_of_ms) >= MIN_QUOTE_RATE_CONFIRM_GAP_MS
            && !self.expired(now_ms)
    }
}

enum Offer {
    /// Take the offered rate.
    Set,
    /// The offer is older than the standing rate: keep the standing one.
    Keep,
    /// The offer moves the standing rate by more than one step and no earlier observation of it
    /// stands to confirm it.
    Jump,
}

impl CcyRate {
    fn judge(&self, offered: f64, as_of_ms: i64, now_ms: i64) -> Offer {
        if as_of_ms < self.as_of_ms {
            return Offer::Keep;
        }
        let step = if offered >= self.rate { offered / self.rate } else { self.rate / offered };
        if step <= MAX_QUOTE_RATE_STEP {
            return Offer::Set;
        }
        match self.pending {
            Some(p) if p.confirmed_by(offered, as_of_ms, now_ms) => Offer::Set,
            _ => Offer::Jump,
        }
    }
}

impl FxBook {
    fn new(account_ccy: &str) -> Self {
        FxBook {
            // The canonical spelling, so a `quote_asset` assigned as `usd` still names `USD`.
            account_ccy: Ustr::from(ccy_code(account_ccy).as_deref().unwrap_or(account_ccy)),
            symbols: IndexMap::new(),
            rates: IndexMap::new(),
            unconverted: 0,
            said: Vec::new(),
        }
    }

    /// The published rows ([`Account::quote_rate_table`]): not inlined, so the empty case stays a
    /// null test at the call site.
    #[inline(never)]
    fn table(&self, now_ms: i64) -> (Interned, Vec<QuoteRateView>) {
        let rows = self
            .rates
            .values()
            .map(|c| QuoteRateView {
                ccy: Interned::from(c.name),
                rate: c.rate,
                as_of_ms: c.as_of_ms,
                stale: !valid_rate(c.rate) || unreliable_at(c.as_of_ms, now_ms),
            })
            .collect();
        (Interned::from(self.account_ccy), rows)
    }

    /// Count `(what, key)` seen once more and say whether this is a time worth a line.
    fn seen(&mut self, what: &'static str, key: &str) -> Option<u64> {
        let n = match self.said.iter_mut().find(|(w, k, _)| *w == what && k == key) {
            Some((_, _, n)) => {
                *n += 1;
                *n
            }
            None => {
                self.said.push((what, key.to_string(), 1));
                1
            }
        };
        worth_saying(n).then_some(n)
    }

    /// Count one amount left out of the ledger and say so on the 1st, 10th, 100th... time
    /// `(what, key)` is seen, with the count, so a standing fault is not a one-line event that goes
    /// quiet (`Account::fees_unconverted` has no other reader).
    fn note_unconverted(&mut self, what: &'static str, key: &str) {
        self.unconverted += 1;
        if let Some(times) = self.seen(what, key) {
            tracing::warn!(
                target: "vike_exec::account",
                what,
                key,
                times,
                account_ccy = %self.account_ccy,
                "NOT BOOKED: an amount whose currency this account cannot convert into its own \
                 (a fee in a third currency, or a funding amount / forced-close fee, which name no \
                 currency). Booking it at par would be a guess; the venue's balance holds it and \
                 reconcile re-converges the rest."
            );
        }
    }
}

impl Account {
    /// Whether this account holds a symbol quoted in a currency other than its own. One `Option`
    /// check — the test every path outside the fold makes before it does anything about currencies.
    #[inline]
    pub fn has_foreign_quote(&self) -> bool {
        self.fx.is_some()
    }

    /// The currency the rates of this ledger's foreign-quoted symbols are expressed in; `None`
    /// while it holds no such symbol.
    pub fn fx_account_ccy(&self) -> Option<&str> {
        self.fx.as_ref().map(|f| f.account_ccy.as_str())
    }

    /// Whether `symbol` is quoted in a currency other than the account's.
    pub fn is_foreign_quoted(&self, symbol: &str) -> bool {
        self.fx.as_ref().is_some_and(|f| f.symbols.contains_key(symbol))
    }

    /// The currency `symbol` is quoted in, when it is not the account's.
    pub fn quote_ccy_of(&self, symbol: &str) -> Option<&str> {
        self.fx.as_ref()?.symbols.get(symbol).map(|s| s.ccy.as_str())
    }

    /// Account-currency units per one unit of `symbol`'s quote currency: `1.0` for a symbol quoted
    /// in the account's own currency (and for one the account does not know). `NaN` for a
    /// foreign-quoted symbol with no standing rate, which only a hand-edited snapshot can produce:
    /// never a number that would pass for par. The gate reads [`Self::quote_leg`], which carries the
    /// staleness verdict too.
    #[inline]
    pub fn quote_rate_of(&self, symbol: &str) -> f64 {
        let Some(fx) = &self.fx else { return 1.0 };
        match fx.symbols.get(symbol) {
            None => 1.0,
            Some(s) => fx.rates.get(s.ccy.as_str()).map_or(f64::NAN, |c| c.rate),
        }
    }

    /// The pre-trade gate's view of `symbol`'s quote currency: `(rate, stale)`. `(1.0, false)` for a
    /// symbol quoted in the account's currency; for a foreign one the standing rate and whether it is
    /// older than [`DEFAULT_QUOTE_RATE_TTL_MS`] at `now_ms` (the engine's clock, which a replay
    /// reproduces) or stamped further ahead of that clock than [`QUOTE_RATE_FUTURE_SKEW_MS`] (the
    /// clock stepped back: a stamp the clock has not reached is not a fresh rate). A foreign symbol
    /// with no standing rate (a hand-edited snapshot only) is `(1.0, true)`, and one whose rate is
    /// not a usable number is `(that rate, true)`: stale for ever, so an opening order is denied
    /// rather than valued at par.
    #[inline]
    pub fn quote_leg(&self, symbol: &str, now_ms: i64) -> (f64, bool) {
        let Some(fx) = &self.fx else { return (1.0, false) };
        let Some(sym) = fx.symbols.get(symbol) else { return (1.0, false) };
        match fx.rates.get(sym.ccy.as_str()) {
            // A rate row that is not a usable number (a hand-edited snapshot only) is stale for
            // ever, whatever its age: the same verdict as a missing row.
            Some(c) => (c.rate, !valid_rate(c.rate) || unreliable_at(c.as_of_ms, now_ms)),
            None => (1.0, true),
        }
    }

    /// The published rate table: the currency the rates are expressed in and one row per quote
    /// currency, each with its age verdict at `now_ms`. `("", [])` — no allocation — for an account
    /// with no foreign-quoted symbol. Cold per publish, behind the same `Option` check.
    #[inline]
    pub fn quote_rate_table(&self, now_ms: i64) -> (Interned, Vec<QuoteRateView>) {
        match &self.fx {
            None => (Interned::default(), Vec::new()),
            Some(fx) => fx.table(now_ms),
        }
    }

    /// Fees, funding amounts and forced-close fees left out of the ledger because their currency
    /// could not be converted (see the module doc). Not snapshotted.
    pub fn fees_unconverted(&self) -> u64 {
        self.fx.as_ref().map_or(0, |f| f.unconverted)
    }

    /// Admit `symbol` as quoted in `ccy`, a currency other than the account's `account_ccy`, at the
    /// rate `rate = (units, observed_at_ms)`; or REFRESH it (the same call is how a rate is renewed).
    /// `base` is the venue's own unit (folded to a sane factor first, as every value entering the
    /// money path is); `None` keeps the one a refresh already holds, else the account's default
    /// unit. Everything is judged before anything is written: an `Err` changes nothing, except that
    /// a `quote-rate-jump` records the refused rate so that the same one sent again is accepted.
    /// The words are the stable refusal reasons of `AdmitOutcome::Refused`:
    ///
    /// * `no-quote-rate`: no rate, or one that is not a finite number in `[1e-12, 1e12]`.
    /// * `quote-rate-future`: the rate is stamped more than [`QUOTE_RATE_FUTURE_SKEW_MS`] ahead of
    ///   `now_ms` (a µs or ns epoch, `i64::MAX`): it would pin the rate, see the constant.
    /// * `quote-rate-stale`: the rate was already older than [`DEFAULT_QUOTE_RATE_TTL_MS`] at `now_ms`.
    /// * `quote-ccy-changed`: the symbol is already booked under another currency.
    /// * `quote-rate-jump`: the rate moved by more than [`MAX_QUOTE_RATE_STEP`] from the standing one
    ///   and no earlier, distinct observation of it stands to confirm it (`Pending::confirmed_by`).
    /// * `quote-rate-range`: the rate would push some symbol's value out of the representable range.
    ///
    /// `ccy` and `account_ccy` are canonical codes (`ccy_code`); the engine's admission normalises
    /// them before it gets here.
    ///
    /// A rate OLDER than the standing one for `ccy` is not taken (the standing one is kept) and is
    /// not an error: the symbol is admitted at the standing rate. A rate that is taken is written
    /// into the effective row of EVERY symbol quoted in `ccy`, in one copy-on-write of the grid.
    pub fn admit_quoted(
        &mut self,
        symbol: &str,
        ccy: &str,
        base: Option<ContractValue>,
        rate: Option<(f64, i64)>,
        account_ccy: &str,
        now_ms: i64,
    ) -> Result<(), &'static str> {
        let base = base
            .or_else(|| self.fx.as_ref().and_then(|f| f.symbols.get(symbol)).map(|s| s.base))
            .unwrap_or_else(|| self.contract_value_default())
            .folded();
        let Some((offered, as_of)) = rate.filter(|(r, _)| valid_rate(*r)) else {
            return Err("no-quote-rate");
        };
        // Only the PAST was ever bounded, and a stamp from the future pins the rate (see
        // `QUOTE_RATE_FUTURE_SKEW_MS`). `saturating_sub`: `i64::MAX` is a refusal, not an overflow.
        if as_of.saturating_sub(now_ms) > QUOTE_RATE_FUTURE_SKEW_MS {
            return Err("quote-rate-future");
        }
        if now_ms.saturating_sub(as_of) > DEFAULT_QUOTE_RATE_TTL_MS {
            return Err("quote-rate-stale");
        }
        if self.fx.as_ref().and_then(|f| f.symbols.get(symbol)).is_some_and(|s| s.ccy != ccy) {
            return Err("quote-ccy-changed");
        }
        // The rate that will stand after this call: the offer, or the one already standing.
        let cur = self.fx.as_ref().and_then(|f| f.rates.get(ccy)).copied();
        let standing = match cur {
            None => {
                CcyRate { name: Ustr::from(ccy), rate: offered, as_of_ms: as_of, pending: None }
            }
            Some(cur) => match cur.judge(offered, as_of, now_ms) {
                Offer::Set => {
                    CcyRate { name: cur.name, rate: offered, as_of_ms: as_of, pending: None }
                }
                Offer::Keep => cur,
                Offer::Jump => {
                    if let Some(c) = self.fx.as_mut().and_then(|f| f.rates.get_mut(ccy)) {
                        // The memory is the EARLIEST observation of the rate it holds. An offer of
                        // that same rate is not a new memory whatever its stamp: stamped no later
                        // (the same refresh batch, a re-delivery) it is the same observation again,
                        // and stamped later but too soon to confirm it must not push the confirming
                        // time out by being repeated. Only a DIFFERENT rate, or a memory that has
                        // expired, is replaced, and only by a later observation.
                        let replace = match c.pending {
                            None => true,
                            Some(p) => {
                                p.expired(now_ms) || (as_of > p.as_of_ms && !p.same_rate(offered))
                            }
                        };
                        if replace {
                            c.pending = Some(Pending { rate: offered, as_of_ms: as_of });
                        }
                    }
                    return Err("quote-rate-jump");
                }
            },
        };
        // Effective values at the standing rate, for this symbol and for every other symbol quoted
        // in `ccy`: all of them representable, or nothing is written.
        let Some(effective) = base.in_account(standing.rate) else {
            return Err("quote-rate-range");
        };
        let mut siblings: Vec<(String, ContractValue)> = Vec::new();
        if let Some(fx) = &self.fx {
            for (s, row) in &fx.symbols {
                if row.ccy == ccy && s != symbol {
                    match row.base.in_account(standing.rate) {
                        Some(v) => siblings.push((s.clone(), v)),
                        None => return Err("quote-rate-range"),
                    }
                }
            }
        }
        let fx = self.fx.get_or_insert_with(|| Box::new(FxBook::new(account_ccy)));
        fx.rates.insert(ccy.to_string(), standing);
        fx.symbols.insert(symbol.to_string(), FxSymbol { ccy: ccy.to_string(), base });
        let values = Arc::make_mut(&mut self.values);
        for (s, v) in siblings {
            values.insert(s, v);
        }
        // Never the sparse rule here: a foreign symbol's row stays listed even when its effective
        // value happens to equal the default, because the snapshot walks this grid.
        values.insert(symbol.to_string(), effective);
        Ok(())
    }

    /// The BASE value of the grid row `symbol` holds `effective` as: the venue's own unit for a
    /// foreign-quoted symbol, `effective` itself for every other (what a snapshot writes).
    pub(super) fn base_value(&self, symbol: &str, effective: ContractValue) -> ContractValue {
        match &self.fx {
            None => effective,
            Some(fx) => fx.symbols.get(symbol).map_or(effective, |s| s.base),
        }
    }

    pub(super) fn fx_snapshot(&self) -> Option<FxSnapshot> {
        let fx = self.fx.as_ref()?;
        Some(FxSnapshot {
            account_ccy: fx.account_ccy.to_string(),
            symbols: fx.symbols.iter().map(|(s, r)| (s.clone(), r.ccy.clone())).collect(),
            rates: fx
                .rates
                .iter()
                .map(|(ccy, c)| FxRateSnapshot {
                    ccy: ccy.clone(),
                    rate: c.rate,
                    as_of_ms: c.as_of_ms,
                    pending: c.pending.map(|p| p.rate),
                    pending_as_of_ms: c.pending.map(|p| p.as_of_ms),
                })
                .collect(),
        })
    }

    /// The memory of a refused jump as a snapshot holds it. A `pending` rate with no stamp (a
    /// snapshot written before the stamp existed) cannot be confirmed by a distinct observation, so
    /// it is not a memory: dropped, and the next jump starts again.
    fn restored_pending(r: &FxRateSnapshot) -> Option<Pending> {
        Some(Pending { rate: r.pending?, as_of_ms: r.pending_as_of_ms? })
    }

    /// Bring every stored rate stamped further ahead of `now_ms` than [`QUOTE_RATE_FUTURE_SKEW_MS`]
    /// back to a stamp that is already past the staleness window. Only a hand-edited snapshot or one
    /// written by a build that admitted such a stamp holds one (admission refuses it now), and what
    /// it says about the rate's age is not to be believed: the rate stays (the position holding it is
    /// still valued) but an OPENING order is denied until the node admits the symbol again with a
    /// real stamp, which it does right after a boot. A pending jump stamped in the future is dropped.
    /// `now_ms` is the snapshot's own clock, never a wall clock, so a replay clamps identically.
    pub(crate) fn clamp_future_rates(&mut self, now_ms: i64) {
        let Some(fx) = self.fx.as_mut() else { return };
        let stale = now_ms.saturating_sub(DEFAULT_QUOTE_RATE_TTL_MS + 1);
        for (ccy, c) in fx.rates.iter_mut() {
            if c.as_of_ms.saturating_sub(now_ms) > QUOTE_RATE_FUTURE_SKEW_MS {
                tracing::error!(
                    target: "vike_exec::account",
                    ccy = %ccy,
                    as_of_ms = c.as_of_ms,
                    now_ms,
                    "RESTORE: a conversion rate is stamped in the future; it is kept but treated as \
                     stale until the symbol is admitted again with a real stamp"
                );
                c.as_of_ms = stale;
            }
            if c.pending
                .is_some_and(|p| p.as_of_ms.saturating_sub(now_ms) > QUOTE_RATE_FUTURE_SKEW_MS)
            {
                c.pending = None;
            }
        }
    }

    /// Rebuild the whole book from a snapshot onto an account whose grid currently holds BASE values
    /// (`Account::restore`), then turn each foreign-quoted symbol's row into its effective value.
    /// A snapshot without an fx table (every account that never held a foreign-quoted symbol)
    /// returns the account untouched.
    pub(super) fn with_restored_fx(
        mut self,
        snap: &crate::engine_snapshot::AccountSnapshot,
    ) -> Self {
        let Some(fxs) = &snap.fx else { return self };
        let mut book = FxBook::new(&fxs.account_ccy);
        for r in &fxs.rates {
            book.rates.insert(
                r.ccy.clone(),
                CcyRate {
                    name: Ustr::from(r.ccy.as_str()),
                    rate: r.rate,
                    as_of_ms: r.as_of_ms,
                    pending: Self::restored_pending(r),
                },
            );
        }
        for (symbol, ccy) in &fxs.symbols {
            let base = self
                .values
                .get(symbol.as_str())
                .copied()
                .unwrap_or_else(|| self.contract_value_default());
            book.symbols.insert(symbol.clone(), FxSymbol { ccy: ccy.clone(), base });
        }
        self.fx = Some(Box::new(book));
        self.rewrite_effective(None);
        self
    }

    /// Put ONE restored foreign-quoted symbol back onto an account its mount already built
    /// (`ExecutionEngine::restore_runtime_state`): its row, its currency's rate from `fxs` and its
    /// effective value. `base` is the BASE value the snapshot's `mult` / `inverse` list holds for it.
    /// A symbol `fxs` does not list is not touched ([`FxRestore::NotForeign`], so the caller
    /// restores it as an ordinary one); a listed one whose currency has no rate in `fxs` is left out
    /// and said at `error!` ([`FxRestore::Unusable`]): valuing it at par is the one thing a restore
    /// must not do, so the caller does not accept the symbol at all, and the node re-admits every
    /// held symbol with a fresh rate right after the boot.
    pub(crate) fn restore_fx_symbol(
        &mut self,
        fxs: &FxSnapshot,
        symbol: &str,
        base: ContractValue,
    ) -> FxRestore {
        let Some((_, ccy)) = fxs.symbols.iter().find(|(s, _)| s == symbol) else {
            return FxRestore::NotForeign;
        };
        let Some(r) = fxs.rates.iter().find(|r| r.ccy == *ccy) else {
            tracing::error!(
                target: "vike_exec::account",
                symbol,
                ccy = %ccy,
                "RESTORE: a foreign-quoted symbol's snapshot carries no rate for its currency; \
                 the symbol is left out until it is admitted again with one"
            );
            return FxRestore::Unusable;
        };
        let book = self.fx.get_or_insert_with(|| Box::new(FxBook::new(&fxs.account_ccy)));
        book.rates.insert(
            ccy.clone(),
            CcyRate {
                name: Ustr::from(ccy.as_str()),
                rate: r.rate,
                as_of_ms: r.as_of_ms,
                pending: Self::restored_pending(r),
            },
        );
        book.symbols.insert(symbol.to_string(), FxSymbol { ccy: ccy.clone(), base: base.folded() });
        self.rewrite_effective(Some(ccy.as_str()));
        FxRestore::Restored
    }

    /// Recompute the effective grid rows of the foreign-quoted symbols (of `only` currency, or all)
    /// from their BASE values and standing rates. A row whose currency has no usable rate keeps its
    /// BASE (so a position there is at least valued in its own unit), is said at `error!`, and is
    /// stale for ever (`quote_leg`): only a hand-edited snapshot gets here.
    fn rewrite_effective(&mut self, only: Option<&str>) {
        let Some(fx) = &self.fx else { return };
        let mut rows: Vec<(String, ContractValue)> = Vec::new();
        for (s, row) in &fx.symbols {
            if only.is_some_and(|c| c != row.ccy) {
                continue;
            }
            let effective = fx
                .rates
                .get(row.ccy.as_str())
                .filter(|c| valid_rate(c.rate))
                .and_then(|c| row.base.in_account(c.rate));
            if effective.is_none() {
                tracing::error!(
                    target: "vike_exec::account",
                    symbol = %s,
                    ccy = %row.ccy,
                    "a foreign-quoted symbol has no usable rate: kept at its own unit and treated as \
                     stale until it is admitted again with one"
                );
            }
            rows.push((s.clone(), effective.unwrap_or(row.base)));
        }
        if rows.is_empty() {
            return;
        }
        let values = Arc::make_mut(&mut self.values);
        for (s, v) in rows {
            values.insert(s, v);
        }
    }

    /// The commission of `fill` in the ledger's currency, when this account holds a foreign-quoted
    /// symbol; `None` when the fee is in a currency it cannot convert (see the module doc). A fee
    /// stated in the account's currency (or with no currency named) is as stated; one stated in the
    /// symbol's quote currency is converted at that symbol's rate; a symbol quoted in the account's
    /// own currency keeps the single-currency rule.
    pub(super) fn fx_commission(&self, value: ContractValue, fill: &FillEvent) -> Option<f64> {
        let Some(fx) = &self.fx else {
            return Some(Self::commission_in_quote(value, fill));
        };
        let Some(sym) = fx.symbols.get(fill.symbol.as_str()) else {
            return Some(Self::commission_in_quote(value, fill));
        };
        let asset = fill.commission_asset.as_str();
        if asset.is_empty() || same_ccy(asset, fx.account_ccy.as_str()) {
            return Some(Self::commission_in_quote(value, fill));
        }
        if same_ccy(asset, &sym.ccy) {
            let rate = fx.rates.get(sym.ccy.as_str())?.rate;
            return Some(fill.commission * rate);
        }
        None
    }

    /// A venue balance frame that names no balance in the account's currency, on a ledger with a
    /// foreign-quoted symbol (`Account::apply_account_state`): the ledger stops following the venue's
    /// cash, and the usual cause is an engine that was never told its account currency
    /// (`ExecutionEngine::set_account_ccy`, which a foreign-quoted admission requires, so the likelier
    /// cause today is a venue that spells it differently). Said at `error!` on the 1st, 10th,
    /// 100th... frame ([`worth_saying`]); a frame arrives per balance push, not per market message.
    pub(super) fn fx_balance_frame_unmatched(&mut self, quote_asset: &str) {
        let Some(fx) = self.fx.as_mut() else { return };
        if let Some(times) = fx.seen("balance frame", quote_asset) {
            tracing::error!(
                target: "vike_exec::account",
                account_ccy = %quote_asset,
                times,
                "a venue balance frame names no balance in the account's currency, so the ledger \
                 did NOT follow it: the engine's account currency is not the currency the venue \
                 states its cash in (or was never set)"
            );
        }
    }

    /// [`Self::fx_commission`], counting and saying the fee it leaves out (`0.0` for it).
    pub(super) fn fx_commission_booked(&mut self, value: ContractValue, fill: &FillEvent) -> f64 {
        match self.fx_commission(value, fill) {
            Some(charge) => charge,
            None => {
                if let Some(fx) = self.fx.as_mut() {
                    fx.note_unconverted("commission", fill.commission_asset.as_str());
                }
                0.0
            }
        }
    }

    /// If `symbol` is foreign-quoted, count one amount of `what` ("funding", "liquidation fee") left
    /// out of the ledger, say it the first time for that symbol, and answer `true`.
    pub(super) fn fx_unconverted_symbol(&mut self, symbol: &str, what: &'static str) -> bool {
        let Some(fx) = self.fx.as_mut() else { return false };
        if !fx.symbols.contains_key(symbol) {
            return false;
        }
        fx.note_unconverted(what, symbol);
        true
    }

    /// What this account folded `fill` with, for the per-mount ledger (`AppliedFill::fx`): the
    /// effective contract value and the commission as booked. Call it after the fold; it reads, and
    /// counts nothing.
    pub fn applied_fx(&self, fill: &FillEvent) -> AppliedFx {
        let value = self.contract_value_of(&fill.symbol);
        AppliedFx { value, charge: self.fx_commission(value, fill).unwrap_or(0.0) }
    }
}
