//! A COVERED CALL over recorded option chains ([`CoveredCall`]) that picks its strike BY DELTA: hold
//! `quantity` of the underlying, and at each sale instant sell `quantity` of the candidate call whose
//! delta is nearest `target_delta`, holding it to its expiry, where the backtest settles it to cash at its
//! intrinsic value. The weekly covered call (each Friday, the one-week call of about 0.3 delta) is this
//! file with one sale instant a week and that week's calls as candidates; which calls are candidates is
//! the profile's (the study `covered_call` writes it, the book's options chapter).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = """\
//!     it trades the option legs a backtest profile's `[data.options]` table reads from a \
//!     stored chain, on the event lane; no mount serves a chain's legs as prices"""
//!
//! [params.underlying]
//! type = "text"
//!
//! [params.quantity]
//! type = "number"
//! default = 1.0
//!
//! [params.target_delta]
//! type = "number"
//! default = 0.3
//! range = [0.0, 1.0]
//!
//! [params.tenor_days]
//! type = "number"
//! default = 7.0
//!
//! [params.max_quote_age_hours]
//! type = "number"
//! default = 24.0
//!
//! [params.calls]
//! type = "array"
//!
//! [params.strikes]
//! type = "array"
//!
//! [params.expiries]
//! type = "array"
//!
//! [params.sell_at]
//! type = "array"
//! ```
//!
//! ## What it does
//!
//! On the run's first quote of `underlying` it buys `quantity` of it at market (the cover). The candidates are
//! `calls`, each with its strike (`strikes`) and its expiry in epoch milliseconds (`expiries`), paired by
//! position. At the first quote of the run at or after each `sell_at` instant (epoch milliseconds, UTC) it
//! looks at the candidates quoted within `max_quote_age_hours`, still live and never sold, keeps those of
//! the expiry nearest the sale instant plus `tenor_days`, and computes each one's delta on the quotes it
//! holds then: its IV inverted from its last price against the underlying's last price, then the delta at
//! that IV (`vike_options::delta_at_quote`), the platform's one Black–Scholes (365-day year, no rate, no
//! dividend). It sells the call whose delta is nearest `target_delta` (the higher strike on a tie). With
//! no candidate to price yet it asks again at the next quote, until the next sale instant comes, which
//! abandons the sale.
//!
//! A market order fills on the leg's NEXT priced row (no fill without a price). A candidate the chain
//! never priced is no symbol of the run, so nothing is placed on it. It closes nothing itself: the engine
//! settles each call at its expiry against the underlying's price. With no `underlying` it places nothing.
//!
//! Only untagged market [`OrderBroker::place`]s.

use vike_marketdata::{OptionKind, TickQuote};
use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, OrderPlacement, Strategy};

const HOUR_MS: f64 = 3_600_000.0;

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(CoveredCall::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    let c = CoveredCall::from_params(params);
    let list = |cell: &dyn Fn(&Candidate) -> String| {
        format!("[{}]", c.candidates.iter().map(cell).collect::<Vec<_>>().join(", "))
    };
    let sales: Vec<String> = c.sell_at.iter().map(i64::to_string).collect();
    vec![
        (
            "underlying",
            c.underlying.clone().unwrap_or_else(|| "(none: places nothing)".to_string()),
        ),
        ("quantity", format!("{}", c.quantity)),
        ("target_delta", format!("{}", c.target_delta)),
        ("tenor_days", format!("{}", c.tenor_ms as f64 / (24.0 * HOUR_MS))),
        ("max_quote_age_hours", format!("{}", c.max_quote_age_ms as f64 / HOUR_MS)),
        ("calls", list(&|k| k.call.clone())),
        ("strikes", list(&|k| k.strike.to_string())),
        ("expiries", list(&|k| k.expiry_ms.to_string())),
        ("sell_at", format!("[{}]", sales.join(", "))),
    ]
}

/// One call the file may sell, and the last quote it saw of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The call's symbol, as the chain names it.
    pub call: String,
    /// Its strike.
    pub strike: f64,
    /// Its expiry, epoch milliseconds.
    pub expiry_ms: i64,
    /// `(when, price)` of its last quote; `None` until it is quoted.
    last: Option<(i64, f64)>,
    sold: bool,
}

/// Hold the underlying, sell the call nearest the target delta at each sale instant. See the module doc.
#[derive(Debug, Clone)]
pub struct CoveredCall {
    /// The symbol held as the cover. `None`: nothing is placed.
    pub underlying: Option<String>,
    /// Units of the underlying held, and of each call sold. Default `1.0`.
    pub quantity: f64,
    /// The delta sought. Default `0.3`.
    pub target_delta: f64,
    /// The expiry sought, as a distance from the sale instant. Default seven days.
    pub tenor_ms: i64,
    /// How old a candidate's last quote may be when it is priced. Default a day.
    pub max_quote_age_ms: i64,
    /// The calls, in the profile's order.
    pub candidates: Vec<Candidate>,
    /// The sale instants, ascending.
    pub sell_at: Vec<i64>,
    next_sale: usize,
    spot: Option<f64>,
    covered: bool,
}

