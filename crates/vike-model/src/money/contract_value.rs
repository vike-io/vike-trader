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
//! | initial / maintenance margin | `rate·u·p·q·initial_margin` / `\|q\|·p·u·rate·mm` | `rate·u·q·initial_margin` / `\|q\|·u·rate·mm` |
//! | realized and unrealized PnL | `(p−e)·s·u` | `(p−e)/e·s·u` |
//! | average entry on an add, an order's fill VWAP, a book walk ([`ContractValue::average_price`]); its running `qty × average` ([`ContractValue::add_quantity_price`]) | size-weighted arithmetic | harmonic: `Σ\|q\| / Σ(\|q\|/p)` |
//! | one contract's units as another's on the same asset ([`ContractValue::units_as`]) | `q·u_a/u_b` | by value at one price |
//! | one unit's worth in the account's money (sizing, `r` = 0183's factor) | `p·u·r` | `u·r` |
//! | a fee at `rate` (quote money) | `q·p·rate·u` | `\|q\|·\|u\|·rate` |
//! | funding at `rate` (settle money) | `s·p·rate·u` | `rate·s·u/p` coins |
//! | a settle amount in the quote | itself | `amount·p` |
//! | the loss of one unit moved from `b` to `t` | `\|b−t\|·u` | `\|u·(1/t − 1/b)\|·b` |
//! | a cross liquidation mark (GUI badge) | the pool's linear solution | none (`0.0`) |
//!
//! Every LINEAR arm calls the `crate::scalar` / `crate::money::margin` / `crate::money::fill`
//! helper that spells it, so a linear instrument is bit-identical to a world without this type
//! (the frozen `fixtures/r0/` bytes pin the fill). The inverse PnL is quoted in the quote currency
//! at the exit or the mark: it equals the coin PnL `s·u·(1/e − 1/p)` times `p`.
//!
//! # One home for contract money, each product in the factor order its consumer pins
//!
//! Every money product on a contract is a method here (owner, 2026-11-09: it is universal for
//! stocks, spot crypto and every derivative, and it keeps a contract's kind from being dropped at a
//! site). f64 multiplication is not associative, so where two consumers fold the same quantity in
//! different orders each order is its own method, named for what it is, and the frozen fixtures
//! keep their bits: [`ContractValue::fee`] and [`ContractValue::funding`] are the simulator's
//! `q·p·rate·u` (the `fixtures/r1/` broker_sim bytes), [`ContractValue::gross_by_unit`] is the
//! simulator gate's `|s|·(p·u)` beside [`ContractValue::gross_notional`]'s `(|s|·p)·u`, and
//! [`ContractValue::cross_liquidation_price`] keeps the badge's `(s·u)·p`. `crates/vike-ops/tests/hygiene/duplicate_shape_gate.rs`'s `ContractMoney`
//! shape fails a new product of a quantity or a price with a contract unit spelled anywhere else.

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

