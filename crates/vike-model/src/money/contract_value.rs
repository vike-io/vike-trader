//! `ContractValue` — what ONE unit of an order's quantity is worth, as a KIND plus a factor, and
//! the money arithmetic that reads it (decision
//! `docs/decisions/0170-the-notional-of-an-order-carries-its-kind-linear-or-inverse-from-the-contract-to-every-consumer.md`).
//!
//! # Why a pair and not one `f64`
//!
//! A bare multiplier expresses a LINEAR contract (`value = qty · price · m`) and cannot express an
//! INVERSE one, whose quantity is already a quote-currency amount (`value = qty · u`, the price never
//! enters). Deribit's `BTC-PERPETUAL` is inverse: its `amount` is US dollars, so an order of 10 is
//! worth 10 USD, not `10 · 65,700 · 10`. This pair is the money path's one fact about a contract;
//! the word `multiplier` leaves the engine, the risk gate, the snapshot and the control door.
//!
//! * [`NotionalKind::Linear`] — `per_unit` is the BASE units one unit of quantity stands for
//!   (1.0 for spot, linear perpetuals and options; an exchange multiplier for IBKR futures).
//! * [`NotionalKind::Inverse`] — `per_unit` is the QUOTE-currency units one unit of quantity
//!   stands for (1.0 on deribit, whose amount is already dollars).
//!
//! # The arithmetic, one home each
//!
//! `u` is `per_unit`; `s` a signed size, `q` a quantity, `p` a price, `e` an entry price:
//!
//! | quantity | linear (bit-identical to the scalar helpers) | inverse |
//! |---|---|---|
//! | order notional | `\|q\|·\|p\|·\|u\|` | `\|q\|·\|u\|` |
//! | signed / gross position notional | `s·p·u` / `\|s\|·p·u` | `s·u` / `\|s\|·u` |
//! | initial / maintenance margin | `rate·u·p·q·im` / `\|q\|·p·u·rate·mm` | `rate·u·q·im` / `\|q\|·u·rate·mm` |
//! | realized and unrealized PnL | `(p−e)·s·u` | `(p−e)/e·s·u` |
//! | average entry on an add | size-weighted arithmetic | harmonic: `Σ\|q\| / Σ(\|q\|/p)` |
//!
//! Every LINEAR arm calls the `crate::scalar` / `crate::money::margin` / `crate::money::fill`
//! helper that spells it, so a linear instrument is bit-identical to a world without this type
//! (the frozen `fixtures/r0/` bytes pin the fill). The inverse PnL is quoted in the quote currency
//! at the exit or the mark: it equals the coin PnL `s·u·(1/e − 1/p)` times `p`.

use serde::{Deserialize, Serialize};

use crate::instrument::contract::{Contract, Margining};

/// How a quantity becomes a money value. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotionalKind {
    /// `value = qty · price · per_unit`. The default, and every instrument but a coin-margined
    /// future or perpetual.
    #[default]
    Linear,
    /// `value = qty · per_unit`, the price never entering: the quantity is a quote-currency amount.
    Inverse,
}

/// The kind and the per-unit factor of one contract. `Copy`, 16 bytes, the value the fold looks up
/// where it used to look up a bare `f64`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContractValue {
    /// Linear or inverse.
    pub kind: NotionalKind,
    /// Base units (linear) or quote-currency units (inverse) one unit of quantity stands for.
    pub per_unit: f64,
}

impl Default for ContractValue {
    /// Linear, 1.0: the ordinary instrument, and what an unlisted symbol reads.
    fn default() -> Self {
        ContractValue::linear(1.0)
    }
}

impl ContractValue {
    /// A linear value. The factor is stored AS GIVEN (the sim and paper wrap the multiplier they
    /// have today, byte for byte); only [`ContractValue::of`] folds a degenerate factor.
    #[must_use]
    pub const fn linear(per_unit: f64) -> Self {
        ContractValue { kind: NotionalKind::Linear, per_unit }
    }

    /// An inverse value; `per_unit` is quote-currency units per unit of quantity.
    #[must_use]
    pub const fn inverse(per_unit: f64) -> Self {
        ContractValue { kind: NotionalKind::Inverse, per_unit }
    }

