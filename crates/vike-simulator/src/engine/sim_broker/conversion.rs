//! A symbol QUOTED in another currency than the run's ledger (decision 0243, phase 2): the rate a run
//! converts one quote currency into its ledger by, read off the history of the conversion pair the profile
//! names for it (`engine.conversion` in vike-backtest, which loads and judges the series before any price
//! is read), and the BOOKING of a symbol's money at that rate. [`crate::StrategyEngineConfig::conversions`]
//! lists the series and the symbols each one books; every other symbol is booked in its own quote currency,
//! as every run was before.
//!
//! # The rule
//!
//! The rate at `t` is the CLOSE of the last bar of the pair that had CLOSED by `t` (a bar opened at
//! `ts` closes at `ts + interval`; `vike_marketdata::Bar::closed_at`, the one as-of rule an input reads
//! by too), never the close of the bar `t` falls in, and none before the first bar closes. The close is
//! turned the right way round by the pair's [`AccountPerQuoteLeg`], read off its name against the
//! ledger when the harness loaded it, through [`AccountPerQuote::of_mid`]: account-currency units per
//! ONE unit of the quote currency, multiplied into a quote amount, never divided.
//!
//! A rate whose bar closed more than TWICE the pair's interval before `t` is STALE: an OPENING order is
//! denied on it, as the live engine denies `quote-rate-stale`, and a reduce is never held up by it.
//!
//! # Where the rate enters: the booking, never the fill law
//!
//! `vike_fills` prices a fill and its fee in the symbol's OWN quote currency and knows no ledger. The rate
//! enters where money is booked, in ONE arithmetic: the symbol's per-symbol value
//! (`OrderBrokerSimulator::value_of_index`) is its quote-currency value times the rate in force,
//! `vike_model::ContractValue::in_account`, the live engine's own (decision 0183, item 2). So the cash leg
//! and the equity mark, the trade fold, margin, liquidation and every sizer, which all read that value,
//! are in the ledger's money with no edit of their own. Two numbers are the VENUE's, in its quote
//! currency, and stay there until booked: a fill's fee is priced on the quote-currency value and then
//! multiplied by the rate (the live engine's `fill.commission * rate` for a fee stated in the quote
//! currency, decision 0183 item 9), and the grid's `min_notional` is compared with the quote-currency
//! notional (a venue number is never converted to meet the engine's, decision 0183 item 3).
//!
//! # When the rate moves: once per bar of the pair
//!
//! [`OrderBrokerSimulator::advance_quote_rates`] steps each symbol's cursor past the pair's bars that had
//! closed by the event's time and writes the new value once; no fill looks a rate up. Both run loops call
//! it where the event's time is set, before anything of the event books, so every number of an event (its
//! fills, its funding, its marks, the strategy's sizing) is at the rate in force when the event began.
//!
//! A held position's ENTRY notional moves to the new rate in that same step (`cash` moves by the
//! difference of its two cash legs at the entry price): the margin-account convention of decision 0183,
//! item 4. A close realizes the quote PnL at the rate in force when it fills, the unrealized PnL and the
//! margin are at the rate now, and no entry rate is stored; equity moves by the open quote PnL times the
//! change of rate and by nothing else.
//!
//! # Fail closed, never par, but for the one declared exception
//!
//! Before the pair's first close, and while the rate in force is stale, an OPENING order on the symbol is
//! refused `quote-rate-stale` (the live gate's word, `vike_execution::RiskGate`); a covered reduce is never
//! held up. With no rate yet the symbol carries its quote-currency value, which books nothing: no order can
//! open a position there.
//!
//! The ONE exception is a series the harness marks [`ConversionSeries::par_where_missing`] (decision 0243,
//! amended by the owner's rulings of 2026-11-16: a USDT quote on a dollar ledger, the engine naming no
//! currency itself): wherever it has no rate, one unit of the quote is one unit of the ledger, a rate that is
//! never stale ([`ConversionRate::par`]): before its first bar closes, and whenever the bar that should have
//! closed last, one interval before the moment, is missing (a hole, or past its last bar). Every other pair
//! still fails closed as above.
//!
//! An empty [`crate::StrategyEngineConfig::conversions`] (every run that names no pair) keeps an EMPTY
//! table, so it pays one `is_empty` per event and one `Vec::get` per fill, and books every number it booked
//! before this module existed, bit for bit.

