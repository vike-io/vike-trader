//! Property suite for the multi-currency money seam (phase 1): a symbol QUOTED in a currency other
//! than the account's books its money in the account's currency through one rate per quote currency
//! (`crates/vike-exec/src/account/fx.rs`'s module doc is the contract; `Account::admit_quoted` is the
//! one writer). The rate `r` is account-currency units per ONE unit of the quote currency (1/150 for
//! JPY on a USD account, never 150).
//!
//! The laws, in the order of the design's test strategy (section 7):
//!
//! 1. IDENTITY: a symbol quoted in the account's currency folds bit-identically beside an untouched
//!    foreign-quoted one (`to_bits` on every ledger field).
//! 2. RELABEL INVARIANCE (metamorphic): a JPY-quoted symbol at a constant rate books the same realized
//!    PnL, equity, margin in use and notional as an account-quoted symbol whose every price is `r`
//!    times as large. The two sides associate one product differently, so they agree to
//!    `SCALE_TOL` times the product's scale, not exactly.
//! 3. ROUND TRIP IN TWO CURRENCIES: interleaved fills on a JPY- and a GBP-quoted symbol with the rate
//!    refreshed between fills realize what a hand-written oracle (each closing fill at its own
//!    rate-at-fold) realizes; at constant rates the total is the sum of the single-currency runs.
//! 4. ORDER INDEPENDENCE: refreshes of different currencies commute, the newest `as_of` wins for one
//!    currency whatever the arrival order, and a refresh never changes what an earlier fill booked.
//! 5. FAIL CLOSED: a rate that is stale, jump-refused, missing or invalid never opens an order, never
//!    admits a symbol and never changes a standing state; a covered reduce is never held up.
//! 6. ORIENTATION: an admitted rate reads back as account units per quote unit (the real admission
//!    path; `RateBook`, which no production code calls, is not part of this law), and a flipped rate
//!    is refused by the jump cap (outside the band the cap cannot see).
//! 7. SNAPSHOT: a restore reproduces the effective grid bit for bit and re-snapshots equal; an account
//!    or engine with no foreign symbol serializes with no `fx` key at all.
//!
//! No wall clock is read: every `now_ms` is passed in. FAILURE POLICY: a counterexample is a REAL
//! finding about the engine (never adjust a bound here to make it pass, decision 0021): commit the
//! seed beside this file as `quote_rate_props.proptest-regressions`. Own test binary ON PURPOSE: a
//! proptest sidecar disqualifies a file from a grouped binary (`crates/vike-exec/CLAUDE.md`).

use std::collections::BTreeSet;

use proptest::prelude::*;
use ustr::Ustr;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AccountSnapshot, AdmitOutcome, BalanceMode, DEFAULT_QUOTE_RATE_TTL_MS, EngineSnapshot,
    EventHandler, ExecutionEngine, Fold, MAX_QUOTE_RATE_STEP, MarkSource, Outbox, PositionEntry,
    RateBook, RiskGate, SymbolAdmission, state_hash,
};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{ContractValue, NotionalKind, OrderRequest, OrderType, RiskLimits, SymbolGrid};

const VENUE: &str = "oanda";
const ACCT: &str = "USD";
const MOUNTED: &str = "BTCUSD";
/// The one instant every fixture is built at; each test moves time explicitly from it.
const NOW: i64 = 1_700_000_000_000;

/// Two evaluations of one product that differ only in association agree within a few ULP of the
/// product's scale (`|p|·|q|·u·r`): `f64::EPSILON` is 2.2e-16, so 1e-12 is ~4,500 ULP, far above any
/// honest rounding of a 40-fill fold and far below a real conversion error (a 0.01% one is 1e-4).
const SCALE_TOL: f64 = 1e-12;

type Check = Result<(), TestCaseError>;

// ---- fixtures ---------------------------------------------------------------------------------

fn fill(id: usize, symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("T", id),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn account() -> Account {
    Account::new(1.0, VENUE, None, BalanceMode::Delta)
}

/// Admit `symbol` as quoted in `ccy` at `rate`, observed at `NOW`.
fn quote(a: &mut Account, symbol: &str, ccy: &str, unit: f64, rate: f64) {
    let admitted = a.admit_quoted(
        symbol,
        ccy,
        Some(ContractValue::linear(unit)),
        Some((rate, NOW)),
        ACCT,
        NOW,
    );
    assert_eq!(admitted, Ok(()), "a fresh, in-range first rate is admitted");
}

fn mark(a: &mut Account, symbol: &str, px: f64) {
    a.set_mark_from(VENUE, symbol, px, MarkSource::TradeTick, NOW);
}

fn position(a: &Account, symbol: &str) -> Option<PositionEntry> {
    a.positions.get(&(Ustr::from(VENUE), Ustr::from(symbol), PositionSide::Both)).copied()
}

/// `|a - b|` within `SCALE_TOL` of a SCALE the caller names (the cancelling terms' size).
fn close(a: f64, b: f64, scale: f64) -> bool {
    (a - b).abs() <= SCALE_TOL * scale
}

/// Two evaluations of ONE non-cancelling product: relative to their own size.
fn rel_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= SCALE_TOL * a.abs().max(b.abs())
}

// ---- generators -------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Step {
    sym: usize,
    side: i32,
    qty: f64,
    px: f64,
}

/// A fill on one of `nsym` symbols. Quantities are small whole numbers so that adds, partial
/// reduces, flips and exact closes all occur and every size is exact: position state then never
/// depends on rounding, only prices do.
fn step(nsym: usize) -> impl Strategy<Value = Step> {
    (0..nsym, prop_oneof![Just(1i32), Just(-1i32)], 1u8..=6, 0.5f64..500.0)
        .prop_map(|(sym, side, q, px)| Step { sym, side, qty: f64::from(q), px })
}

/// Account units per quote unit: yen-like, pound-like, near par, and exactly par.
fn any_rate() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => 1e-4f64..1e2,
        3 => (1.0f64 / 400.0)..(1.0f64 / 50.0),
        2 => 0.5f64..2.0,
        1 => Just(1.0f64),
    ]
}

fn unit() -> impl Strategy<Value = f64> {
    1e-2f64..1e3
}

fn any_value() -> impl Strategy<Value = ContractValue> {
    prop_oneof![unit().prop_map(ContractValue::linear), unit().prop_map(ContractValue::inverse)]
}

// ---- law 1: identity --------------------------------------------------------------------------

/// Every ledger field and every valuation read of two accounts, to the bit.
fn same_ledger(a: &Account, b: &Account) -> Check {
    prop_assert_eq!(a.positions.len(), b.positions.len());
    for ((ka, pa), (kb, pb)) in a.positions.iter().zip(b.positions.iter()) {
        prop_assert_eq!(ka, kb);
        prop_assert_eq!(pa.size.to_bits(), pb.size.to_bits());
        prop_assert_eq!(pa.avg_px.to_bits(), pb.avg_px.to_bits());
        prop_assert_eq!(pa.margin_mode, pb.margin_mode);
        prop_assert_eq!(pa.isolated_margin.map(f64::to_bits), pb.isolated_margin.map(f64::to_bits));
    }
    prop_assert_eq!(a.realized_pnl.to_bits(), b.realized_pnl.to_bits());
    prop_assert_eq!(a.closed_pnls.len(), b.closed_pnls.len());
    for (x, y) in a.closed_pnls.iter().zip(&b.closed_pnls) {
        prop_assert_eq!(x.to_bits(), y.to_bits());
    }
    prop_assert_eq!(a.balance.to_bits(), b.balance.to_bits());
    prop_assert_eq!(a.fees_paid.to_bits(), b.fees_paid.to_bits());
    prop_assert_eq!(a.funding_paid.to_bits(), b.funding_paid.to_bits());
    prop_assert_eq!(a.fees_by_asset.len(), b.fees_by_asset.len());
    for ((ka, x), (kb, y)) in a.fees_by_asset.iter().zip(b.fees_by_asset.iter()) {
        prop_assert_eq!(ka, kb);
        prop_assert_eq!(x.to_bits(), y.to_bits());
    }
    prop_assert_eq!(a.equity_all(1_000.0).to_bits(), b.equity_all(1_000.0).to_bits());
    let margin = |acct: &Account| {
        acct.margin_in_use_priced(|(v, s, _), _| acct.mark_of_key(&(*v, *s)), |_, _| Some(0.05))
    };
    prop_assert_eq!(margin(a).to_bits(), margin(b).to_bits());
    let net = |acct: &Account| acct.net_notional_priced(|(v, s, _), _| acct.mark_of_key(&(*v, *s)));
    prop_assert_eq!(net(a).to_bits(), net(b).to_bits());
    let gross =
        |acct: &Account| acct.gross_notional_priced(|(v, s, _), _| acct.mark_of_key(&(*v, *s)));
    prop_assert_eq!(gross(a).to_bits(), gross(b).to_bits());
    Ok(())
}