    /// The value a contract's own row defines, with `per_unit` as the venue's amount convention
    /// gives it. A `Perp` or `Future` whose [`Margining`] is `Inverse` is [`NotionalKind::Inverse`];
    /// every other class is `Linear`, an OPTION included: an option is priced `qty · premium`
    /// whichever currency it settles in. A `Quanto` contract yields `None` (no shipped venue
    /// lists one and a guess is the defect this type exists to remove): the caller refuses the
    /// mount with a reason.
    ///
    /// A degenerate `per_unit` (not finite, or not positive) folds to 1.0 HERE, the one rule
    /// `SymbolProperties::multiplier` applies, so a bad venue number can neither zero a notional
    /// nor invert it.
    #[must_use]
    pub fn of(contract: &Contract, per_unit: f64) -> Option<ContractValue> {
        let per_unit = if per_unit.is_finite() && per_unit > 0.0 { per_unit } else { 1.0 };
        match contract.margining() {
            Some(Margining::Quanto) => None,
            Some(Margining::Inverse)
                if matches!(contract, Contract::Perp(_) | Contract::Future(_)) =>
            {
                Some(ContractValue::inverse(per_unit))
            }
            _ => Some(ContractValue::linear(per_unit)),
        }
    }

    /// This value with a DEGENERATE per-unit factor (not finite, or not positive) folded to 1.0, the
    /// kind kept. A zero unit would size every order at nothing and a negative one is a sign
    /// error: neither may reach the gate, so every place a value ENTERS the money path from outside
    /// this crate (the wire reply, the mount's grid, a restored snapshot, a remote observer's frame)
    /// passes it through here, the one rule [`ContractValue::of`] applies at its own door.
    #[inline]
    #[must_use]
    pub fn folded(self) -> Self {
        if self.per_unit.is_finite() && self.per_unit > 0.0 {
            self
        } else {
            ContractValue { kind: self.kind, per_unit: 1.0 }
        }
    }