impl CoveredCall {
    /// Read the knobs (module doc): the lists pair by position and stop at the shortest; a cell of the
    /// wrong type drops its candidate.
    pub fn from_params(params: &Params) -> Self {
        let text = |key: &str| params.get(key).and_then(ParamValue::as_str).map(str::to_string);
        let number = |key: &str, default: f64| {
            params.get(key).and_then(ParamValue::as_f64).unwrap_or(default)
        };
        let list = |key: &str| {
            params.get(key).and_then(ParamValue::as_array).map(Vec::as_slice).unwrap_or_default()
        };
        let candidates = list("calls")
            .iter()
            .zip(list("strikes"))
            .zip(list("expiries"))
            .filter_map(|((call, strike), expiry)| {
                Some(Candidate {
                    call: call.as_str()?.to_string(),
                    strike: strike.as_f64()?,
                    expiry_ms: expiry.as_integer()?,
                    last: None,
                    sold: false,
                })
            })
            .collect();
        let mut sell_at: Vec<i64> =
            list("sell_at").iter().filter_map(ParamValue::as_integer).collect();
        sell_at.sort_unstable();
        CoveredCall {
            underlying: text("underlying"),
            quantity: number("quantity", 1.0),
            target_delta: number("target_delta", 0.3),
            tenor_ms: (number("tenor_days", 7.0) * 24.0 * HOUR_MS) as i64,
            max_quote_age_ms: (number("max_quote_age_hours", 24.0) * HOUR_MS) as i64,
            candidates,
            sell_at,
            next_sale: 0,
            spot: None,
            covered: false,
        }
    }

    /// The delta of `c` at `now` on its last price and the underlying's `spot`, through its implied
    /// volatility; `None` when either cannot be computed (an expired leg, a price outside the model's).
    fn delta_of(c: &Candidate, now: i64, spot: f64) -> Option<f64> {
        let (_, price) = c.last?;
        let years = vike_options::years_to_expiry_ms(c.expiry_ms, now);
        vike_options::delta_at_quote(
            None,
            Some(price),
            spot,
            c.strike,
            years,
            OptionKind::Call,
            0.0,
        )
    }

    /// The candidate to sell at `now` for the sale instant `sale`, or `None` (module doc).
    fn choose(&self, now: i64, sale: i64) -> Option<usize> {
        let spot = self.spot?;
        let priced: Vec<(usize, f64)> = self
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                !c.sold
                    && c.expiry_ms > now
                    && c.last.is_some_and(|(at, _)| now - at <= self.max_quote_age_ms)
            })
            .filter_map(|(i, c)| Some((i, Self::delta_of(c, now, spot)?)))
            .collect();
        let target = sale + self.tenor_ms;
        let expiry = |i: usize| self.candidates[i].expiry_ms;
        let nearest =
            priced.iter().map(|(i, _)| expiry(*i)).min_by_key(|e| ((e - target).abs(), *e))?;
        priced
            .iter()
            .filter(|(i, _)| expiry(*i) == nearest)
            .min_by(|(a, da), (b, db)| {
                let gap = |d: f64| (d - self.target_delta).abs();
                let (ka, kb) = (self.candidates[*a].strike, self.candidates[*b].strike);
                gap(*da).total_cmp(&gap(*db)).then(kb.total_cmp(&ka))
            })
            .map(|(i, _)| *i)
    }
}

impl<B: OrderBroker> Strategy<B> for CoveredCall {
    fn on_quote_tick(&mut self, broker: &mut B, quote: &TickQuote) {
        let Some(underlying) = self.underlying.clone() else { return };
        let now = quote.timestamp;
        if quote.symbol == underlying {
            self.spot = Some(quote.mid());
            if !self.covered {
                broker.place(&underlying, "", 1, self.quantity, OrderPlacement::Market, false);
                self.covered = true;
            }
        } else if let Some(c) = self.candidates.iter_mut().find(|c| c.call == quote.symbol) {
            c.last = Some((now, quote.mid()));
        }
        // A sale still unmade when the next one comes is abandoned.
        while self.sell_at.get(self.next_sale + 1).is_some_and(|next| now >= *next) {
            self.next_sale += 1;
        }
        let Some(&sale) = self.sell_at.get(self.next_sale).filter(|sale| now >= **sale) else {
            return;
        };
        if let Some(i) = self.choose(now, sale) {
            broker.place(
                &self.candidates[i].call,
                "",
                -1,
                self.quantity,
                OrderPlacement::Market,
                false,
            );
            self.candidates[i].sold = true;
            self.next_sale += 1;
        }
    }
}