use std::ops::Range;
use std::sync::Arc;

use vike_marketdata::Bar;
use vike_model::{AccountPerQuote, AccountPerQuoteLeg, ContractValue};

use super::ledger_events::{LedgerEvent, LedgerEventKind};
use super::state::OrderBrokerSimulator;

/// The live gate's word for an opening order on a symbol whose rate is stale (`vike_execution::RiskGate`).
pub(crate) const QUOTE_RATE_STALE: &str = "quote-rate-stale";

/// One quote currency's conversion series, as the harness loaded and judged it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversionSeries {
    /// The quote currency it converts, upper-case (`vike_model::currency_code`): `JPY`, `USDT`.
    pub quote_currency: String,
    /// The series read, in the input grammar (`dukascopy:USDJPY:1h`, `user-coinbase:coinbase:USDT-USD:1d`),
    /// for a refusal's words.
    pub pair: String,
    /// Which side of the pair the ledger is on (`AccountPerQuoteLeg::of_pair_name`).
    pub leg: AccountPerQuoteLeg,
    /// One bar's length in ms: a bar closes `interval_ms` after it opens.
    pub interval_ms: i64,
    /// Sorted by `ts`, each close a usable rate (the harness refused the run otherwise). Shared (`Arc`):
    /// every run over one load (a walk-forward's windows) holds the same series.
    pub bars: Arc<Vec<Bar>>,
    /// The run's symbols quoted in [`Self::quote_currency`], each BOOKED through this series (the module doc).
    /// The engine names no currency: the harness says which symbols these are. A LINEAR contract each.
    pub symbols: Vec<String>,
    /// Wherever the series has no rate (before its first close, in a hole, past its last bar), one unit of the
    /// quote is worth one unit of the ledger instead of no rate at all (the module doc's one exception). The
    /// harness sets it, and only for the pair decision 0243's amendments of 2026-11-16 name; `false` is every
    /// other series, which fails closed there.
    pub par_where_missing: bool,
}

/// The rate of one moment ([`ConversionSeries::rate_at`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConversionRate {
    /// Account-currency units per one unit of the quote currency.
    pub rate: AccountPerQuote,
    /// The close of the bar it is the close of (`ts + interval_ms`), epoch ms.
    pub as_of_ms: i64,
    /// Older than twice the series' interval at the moment asked: deny an OPENING order, never a reduce.
    pub stale: bool,
    /// The one-to-one rate of a [`ConversionSeries::par_where_missing`] series where it has none: no bar's
    /// close, `as_of_ms` the moment asked, never stale.
    pub par: bool,
}

impl ConversionSeries {
    /// **The rate at `t`**: the close of the last bar that had closed by `t`, oriented by [`Self::leg`],
    /// with its staleness at `t`; before the first bar closes, `None` (fail closed: no rate, no opening). On a
    /// [`Self::par_where_missing`] series, one-to-one wherever it has no rate: before its first close, and when
    /// the last close in force is a whole interval or more old (the bar that should have closed since is missing).
    #[must_use]
    pub fn rate_at(&self, t: i64) -> Option<ConversionRate> {
        self.rate_of_closed(self.bars.partition_point(|b| b.closed_at(self.interval_ms, t)), t)
    }

    /// The rate at `t` once the first `closed` bars have closed: [`Self::rate_at`]'s answer, for a caller
    /// that counts the closed bars itself (the booking's cursor steps the count once per bar).
    fn rate_of_closed(&self, closed: usize, t: i64) -> Option<ConversionRate> {
        let par = || ConversionRate {
            rate: AccountPerQuote::new(1.0).expect("one is a rate"),
            as_of_ms: t,
            stale: false,
            par: true,
        };
        let Some(last) = closed.checked_sub(1) else {
            return self.par_where_missing.then(par);
        };
        let bar = self.bars.get(last)?;
        let as_of_ms = bar.timestamp.saturating_add(self.interval_ms);
        if self.par_where_missing && t.saturating_sub(as_of_ms) >= self.interval_ms {
            return Some(par());
        }
        let rate = AccountPerQuote::of_mid(self.leg, bar.close).ok()?;
        Some(ConversionRate { rate, as_of_ms, stale: self.stale_at(as_of_ms, t), par: false })
    }