proptest! {
    /// LAW 1. A random fill sequence (fees and rebates, linear and inverse contracts) on a symbol quoted
    /// in the account's currency folds to the same bits whether or not the account also holds a
    /// foreign-quoted symbol it never trades. A LINEAR contract's fees are in the account's currency or
    /// in none; a fee in a third currency is the one thing a foreign symbol on the book changes for it
    /// (it converts at the book's rate, F3: `quote_money`'s test owns that), but an INVERSE contract's
    /// coin fee keeps its own conversion, so the inverse run still draws every currency.
    #[test]
    fn a_symbol_quoted_in_the_account_currency_folds_bit_identically_beside_an_untouched_foreign_one(
        value in any_value(),
        foreign_unit in unit(),
        foreign_rate in any_rate(),
        steps in prop::collection::vec((step(1), -0.5f64..2.0, 0usize..4), 1..40),
    ) {
        let assets: &[&str] = if value.kind == NotionalKind::Inverse {
            &["", "USD", "JPY", "BNB"]
        } else {
            &["", "USD"]
        };
        let mut plain = account();
        plain.admit_contract_value("EURUSD", value);
        let mut with_fx = plain.clone();
        quote(&mut with_fx, "USDJPY", "JPY", foreign_unit, foreign_rate);
        mark(&mut with_fx, "USDJPY", 150.0);
        for (i, (s, fee, asset)) in steps.iter().enumerate() {
            let f = FillEvent {
                commission: *fee,
                commission_asset: assets[*asset % assets.len()].into(),
                ..fill(i, "EURUSD", s.side, s.qty, s.px)
            };
            prop_assert_eq!(plain.apply_fill(&f), with_fx.apply_fill(&f));
            mark(&mut plain, "EURUSD", s.px);
            mark(&mut with_fx, "EURUSD", s.px);
        }
        same_ledger(&plain, &with_fx)?;
        prop_assert_eq!(with_fx.fees_unconverted(), 0);
    }
}

// ---- law 2: relabel invariance ----------------------------------------------------------------

proptest! {
    /// LAW 2. A JPY-quoted linear symbol at constant `r` and an account-quoted symbol with every
    /// price (fills and marks) multiplied by `r` agree on realized PnL, every closed PnL, equity,
    /// margin in use and both notionals. Sizes do not depend on a price, so they are bit-equal.
    #[test]
    fn a_foreign_quoted_symbol_books_what_the_relabelled_account_quoted_one_books(
        u in unit(),
        r in any_rate(),
        steps in prop::collection::vec(step(1), 1..40),
        m in 0.5f64..500.0,
    ) {
        let mut fx_acct = account();
        quote(&mut fx_acct, "XYZJPY", "JPY", u, r);
        let mut loc_acct = account();
        loc_acct.admit_contract_value("XYZJPY", ContractValue::linear(u));
        let mut scale = 0.0;
        for (i, s) in steps.iter().enumerate() {
            fx_acct.apply_fill(&fill(i, "XYZJPY", s.side, s.qty, s.px));
            loc_acct.apply_fill(&fill(i, "XYZJPY", s.side, s.qty, s.px * r));
            scale += s.qty * s.px * u * r;
        }
        mark(&mut fx_acct, "XYZJPY", m);
        mark(&mut loc_acct, "XYZJPY", m * r);
        let f = position(&fx_acct, "XYZJPY").expect("a fill leaves a row");
        let l = position(&loc_acct, "XYZJPY").expect("a fill leaves a row");
        prop_assert_eq!(f.size.to_bits(), l.size.to_bits());
        prop_assert!(close(f.avg_px * r, l.avg_px, l.avg_px.abs()), "avg {} vs {}", f.avg_px * r, l.avg_px);
        let scale_all = scale + f.size.abs() * m * u * r;
        prop_assert!(
            close(fx_acct.realized_pnl, loc_acct.realized_pnl, scale),
            "realized {} vs {}", fx_acct.realized_pnl, loc_acct.realized_pnl
        );
        prop_assert_eq!(fx_acct.closed_pnls.len(), loc_acct.closed_pnls.len());
        for (x, y) in fx_acct.closed_pnls.iter().zip(&loc_acct.closed_pnls) {
            prop_assert!(close(*x, *y, scale), "closed {x} vs {y}");
        }
        let (ef, el) = (fx_acct.equity_all(0.0), loc_acct.equity_all(0.0));
        prop_assert!(close(ef, el, scale_all), "equity {ef} vs {el}");
        let margin = |a: &Account| {
            a.margin_in_use_priced(|(v, s, _), _| a.mark_of_key(&(*v, *s)), |_, _| Some(0.05))
        };
        prop_assert!(rel_close(margin(&fx_acct), margin(&loc_acct)));
        let net = |a: &Account| a.net_notional_priced(|(v, s, _), _| a.mark_of_key(&(*v, *s)));
        prop_assert!(rel_close(net(&fx_acct), net(&loc_acct)));
        let gross = |a: &Account| a.gross_notional_priced(|(v, s, _), _| a.mark_of_key(&(*v, *s)));
        prop_assert!(rel_close(gross(&fx_acct), gross(&loc_acct)));
    }
}

// ---- law 3: round trip in two currencies ------------------------------------------------------

const SYMS: [(&str, &str); 2] = [("USDJPY", "JPY"), ("EURGBP", "GBP")];

/// The textbook position fold, written from the formulas and not from `compute_fill`: whole-number
/// quantities keep every size exact, so only the average price carries rounding.
#[derive(Debug, Clone, Copy, Default)]
struct Oracle {
    size: f64,
    avg: f64,
}

impl Oracle {
    /// `(closed quantity, entry average, sign of the position closed)` when the fill closes any.
    fn fold(&mut self, side: i32, qty: f64, px: f64) -> Option<(f64, f64, f64)> {
        let delta = f64::from(side) * qty;
        if self.size == 0.0 {
            *self = Oracle { size: delta, avg: px };
            return None;
        }
        if (self.size > 0.0) == (delta > 0.0) {
            let n = self.size + delta;
            self.avg = (self.avg * self.size.abs() + px * qty) / n.abs();
            self.size = n;
            return None;
        }
        let sign = if self.size > 0.0 { 1.0 } else { -1.0 };
        let closed = qty.min(self.size.abs());
        let entry = self.avg;
        let remaining = self.size.abs() - closed;
        if remaining > 0.0 {
            self.size = sign * remaining;
        } else if qty > closed {
            *self = Oracle { size: delta.signum() * (qty - closed), avg: px };
        } else {
            *self = Oracle::default();
        }
        Some((closed, entry, sign))
    }
}

type Leg = (Step, Option<f64>, Option<f64>);

struct Played {
    acct: Account,
    /// The oracle's realized PnL, and the size of the terms it sums.
    oracle: f64,
    scale: f64,
    /// Per closing fill, in order: the oracle's PnL and its own scale.
    closes: Vec<(f64, f64)>,
}

/// Fold `legs` on an account holding both symbols. Before a fill, either currency's rate may be
/// refreshed to `r0 * factor` (always inside the jump cap, always newer, always fresh).
fn play(units: [f64; 2], r0: [f64; 2], legs: &[Leg]) -> Played {
    let mut acct = account();
    for (k, (sym, ccy)) in SYMS.iter().enumerate() {
        quote(&mut acct, sym, ccy, units[k], r0[k]);
    }
    let mut rate = r0;
    let mut oracle = [Oracle::default(); 2];
    let mut out = Played { acct: account(), oracle: 0.0, scale: 0.0, closes: Vec::new() };
    for (i, (s, rj, rg)) in legs.iter().enumerate() {
        let t = NOW + 1_000 * (i as i64 + 1);
        for (k, factor) in [rj, rg].into_iter().enumerate() {
            if let Some(f) = factor {
                rate[k] = r0[k] * f;
                let sent =
                    acct.admit_quoted(SYMS[k].0, SYMS[k].1, None, Some((rate[k], t)), ACCT, t);
                assert_eq!(sent, Ok(()), "a refresh inside the step cap is taken");
            }
        }
        acct.apply_fill(&fill(i, SYMS[s.sym].0, s.side, s.qty, s.px));
        if let Some((closed, entry, sign)) = oracle[s.sym].fold(s.side, s.qty, s.px) {
            let pnl = closed * sign * (s.px - entry) * units[s.sym] * rate[s.sym];
            let scale = closed * (s.px.abs() + entry.abs()) * units[s.sym] * rate[s.sym];
            out.oracle += pnl;
            out.scale += scale;
            out.closes.push((pnl, scale));
        }
    }
    out.acct = acct;
    out
}