    /// Whether this is the default (`Linear`, exactly 1.0): the value an unlisted symbol reads,
    /// and the only one a published grid leaves out.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.kind == NotionalKind::Linear && self.per_unit == 1.0
    }

    /// ORDER notional magnitude — the order gate's number. Linear is
    /// [`crate::scalar::order_notional`] exactly (all three factors absolute); inverse is
    /// `|qty| · |per_unit|` and `ref_price` is not read.
    #[inline]
    #[must_use]
    pub fn order_notional(self, qty: f64, ref_price: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => crate::scalar::order_notional(qty, ref_price, self.per_unit),
            NotionalKind::Inverse => qty.abs() * self.per_unit.abs(),
        }
    }

    /// SIGNED position notional (long +, short −). Linear is [`crate::scalar::signed_notional`].
    #[inline]
    #[must_use]
    pub fn signed_notional(self, size: f64, price: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => crate::scalar::signed_notional(size, price, self.per_unit),
            NotionalKind::Inverse => size * self.per_unit,
        }
    }

    /// GROSS position notional (magnitude in the size). Linear is [`crate::scalar::gross_notional`].
    #[inline]
    #[must_use]
    pub fn gross_notional(self, size: f64, price: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => crate::scalar::gross_notional(size, price, self.per_unit),
            NotionalKind::Inverse => size.abs() * self.per_unit,
        }
    }

    /// Signed initial margin of `qty` units. Linear is [`crate::money::margin::initial_margin`].
    #[inline]
    #[must_use]
    pub fn initial_margin(self, price: f64, qty: f64, rate: f64, im_req: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => {
                crate::money::margin::initial_margin(price, qty, self.per_unit, rate, im_req)
            }
            NotionalKind::Inverse => rate * self.per_unit * qty * im_req,
        }
    }

    /// Maintenance margin of a holding. Linear is [`crate::money::margin::maintenance_margin`].
    #[inline]
    #[must_use]
    pub fn maintenance_margin(self, price: f64, qty: f64, rate: f64, mm_req: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => {
                crate::money::margin::maintenance_margin(price, qty, self.per_unit, rate, mm_req)
            }
            NotionalKind::Inverse => qty.abs() * self.per_unit * rate * mm_req,
        }
    }

    /// The margin a REVERSING order frees and re-opens (`2 · |size| · price · unit · im_req` on a
    /// linear contract, in that factor order; `2 · |size| · unit · im_req` on an inverse one).
    #[inline]
    #[must_use]
    pub fn margin_credit(self, size: f64, price: f64, im_req: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => 2.0 * size.abs() * price * self.per_unit * im_req,
            NotionalKind::Inverse => 2.0 * size.abs() * self.per_unit * im_req,
        }
    }

    /// Unrealized PnL of a signed `size` bought at `avg_px`, valued at `px`: linear
    /// `(px − avg_px) · size · u` (the expression `Account::unrealized_pnl` always spelled),
    /// inverse `(px − avg_px) / avg_px · size · u`, in the quote currency at `px`. A non-positive
    /// `avg_px` has no inverse PnL and reads 0.
    #[inline]
    #[must_use]
    pub fn unrealized_pnl(self, avg_px: f64, px: f64, size: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => (px - avg_px) * size * self.per_unit,
            NotionalKind::Inverse => {
                if avg_px > 0.0 {
                    (px - avg_px) / avg_px * size * self.per_unit
                } else {
                    0.0
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::contract::{FutureFamily, FutureSpec, OptionSpec, PerpSpec};
    use vike_marketdata::OptionKind;

    fn perp(m: Margining) -> Contract {
        Contract::Perp(PerpSpec {
            margining: m,
            settle: "BTC".into(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms: None,
        })
    }

    fn future(m: Margining) -> Contract {
        Contract::Future(FutureSpec {
            margining: m,
            settle: "BTC".into(),
            value_ccy: None,
            expiry_ms: 1_793_000_000_000,
            tenor: None,
            delivery: crate::instrument::contract::Delivery::Cash,
            family: FutureFamily::Crypto,
        })
    }

    fn option(m: Margining) -> Contract {
        Contract::Option(OptionSpec {
            right: OptionKind::Call,
            strike: 60_000.0,
            expiry_ms: 1_793_000_000_000,
            style: crate::instrument::contract::ExerciseStyle::European,
            margining: m,
            settle: "BTC".into(),
            value_ccy: None,
            underlying: None,
        })
    }

    /// THE defect: deribit's `BTC-PERPETUAL` order of 10 is worth 10, whatever the price.
    #[test]
    fn an_inverse_order_is_its_quote_size_and_the_price_never_enters() {
        let v = ContractValue::inverse(1.0);
        assert_eq!(v.order_notional(10.0, 65_700.0), 10.0);
        assert_eq!(v.order_notional(10.0, 100_000.0), 10.0);
        assert_eq!(v.order_notional(10.0, 0.0), 10.0);
        assert_eq!(v.order_notional(-10.0, 65_700.0), 10.0, "a magnitude, like the linear gate");
        assert_eq!(ContractValue::inverse(100.0).order_notional(3.0, 1.0), 300.0);
    }

    /// The linear USDC perpetual: 0.01 BTC at 65,700 is 657, no longer 0.0657.
    #[test]
    fn a_linear_order_is_qty_times_price_times_unit() {
        let v = ContractValue::linear(1.0);
        assert!((v.order_notional(0.01, 65_700.0) - 657.0).abs() < 1e-9);
    }

    #[test]
    fn linear_is_the_scalar_helpers_bit_for_bit() {
        let cases = [(2.0, 3.0, 1.0), (-2.0, -3.0, 10.0), (0.1, 0.2, 0.0001), (7.0, 65_700.0, 1.0)];
        for (a, b, m) in cases {
            let v = ContractValue::linear(m);
            assert_eq!(v.order_notional(a, b).to_bits(), crate::order_notional(a, b, m).to_bits());
            assert_eq!(
                v.signed_notional(a, b).to_bits(),
                crate::signed_notional(a, b, m).to_bits()
            );
            assert_eq!(v.gross_notional(a, b).to_bits(), crate::gross_notional(a, b, m).to_bits());
            assert_eq!(
                v.initial_margin(b, a, 1.0, 0.5).to_bits(),
                crate::initial_margin(b, a, m, 1.0, 0.5).to_bits()
            );
            assert_eq!(
                v.maintenance_margin(b, a, 1.0, 0.5).to_bits(),
                crate::maintenance_margin(b, a, m, 1.0, 0.5).to_bits()
            );
            assert_eq!(
                v.unrealized_pnl(b, a, 3.0).to_bits(),
                ((a - b) * 3.0 * m).to_bits(),
                "the expression Account::unrealized_pnl always spelled"
            );
        }
    }

    #[test]
    fn an_inverse_position_is_valued_in_quote_units_with_its_sign() {
        let v = ContractValue::inverse(1.0);
        assert_eq!(v.signed_notional(-30.0, 65_700.0), -30.0);
        assert_eq!(v.gross_notional(-30.0, 65_700.0), 30.0);
        assert_eq!(v.initial_margin(65_700.0, 30.0, 1.0, 0.5), 15.0);
        assert_eq!(v.initial_margin(1.0, -30.0, 1.0, 0.5), -15.0);
        assert_eq!(v.maintenance_margin(65_700.0, -30.0, 1.0, 0.5), 15.0);
    }

    /// The coin identity: quote PnL at the exit equals `s·u·(1/e − 1/p)·p`. It equates two
    /// different expressions, so it is the one assertion here that is not bit-for-bit.
    #[test]
    fn inverse_pnl_is_the_coin_pnl_times_the_exit_price() {
        let v = ContractValue::inverse(1.0);
        let (s, e, p) = (30.0, 60_000.0, 66_000.0);
        let coin = s * (1.0 / e - 1.0 / p);
        let want = coin * p;
        let got = v.unrealized_pnl(e, p, s);
        assert!((got - want).abs() <= 1e-12 * want.abs().max(1.0), "{got} vs {want}");
        assert_eq!(v.unrealized_pnl(e, e, s), 0.0);
        assert_eq!(v.unrealized_pnl(0.0, p, s), 0.0, "no entry, no inverse PnL");
        assert_eq!(v.unrealized_pnl(e, p, -s), -got, "short is the exact negative");
    }

    #[test]
    fn of_derives_the_kind_from_the_contract() {
        assert_eq!(
            ContractValue::of(&perp(Margining::Inverse), 1.0),
            Some(ContractValue::inverse(1.0))
        );
        assert_eq!(
            ContractValue::of(&future(Margining::Inverse), 1.0),
            Some(ContractValue::inverse(1.0))
        );
        assert_eq!(
            ContractValue::of(&perp(Margining::Linear), 1.0),
            Some(ContractValue::linear(1.0))
        );
        assert_eq!(
            ContractValue::of(&future(Margining::Linear), 2.0),
            Some(ContractValue::linear(2.0))
        );
        // an option is priced qty · premium whichever currency it settles in
        assert_eq!(
            ContractValue::of(&option(Margining::Inverse), 1.0),
            Some(ContractValue::linear(1.0))
        );
        assert_eq!(ContractValue::of(&Contract::CryptoSpot, 1.0), Some(ContractValue::linear(1.0)));
        // a quanto contract yields no value
        assert_eq!(ContractValue::of(&perp(Margining::Quanto), 1.0), None);
        assert_eq!(ContractValue::of(&option(Margining::Quanto), 1.0), None);
    }

    #[test]
    fn of_folds_a_degenerate_unit_to_one() {
        for bad in [0.0, -3.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                ContractValue::of(&perp(Margining::Linear), bad),
                Some(ContractValue::linear(1.0))
            );
            assert_eq!(
                ContractValue::of(&perp(Margining::Inverse), bad),
                Some(ContractValue::inverse(1.0))
            );
        }
    }

    #[test]
    fn folded_replaces_a_degenerate_unit_with_one_and_keeps_the_kind() {
        for bad in [0.0, -0.0, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(ContractValue::linear(bad).folded(), ContractValue::linear(1.0), "{bad}");
            assert_eq!(ContractValue::inverse(bad).folded(), ContractValue::inverse(1.0), "{bad}");
        }
        for ok in [1.0, 0.0001, 50.0] {
            assert_eq!(ContractValue::linear(ok).folded(), ContractValue::linear(ok));
        }
    }

    #[test]
    fn the_default_is_linear_one_and_only_it_is_default() {
        assert!(ContractValue::default().is_default());
        assert!(ContractValue::linear(1.0).is_default());
        assert!(!ContractValue::linear(10.0).is_default());
        assert!(!ContractValue::inverse(1.0).is_default());
    }

    #[test]
    fn the_serde_shape_names_its_kind_and_round_trips() {
        for v in
            [ContractValue::linear(1.0), ContractValue::inverse(1.0), ContractValue::linear(0.5)]
        {
            let s = serde_json::to_string(&v).unwrap();
            assert_eq!(serde_json::from_str::<ContractValue>(&s).unwrap(), v);
        }
        assert_eq!(
            serde_json::to_string(&ContractValue::inverse(1.0)).unwrap(),
            r#"{"kind":"inverse","per_unit":1.0}"#
        );
    }
}