    /// Whether a rate whose bar closed at `as_of_ms` is stale at `t`: older than twice the interval.
    fn stale_at(&self, as_of_ms: i64, t: i64) -> bool {
        t.saturating_sub(as_of_ms) > self.interval_ms.saturating_mul(2)
    }

    /// The moments `m` at which a cursor that has counted `closed` bars and stepped at `t` would step to the
    /// same answer: no further bar has closed by `m` (`Bar::closed_at`, the cursor's loop), and
    /// [`Self::rate_of_closed`] keeps its kind, a bar's rate or par, whose turn is found by that function's own
    /// test, `m - as_of >= interval`, which only ever turns once as `m` grows. Every field it reads of the rate
    /// is then the same (a par rate's `as_of_ms` and any rate's `stale` are read by no booking: `quote_rate_stale`
    /// asks the clock). Empty when the turn cannot be told at the ends of `i64`: every step is then asked.
    fn steady_span(&self, closed: usize, t: i64) -> Range<i64> {
        let next = self
            .bars
            .get(closed)
            .map_or(i64::MAX, |b| b.timestamp.saturating_add(self.interval_ms));
        let (Some(last), true) = (closed.checked_sub(1), self.par_where_missing) else {
            return i64::MIN..next; // one kind of answer whatever the moment
        };
        let as_of = self.bars[last].timestamp.saturating_add(self.interval_ms);
        let par = |m: i64| m.saturating_sub(as_of) >= self.interval_ms;
        let turn = as_of.saturating_add(self.interval_ms);
        if !par(turn) || turn == i64::MIN || par(turn - 1) {
            return t..t;
        }
        if par(t) { turn..next } else { i64::MIN..turn.min(next) }
    }
}

/// One symbol's booking through a [`ConversionSeries`], as the run steps through it.
pub(crate) struct QuoteConversionCursor {
    /// The symbol's value in its QUOTE currency: what the run stated for it.
    pub(crate) quote_value: ContractValue,
    /// Its series, an index into `OrderBrokerSimulator::conversions`.
    series: usize,
    /// How many of the series' bars had closed by the last advance.
    closed: usize,
    /// The rate in force, `None` before the series' first close (one-to-one wherever it has none on a
    /// [`ConversionSeries::par_where_missing`] series).
    current: Option<ConversionRate>,
    /// The moments at which a step would book exactly what the last one did ([`ConversionSeries::steady_span`]),
    /// so `advance_quote_rates` passes the cursor by. Empty until its first step.
    steady: Range<i64>,
}

impl QuoteConversionCursor {
    /// ⚠ A LAST line of defence, as `StrategyEngine::new`'s other asserts: the harness refuses such a run
    /// in words before it builds one.
    pub(crate) fn new(quote_value: ContractValue, series: usize, of: &ConversionSeries) -> Self {
        assert!(
            quote_value.kind == vike_model::NotionalKind::Linear,
            "a conversion books a LINEAR contract: an inverse one makes the run a coin ledger"
        );
        assert!(
            of.bars.is_sorted_by_key(|b| b.timestamp),
            "a conversion series must be sorted by ts"
        );
        // One-to-one until the first close on a `par_where_missing` series, whose value is then the quote
        // value it already carries (times one); no rate at all on any other.
        let current = of.rate_of_closed(0, i64::MIN);
        QuoteConversionCursor { quote_value, series, closed: 0, current, steady: 0..0 }
    }
}