fn many_legs() -> impl Strategy<Value = Vec<Leg>> {
    prop::collection::vec(
        (step(2), prop::option::of(0.85f64..1.17), prop::option::of(0.85f64..1.17)),
        1..40,
    )
}

proptest! {
    /// LAW 3a. Each closing fill converts at the rate in force when it folds: the account's realized
    /// PnL, and every entry of `closed_pnls`, equal the oracle's sum of
    /// `closed · (exit - entry average) · unit · rate`.
    #[test]
    fn realized_pnl_converts_each_closing_fill_at_its_own_rate_at_fold(
        units in [unit(), unit()],
        r0 in [any_rate(), any_rate()],
        legs in many_legs(),
    ) {
        let p = play(units, r0, &legs);
        prop_assert!(
            close(p.acct.realized_pnl, p.oracle, p.scale),
            "realized {} vs oracle {}", p.acct.realized_pnl, p.oracle
        );
        prop_assert_eq!(p.acct.closed_pnls.len(), p.closes.len());
        for (got, (want, scale)) in p.acct.closed_pnls.iter().zip(&p.closes) {
            prop_assert!(close(*got, *want, *scale), "closed {got} vs oracle {want}");
        }
    }

    /// LAW 3b. With the rates held constant (a refresh at the same rate is a no-op) the two-currency
    /// account is the two single-currency accounts: every closed PnL and every position bit for bit,
    /// the total up to the order the same terms are added in.
    #[test]
    fn at_constant_rates_the_total_is_the_sum_of_the_single_currency_runs(
        units in [unit(), unit()],
        r0 in [any_rate(), any_rate()],
        steps in prop::collection::vec((step(2), any::<bool>(), any::<bool>()), 1..40),
    ) {
        let mut both = account();
        let mut solo = [account(), account()];
        for (k, (sym, ccy)) in SYMS.iter().enumerate() {
            quote(&mut both, sym, ccy, units[k], r0[k]);
            quote(&mut solo[k], sym, ccy, units[k], r0[k]);
        }
        let mut closes: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
        for (i, (s, rj, rg)) in steps.iter().enumerate() {
            let t = NOW + 1_000 * (i as i64 + 1);
            for (k, again) in [rj, rg].into_iter().enumerate() {
                if *again {
                    let sent = both.admit_quoted(SYMS[k].0, SYMS[k].1, None, Some((r0[k], t)), ACCT, t);
                    prop_assert_eq!(sent, Ok(()));
                }
            }
            let f = fill(i, SYMS[s.sym].0, s.side, s.qty, s.px);
            let before = both.closed_pnls.len();
            both.apply_fill(&f);
            if both.closed_pnls.len() > before {
                closes[s.sym].push(*both.closed_pnls.last().expect("it just grew"));
            }
            solo[s.sym].apply_fill(&f);
        }
        let mut scale = 0.0;
        for (k, (sym, _)) in SYMS.iter().enumerate() {
            prop_assert_eq!(closes[k].len(), solo[k].closed_pnls.len());
            for (x, y) in closes[k].iter().zip(&solo[k].closed_pnls) {
                prop_assert_eq!(x.to_bits(), y.to_bits());
                scale += y.abs();
            }
            let (a, b) = (position(&both, sym), position(&solo[k], sym));
            prop_assert_eq!(a.map(|p| p.size.to_bits()), b.map(|p| p.size.to_bits()));
            prop_assert_eq!(a.map(|p| p.avg_px.to_bits()), b.map(|p| p.avg_px.to_bits()));
        }
        let sum = solo[0].realized_pnl + solo[1].realized_pnl;
        // The same terms added in another order: the bound is the size of the terms, not of the sum.
        prop_assert!(close(both.realized_pnl, sum, scale), "{} vs {}", both.realized_pnl, sum);
    }

    /// LAW 3c (fees, the rule in `Account::fx_commission`). On a foreign-quoted symbol a fee stated in
    /// the account's currency or in none is booked as stated, one stated in the symbol's quote
    /// currency is booked times that symbol's rate, one in a THIRD currency the book holds a rate for
    /// (GBP, the quote of a second symbol) is booked times THAT rate, and one in a third currency the
    /// book has no rate for is left out of the ledger (counted, never booked at par); the per-asset
    /// tally keeps the venue's raw number.
    #[test]
    fn a_fee_is_booked_in_the_account_currency_or_not_at_all(
        u in unit(),
        r in any_rate(),
        r_gbp in any_rate(),
        fee in -0.5f64..2.0,
        asset in 0usize..5,
        s in step(1),
    ) {
        const ASSETS: [&str; 5] = ["", ACCT, "JPY", "BNB", "GBP"];
        let mut a = account();
        quote(&mut a, "USDJPY", "JPY", u, r);
        quote(&mut a, "EURGBP", "GBP", u, r_gbp);
        let f = FillEvent {
            commission: fee,
            commission_asset: ASSETS[asset].into(),
            ..fill(0, "USDJPY", s.side, s.qty, s.px)
        };
        a.apply_fill(&f);
        let (booked, left_out) = match asset {
            0 | 1 => (fee, 0),
            2 => (fee * r, 0),
            4 => (fee * r_gbp, 0),
            _ => (0.0, 1),
        };
        prop_assert_eq!(a.balance.to_bits(), (0.0 - booked).to_bits());
        prop_assert_eq!(a.fees_paid.to_bits(), (0.0 + booked).to_bits());
        prop_assert_eq!(a.fees_unconverted(), left_out);
        if asset != 0 {
            let raw = a.fees_by_asset.get(&Ustr::from(ASSETS[asset])).copied();
            prop_assert_eq!(raw.map(f64::to_bits), Some((0.0 + fee).to_bits()));
        }
    }
}

/// The oracle of law 3 can tell the rate in force when a fill folds from the rate the position was
/// opened at: a crafted run where they differ by 20% books the CLOSE-time number, and the entry-rate
/// number is further from it than the tolerance (so law 3a would fail on a book that used it).
#[test]
fn the_round_trip_oracle_separates_the_close_time_rate_from_the_entry_rate() {
    let (u, r1) = (100.0, 0.0075);
    let r2 = r1 * 1.2;
    let mut a = account();
    quote(&mut a, "USDJPY", "JPY", u, r1);
    a.apply_fill(&fill(0, "USDJPY", 1, 4.0, 100.0));
    let sent = a.admit_quoted("USDJPY", "JPY", None, Some((r2, NOW + 1_000)), ACCT, NOW + 1_000);
    assert_eq!(sent, Ok(()));
    a.apply_fill(&fill(1, "USDJPY", -1, 4.0, 110.0));
    let at_close = 4.0 * 10.0 * u * r2;
    let at_entry = 4.0 * 10.0 * u * r1;
    assert!(close(a.realized_pnl, at_close, at_close.abs()));
    assert!(!close(a.realized_pnl, at_entry, at_entry.abs()));
}

// ---- law 4: order independence ----------------------------------------------------------------

const THREE: [(&str, &str); 3] = [("USDJPY", "JPY"), ("EURGBP", "GBP"), ("USDCHF", "CHF")];

/// `(factor on the base rate, age in ms at NOW)` of one refresh.
type Refresh = (f64, i64);

fn refreshes() -> impl Strategy<Value = Vec<Refresh>> {
    prop::collection::vec((0.3f64..3.0, 0i64..110_000), 0..5)
}

/// Three currencies admitted in a fixed order, so only the REFRESHES differ between two runs.
fn three(r0: [f64; 3]) -> Account {
    let mut a = account();
    for (k, (sym, ccy)) in THREE.iter().enumerate() {
        let first = Some((r0[k], NOW - 119_000));
        let value = Some(ContractValue::linear(10.0 + k as f64));
        assert_eq!(a.admit_quoted(sym, ccy, value, first, ACCT, NOW), Ok(()));
    }
    a
}

/// Merge the three per-currency sequences into one stream, `picks` choosing whose turn it is
/// (a currency with nothing left yields its turn). Each currency's own order is kept.
fn interleave(seqs: &[Vec<Refresh>; 3], picks: &[usize]) -> Vec<(usize, Refresh)> {
    let total: usize = seqs.iter().map(Vec::len).sum();
    let mut next = [0usize; 3];
    let mut out = Vec::with_capacity(total);
    let mut p = 0;
    while out.len() < total {
        let mut c = picks.get(p).copied().unwrap_or(0) % 3;
        p += 1;
        while next[c] == seqs[c].len() {
            c = (c + 1) % 3;
        }
        out.push((c, seqs[c][next[c]]));
        next[c] += 1;
    }
    out
}

