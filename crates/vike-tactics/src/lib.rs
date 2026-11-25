//! # vike-tactics: the order language a strategy rule is written in
//!
//! Decision 0269 (`docs/decisions/0269-the-strategy-labs-order-language-becomes-its-own-crate-on-the-order-layer-and-its-in-bar-fill-rules-become-simulator-options.md`).
//! A rule DECLARES orders; the order layer fills them. A rule implements [`Rule`]: its [`RuleDefaults`]
//! (the size, pyramiding and commission its verbs default to), and what it does on a bar and after
//! its own fill, through [`Tactics`] (by convention the parameter `strategy`):
//!
//! ```ignore
//! fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
//!     if self.long_signal(bar) {
//!         strategy.entry("L1", OrderSide::Long);
//!         strategy.exit("L1").stop_pct(1.0).take_pct(2.0);
//!     }
//!     if self.flat_signal(bar) {
//!         strategy.close("L1");
//!     }
//! }
//! ```
//!
//! [`RuleRun`] makes a rule a `vike_model::Strategy<B>` for EVERY broker implementing
//! `vike_model::OrderBroker`, so one rule runs in a backtest, on paper and live through one path.
//!
//! A rule trades its run's own symbol and those the run declares ([`RuleRun::trades`]), one book each:
//! [`Tactics::on`] makes another current for the verbs and reads after it, and
//! [`Tactics::position_of`] / [`Tactics::price_of`] read any. The signal on one symbol, the trade on
//! another:
//!
//! ```ignore
//! fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
//!     if bar.symbol.as_deref() == Some("BTCUSDT") && self.breaks_out(bar)
//!         && strategy.position_of("ETHUSDT") == 0.0
//!     {
//!         strategy.on("ETHUSDT").entry("L", OrderSide::Long);
//!         strategy.exit("L").stop_pct(1.0).take_pct(2.0);
//!     }
//! }
//! let run = RuleRun::new(EthOnBtc::default(), "BTCUSDT").trades(&["ETHUSDT"]);
//! ```
//!
//! A rule works a size out from [`TacticsSizing`] ([`Tactics::sizing`]): the equity, the contract's value, the
//! header's commission, and the slippage and quantity step its run states ([`RuleRun::slippage`],
//! [`RuleRun::qty_step`]). A value, so a size read inside an order verb's builder borrows nothing.
//!
//! A rule reads a stored series beside its own bars by name, as of its last closed bar
//! ([`Tactics::input`], a [`TacticsInput`], declared in [`Rule::inputs`]; decision 0271), and the run's
//! closed trades so far as the broker's closed-trade ledger booked them ([`Tactics::closed_trades`]).
//! A week, a month, a quarter or an hour is built from a series' bars by [`HigherTimeframe`]
//! ([`HigherTimeframePeriod`]). The one read ahead, an input's WHOLE series ([`Tactics::input_whole`]),
//! is served by a backtest of a file marked `runs_on = "backtest"` alone (decision 0271's amendment).
//!
//! **What it is not.** It computes no fill and defines no exit kind: every order it sends is a kind
//! or a leg of the order layer, and meets the pre-trade gate and, live, the venue's capability row
//! as any order does. It reads no engine state beyond the seam's reads, names no simulator, and
//! holds no money: the lots and bindings it books are quantities and prices only (a closed trade's
//! money is the broker's ledger's, read through the seam). Normal edges: `vike-model`,
//! `vike-marketdata`, `vike-utility`'s calendar and `libm` (a size's floor), nothing else (layer 15).
//!
//! The crate-root names below are the vocabulary; the modules are private, except the six a strategy
//! file names by path, what the strategy gallery's rules share (plan 2b, C3): [`signal`], [`helpers`],
//! [`rolling`], [`sizing`] (its `TacticsSizing` is named at the root), [`calendar`] and [`higher`]. What of
//! that vocabulary reads an indicator is `vike_indicators::strategy_kit`'s.

mod book;
pub mod calendar;
mod frame;
mod header;
pub mod helpers;
pub mod higher;
mod input;
mod lots;
pub mod rolling;
mod rule;
mod run;
pub mod signal;
pub mod sizing;
mod tactics;
mod verbs;

pub use frame::{HigherTimeframe, HigherTimeframeHistoryBefore, HigherTimeframePeriod};
pub use header::RuleDefaults;
pub use input::TacticsInput;
pub use lots::{TacticsBookBinding, TacticsBookLot};
pub use rule::Rule;
pub use run::{RuleRun, RuleRunOptions};
pub use sizing::TacticsSizing;
pub use tactics::Tactics;
pub use verbs::{TacticsClose, TacticsEntry, TacticsExit, TacticsOrder, TacticsTrail};