// The kind's word, the serde tag above (`linear` / `inverse`): what the observe wire's contract-value
// row carries as text. IMPL-ONLY, so the serde attributes stay on the literal block.
vike_utility::word_enum! {
    impl NotionalKind {
        Linear => "linear",
        Inverse => "inverse",
    }
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
    /// gives it. A contract that [`Contract::is_inverse`] (a `Perp` or `Future` whose [`Margining`]
    /// is `Inverse`) is [`NotionalKind::Inverse`];
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
        if contract.margining() == Some(Margining::Quanto) {
            return None;
        }
        Some(if contract.is_inverse() {
            ContractValue::inverse(per_unit)
        } else {
            ContractValue::linear(per_unit)
        })
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

    /// This value in the ACCOUNT's currency: `per_unit · r`, the kind kept, where `r` is the
    /// account-currency units one unit of the symbol's QUOTE currency is worth (USDJPY on a USD
    /// account: `r = 1/150`, never `150`). Every money product the table above spells is linear in
    /// `per_unit`, so scaling it here converts the notional, the margin, the unrealized PnL and the
    /// realized PnL of a fill at once, with no edit at a consumer (the multi-currency seam).
    ///
    /// * `r == 1.0` returns `self` unchanged, bit for bit: a symbol quoted in the account's own
    ///   currency is exactly what it was before this method existed.
    /// * A rate that is not finite and positive, or a product that is not (an overflow, an
    ///   underflow to zero), yields `None`. **Never fold such a rate to 1.0**:
    ///   [`ContractValue::folded`] and [`ContractValue::of`] turn a degenerate factor into `1.0`, which
    ///   for a rate is the forbidden "value an unpriced currency at par" shortcut, so the caller
    ///   validates through THIS method BEFORE any `folded` runs and refuses on `None`.
    #[inline]
    #[must_use]
    pub fn in_account(self, r: f64) -> Option<Self> {
        if r == 1.0 {
            return Some(self);
        }
        let per_unit = self.per_unit * r;
        (r.is_finite() && r > 0.0 && per_unit.is_finite() && per_unit > 0.0)
            .then_some(ContractValue { kind: self.kind, per_unit })
    }

    /// **What ONE unit of quantity is worth in the ACCOUNT's currency at `price`: the one sizing law**
    /// (decision 0170, item 9, amended 2026-11-08). It is [`Self::signed_notional`] of one unit of
    /// [`Self::in_account`]`(r)`, so the money arithmetic stays the table's: linear `price · per_unit · r`,
    /// inverse `per_unit · r` (the price never enters: one Deribit `BTC-PERPETUAL` contract is one dollar at
    /// any price). `r` is decision 0183's factor, account-currency units per ONE unit of the symbol's quote
    /// currency, taken from the caller and never read here: `1.0` for a symbol quoted in the account's own
    /// currency, where the answer is linear `price · per_unit` bit for bit.
    ///
    /// The money is the account's, the one an engine's equity is in; a coin ledger (decision 0258) values its
    /// coin there too, so an inverse face is already in that money and is never divided by the price.
    ///
    /// `None` when `r` is not a usable rate (a symbol quoted in a currency the engine holds no rate for): the
    /// caller sizes nothing, never at par.
    #[inline]
    #[must_use]
    pub fn unit_value(self, price: f64, r: f64) -> Option<f64> {
        Some(self.in_account(r)?.signed_notional(1.0, price))
    }

    /// How many units are worth `target` (account currency) at `price`: `target` over
    /// [`Self::unit_value`], `0.0` when one unit is not worth a positive amount (no usable price), `None`
    /// when `r` is not usable. A value sizer passes its value, a percent sizer `pct · equity`.
    #[inline]
    #[must_use]
    pub fn units_worth(self, target: f64, price: f64, r: f64) -> Option<f64> {
        let unit = self.unit_value(price, r)?;
        Some(if unit > 0.0 { target / unit } else { 0.0 })
    }

    /// **THE averaging law** (the table's "average entry on an add" row): the average price of `held`
    /// units averaging `held_price` joined by `qty` more at `px`, both MAGNITUDES with `held + qty > 0`.
    ///
    /// - linear: size-weighted arithmetic, `(held_price·held + px·qty) / (held + qty)`, the expression
    ///   `OrderRecord`'s fill VWAP and `crate::money::fill::compute_fill`'s add always spelled, bit for
    ///   bit (an empty `held` is that expression too, so a first fill keeps its old bits);
    /// - inverse: HARMONIC, `(held + qty) / (held/held_px + qty/px)`. A coin-margined position's cost
    ///   is its coin outlay `q/p`, so the price that averages is the reciprocal one: 1,000 USD at 50,000
    ///   and 1,000 at 60,000 average 54,545.45, not 55,000. An empty `held` is `px` itself, and a
    ///   non-positive price has no reciprocal, so the arithmetic average stands (never a NaN).
    ///
    /// Every running average of fills on a contract calls this: the cost-basis fold
    /// (`compute_fill_value`), an order's fill VWAP, a strategy's entry average, the book walk's taker
    /// price ([`Self::taker_price`]).
    #[inline]
    #[must_use]
    pub fn average_price(self, held: f64, held_price: f64, qty: f64, px: f64) -> f64 {
        let arithmetic = (held_price * held + px * qty) / (held + qty);
        match self.kind {
            NotionalKind::Linear => arithmetic,
            NotionalKind::Inverse if held == 0.0 => px,
            NotionalKind::Inverse if held_price > 0.0 && px > 0.0 => {
                (held + qty) / (held / held_price + qty / px)
            }
            NotionalKind::Inverse => arithmetic,
        }
    }

    /// `qty` units of THIS contract as units of `other`, a contract on the SAME asset, by value at
    /// `price`: what a hedge on another venue, or a leg sized in base units, must hold. The same value
    /// is `qty` itself, bit for bit; otherwise it is `other`'s [`Self::units_worth`] of this
    /// position's [`Self::signed_notional`] at the one price, so the sign is kept and linear
    /// `(q·u_a)/u_b`, inverse to linear `q·u/p` coins, linear to inverse `q·p/u` dollars. No usable
    /// price between two different values is `0.0`: the caller sizes nothing.
    #[inline]
    #[must_use]
    pub fn units_as(self, qty: f64, price: f64, other: ContractValue) -> f64 {
        if self == other {
            return qty;
        }
        if !(price.is_finite() && price > 0.0) {
            return 0.0;
        }
        other.units_worth(self.signed_notional(qty, price), price, 1.0).unwrap_or(0.0)
    }

    /// What `qty` units of `side` cost against the displayed `book`, at most `limit`: the book walk's
    /// taker price for THIS contract. `vike_marketdata::book_taker_price` decides whether the book
    /// can take the order at all (its refusals are this answer's) and IS the linear answer, bit for
    /// bit; an inverse contract averages the walked levels through [`Self::average_price`], harmonic.
    /// The walk sits below this crate and knows no contract, so it hands over its levels
    /// (`L2Book::for_each_take`, which collects nothing) and the average is taken here. Allocation
    /// free on both kinds, so the pre-trade impact veto can read it.
    #[must_use]
    pub fn taker_price(
        self,
        book: &vike_marketdata::L2Book,
        side: i32,
        qty: f64,
        limit: Option<f64>,
    ) -> Option<f64> {
        let px = vike_marketdata::book_taker_price(book, side, qty, limit)?;
        if self.kind == NotionalKind::Linear {
            return Some(px);
        }
        let (mut held, mut avg) = (0.0, 0.0);
        book.for_each_take(side, qty, |level| {
            avg = self.average_price(held, avg, level.qty, level.price);
            held += level.qty;
        });
        Some(avg)
    }

    /// The running `qty × average price` of `held` units whose sum so far is `held_quantity_price`, joined by
    /// `qty` more at `px` (magnitudes): what an order keeps per combo leg
    /// (`OrderRecord::combo_leg_fills`) and the simulator per order (`OrderFees::quantity_price`), so that the
    /// sum over the units is the [`Self::average_price`] of the fills.
    ///
    /// - linear: `held_quantity_price + qty·px`, the `Σ qty × px` both always kept, bit for bit;
    /// - inverse: `(held + qty)` times the HARMONIC average, so the same division reads it back. The
    ///   stored shape does not change: an older build dividing it reads the harmonic average too.
    #[inline]
    #[must_use]
    pub fn add_quantity_price(self, held: f64, held_quantity_price: f64, qty: f64, px: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => held_quantity_price + qty * px,
            NotionalKind::Inverse => {
                let held_price = if held > 0.0 { held_quantity_price / held } else { 0.0 };
                (held + qty) * self.average_price(held, held_price, qty, px)
            }
        }
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
    pub fn initial_margin(
        self,
        price: f64,
        qty: f64,
        rate: f64,
        initial_margin_requirement: f64,
    ) -> f64 {
        match self.kind {
            NotionalKind::Linear => crate::money::margin::initial_margin(
                price,
                qty,
                self.per_unit,
                rate,
                initial_margin_requirement,
            ),
            NotionalKind::Inverse => rate * self.per_unit * qty * initial_margin_requirement,
        }
    }

    /// Maintenance margin of a holding. Linear is [`crate::money::margin::maintenance_margin`].
    #[inline]
    #[must_use]
    pub fn maintenance_margin(self, price: f64, qty: f64, rate: f64, mm_request: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => crate::money::margin::maintenance_margin(
                price,
                qty,
                self.per_unit,
                rate,
                mm_request,
            ),
            NotionalKind::Inverse => qty.abs() * self.per_unit * rate * mm_request,
        }
    }

    /// The margin a REVERSING order frees and re-opens (`2 · |size| · price · unit · initial_margin_requirement` on a
    /// linear contract, in that factor order; `2 · |size| · unit · initial_margin_requirement` on an inverse one).
    #[inline]
    #[must_use]
    pub fn margin_credit(self, size: f64, price: f64, initial_margin_requirement: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => {
                2.0 * size.abs() * price * self.per_unit * initial_margin_requirement
            }
            NotionalKind::Inverse => 2.0 * size.abs() * self.per_unit * initial_margin_requirement,
        }
    }

    /// Unrealized PnL of a signed `size` bought at `average_price`, valued at `px`: linear
    /// `(px − average_price) · size · u` (the expression `Account::unrealized_pnl` always spelled),
    /// inverse `(px − average_price) / average_price · size · u`, in the quote currency at `px`. A non-positive
    /// `average_price` has no inverse PnL and reads 0.
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

    /// A quote-currency `amount` valued at `px`, in the money this contract SETTLES in: the amount
    /// itself on a linear contract (it settles in its quote), `amount / px` coins on an inverse one,
    /// whose balance, PnL, fees and funding every venue keeps in the settle coin (decision 0258). A
    /// non-positive `px` has no coin value and reads 0.0, as an inverse PnL with no entry does.
    ///
    /// The coin arm is the division `vike_execution::Account` books a coin ledger's realized PnL with (the
    /// dollars at the exit over the exit price), so a coin figure built here equals the live one bit
    /// for bit.
    #[inline]
    #[must_use]
    pub fn in_settle(self, amount: f64, px: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => amount,
            NotionalKind::Inverse => {
                if px > 0.0 {
                    amount / px
                } else {
                    0.0
                }
            }
        }
    }

    /// The CASH LEG of a signed `size` at `px`, in the settle money: what a cash ledger pays to take
    /// it on (`cash -= cash_leg(Δ, p)`) and what the holding is worth (`equity = cash + Σ cash_leg(s,
    /// p_now)`). Linear is [`crate::scalar::signed_notional`]. Inverse is `−s·u/p` coins: an inverse
    /// contract is a linear one on the reciprocal price `1/p` with size `−s·u` (long the dollar
    /// contract is short the coin), so a round trip from `e` to `p` leaves `s·u·(1/e − 1/p)` coins,
    /// the venues' own PnL.
    ///
    /// It is additive across marks, as the linear leg is: moving a position's basis from one mark to
    /// the next changes no equity, which a variation-margin sweep relies on.
    #[inline]
    #[must_use]
    pub fn cash_leg(self, size: f64, px: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => crate::scalar::signed_notional(size, px, self.per_unit),
            NotionalKind::Inverse => -self.in_settle(self.signed_notional(size, px), px),
        }
    }

    /// A settle-money `amount` in the QUOTE currency at `px`, the reverse of [`Self::in_settle`]:
    /// itself on a linear contract, `amount · px` on an inverse one (a coin fee, a coin funding, a coin
    /// ledger's equity valued at the contract's own price, decision 0258). No guard on `px`: the
    /// consumers it replaced (`vike_execution::Account::commission_in_quote`, the dollar ledger's coin
    /// funding, `vike_simulator::OrderBrokerSimulator`'s sizing equity) multiplied by it as it came.
    #[inline]
    #[must_use]
    pub fn in_quote(self, amount: f64, px: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => amount,
            NotionalKind::Inverse => amount * px,
        }
    }

    /// What a fee RATE is charged on, as the three factors `(q, p, m)` of `q · p · rate · m`: linear
    /// `(qty, px, u)`, the order at its price scaled by the unit; inverse `(|qty|·|u|, 1, 1)`, the
    /// order's dollars priced at one, price-free like its notional ([`Self::order_notional`]).
    /// [`Self::fee`] multiplies them in that order; `crate::FeeSchedule::commission_on`'s notional cap
    /// and premium terms read them in their own.
    #[inline]
    #[must_use]
    pub(crate) fn fee_basis(self, qty: f64, px: f64) -> (f64, f64, f64) {
        match self.kind {
            NotionalKind::Linear => (qty, px, self.per_unit),
            NotionalKind::Inverse => (self.order_notional(qty, px), 1.0, 1.0),
        }
    }

    /// The fee at `rate` on what `qty` at `px` is worth, in the QUOTE currency: linear
    /// `qty · px · rate · u`, the factor order the frozen `fixtures/r1/` broker_sim bytes pin (it was
    /// `vike_fills::broker_sim::fee`); inverse `|qty| · |u| · rate`, the rate on the order's dollars,
    /// which is every venue's `rate·|q|·u/p` coins valued at the fill price `p` (a coin ledger divides
    /// it back by [`Self::in_settle`]).
    #[inline]
    #[must_use]
    pub fn fee(self, qty: f64, px: f64, rate: f64) -> f64 {
        let (q, p, m) = self.fee_basis(qty, px);
        q * p * rate * m
    }

    /// Perp funding of a held signed `size` at `mark`, in the money the contract SETTLES in (positive
    /// = paid: a long pays a positive rate, a short receives it):
    ///
    /// - linear: `size · mark · rate · u`, the factor order the frozen `fixtures/r1/` bytes pin (it was
    ///   `vike_fills::broker_sim::funding_charge`);
    /// - inverse: the rate on the position's dollars `size · u`, in coins at the mark,
    ///   `rate · size · u / mark`: bybit's "position value × funding rate, position value = quantity ÷
    ///   mark price", and okx's and binance COIN-M's the same. No mark, no coin value: `0.0`.
    #[inline]
    #[must_use]
    pub fn funding(self, size: f64, mark: f64, rate: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => size * mark * rate * self.per_unit,
            NotionalKind::Inverse => self.in_settle(self.signed_notional(size, mark) * rate, mark),
        }
    }

    /// A `charge` a fee schedule computed on `qty` units of a contract whose unit is ONE (an option's
    /// `crate::FeeSchedule::commission_with_underlying`, which knows no contract), on THIS linear
    /// contract: `charge · u`, the factor every rate term of `crate::FeeSchedule::commission_on` ends
    /// on. `None` on an inverse contract, whose quantity is dollars and has no such per-unit charge (an
    /// option is always linear, [`Self::of`]): the caller charges it as any other fill.
    #[inline]
    #[must_use]
    pub fn linear_charge(self, charge: f64) -> Option<f64> {
        match self.kind {
            NotionalKind::Linear => Some(charge * self.per_unit),
            NotionalKind::Inverse => None,
        }
    }

    /// GROSS notional with one unit's worth taken FIRST: linear `|size| · (price · u)`, the factor
    /// order the simulator's margin and exposure folds pin (`vike_simulator::OrderBrokerSimulator`'s gate), NOT
    /// [`Self::gross_notional`]'s `(|size| · price) · u`; inverse `|size| · |u|`, the price never
    /// entering ([`Self::order_notional`] of one unit). Decision 0170's 2026-11-07 amendment names the
    /// difference; this method keeps it so those folds stay bit for bit.
    #[inline]
    #[must_use]
    pub fn gross_by_unit(self, size: f64, price: f64) -> f64 {
        let unit = match self.kind {
            NotionalKind::Linear => price * self.per_unit,
            NotionalKind::Inverse => self.per_unit.abs(),
        };
        size.abs() * unit
    }

    /// **What a price move from the entry `basis` to `to` loses ONE unit**, in the money a sizer's
    /// context is in (the quote currency at par, a coin ledger's coin valued at `basis`):
    ///
    /// - linear: `|basis − to| · u`, the price distance times the unit, the expression the risk sizers
    ///   always spelled, bit for bit;
    /// - inverse: the contract's coin PnL of the move, `u·|1/to − 1/basis|` ([`Self::unrealized_pnl`]
    ///   through [`Self::in_settle`], the arithmetic every coin ledger books), valued at `basis`, so
    ///   risking 1% of the equity risks 1% of the coin.
    #[inline]
    #[must_use]
    pub fn move_risk(self, basis: f64, to: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => (basis - to).abs() * self.per_unit,
            NotionalKind::Inverse => {
                (self.in_settle(self.unrealized_pnl(basis, to, 1.0), to) * basis).abs()
            }
        }
    }

    /// [`Self::move_risk`] of a move of `distance` AGAINST a `side` position (+1 long, −1 short) from
    /// `basis`: linear `distance · u` (the volatility sizer's `atr · u`, which no `to` price spells bit
    /// for bit), inverse the move to `basis − side · distance`, `None` when that loses nothing (a move
    /// carrying the price to zero or past it), so the caller falls back.
    #[inline]
    #[must_use]
    pub fn move_risk_against(self, basis: f64, side: i32, distance: f64) -> Option<f64> {
        match self.kind {
            NotionalKind::Linear => Some(distance * self.per_unit),
            NotionalKind::Inverse => {
                let to = basis - f64::from(side) * distance;
                Some(self.move_risk(basis, to)).filter(|r| *r > 0.0)
            }
        }
    }

    /// What a held signed `size` loses if the price moves from `price` to its `stop`, in a sizer's
    /// money: linear `|price − stop| · |size| · u`, in the factor order the simulator's open-risk fold
    /// pins; inverse `|size| · move_risk(price, stop)`.
    #[inline]
    #[must_use]
    pub fn stop_risk(self, size: f64, price: f64, stop: f64) -> f64 {
        match self.kind {
            NotionalKind::Linear => (price - stop).abs() * size.abs() * self.per_unit,
            NotionalKind::Inverse => size.abs() * self.move_risk(price, stop),
        }
    }

    /// The GUI badge's estimate of a CROSS position's liquidation mark
    /// (`crate::money::liquidation`'s law): the mark `P` of ONE position of signed `size` at `mark` at
    /// which the WHOLE cross pool first meets the law's condition 2, every other position frozen.
    /// Solving `pool_equity + (P − mark)·s·u = rate · (others_notional + |s|·u·P)` for `P`:
    ///
    /// `P = (rate·others_notional − pool_equity + s·u·mark) / (u·(s − rate·|s|))`
    ///
    /// with the own-position term folded `(s·u)·mark` as this estimate always folded it (NOT
    /// [`Self::signed_notional`]'s `(s·mark)·u`). Advisory only: the shared pool makes a
    /// per-position cross price an estimate, and the watchdog acts on the account-level law. The LEAN
    /// buffer arm is ignored (the badge marks where condition 2 first binds, the earlier line).
    ///
    /// `0.0` ("no liquidation by price") for a flat or degenerate input, a non-finite or non-positive
    /// solution, and every INVERSE contract: its notional does not contain the price (decision 0170),
    /// so the equation has no such mark and the badge is withheld rather than shown wrong.
    #[must_use]
    pub fn cross_liquidation_price(
        self,
        pool_equity: f64,
        size: f64,
        mark: f64,
        others_notional: f64,
        maint_rate: f64,
    ) -> f64 {
        let u = self.per_unit;
        if self.kind == NotionalKind::Inverse
            || size == 0.0
            || mark <= 0.0
            || u <= 0.0
            || maint_rate <= 0.0
        {
            return 0.0;
        }
        let denominator = u * (size - maint_rate * size.abs());
        if denominator == 0.0 {
            return 0.0;
        }
        let p = (maint_rate * others_notional - pool_equity + size * u * mark) / denominator;
        if p.is_finite() && p > 0.0 { p } else { 0.0 }
    }

    /// An ISOLATED position's liquidation mark: the linear closed form
    /// ([`crate::liquidation_price`], in which the unit cancels) on a linear contract, and `0.0` (no
    /// badge) on an inverse one, which that form does not describe.
    #[must_use]
    pub fn isolated_liquidation_price(
        self,
        avg_px: f64,
        side: i32,
        initial_margin: f64,
        mm: f64,
    ) -> f64 {
        match self.kind {
            NotionalKind::Linear => {
                crate::money::margin::liquidation_price(avg_px, side, initial_margin, mm)
            }
            NotionalKind::Inverse => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::contract::{
        ContractFuture, ContractFutureFamily, ContractOption, ContractPerp,
    };
    use vike_marketdata::OptionKind;

    fn perp(m: Margining) -> Contract {
        Contract::Perp(ContractPerp {
            margining: m,
            settle: "BTC".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        })
    }

    fn future(m: Margining) -> Contract {
        Contract::Future(ContractFuture {
            margining: m,
            settle: "BTC".into(),
            value_currency: None,
            expiry_ms: 1_793_000_000_000,
            tenor: None,
            delivery: crate::instrument::contract::ContractFutureDelivery::Cash,
            family: ContractFutureFamily::Crypto,
        })
    }

    fn option(m: Margining) -> Contract {
        Contract::Option(ContractOption {
            right: OptionKind::Call,
            strike: 60_000.0,
            expiry_ms: 1_793_000_000_000,
            style: crate::instrument::contract::ContractOptionExerciseStyle::European,
            margining: m,
            settle: "BTC".into(),
            value_currency: None,
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
    fn a_linear_order_is_quantity_times_price_times_unit() {
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
            assert_eq!(v.cash_leg(a, b).to_bits(), crate::signed_notional(a, b, m).to_bits());
            assert_eq!(v.in_settle(a, b).to_bits(), a.to_bits(), "a linear amount is its own");
            assert_eq!(
                v.unrealized_pnl(b, a, 3.0).to_bits(),
                (3.0 * (a - b) * m).to_bits(),
                "the simulator's `size * (px - avg) * mult`: two-operand products commute"
            );
        }
    }

    /// The sizing law on a linear value at par is the law it replaced, `target / (price · u)` with `0.0` on a
    /// non-positive denominator, bit for bit, degenerate inputs included.
    #[test]
    fn units_worth_on_a_linear_value_at_par_is_the_old_law_bit_for_bit() {
        let old = |target: f64, price: f64, u: f64| {
            let denominator = price * u;
            if denominator > 0.0 { target / denominator } else { 0.0 }
        };
        let prices = [50_000.0, 0.25, 1.25, 65_700.0, 0.0, -3.0, 1e-300];
        let units = [1.0, 50.0, 10_000.0, 0.0001, 0.0, -2.0];
        for p in prices {
            for u in units {
                for t in [1_000.0, -1_000.0, 0.0, 0.3 * 12_345.678] {
                    let got = ContractValue::linear(u).units_worth(t, p, 1.0).expect("par");
                    assert_eq!(got.to_bits(), old(t, p, u).to_bits(), "{t} at {p} x {u}");
                }
                let unit = ContractValue::linear(u).unit_value(p, 1.0).expect("par");
                assert_eq!(unit.to_bits(), (p * u).to_bits(), "unit {p} x {u}");
            }
        }
    }

    /// An inverse unit is its face whatever the price; the factor scales either kind; an unusable factor
    /// sizes nothing rather than at par.
    #[test]
    fn units_worth_reads_the_kind_and_refuses_an_unusable_factor() {
        let perp = ContractValue::inverse(1.0);
        for p in [60_000.0, 100_000.0, 0.5] {
            assert_eq!(perp.unit_value(p, 1.0), Some(1.0));
            assert_eq!(perp.units_worth(1_000.0, p, 1.0), Some(1_000.0));
        }
        assert_eq!(ContractValue::inverse(100.0).units_worth(1_000.0, 60_000.0, 1.0), Some(10.0));
        // USDJPY in IG contracts of 10,000 on a USD account at 128: one contract is 10,000 dollars
        let jpy = ContractValue::linear(10_000.0);
        assert_eq!(jpy.unit_value(128.0, 1.0 / 128.0), Some(10_000.0));
        assert_eq!(jpy.units_worth(100_000.0, 128.0, 1.0 / 128.0), Some(10.0));
        for bad in [f64::NAN, 0.0, -1.0, f64::INFINITY] {
            assert_eq!(jpy.unit_value(128.0, bad), None, "rate {bad}");
            assert_eq!(perp.units_worth(1_000.0, 60_000.0, bad), None, "rate {bad}");
        }
    }

    /// The venues' coin law (the inverse research, Part 1.5): long `s` dollars from `e` to `p` is
    /// `s·u·(1/e − 1/p)` coins, and the dollar figure at `p` is that coin figure times `p`.
    #[test]
    fn an_inverse_amount_in_its_settle_coin_is_the_dollars_over_the_price() {
        let v = ContractValue::inverse(1.0);
        // Bybit's own example: 1,000 contracts long from 5,000 to 5,500 = 0.01819 BTC.
        let coin = v.in_settle(v.unrealized_pnl(5_000.0, 5_500.0, 1_000.0), 5_500.0);
        assert!((coin - 1_000.0 * (1.0 / 5_000.0 - 1.0 / 5_500.0)).abs() < 1e-15, "{coin}");
        // The page prints 0.01819 for 0.0181818: its rounding, not a different law.
        assert!((coin - 0.01819).abs() < 1e-5, "{coin}");
        // Binance COIN-M's: 100 contracts of 100 USD short from 50,000, mark 45,500 = 0.0198 BTC.
        let b = ContractValue::inverse(100.0);
        let coin = b.in_settle(b.unrealized_pnl(50_000.0, 45_500.0, -100.0), 45_500.0);
        assert!((coin - 0.019_78).abs() < 1e-5, "{coin}");
        assert_eq!(v.in_settle(100.0, 0.0), 0.0, "no price, no coin value");
        assert_eq!(v.in_settle(100.0, -1.0), 0.0);
    }

    /// The cash leg of an inverse round trip is the coin PnL, and a basis moved to an intermediate mark
    /// changes no equity (the property a variation-margin sweep needs, which the quote-PnL form lacks).
    #[test]
    fn an_inverse_cash_leg_is_the_coin_pnl_and_adds_across_marks() {
        let v = ContractValue::inverse(1.0);
        let (s, e, m, p) = (1_000.0, 60_000.0, 63_000.0, 66_000.0);
        assert_eq!(v.cash_leg(s, e), -(s / e), "long dollars is short coins");
        let round_trip = v.cash_leg(s, p) - v.cash_leg(s, e);
        let coin = s * (1.0 / e - 1.0 / p);
        assert!((round_trip - coin).abs() <= 1e-12 * coin, "{round_trip} vs {coin}");
        let two_legs =
            (v.cash_leg(s, m) - v.cash_leg(s, e)) + (v.cash_leg(s, p) - v.cash_leg(s, m));
        assert!((two_legs - coin).abs() <= 1e-12 * coin, "{two_legs} vs {coin}");
        // The quote-PnL form does not add: 10% then 4.76% is not 10% of the first price.
        let quote = v.unrealized_pnl(e, m, s) + v.unrealized_pnl(m, p, s);
        assert!((quote - v.unrealized_pnl(e, p, s)).abs() > 1.0, "{quote}");
        // ...and the coin value of a holding at the mark is the dollars over the mark.
        assert_eq!(v.cash_leg(-s, p), s / p);
        assert_eq!(v.cash_leg(s, 0.0), 0.0, "no price, no coin value");
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

    /// A symbol quoted in the account's own currency is what it was: `r == 1.0` is the value itself.
    #[test]
    fn in_account_at_one_is_the_value_bit_for_bit() {
        for v in [
            ContractValue::linear(1.0),
            ContractValue::linear(10.0),
            ContractValue::inverse(1.0),
            ContractValue::inverse(100.0),
            // even a value no admission would let in is returned untouched at par: the method scales,
            // it does not validate the base (the caller folds the base first)
            ContractValue::linear(0.0),
        ] {
            let out = v.in_account(1.0).expect("par is always representable");
            assert_eq!(out.kind, v.kind);
            assert_eq!(out.per_unit.to_bits(), v.per_unit.to_bits());
        }
    }

    #[test]
    fn in_account_scales_the_unit_and_keeps_the_kind() {
        let lin = ContractValue::linear(10.0).in_account(0.5).unwrap();
        assert_eq!(lin, ContractValue::linear(5.0));
        let inverse = ContractValue::inverse(100.0).in_account(1.35).unwrap();
        assert_eq!(inverse.kind, NotionalKind::Inverse);
        assert_eq!(inverse.per_unit, 100.0 * 1.35);
    }

    /// FAIL CLOSED: a rate that is not finite and positive is `None`, never `1.0`, and `folded` (which
    /// WOULD turn the degenerate product into 1.0) is never reached by this method.
    #[test]
    fn in_account_refuses_a_degenerate_rate_instead_of_valuing_at_par() {
        for bad in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(ContractValue::linear(1.0).in_account(bad), None, "rate {bad}");
            assert_eq!(ContractValue::inverse(1.0).in_account(bad), None, "rate {bad}");
        }
        // a finite positive rate whose product leaves the representable range is refused as well
        assert_eq!(ContractValue::linear(1e300).in_account(1e300), None, "overflow");
        assert_eq!(ContractValue::linear(1e-300).in_account(1e-300), None, "underflow to zero");
        // ...and so is a base that is not itself a positive factor, once the rate is not par
        assert_eq!(ContractValue::linear(0.0).in_account(0.5), None);
        assert_eq!(ContractValue::linear(-3.0).in_account(0.5), None);
    }

    /// The headline numbers of the design (section 1), as GOLDENS: a USD account, USDJPY at 150,
    /// 100,000 units. Without the conversion the engine books JPY as if it were USD.
    #[test]
    fn usdjpy_on_a_usd_account_is_booked_in_dollars_not_yen() {
        use crate::money::fill::compute_fill_value;
        let raw = ContractValue::linear(1.0);
        assert_eq!(raw.order_notional(100_000.0, 150.0), 15_000_000.0, "the old 150x-high figure");

        // r = USD per JPY = 1 / 150, NEVER 150 (the flipped pair is a 22,500x error)
        let at_open = ContractValue::linear(1.0).in_account(1.0 / 150.0).unwrap();
        let notional = at_open.order_notional(100_000.0, 150.0);
        assert!((notional - 100_000.0).abs() < 1e-8, "{notional}");

        // Close the long at 150.10: the rate in force at the close is 1 / 150.10 (the margin form)
        let at_close = ContractValue::linear(1.0).in_account(1.0 / 150.10).unwrap();
        let out = compute_fill_value(100_000.0, 150.0, -1, 100_000.0, 150.10, at_close);
        let want = 10_000.0 / 150.10; // 66.6222...
        assert!((out.realized_pnl - want).abs() < 1e-9, "{} vs {want}", out.realized_pnl);
        let unconverted = compute_fill_value(100_000.0, 150.0, -1, 100_000.0, 150.10, raw);
        assert!(
            (unconverted.realized_pnl - 10_000.0).abs() < 1e-2,
            "the same fill without the conversion books 10,000 \"USD\" (150x high): {}",
            unconverted.realized_pnl
        );
        assert!(out.realized_pnl < 100.0 && unconverted.realized_pnl > 9_000.0);
    }

    /// EURGBP on a USD account: GBP-quoted, so the raw figure is 1.35x LOW.
    #[test]
    fn eurgbp_on_a_usd_account_is_booked_in_dollars_not_pounds() {
        let raw = ContractValue::linear(1.0);
        assert!((raw.order_notional(100_000.0, 0.85) - 85_000.0).abs() < 1e-9);
        let usd = ContractValue::linear(1.0).in_account(1.35).unwrap();
        let notional = usd.order_notional(100_000.0, 0.85);
        assert!((notional - 114_750.0).abs() < 1e-8, "{notional}");
    }

    /// The fee and the funding are the simulator's frozen products, bit for bit (the `fixtures/r1/`
    /// broker_sim bytes pin the linear ones): `vike_fills::broker_sim::fee(q, p, rate, m)` was
    /// `q·p·rate·m` and the inverse commission `fee(|q|·|u|, 1.0, rate, 1.0)`; `funding_charge` was
    /// `s·p·rate·u` and, inverse, the dollars `s·u` times the rate over the mark.
    #[test]
    fn fee_and_funding_keep_the_frozen_factor_order() {
        let cases = [
            (3.0, 7.0, 0.002, 5.0),
            (-2.5, 65_700.0, 0.0001, 1.0),
            (0.1, 0.2, 0.000_7, 0.0001),
            (1e-3, 46_842.6, 0.000_55, 10.0),
        ];
        for (q, p, r, m) in cases {
            let v = ContractValue::linear(m);
            assert_eq!(v.fee(q, p, r).to_bits(), (q * p * r * m).to_bits(), "fee {q} {p} {r} {m}");
            assert_eq!(v.funding(q, p, r).to_bits(), (q * p * r * m).to_bits());
            let inverse = ContractValue::inverse(m);
            assert_eq!(
                inverse.fee(q, p, r).to_bits(),
                (q.abs() * m.abs() * 1.0 * r * 1.0).to_bits()
            );
            assert_eq!(inverse.funding(q, p, r).to_bits(), (q * m * r / p).to_bits());
        }
        let inverse = ContractValue::inverse(1.0);
        assert_eq!(inverse.funding(10_000.0, 0.0, 0.0001), 0.0, "no mark, no coin value");
        assert_eq!(inverse.fee(-1_000.0, 1.0, 0.000_35), inverse.fee(1_000.0, 60_000.0, 0.000_35));
    }

    /// Inverse funding is the rate on the position's dollars in coin at the mark (bybit: position value
    /// = quantity ÷ mark), and a long pays a positive rate while a short receives it.
    #[test]
    fn an_inverse_funding_charge_is_paid_in_coin_on_the_position_value_at_the_mark() {
        let v = ContractValue::inverse(1.0);
        let long = v.funding(10_000.0, 50_000.0, 0.000_1);
        assert!((long - 10_000.0 / 50_000.0 * 0.000_1).abs() < 1e-17, "{long}");
        assert!(long > 0.0, "a long pays a positive rate");
        assert_eq!(v.funding(-10_000.0, 50_000.0, 0.000_1), -long, "a short receives");
        assert_eq!(v.funding(10_000.0, 0.0, 0.000_1), 0.0, "no mark, no coin value");
    }

    /// The reverse of `in_settle`, and the bare product the coin conversions spelled.
    #[test]
    fn in_quote_is_the_settle_amount_at_the_price() {
        assert_eq!(ContractValue::linear(5.0).in_quote(3.0, 60_000.0), 3.0);
        let inverse = ContractValue::inverse(1.0);
        assert_eq!(inverse.in_quote(0.5, 60_000.0).to_bits(), (0.5f64 * 60_000.0).to_bits());
        let back = inverse.in_quote(inverse.in_settle(1_000.0, 66_000.0), 66_000.0);
        assert!((back - 1_000.0).abs() < 1e-9, "{back}");
    }

    #[test]
    fn a_linear_charge_scales_by_the_unit_and_an_inverse_has_none() {
        let es = ContractValue::linear(50.0);
        assert_eq!(es.linear_charge(0.85).map(f64::to_bits), Some((0.85f64 * 50.0).to_bits()));
        assert_eq!(ContractValue::inverse(1.0).linear_charge(0.85), None);
    }

    /// The simulator gate's `|s|·(p·u)` is not `gross_notional`'s `(|s|·p)·u` in the last bit, which
    /// is why it is a method of its own.
    #[test]
    fn gross_by_unit_takes_the_unit_first() {
        let v = ContractValue::linear(0.7);
        assert_eq!(v.gross_by_unit(-3.0, 0.1).to_bits(), (3.0 * (0.1 * 0.7f64)).to_bits());
        assert_ne!(v.gross_by_unit(-3.0, 0.1).to_bits(), v.gross_notional(-3.0, 0.1).to_bits());
        let inverse = ContractValue::inverse(10.0);
        assert_eq!(inverse.gross_by_unit(-3.0, 65_700.0), 30.0, "the price never enters");
        assert_eq!(inverse.gross_by_unit(1.0, 65_700.0), inverse.order_notional(1.0, 65_700.0));
    }

    /// The risk sizers' price-move arithmetic, moved here bit for bit.
    #[test]
    fn the_move_risks_are_the_sizers_products() {
        for (b, t, u) in [(100.0, 97.5, 1.0), (5_000.0, 4_987.25, 50.0), (0.25, 0.3, 10_000.0)] {
            let v = ContractValue::linear(u);
            assert_eq!(v.move_risk(b, t).to_bits(), ((b - t).abs() * u).to_bits());
            assert_eq!(v.move_risk_against(b, 1, 2.5).map(f64::to_bits), Some((2.5 * u).to_bits()));
            assert_eq!(v.stop_risk(-3.0, b, t).to_bits(), ((b - t).abs() * 3.0 * u).to_bits());
        }
        let inverse = ContractValue::inverse(1.0);
        let want = (inverse.in_settle(inverse.unrealized_pnl(60_000.0, 57_000.0, 1.0), 57_000.0)
            * 60_000.0)
            .abs();
        assert_eq!(inverse.move_risk(60_000.0, 57_000.0).to_bits(), want.to_bits());
        assert_eq!(inverse.move_risk_against(60_000.0, 1, 3_000.0), Some(want));
        assert_eq!(inverse.move_risk_against(60_000.0, 1, 60_000.0), None, "to zero: no coin risk");
        assert_eq!(
            inverse.stop_risk(-2_000.0, 60_000.0, 57_000.0).to_bits(),
            (2_000.0 * want).to_bits()
        );
    }

    // --- the liquidation badges (moved from `crate::money::liquidation`, unchanged) -------------

    fn cross(equity: f64, size: f64, mark: f64, u: f64, others: f64, rate: f64) -> f64 {
        ContractValue::linear(u).cross_liquidation_price(equity, size, mark, others, rate)
    }

    #[test]
    fn cross_est_single_long_below_mark() {
        // long 1 @ mark 100, equity 60, rate 0.05, no others:
        // P = (0 - 60 + 100) / (1·(1 - 0.05)) = 40/0.95 ≈ 42.105 — below the mark
        let p = cross(60.0, 1.0, 100.0, 1.0, 0.0, 0.05);
        assert!((p - 40.0 / 0.95).abs() < 1e-9);
        assert!(p < 100.0);
        // sanity: at P the law's condition-2 binds: equity(P) == rate·notional(P)
        let eq_at = 60.0 + (p - 100.0) * 1.0;
        assert!((eq_at - 0.05 * p).abs() < 1e-9);
    }

    #[test]
    fn cross_est_single_short_above_mark() {
        // short 1 @ mark 100, equity 60: P = (−60 − 100)/(1·(−1 − 0.05)) = 160/1.05 ≈ 152.4
        let p = cross(60.0, -1.0, 100.0, 1.0, 0.0, 0.05);
        assert!(p > 100.0);
        let eq_at = 60.0 + (100.0 - p) * 1.0;
        assert!((eq_at - 0.05 * p).abs() < 1e-9);
    }

    #[test]
    fn cross_est_other_positions_pull_the_line_closer() {
        let alone = cross(60.0, 1.0, 100.0, 1.0, 0.0, 0.05);
        let crowded = cross(60.0, 1.0, 100.0, 1.0, 500.0, 0.05);
        assert!(crowded > alone); // others' maintenance consumes shared equity → earlier liq
    }

    #[test]
    fn cross_est_healthy_long_clamps_to_zero() {
        // equity so large the solution is negative → 0.0 = "no liquidation by price"
        assert_eq!(cross(1_000.0, 1.0, 100.0, 1.0, 0.0, 0.05), 0.0);
    }

    #[test]
    fn cross_est_degenerate_inputs_zero() {
        assert_eq!(cross(60.0, 0.0, 100.0, 1.0, 0.0, 0.05), 0.0);
        assert_eq!(cross(60.0, 1.0, 0.0, 1.0, 0.0, 0.05), 0.0);
        assert_eq!(cross(60.0, 1.0, 100.0, 0.0, 0.0, 0.05), 0.0);
        assert_eq!(cross(60.0, 1.0, 100.0, 1.0, 0.0, 0.0), 0.0);
        assert_eq!(cross(f64::NAN, 1.0, 100.0, 1.0, 0.0, 0.05), 0.0);
    }

    /// The badge's own-position term is `(s·u)·mark`, the order it always had, and an inverse
    /// contract has no badge.
    #[test]
    fn cross_est_keeps_its_factor_order_and_withholds_an_inverse_badge() {
        let (eq, s, mark, u, others, rate): (f64, f64, f64, f64, f64, f64) =
            (0.1, 3.0, 0.1, 0.7, 1.2, 0.05);
        let denominator = u * (s - rate * s.abs());
        let was = (rate * others - eq + s * u * mark) / denominator;
        assert!(was.is_finite() && was > 0.0, "{was}");
        assert_eq!(cross(eq, s, mark, u, others, rate).to_bits(), was.to_bits());
        assert_eq!(
            ContractValue::inverse(1.0).cross_liquidation_price(60.0, 1.0, 100.0, 0.0, 0.05),
            0.0
        );
    }

    #[test]
    fn the_isolated_badge_is_the_linear_closed_form_and_none_on_inverse() {
        let lin = ContractValue::linear(50.0).isolated_liquidation_price(64_000.0, 1, 0.1, 0.005);
        assert_eq!(lin.to_bits(), crate::liquidation_price(64_000.0, 1, 0.1, 0.005).to_bits());
        assert_eq!(
            ContractValue::inverse(1.0).isolated_liquidation_price(64_000.0, 1, 0.1, 0.005),
            0.0
        );
    }

    /// THE averaging law: two 1,000-USD buys at 50,000 and 60,000 average 54,545.45 on an inverse
    /// contract (the coin outlay `Σq/p`), 55,000 on a linear one; the linear arm is the expression an
    /// order's VWAP and the cost-basis add always spelled, bit for bit, an empty holding included.
    #[test]
    fn the_averaging_law_is_arithmetic_linear_and_harmonic_inverse() {
        let inverse = ContractValue::inverse(1.0);
        let avg = inverse.average_price(1_000.0, 50_000.0, 1_000.0, 60_000.0);
        assert!((avg - 600_000_000.0 / 11_000.0).abs() < 1e-9, "{avg}");
        assert!((avg - 54_545.454_545).abs() < 1e-6, "{avg}");
        let lin = ContractValue::linear(1.0);
        assert_eq!(lin.average_price(1_000.0, 50_000.0, 1_000.0, 60_000.0), 55_000.0);
        for (held, held_price, q, px) in
            [(0.0, 0.0, 0.3, 65_700.1), (0.7, 1.1, 0.2, 1.3), (3.0, 100.0, 1.0, 130.0)]
        {
            for v in [ContractValue::linear(1.0), ContractValue::linear(50.0)] {
                let old = (held_price * held + px * q) / (held + q);
                assert_eq!(v.average_price(held, held_price, q, px).to_bits(), old.to_bits());
            }
        }
        // an empty holding is its first price exactly; a price with no reciprocal stays arithmetic
        assert_eq!(inverse.average_price(0.0, 0.0, 7.0, 65_700.3).to_bits(), 65_700.3f64.to_bits());
        assert_eq!(inverse.average_price(10.0, 0.0, 10.0, 200.0), 100.0, "no entry: arithmetic");
        // the unit does not average: the law is the same at any face
        let face = ContractValue::inverse(100.0).average_price(10.0, 50_000.0, 10.0, 60_000.0);
        assert_eq!(face.to_bits(), inverse.average_price(10.0, 50_000.0, 10.0, 60_000.0).to_bits());
    }

    /// One contract's units as another's: 10 Deribit dollars are 10/65,000 BTC, not 10 BTC; one BTC
    /// is 650 contracts of 100 dollars; the same value is the quantity itself, bit for bit.
    #[test]
    fn units_as_converts_by_value_at_one_price_and_keeps_the_sign() {
        let deribit = ContractValue::inverse(1.0);
        let btc = ContractValue::linear(1.0);
        assert_eq!(deribit.units_as(10.0, 65_000.0, btc), 10.0 / 65_000.0);
        assert_eq!(deribit.units_as(-10.0, 65_000.0, btc), -10.0 / 65_000.0, "the sign is kept");
        assert_eq!(btc.units_as(1.0, 65_000.0, ContractValue::inverse(100.0)), 650.0);
        assert_eq!(
            ContractValue::linear(50.0).units_as(2.0, 5_000.0, ContractValue::linear(5.0)),
            20.0
        );
        for q in [0.1, -3.7, 1e-9] {
            for v in [btc, deribit, ContractValue::linear(0.7)] {
                assert_eq!(v.units_as(q, 123.456, v).to_bits(), q.to_bits(), "{v:?}");
            }
        }
        assert_eq!(deribit.units_as(10.0, f64::NAN, btc), 0.0, "no price, no conversion");
        assert_eq!(deribit.units_as(10.0, 0.0, btc), 0.0);
    }

    /// The book walk's taker price on an inverse book is the harmonic average of the walked levels;
    /// on a linear one it is the walk's own VWAP, bit for bit, refusals included.
    #[test]
    fn the_taker_price_of_an_inverse_book_is_harmonic() {
        use vike_marketdata::{L2Book, L2BookLevel, book_taker_price};
        let mut b = L2Book::new(0.5);
        b.apply_snapshot(
            1,
            &[L2BookLevel::new(49_000.0, 1_000.0)],
            &[L2BookLevel::new(50_000.0, 1_000.0), L2BookLevel::new(60_000.0, 1_000.0)],
        );
        let inverse = ContractValue::inverse(1.0);
        let got = inverse.taker_price(&b, 1, 2_000.0, None).expect("covered");
        assert!((got - 54_545.454_545).abs() < 1e-6, "{got}");
        let lin = ContractValue::linear(1.0).taker_price(&b, 1, 2_000.0, None);
        assert_eq!(lin, Some(55_000.0));
        for (side, qty, limit) in
            [(1, 1_500.0, None), (1, 2_000.0, Some(55_000.0)), (-1, 10.0, None)]
        {
            assert_eq!(
                ContractValue::linear(10.0).taker_price(&b, side, qty, limit),
                book_taker_price(&b, side, qty, limit)
            );
        }
        assert_eq!(inverse.taker_price(&b, 1, 5_000.0, None), None, "beyond the displayed depth");
        assert_eq!(inverse.taker_price(&b, 1, 2_000.0, Some(55_000.0)), None, "past the limit");
        assert_eq!(inverse.taker_price(&b, 1, 500.0, None), Some(50_000.0), "one level: its price");
    }

    /// The running `qty × average` is the `Σ qty × px` it always was on a linear contract, bit for
    /// bit, and on an inverse one divides back to the harmonic average.
    #[test]
    fn the_running_quantity_price_reads_back_the_averaging_law() {
        for v in [ContractValue::linear(1.0), ContractValue::linear(50.0)] {
            for (held, sum, q, px) in [(0.0, 0.0, 0.3, 65_700.1), (0.7, 0.77, 0.2, 1.3)] {
                assert_eq!(
                    v.add_quantity_price(held, sum, q, px).to_bits(),
                    (sum + q * px).to_bits()
                );
            }
        }
        let inverse = ContractValue::inverse(1.0);
        let first = inverse.add_quantity_price(0.0, 0.0, 1_000.0, 50_000.0);
        assert_eq!(first, 50_000_000.0, "a first fill is its own price times its size");
        let both = inverse.add_quantity_price(1_000.0, first, 1_000.0, 60_000.0);
        assert!((both / 2_000.0 - 54_545.454_545).abs() < 1e-6, "{}", both / 2_000.0);
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