type Outcomes = [Vec<Result<(), &'static str>>; 3];

fn deliver(r0: [f64; 3], stream: &[(usize, Refresh)]) -> (Account, Outcomes) {
    let mut a = three(r0);
    let mut outcomes: Outcomes = Default::default();
    for (c, (factor, age)) in stream {
        let sent = a.admit_quoted(
            THREE[*c].0,
            THREE[*c].1,
            None,
            Some((r0[*c] * factor, NOW - age)),
            ACCT,
            NOW,
        );
        outcomes[*c].push(sent);
    }
    (a, outcomes)
}

proptest! {
    /// LAW 4a. Refreshes of different currencies commute: any interleaving that keeps each
    /// currency's own order ends in the same grid bits, the same snapshot and the same per-refresh
    /// verdicts (accepted, kept, or jump-refused and remembered).
    #[test]
    fn refreshes_of_different_currencies_commute(
        r0 in (any_rate(), any_rate(), any_rate()),
        seqs in (refreshes(), refreshes(), refreshes()),
        picks in prop::collection::vec(0usize..3, 0..30),
    ) {
        let r0 = [r0.0, r0.1, r0.2];
        let seqs = [seqs.0, seqs.1, seqs.2];
        let in_blocks: Vec<(usize, Refresh)> =
            seqs.iter().enumerate().flat_map(|(c, v)| v.iter().map(move |r| (c, *r))).collect();
        let (a, out_a) = deliver(r0, &in_blocks);
        let (b, out_b) = deliver(r0, &interleave(&seqs, &picks));
        prop_assert_eq!(out_a, out_b);
        for (k, row) in a.contract_value_grid().iter() {
            let other = b.contract_value_of(k);
            prop_assert_eq!(row.kind, other.kind);
            prop_assert_eq!(row.per_unit.to_bits(), other.per_unit.to_bits());
        }
        prop_assert_eq!(a.quote_rate_table(NOW), b.quote_rate_table(NOW));
        prop_assert_eq!(a.snapshot(), b.snapshot());
    }

    /// LAW 4b. For one currency, whatever order refreshes arrive in, the one with the newest `as_of`
    /// stands, and it is written into EVERY symbol quoted in that currency.
    #[test]
    fn for_one_currency_the_newest_as_of_wins_whatever_the_arrival_order(
        (u1, u2) in (unit(), unit()),
        r0 in any_rate(),
        (factors, order) in prop::collection::vec(0.85f64..1.17, 1..8).prop_flat_map(|fs| {
            let n = fs.len();
            (Just(fs), Just((0..n).collect::<Vec<usize>>()).prop_shuffle())
        }),
    ) {
        let as_of0 = NOW - 119_000;
        let mut a = account();
        for (sym, u) in [("S1", u1), ("S2", u2)] {
            let first = Some((r0, as_of0));
            let value = Some(ContractValue::linear(u));
            prop_assert_eq!(a.admit_quoted(sym, "JPY", value, first, ACCT, NOW), Ok(()));
        }
        for &i in &order {
            let sent = Some((r0 * factors[i], as_of0 + 1_000 * (i as i64 + 1)));
            prop_assert_eq!(a.admit_quoted("S1", "JPY", None, sent, ACCT, NOW), Ok(()));
        }
        let newest = factors.len() - 1;
        let want = r0 * factors[newest];
        let (_, rows) = a.quote_rate_table(NOW);
        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(rows[0].rate.to_bits(), want.to_bits());
        prop_assert_eq!(rows[0].as_of_ms, as_of0 + 1_000 * (newest as i64 + 1));
        for (sym, u) in [("S1", u1), ("S2", u2)] {
            prop_assert_eq!(a.contract_value_of(sym).per_unit.to_bits(), (u * want).to_bits());
        }
    }

    /// LAW 4c. A refresh between two fills never changes what the first one booked, nor a position,
    /// the balance or an earlier closed PnL; the second fill converts at the new rate.
    #[test]
    fn a_refresh_between_two_fills_never_changes_what_the_first_one_booked(
        u in unit(),
        r1 in any_rate(),
        f in 0.85f64..1.17,
        dir in prop_oneof![Just(1i32), Just(-1i32)],
        held in 3u8..=9,
        first_close in 1u8..=2,
        (entry, exit1, exit2) in (0.5f64..500.0, 0.5f64..500.0, 0.5f64..500.0),
    ) {
        let (held, first_close) = (f64::from(held), f64::from(first_close));
        let mut a = account();
        quote(&mut a, "USDJPY", "JPY", u, r1);
        a.apply_fill(&fill(0, "USDJPY", dir, held, entry));
        a.apply_fill(&fill(1, "USDJPY", -dir, first_close, exit1));
        let sign = f64::from(dir);
        let want1 = first_close * sign * (exit1 - entry) * u * r1;
        let scale1 = first_close * (exit1.abs() + entry.abs()) * u * r1;
        prop_assert!(close(a.realized_pnl, want1, scale1), "{} vs {}", a.realized_pnl, want1);
        let (realized, closed, balance) = (a.realized_pnl, a.closed_pnls.clone(), a.balance);
        let held_row = position(&a, "USDJPY").expect("still open");

        let r2 = r1 * f;
        let sent = a.admit_quoted("USDJPY", "JPY", None, Some((r2, NOW + 1_000)), ACCT, NOW + 1_000);
        prop_assert_eq!(sent, Ok(()));
        prop_assert_eq!(a.realized_pnl.to_bits(), realized.to_bits());
        prop_assert_eq!(a.balance.to_bits(), balance.to_bits());
        prop_assert_eq!(a.closed_pnls.len(), closed.len());
        for (x, y) in a.closed_pnls.iter().zip(&closed) {
            prop_assert_eq!(x.to_bits(), y.to_bits());
        }
        let after = position(&a, "USDJPY").expect("still open");
        prop_assert_eq!(after.size.to_bits(), held_row.size.to_bits());
        prop_assert_eq!(after.avg_px.to_bits(), held_row.avg_px.to_bits());

        a.apply_fill(&fill(2, "USDJPY", -dir, held - first_close, exit2));
        let want2 = (held - first_close) * sign * (exit2 - entry) * u * r2;
        let scale2 = (held - first_close) * (exit2.abs() + entry.abs()) * u * r2;
        prop_assert_eq!(a.closed_pnls.len(), 2);
        prop_assert_eq!(a.closed_pnls[0].to_bits(), closed[0].to_bits());
        prop_assert!(close(a.closed_pnls[1], want2, scale2), "{} vs {}", a.closed_pnls[1], want2);
    }
}

// ---- law 5: fail closed -----------------------------------------------------------------------

fn engine(now_ms: i64) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.01),
        lot_size: Some(1.0),
        deny_unadmitted_symbols: true,
        ..RiskLimits::new()
    };
    let mut e = ExecutionEngine::new(
        account(),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    e.quote_asset = ACCT.to_string();
    e.now_ms = now_ms;
    e
}

/// A complete admission of `symbol` quoted in the account's own currency, seeded with a mark.
fn admission(symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        route_key: VENUE.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(100.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    }
}

fn foreign_adm(symbol: &str, ccy: &str, unit: f64, rate: f64, as_of: i64) -> SymbolAdmission {
    SymbolAdmission {
        contract_value: Some(ContractValue::linear(unit)),
        quote_ccy: Some(ccy.into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of),
        ..admission(symbol)
    }
}

fn order(coid: &str, symbol: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(100.0),
        reduce_only,
        ts: 1,
        ..Default::default()
    }
}

/// The reason the gate denied `req` at `now_ms`, or `None` when it let the order through to the
/// client. Exactly one of the two must happen.
fn verdict(
    e: &mut ExecutionEngine<RecordingClient>,
    req: &OrderRequest,
    now_ms: i64,
) -> Option<String> {
    let mut outbox = Outbox::default();
    let sent = e.client.submissions.len();
    e.submit_order(req, now_ms, &mut outbox);
    let denied: Vec<String> = std::mem::take(&mut outbox.0)
        .into_iter()
        .filter_map(|ev| match ev {
            Event::OrderDenied(d) => Some(d.reason.to_string()),
            _ => None,
        })
        .collect();
    match denied.as_slice() {
        [] => {
            assert_eq!(
                e.client.submissions.len(),
                sent + 1,
                "an undenied order reaches the client"
            );
            None
        }
        [one] => {
            assert_eq!(e.client.submissions.len(), sent, "a denied order never reaches the client");
            Some(one.clone())
        }
        many => panic!("one order, several denials: {many:?}"),
    }
}

