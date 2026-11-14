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
//! # Fail closed, never par
//!
//! Before the pair's first close, and while the rate in force is stale, an OPENING order on the symbol is
//! refused `quote-rate-stale` (the live gate's word, `vike_execution::RiskGate`); a covered reduce is never
//! held up. With no rate yet the symbol carries its quote-currency value, which books nothing: no order can
//! open a position there.
//!
//! An empty [`crate::StrategyEngineConfig::conversions`] (every run that names no pair) keeps an EMPTY
//! table, so it pays one `is_empty` per event and one `Vec::get` per fill, and books every number it booked
//! before this module existed, bit for bit.

use vike_marketdata::Bar;
use vike_model::{AccountPerQuote, AccountPerQuoteLeg, ContractValue};

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
    /// Sorted by `ts`, each close a usable rate (the harness refused the run otherwise).
    pub bars: Vec<Bar>,
    /// The run's symbols quoted in [`Self::quote_currency`], each BOOKED through this series (the module doc).
    /// The engine names no currency: the harness says which symbols these are. A LINEAR contract each.
    pub symbols: Vec<String>,
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
}

impl ConversionSeries {
    /// **The rate at `t`**: the close of the last bar that had closed by `t`, oriented by [`Self::leg`],
    /// with its staleness at `t`; `None` before the first bar closes (fail closed: no rate, no opening).
    #[must_use]
    pub fn rate_at(&self, t: i64) -> Option<ConversionRate> {
        self.rate_of_closed(self.bars.partition_point(|b| b.closed_at(self.interval_ms, t)), t)
    }

    /// The rate at `t` once the first `closed` bars have closed: [`Self::rate_at`]'s answer, for a caller
    /// that counts the closed bars itself (the booking's cursor steps the count once per bar).
    fn rate_of_closed(&self, closed: usize, t: i64) -> Option<ConversionRate> {
        let bar = self.bars.get(closed.checked_sub(1)?)?;
        let rate = AccountPerQuote::of_mid(self.leg, bar.close).ok()?;
        let as_of_ms = bar.timestamp.saturating_add(self.interval_ms);
        Some(ConversionRate { rate, as_of_ms, stale: self.stale_at(as_of_ms, t) })
    }

    /// Whether a rate whose bar closed at `as_of_ms` is stale at `t`: older than twice the interval.
    fn stale_at(&self, as_of_ms: i64, t: i64) -> bool {
        t.saturating_sub(as_of_ms) > self.interval_ms.saturating_mul(2)
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
    /// The rate in force, `None` before the series' first close.
    current: Option<ConversionRate>,
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
        QuoteConversionCursor { quote_value, series, closed: 0, current: None }
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
            let series = &self.conversions[c.series];
            let before = c.closed;
            while series.bars.get(c.closed).is_some_and(|b| b.closed_at(series.interval_ms, t)) {
                c.closed += 1;
            }
            if c.closed == before {
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
            self.cash += old.cash_leg(pos.size, pos.average_price)
                - value.cash_leg(pos.size, pos.average_price);
            self.value_by_si[si] = value;
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
    /// or the one in force is older than twice its pair's interval ([`ConversionSeries::rate_at`]'s rule).
    pub(crate) fn quote_rate_stale(&self, si: usize) -> bool {
        let Some(Some(c)) = self.quote_conversion_by_si.get(si) else { return false };
        let series = &self.conversions[c.series];
        c.current.is_none_or(|r| series.stale_at(r.as_of_ms, self.now))
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