impl OrderBrokerSimulator {
    /// Put in force, for every converted symbol, the close of the last bar of its pair that had closed
    /// by `t`, and write the symbol's value at it (the module doc): once per bar of the pair. The entry
    /// notional of a held position moves to the new rate in the same step, so equity changes by the open
    /// quote PnL times the change of rate. A no-op for a run that names no conversion.
    pub(crate) fn advance_quote_rates(&mut self, t: i64) {
        for (si, cursor) in self.quote_conversion_by_si.iter_mut().enumerate() {
            let Some(c) = cursor else { continue };
            if c.steady.contains(&t) {
                continue; // the last step's answer, which it already booked
            }
            let series = &self.conversions[c.series];
            let before = c.closed;
            while series.bars.get(c.closed).is_some_and(|b| b.closed_at(series.interval_ms, t)) {
                c.closed += 1;
            }
            c.steady = series.steady_span(c.closed, t);
            // A `par_where_missing` series is asked at every step: a bar that should have closed by now and
            // did not (a hole, past the last) turns its rate to one without any new close.
            if c.closed == before && !series.par_where_missing {
                continue;
            }
            // A close that is no rate (the harness refused such a series) keeps the last one: never par.
            let Some(now) = series.rate_of_closed(c.closed, t) else { continue };
            // `None` only for a product past f64's range: the last value stands, never par.
            let Some(value) = c.quote_value.in_account(now.rate.get()) else { continue };
            c.current = Some(now);
            let old = self.value_by_si[si];
            if value == old {
                continue;
            }
            let pos = self.symbol[si].pos;
            let moved = old.cash_leg(pos.size, pos.average_price)
                - value.cash_leg(pos.size, pos.average_price);
            self.cash += moved;
            self.value_by_si[si] = value;
            // `log_ledger`'s push, spelled out: the cursor borrow above holds `self`.
            if let Some(log) = &mut self.ledger_events {
                let kind = LedgerEventKind::Revalue { value, cash: moved };
                log.push(LedgerEvent { step: self.step as u32, si: si as u32, kind });
            }
        }
    }

    /// The rate a fee on `symbols[si]` is booked at: the rate in force for a converted symbol, `None` for a
    /// symbol booked in its own quote currency (and for a converted one before its first rate, where no
    /// order can open, so nothing books).
    #[inline]
    pub(crate) fn quote_rate_of(&self, si: usize) -> Option<AccountPerQuote> {
        let c = self.quote_conversion_by_si.get(si)?.as_ref()?;
        c.current.map(|r| r.rate)
    }

    /// `symbols[si]`'s value in its QUOTE currency, what the venue's own numbers are in (the fee the fill
    /// law prices, the grid's `min_notional`): [`Self::value_of_index`] for a symbol the run does not convert.
    #[inline]
    pub(crate) fn quote_value_of_index(&self, si: usize) -> ContractValue {
        match self.quote_conversion_by_si.get(si) {
            Some(Some(c)) => c.quote_value,
            _ => self.value_by_si[si],
        }
    }

    /// Whether `symbols[si]` is converted and its rate is stale at the engine's clock: none in force yet,
    /// or the one in force is older than twice its pair's interval ([`ConversionSeries::rate_at`]'s rule); the
    /// one-to-one rate before a [`ConversionSeries::par_where_missing`] series' first close never is.
    pub(crate) fn quote_rate_stale(&self, si: usize) -> bool {
        let Some(Some(c)) = self.quote_conversion_by_si.get(si) else { return false };
        let series = &self.conversions[c.series];
        c.current.is_none_or(|r| !r.par && series.stale_at(r.as_of_ms, self.now))
    }

    /// The refusal of an order of `side_sign` · `size` on `symbols[si]` while its rate is stale, in the live
    /// gate's word, or `None`: a covered reduce is never held up (`vike_model::is_covered_reduce`, the
    /// gate's own predicate), and a symbol the run does not convert is never stale.
    pub(super) fn quote_stale_refusal(
        &self,
        si: usize,
        side_sign: i32,
        size: f64,
    ) -> Option<&'static str> {
        let reduce = vike_model::is_covered_reduce(side_sign, self.symbol[si].pos.size, size);
        (!reduce && self.quote_rate_stale(si)).then_some(QUOTE_RATE_STALE)
    }
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod conversion_tests;