/// Give `symbol` a long position of `qty` (folded straight into the account, the way a venue fill
/// would be).
fn hold(e: &mut ExecutionEngine<RecordingClient>, symbol: &str, qty: f64) {
    e.account.apply_fill(&fill(900, symbol, 1, qty, 100.0));
}

proptest! {
    /// LAW 5a. A rate older than the window denies every order that is not a covered reduce, with
    /// the reason `quote-rate-stale`; a covered reduce (and everything at an age inside the window)
    /// goes through. The boundary is exact: age == window is fresh, one ms more is stale.
    #[test]
    fn a_rate_that_has_aged_out_denies_every_opening_order_and_no_covered_reduce(
        u in unit(),
        r in any_rate(),
        held in 2u8..=9,
        age in prop_oneof![
            Just(0i64),
            Just(DEFAULT_QUOTE_RATE_TTL_MS),
            Just(DEFAULT_QUOTE_RATE_TTL_MS + 1),
            0i64..400_000,
        ],
        reduce_only in any::<bool>(),
    ) {
        let mut e = engine(NOW);
        prop_assert_eq!(e.admit_symbol(&foreign_adm("USDJPY", "JPY", u, r, NOW)), AdmitOutcome::Admitted);
        let held = f64::from(held);
        hold(&mut e, "USDJPY", held);
        let at = NOW + age;
        let open = verdict(&mut e, &order("o1", "USDJPY", 1, 1.0, false), at);
        let flip = verdict(&mut e, &order("o2", "USDJPY", -1, held + 1.0, false), at);
        let reduce = verdict(&mut e, &order("o3", "USDJPY", -1, 1.0, reduce_only), at);
        if age > DEFAULT_QUOTE_RATE_TTL_MS {
            prop_assert_eq!(open.as_deref(), Some("quote-rate-stale"));
            prop_assert_eq!(flip.as_deref(), Some("quote-rate-stale"));
        } else {
            prop_assert_eq!(open, None);
            prop_assert_eq!(flip, None);
        }
        prop_assert_eq!(reduce, None, "a stale rate must never trap a position");
    }

    /// LAW 5b. A rate refused as a jump leaves the standing rate and the valuation exactly where
    /// they were and does NOT renew it: a stream of jump offers (each a fresh, distinct rate, so none
    /// is "the same rate sent again") cannot keep the symbol tradable once the standing rate ages out.
    #[test]
    fn a_stream_of_jump_refusals_neither_moves_nor_renews_the_standing_rate(
        u in unit(),
        r in any_rate(),
        big in 1.5001f64..1e3,
        down in any::<bool>(),
        n in 1usize..20,
    ) {
        let mut e = engine(NOW);
        prop_assert_eq!(e.admit_symbol(&foreign_adm("USDJPY", "JPY", u, r, NOW)), AdmitOutcome::Admitted);
        hold(&mut e, "USDJPY", 3.0);
        let value = e.account.contract_value_of("USDJPY");
        let leg = e.account.quote_leg("USDJPY", NOW);
        for i in 0..n {
            e.now_ms = NOW + 1_000 * (i as i64 + 1);
            let step = big * 1.01f64.powi(i as i32);
            let wild = if down { r / step } else { r * step };
            let out = e.admit_symbol(&foreign_adm("USDJPY", "JPY", u, wild, e.now_ms));
            prop_assert_eq!(out, AdmitOutcome::Refused("quote-rate-jump"));
            let after = e.account.contract_value_of("USDJPY");
            prop_assert_eq!(after.per_unit.to_bits(), value.per_unit.to_bits());
            prop_assert_eq!(after.kind, value.kind);
            let now = e.account.quote_leg("USDJPY", NOW);
            prop_assert_eq!(now.0.to_bits(), leg.0.to_bits());
        }
        let (_, rows) = e.account.quote_rate_table(e.now_ms);
        prop_assert_eq!(rows[0].as_of_ms, NOW, "a refused offer renews nothing");
        prop_assert_eq!(rows[0].rate.to_bits(), r.to_bits());
        let at = NOW + DEFAULT_QUOTE_RATE_TTL_MS + 1;
        e.now_ms = at;
        let open = verdict(&mut e, &order("o1", "USDJPY", 1, 1.0, false), at);
        prop_assert_eq!(open.as_deref(), Some("quote-rate-stale"));
        prop_assert_eq!(verdict(&mut e, &order("o2", "USDJPY", -1, 1.0, true), at), None);
    }
}

/// Every way an admission can fail on the rate (or the currency), with the word the engine owes.
#[derive(Debug, Clone, Copy)]
enum Bad {
    RateAbsent,
    AsOfAbsent,
    Nan,
    PosInf,
    NegInf,
    Zero,
    Negative,
    Tiny,
    Huge,
    StaleOnArrival,
    CcyAbsent,
    /// Only a FIRST admission can overflow the product: against a standing rate the jump cap speaks
    /// first (a 1e11 rate is a jump from any rate a test holds).
    Overflow,
    /// Only a symbol already held can have its currency changed.
    CcyChanged,
    CcyIsAccount,
}

/// The refusals both a first admission and a refresh meet.
const SHARED: [Bad; 11] = [
    Bad::RateAbsent,
    Bad::AsOfAbsent,
    Bad::Nan,
    Bad::PosInf,
    Bad::NegInf,
    Bad::Zero,
    Bad::Negative,
    Bad::Tiny,
    Bad::Huge,
    Bad::StaleOnArrival,
    Bad::CcyAbsent,
];

fn first_admission_kinds() -> Vec<Bad> {
    let mut all = SHARED.to_vec();
    all.push(Bad::Overflow);
    all
}

fn held_symbol_kinds() -> Vec<Bad> {
    let mut all = SHARED.to_vec();
    all.extend([Bad::CcyChanged, Bad::CcyIsAccount]);
    all
}

fn bad_admission(kind: Bad, symbol: &str, u: f64, r: f64) -> (SymbolAdmission, &'static str) {
    let ok = foreign_adm(symbol, "JPY", u, r, NOW);
    let with_rate = |x: f64| SymbolAdmission { quote_rate: Some(x), ..ok.clone() };
    match kind {
        Bad::RateAbsent => (SymbolAdmission { quote_rate: None, ..ok.clone() }, "no-quote-rate"),
        Bad::AsOfAbsent => (SymbolAdmission { rate_as_of_ms: None, ..ok.clone() }, "no-quote-rate"),
        Bad::Nan => (with_rate(f64::NAN), "no-quote-rate"),
        Bad::PosInf => (with_rate(f64::INFINITY), "no-quote-rate"),
        Bad::NegInf => (with_rate(f64::NEG_INFINITY), "no-quote-rate"),
        Bad::Zero => (with_rate(0.0), "no-quote-rate"),
        Bad::Negative => (with_rate(-r), "no-quote-rate"),
        Bad::Tiny => (with_rate(1e-13), "no-quote-rate"),
        Bad::Huge => (with_rate(1e13), "no-quote-rate"),
        Bad::StaleOnArrival => (
            SymbolAdmission {
                rate_as_of_ms: Some(NOW - DEFAULT_QUOTE_RATE_TTL_MS - 1),
                ..ok.clone()
            },
            "quote-rate-stale",
        ),
        Bad::Overflow => (
            SymbolAdmission {
                contract_value: Some(ContractValue::linear(1e300)),
                quote_rate: Some(1e11),
                ..ok.clone()
            },
            "quote-rate-range",
        ),
        Bad::CcyAbsent => (SymbolAdmission { quote_ccy: None, ..ok.clone() }, "quote-ccy-missing"),
        Bad::CcyChanged => {
            (SymbolAdmission { quote_ccy: Some("EUR".into()), ..ok.clone() }, "quote-ccy-changed")
        }
        Bad::CcyIsAccount => {
            (SymbolAdmission { quote_ccy: Some(ACCT.into()), ..ok.clone() }, "quote-ccy-changed")
        }
    }
}