// Run by `crates/vike-strategy-lab` (`cargo test -p vike-strategy-lab`); a build of this file
// compiles no test. The end-to-end run over a stored chain is the harness's:
// `crates/vike-backtest/tests/option_legs.rs`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::OrderBrokerRecording;

    const DAY: i64 = 86_400_000;

    fn quote(symbol: &str, timestamp: i64, price: f64) -> TickQuote {
        TickQuote {
            timestamp,
            local_timestamp: 0,
            bid: price,
            ask: price,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: symbol.into(),
        }
    }

    fn params(text: &str) -> Params {
        toml::from_str(text).unwrap()
    }

    /// Three calls of one expiry, a week out, each priced at 50% volatility on spot 100.
    fn three_calls(sale: i64) -> (CoveredCall, Vec<TickQuote>) {
        let expiry = sale + 7 * DAY;
        let c = CoveredCall::from_params(&params(&format!(
            "underlying = \"U\"\ncalls = [\"K100\", \"K104\", \"K110\"]\n\
             strikes = [100.0, 104.0, 110.0]\nexpiries = [{expiry}, {expiry}, {expiry}]\n\
             sell_at = [{sale}]\n"
        )));
        let years = vike_options::years_to_expiry_ms(expiry, sale);
        let price =
            |k: f64| vike_options::black_scholes_price(100.0, k, years, 0.5, OptionKind::Call, 0.0);
        let quotes = [("K100", 100.0), ("K104", 104.0), ("K110", 110.0)]
            .iter()
            .map(|(call, k)| quote(call, sale - 1, price(*k).unwrap()))
            .collect();
        (c, quotes)
    }

    #[test]
    fn from_params_reads_the_candidates_and_the_defaults() {
        let (c, _) = three_calls(1_000 * DAY);
        assert_eq!(c.underlying.as_deref(), Some("U"));
        assert_eq!((c.quantity, c.target_delta), (1.0, 0.3));
        assert_eq!((c.tenor_ms, c.max_quote_age_ms), (7 * DAY, DAY));
        assert_eq!(c.candidates.len(), 3);
        assert_eq!(c.candidates[1].strike, 104.0);
        assert_eq!(c.sell_at, vec![1_000 * DAY]);
    }

    /// At 50% volatility a week out (σ√T = 0.5·√(7/365) = 0.06924), the deltas are N(d1) of
    /// d1 = (ln(100/K) + σ²T/2) / σ√T: K 100 → N(0.0346) ≈ 0.514, K 104 → N(−0.5318) ≈ 0.297,
    /// K 110 → N(−1.3419) ≈ 0.090. The nearest to 0.3 is the 104 call; the cover goes first.
    #[test]
    fn it_covers_then_sells_the_call_nearest_the_target_delta_once() {
        let sale = 1_000 * DAY;
        let (mut c, quotes) = three_calls(sale);
        let mut broker = OrderBrokerRecording::default();
        c.on_quote_tick(&mut broker, &quote("U", sale - 2, 100.0));
        for q in &quotes {
            c.on_quote_tick(&mut broker, q);
        }
        assert_eq!(broker.orders, vec![("U".to_string(), 1, 1.0)], "not yet the sale instant");
        c.on_quote_tick(&mut broker, &quote("U", sale, 100.0));
        c.on_quote_tick(&mut broker, &quote("U", sale + 1, 100.0));
        assert_eq!(
            broker.orders,
            vec![("U".to_string(), 1, 1.0), ("K104".to_string(), -1, 1.0)],
            "one cover, one sale, the 0.3-delta call"
        );
    }

    #[test]
    fn a_stale_or_unquoted_candidate_is_not_sold_and_no_underlying_places_nothing() {
        let sale = 1_000 * DAY;
        let (mut c, quotes) = three_calls(sale);
        let mut broker = OrderBrokerRecording::default();
        for q in &quotes {
            c.on_quote_tick(&mut broker, &quote(&q.symbol, sale - 2 * DAY, q.bid));
        }
        c.on_quote_tick(&mut broker, &quote("U", sale, 100.0));
        assert_eq!(broker.orders, vec![("U".to_string(), 1, 1.0)], "quotes two days old");

        let mut idle = CoveredCall { underlying: None, ..three_calls(sale).0 };
        let mut broker = OrderBrokerRecording::default();
        idle.on_quote_tick(&mut broker, &quote("U", sale, 100.0));
        assert!(broker.orders.is_empty());
    }
}