proptest! {
    /// LAW 5c. A first admission with a missing, invalid, stale or unrepresentable rate (or no
    /// currency) is refused with its word, writes NOTHING, and leaves a symbol the engine does not
    /// accept, holds no row, reads as the unlisted default (never a foreign value at par), has its
    /// orders denied and its fills dropped.
    #[test]
    fn a_refused_first_admission_admits_nothing_and_values_nothing_at_par(
        kind in prop::sample::select(first_admission_kinds()),
        u in unit(),
        r in any_rate(),
    ) {
        let mut e = engine(NOW);
        let (adm, word) = bad_admission(kind, "USDJPY", u, r);
        let before = e.snapshot_state();
        prop_assert_eq!(e.admit_symbol(&adm), AdmitOutcome::Refused(word));
        prop_assert_eq!(e.snapshot_state(), before);
        prop_assert!(!e.accepts_symbol("USDJPY"));
        prop_assert!(e.extra_symbols.is_empty());
        prop_assert!(!e.account.has_foreign_quote());
        prop_assert!(!e.account.is_foreign_quoted("USDJPY"));
        prop_assert!(e.account.contract_value_grid().is_empty());
        prop_assert_eq!(e.account.contract_value_of("USDJPY"), e.account.contract_value_default());
        let denied = verdict(&mut e, &order("c1", "USDJPY", 1, 1.0, false), NOW);
        prop_assert_eq!(denied.as_deref(), Some("symbol-not-admitted"));
        let folded = e.on_event(&Event::Fill(fill(1, "USDJPY", 1, 1.0, 150.0)), &mut Outbox::default());
        prop_assert!(matches!(folded, Fold::Dropped), "{folded:?}");
        prop_assert!(position(&e.account, "USDJPY").is_none());
    }

    /// LAW 5d. The same refusals against a symbol the engine already holds change nothing at all:
    /// the standing rate, the valuation and the snapshot are as they were, and a fresh opening order
    /// still passes while a stale one is denied exactly as before.
    #[test]
    fn a_refused_refresh_of_a_held_symbol_changes_nothing(
        kind in prop::sample::select(held_symbol_kinds()),
        u in unit(),
        r in any_rate(),
    ) {
        let mut e = engine(NOW);
        prop_assert_eq!(e.admit_symbol(&foreign_adm("USDJPY", "JPY", u, r, NOW)), AdmitOutcome::Admitted);
        hold(&mut e, "USDJPY", 3.0);
        let before = e.snapshot_state();
        let value = e.account.contract_value_of("USDJPY");
        let (adm, word) = bad_admission(kind, "USDJPY", u, r);
        prop_assert_eq!(e.admit_symbol(&adm), AdmitOutcome::Refused(word));
        prop_assert_eq!(e.snapshot_state(), before);
        prop_assert_eq!(e.account.contract_value_of("USDJPY"), value);
        prop_assert!(e.accepts_symbol("USDJPY"));
        prop_assert_eq!(verdict(&mut e, &order("o1", "USDJPY", 1, 1.0, false), NOW), None);
        let at = NOW + DEFAULT_QUOTE_RATE_TTL_MS + 1;
        let stale = verdict(&mut e, &order("o2", "USDJPY", 1, 1.0, false), at);
        prop_assert_eq!(stale.as_deref(), Some("quote-rate-stale"));
    }

    /// LAW 5e. A restored symbol whose currency has NO rate row is stale for ever: an opening order
    /// on it is refused at any age, and its rate reads as unusable, never as par.
    #[test]
    fn a_restored_symbol_with_no_rate_row_is_stale_for_ever(
        u in unit(),
        r in any_rate(),
        age in 0i64..1_000,
    ) {
        let mut a = account();
        quote(&mut a, "USDJPY", "JPY", u, r);
        let mut snap = a.snapshot();
        snap.fx.as_mut().expect("a foreign symbol writes an fx table").rates.clear();
        let b = Account::restore(&snap);
        prop_assert!(b.quote_leg("USDJPY", NOW + age).1, "no rate is a stale rate");
        prop_assert!(b.quote_rate_of("USDJPY").is_nan(), "no rate must not read as a number");
    }

    /// LAW 5f. A restored symbol whose rate row holds an UNUSABLE number (zero, negative, not
    /// finite, outside the representable band) is stale for ever too: `Account::rewrite_effective`
    /// documents it ("stale until it is admitted again with one"), and the alternative is a symbol
    /// valued in its own unit (the 150x defect) that still opens orders on a fresh `as_of`.
    #[test]
    fn a_restored_symbol_with_an_unusable_rate_is_stale_for_ever(
        u in unit(),
        r in any_rate(),
        bad in prop::sample::select(vec![
            0.0f64, -0.0, -1.0, -1e-9, 1e-13, 1e13, f64::INFINITY, f64::NEG_INFINITY, f64::NAN,
        ]),
        age in 0i64..1_000,
    ) {
        let mut a = account();
        quote(&mut a, "USDJPY", "JPY", u, r);
        let mut snap = a.snapshot();
        snap.fx.as_mut().expect("a foreign symbol writes an fx table").rates[0].rate = bad;
        let b = Account::restore(&snap);
        prop_assert!(
            b.quote_leg("USDJPY", NOW + age).1,
            "rate {bad} read back as {:?}: fresh, so an opening order would be admitted",
            b.quote_leg("USDJPY", NOW + age)
        );
    }
}

// ---- law 6: orientation -----------------------------------------------------------------------

const BOOK: [&str; 6] = ["USD", "JPY", "GBP", "EUR", "CHF", "AUD"];

proptest! {
    /// LAW 6a (REPOINTED at the real admission path: it ran over `RateBook`, which no production code
    /// calls, so it proved nothing about the engine). For any two distinct currency codes and any
    /// usable rate, the rate `admit_quoted` takes IS account units per ONE unit of the quote currency, as
    /// the engine reads it back: `quote_rate_of`, the effective contract value (a unit of 1.0 times the
    /// rate) and the published table's currency all return it unchanged, bit for bit, and a symbol in
    /// the account's own currency stays at par beside it.
    #[test]
    fn an_admitted_rate_is_read_back_as_account_units_per_quote_unit(
        picks in Just((0..BOOK.len()).collect::<Vec<usize>>()).prop_shuffle(),
        r in any_rate(),
    ) {
        let (quote_ccy, account_ccy) = (BOOK[picks[0]], BOOK[picks[1]]);
        let mut acct = account();
        let sent = acct.admit_quoted("SYM", quote_ccy, None, Some((r, NOW)), account_ccy, NOW);
        prop_assert_eq!(sent, Ok(()));
        prop_assert_eq!(acct.quote_rate_of("SYM").to_bits(), r.to_bits());
        prop_assert_eq!(acct.contract_value_of("SYM").per_unit.to_bits(), r.to_bits());
        prop_assert_eq!(acct.quote_rate_table(NOW).0, account_ccy.to_string());
        prop_assert_eq!(acct.quote_rate_of("ACCOUNT_QUOTED"), 1.0);
        prop_assert_eq!(acct.contract_value_of("ACCOUNT_QUOTED"), acct.contract_value_default());
    }

    /// LAW 6b. A rate sent in the wrong direction (`1/r` after `r`) for `r` outside `[1/1.5, 1.5]` is
    /// a step of at least 2.25x, so the jump cap refuses it; a single repeat of the good rate does
    /// not make it acceptable (the good rate clears the memory of the refused one), so alternating
    /// `flipped, good, flipped, good, ...` never moves the standing rate by a bit.
    #[test]
    fn a_flipped_rate_is_refused_and_a_repeat_of_the_good_rate_never_lets_it_in(
        u in unit(),
        r in prop_oneof![1e-4f64..0.66, 1.51f64..1e3],
        rounds in 1usize..10,
    ) {
        let flipped = 1.0 / r;
        prop_assert!(r.max(flipped) / r.min(flipped) > MAX_QUOTE_RATE_STEP);
        let mut a = account();
        quote(&mut a, "USDJPY", "JPY", u, r);
        let value = a.contract_value_of("USDJPY");
        for i in 0..rounds {
            let t = NOW + 2_000 * (i as i64 + 1);
            let bad = a.admit_quoted("USDJPY", "JPY", None, Some((flipped, t - 1_000)), ACCT, t);
            prop_assert_eq!(bad, Err("quote-rate-jump"));
            prop_assert_eq!(a.quote_rate_of("USDJPY").to_bits(), r.to_bits());
            let good = a.admit_quoted("USDJPY", "JPY", None, Some((r, t)), ACCT, t);
            prop_assert_eq!(good, Ok(()));
            prop_assert_eq!(a.quote_rate_of("USDJPY").to_bits(), r.to_bits());
            prop_assert_eq!(a.contract_value_of("USDJPY").per_unit.to_bits(), value.per_unit.to_bits());
            let pending = a.snapshot().fx.expect("an fx table").rates[0].pending;
            prop_assert_eq!(pending, None, "the good rate clears what the refused one left");
        }
    }

    /// LAW 6c (the other half of the cap's contract, pinned). The SAME jump sent twice in a row is a
    /// real move and is taken: the cap is a defence against ONE wrong rate, not against a sustained
    /// one, so two consecutive flipped rates DO get in. That is why orientation is the node's rate
    /// builder's to get right.
    #[test]
    fn the_same_jump_sent_twice_in_a_row_is_taken_as_a_real_move(
        u in unit(),
        r in prop_oneof![1e-4f64..0.66, 1.51f64..1e3],
        wobble in -5e-4f64..5e-4,
    ) {
        let flipped = 1.0 / r;
        let mut a = account();
        quote(&mut a, "USDJPY", "JPY", u, r);
        let first = a.admit_quoted("USDJPY", "JPY", None, Some((flipped, NOW + 1_000)), ACCT, NOW + 1_000);
        prop_assert_eq!(first, Err("quote-rate-jump"));
        let again = flipped * (1.0 + wobble);
        let second = a.admit_quoted("USDJPY", "JPY", None, Some((again, NOW + 2_000)), ACCT, NOW + 2_000);
        prop_assert_eq!(second, Ok(()));
        prop_assert_eq!(a.quote_rate_of("USDJPY").to_bits(), again.to_bits());
    }

    /// LAW 6d (a stated LIMIT, pinned). For `r` inside `[1/sqrt(1.5), sqrt(1.5)]` the flipped rate
    /// `1/r` is a step of at most 1.5x, so the engine takes it: it CANNOT detect an inverted rate
    /// near par (USDCHF, EURGBP...). The node's rate builder is the orientation test home for that
    /// band; nothing in the engine can see it.
    #[test]
    fn a_flipped_rate_near_par_is_taken_because_the_engine_cannot_see_it(
        u in unit(),
        r in 0.8206f64..1.2186,
    ) {
        let flipped = 1.0 / r;
        let mut a = account();
        quote(&mut a, "USDCHF", "CHF", u, r);
        let sent = a.admit_quoted("USDCHF", "CHF", None, Some((flipped, NOW + 1_000)), ACCT, NOW + 1_000);
        prop_assert_eq!(sent, Ok(()));
        prop_assert_eq!(a.quote_rate_of("USDCHF").to_bits(), flipped.to_bits());
    }
}

/// The 150x golden, end to end: USDJPY on a USD account with `1 USD = 150 JPY`. The rate is NOT a constant
/// of this test: it is what the quote book derives from that statement, `rate_to("JPY", "USD")` =
/// `1/150` (account units per yen), with `rate_to("USD", "JPY")` the 150 that is the WRONG orientation.
/// Sent through the real admission path, the right one makes 100,000 units at 150 worth 100,000 USD (not
/// 15,000,000) and a 0.10 move realize 100,000 · 0.10 / 150 = 66.67 USD (not 10,000); the wrong one,
/// through the same door, is 22,500x off, so a swapped orientation can never pass as rounding.
#[test]
fn usdjpy_orientation_is_account_units_per_quote_unit() {
    let mut book = RateBook::new();
    book.set("USD", "JPY", 150.0);
    let r = book.rate_to("JPY", "USD").expect("a pair is registered");
    assert_eq!(r, 1.0 / 150.0);
    let wrong = book.rate_to("USD", "JPY").expect("a pair is registered");
    assert_eq!(wrong, 150.0, "the wrong orientation is the 150");
    let mut w = account();
    assert_eq!(w.admit_quoted("USDJPY", "JPY", None, Some((wrong, NOW)), ACCT, NOW), Ok(()));
    w.apply_fill(&fill(0, "USDJPY", 1, 100_000.0, 150.0));
    mark(&mut w, "USDJPY", 150.0);
    let swapped = w.gross_notional_priced(|(v, s, _), _| w.mark_of_key(&(*v, *s)));
    assert!(
        rel_close(swapped, 100_000.0 * 150.0 * 150.0),
        "the flipped rate is 22,500x: {swapped}"
    );
    let mut a = account();
    assert_eq!(a.admit_quoted("USDJPY", "JPY", None, Some((r, NOW)), ACCT, NOW), Ok(()));
    a.apply_fill(&fill(0, "USDJPY", 1, 100_000.0, 150.0));
    mark(&mut a, "USDJPY", 150.0);
    let notional = a.gross_notional_priced(|(v, s, _), _| a.mark_of_key(&(*v, *s)));
    assert!(rel_close(notional, 100_000.0), "{notional}");
    a.apply_fill(&fill(1, "USDJPY", -1, 100_000.0, 150.10));
    let want = 100_000.0 * 0.10 / 150.0;
    // 150.10 - 150.0 is 0.09999999999999432 in binary, 6e-14 off 0.10: inside SCALE_TOL.
    assert!(rel_close(a.realized_pnl, want), "{} vs {want}", a.realized_pnl);
    assert!(a.realized_pnl < 100.0, "the unconverted number is 10,000");
}

/// All or nothing across siblings: a refresh taken for one symbol that would push a SIBLING quoted in
/// the same currency out of the representable range is refused whole, and the symbol it was sent for
/// is not written either (no half-updated currency).
#[test]
fn a_refresh_that_overflows_a_sibling_is_refused_and_writes_nothing() {
    let mut a = account();
    let first = |unit: f64| Some(ContractValue::linear(unit));
    // S1's effective unit is 1.5e300 · 1e8 = 1.5e308, just under f64::MAX (1.797e308).
    assert_eq!(a.admit_quoted("S1", "JPY", first(1.5e300), Some((1e8, NOW)), ACCT, NOW), Ok(()));
    assert_eq!(a.admit_quoted("S2", "JPY", first(1.0), Some((1e8, NOW)), ACCT, NOW), Ok(()));
    let before = a.snapshot();
    // A 1.2x step is inside the cap; S1 would become 1.8e308, which is not a number.
    let sent = a.admit_quoted("S2", "JPY", None, Some((1.2e8, NOW + 1_000)), ACCT, NOW + 1_000);
    assert_eq!(sent, Err("quote-rate-range"));
    assert_eq!(a.snapshot(), before);
}

// ---- law 7: snapshot --------------------------------------------------------------------------

const FSYMS: [&str; 4] = ["F1", "F2", "F3", "F4"];
const CCY3: [&str; 3] = ["JPY", "GBP", "CHF"];

#[derive(Debug, Clone)]
enum Op {
    Fill(Step),
    Refresh { ccy: usize, factor: f64 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => step(5).prop_map(Op::Fill),
        1 => (0usize..3, 0.85f64..1.17).prop_map(|(ccy, factor)| Op::Refresh { ccy, factor }),
    ]
}

proptest! {
    /// LAW 7a. Restore of a snapshot with an fx table reproduces the EFFECTIVE grid exactly (same
    /// kind, bit-equal `per_unit`, every symbol) whatever mix of linear and inverse foreign symbols,
    /// rates and fills built it; re-snapshotting is equal, the JSON round trip is equal, the rate
    /// table and every symbol's `quote_leg` read the same, and the next fill folds to the same bits.
    #[test]
    fn a_restore_reproduces_the_effective_grid_and_resnapshots_equal(
        local in any_value(),
        specs in prop::collection::vec((0usize..3, any::<bool>(), unit()), 4),
        r0 in (any_rate(), any_rate(), any_rate()),
        ops in prop::collection::vec(op(), 0..40),
        next in step(5),
    ) {
        let r0 = [r0.0, r0.1, r0.2];
        let mut a = account();
        a.admit_contract_value("LOCAL", local);
        for (sym, (ccy, inverse, u)) in FSYMS.iter().zip(&specs) {
            let base = if *inverse { ContractValue::inverse(*u) } else { ContractValue::linear(*u) };
            let sent = a.admit_quoted(sym, CCY3[*ccy], Some(base), Some((r0[*ccy], NOW)), ACCT, NOW);
            prop_assert_eq!(sent, Ok(()));
        }
        let names = ["LOCAL", "F1", "F2", "F3", "F4"];
        let mut t = NOW;
        for (i, o) in ops.iter().enumerate() {
            t += 1_000;
            match o {
                Op::Fill(s) => {
                    a.apply_fill(&fill(i, names[s.sym], s.side, s.qty, s.px));
                    mark(&mut a, names[s.sym], s.px);
                }
                Op::Refresh { ccy, factor } => {
                    if let Some(sym) = FSYMS.iter().zip(&specs).find(|(_, s)| s.0 == *ccy).map(|(n, _)| n) {
                        let sent = a.admit_quoted(sym, CCY3[*ccy], None, Some((r0[*ccy] * factor, t)), ACCT, t);
                        prop_assert_eq!(sent, Ok(()));
                    }
                }
            }
        }
        let snap = a.snapshot();
        let mut b = Account::restore(&snap);
        let grid = a.contract_value_grid();
        prop_assert_eq!(grid.len(), b.contract_value_grid().len());
        for (k, row) in grid.iter() {
            let got = b.contract_value_of(k);
            prop_assert_eq!(got.kind, row.kind, "{}", k);
            prop_assert_eq!(got.per_unit.to_bits(), row.per_unit.to_bits(), "{}", k);
        }
        prop_assert_eq!(b.snapshot(), snap.clone());
        let json = serde_json::to_string(&snap).expect("a snapshot serializes");
        let back: AccountSnapshot = serde_json::from_str(&json).expect("and reads back");
        prop_assert_eq!(&back, &snap);
        prop_assert_eq!(a.quote_rate_table(t), b.quote_rate_table(t));
        for name in names {
            let (x, y) = (a.quote_leg(name, t + 500), b.quote_leg(name, t + 500));
            prop_assert_eq!(x.0.to_bits(), y.0.to_bits());
            prop_assert_eq!(x.1, y.1);
        }
        prop_assert_eq!(a.equity_all(0.0).to_bits(), b.equity_all(0.0).to_bits());
        let f = fill(1_000, names[next.sym], next.side, next.qty, next.px);
        a.apply_fill(&f);
        b.apply_fill(&f);
        prop_assert_eq!(a.realized_pnl.to_bits(), b.realized_pnl.to_bits());
        prop_assert_eq!(a.closed_pnls.len(), b.closed_pnls.len());
        prop_assert_eq!(a.positions.len(), b.positions.len());
        for ((ka, pa), (kb, pb)) in a.positions.iter().zip(b.positions.iter()) {
            prop_assert_eq!(ka, kb);
            prop_assert_eq!(pa.size.to_bits(), pb.size.to_bits());
            prop_assert_eq!(pa.avg_px.to_bits(), pb.avg_px.to_bits());
        }
    }
}

fn keys(v: &serde_json::Value) -> BTreeSet<String> {
    v.as_object().expect("a JSON object").keys().cloned().collect()
}

fn set(words: &[&str]) -> BTreeSet<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The keys an `AccountSnapshot` wrote before the multi-currency field existed.
fn account_keys(inverse: bool) -> BTreeSet<String> {
    let mut all = set(&[
        "venue",
        "mult",
        "mult_default",
        "balance_mode",
        "positions",
        "realized_pnl",
        "closed_pnls",
        "balance",
        "balances_by_asset",
        "marks",
        "funding_paid",
        "fees_paid",
        "fees_by_asset",
    ]);
    if inverse {
        all.insert("inverse".to_string());
    }
    all
}

proptest! {
    /// LAW 7b. An account that never held a foreign-quoted symbol serializes exactly the keys it did
    /// before the feature (no `fx`), whatever it folded; so does one that only had foreign
    /// admissions REFUSED.
    #[test]
    fn an_account_without_a_foreign_symbol_writes_no_fx_key(
        value in any_value(),
        steps in prop::collection::vec((step(1), -0.5f64..2.0), 1..40),
    ) {
        let mut a = account();
        a.admit_contract_value("EURUSD", value);
        for (i, (s, fee)) in steps.iter().enumerate() {
            let f = FillEvent { commission: *fee, ..fill(i, "EURUSD", s.side, s.qty, s.px) };
            a.apply_fill(&f);
            mark(&mut a, "EURUSD", s.px);
        }
        let refused = a.admit_quoted("USDJPY", "JPY", None, Some((f64::NAN, NOW)), ACCT, NOW);
        prop_assert_eq!(refused, Err("no-quote-rate"));
        let snap = a.snapshot();
        prop_assert!(snap.fx.is_none());
        let json = serde_json::to_string(&snap).expect("serializes");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("parses");
        prop_assert_eq!(keys(&tree), account_keys(value.kind == NotionalKind::Inverse));
        prop_assert!(!json.contains("\"fx\""), "{json}");
    }
}

#[derive(Debug, Clone)]
enum EOp {
    Admit { sym: usize, factor: f64, dt: i64 },
    Fill(Step),
}

fn eop() -> impl Strategy<Value = EOp> {
    prop_oneof![
        3 => (0usize..4, 0.85f64..1.17, 0i64..30_000)
            .prop_map(|(sym, factor, dt)| EOp::Admit { sym, factor, dt }),
        3 => step(4).prop_map(EOp::Fill),
    ]
}

const ESYMS: [&str; 4] = ["E0", "E1", "E2", "E3"];

/// An engine fed `ops`: E0 is quoted in JPY and E1 in GBP (each admission refreshes its rate), E2 and
/// E3 are quoted in the account's currency (E3 says so, E2 says nothing); fills fold for every symbol
/// the engine accepts. `include_foreign` leaves E0 and E1 as plain symbols.
fn run_engine(
    ops: &[EOp],
    r0: [f64; 2],
    include_foreign: bool,
) -> ExecutionEngine<RecordingClient> {
    let mut e = engine(NOW);
    let admission_of = |sym: usize, factor: f64, now: i64| -> SymbolAdmission {
        match sym {
            0 if include_foreign => foreign_adm("E0", "JPY", 1_000.0, r0[0] * factor, now),
            1 if include_foreign => foreign_adm("E1", "GBP", 100.0, r0[1] * factor, now),
            3 => SymbolAdmission { quote_ccy: Some(ACCT.into()), ..admission("E3") },
            2 => SymbolAdmission {
                contract_value: Some(ContractValue::linear(10.0)),
                ..admission("E2")
            },
            other => admission(ESYMS[other]),
        }
    };
    for (i, o) in ops.iter().enumerate() {
        match o {
            EOp::Admit { sym, factor, dt } => {
                e.now_ms += dt;
                let now = e.now_ms;
                e.admit_symbol(&admission_of(*sym, *factor, now));
            }
            EOp::Fill(s) => {
                let name = ESYMS[s.sym];
                if e.accepts_symbol(name) {
                    let ev = Event::Fill(fill(i, name, s.side, s.qty, s.px));
                    e.on_event(&ev, &mut Outbox::default());
                }
            }
        }
    }
    e
}

proptest! {
    /// LAW 7c. The engine round trip: whatever admissions, refreshes (accepted or refused) and fills
    /// built it, `from_snapshot` of its snapshot snapshots equal, hashes equal, survives the JSON
    /// round trip the journal gives it, and values every symbol with the same bits.
    #[test]
    fn an_engine_with_foreign_symbols_survives_snapshot_restore_and_the_journal_json(
        r0 in (any_rate(), any_rate()),
        ops in prop::collection::vec(eop(), 0..30),
    ) {
        let e = run_engine(&ops, [r0.0, r0.1], true);
        let snap = e.snapshot_state();
        let back = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
        prop_assert_eq!(back.snapshot_state(), snap.clone());
        prop_assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(std::slice::from_ref(&snap)));
        let json = serde_json::to_string(&snap).expect("serializes");
        let parsed: EngineSnapshot = serde_json::from_str(&json).expect("reads back");
        prop_assert_eq!(&parsed, &snap);
        for name in ESYMS {
            let (x, y) = (e.account.contract_value_of(name), back.account.contract_value_of(name));
            prop_assert_eq!(x.kind, y.kind);
            prop_assert_eq!(x.per_unit.to_bits(), y.per_unit.to_bits());
        }
        prop_assert_eq!(back.quote_asset.as_str(), ACCT);
        prop_assert_eq!(e.account.quote_rate_table(e.now_ms), back.account.quote_rate_table(e.now_ms));
    }

    /// LAW 7d. An engine that only ever admitted symbols quoted in its own currency writes no `fx`
    /// key anywhere: the engine snapshot keeps exactly its pre-feature keys and so does the account.
    #[test]
    fn an_engine_without_a_foreign_symbol_serializes_with_no_fx_key(
        r0 in (any_rate(), any_rate()),
        ops in prop::collection::vec(eop(), 0..30),
    ) {
        let e = run_engine(&ops, [r0.0, r0.1], false);
        let snap = e.snapshot_state();
        prop_assert!(snap.account.fx.is_none());
        let tree = serde_json::to_value(&snap).expect("serializes");
        prop_assert_eq!(
            keys(&tree),
            set(&[
                "venue",
                "symbol",
                "quote_asset",
                "reduce_only_on_close",
                "extra_symbols",
                "trading_state",
                "limits",
                "gate_order_times",
                "registry",
                "account",
                "seen_trade_ids",
                "seen_fsm_trade_ids",
                "seen_liq_ids",
                "dropped_terminal_on_live",
                "equity_seed",
                "collect_applied_fills",
                "now_ms",
            ])
        );
        prop_assert_eq!(keys(&tree["account"]), account_keys(false));
        prop_assert!(!serde_json::to_string(&snap).expect("serializes").contains("\"fx\""));
    }
}
