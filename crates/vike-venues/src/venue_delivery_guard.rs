//! `DeliveryGuardRow`: how long before a dated contract's EXPIRY a venue stops taking an OPENING order on it (the
//! delivery guard), and [`DeliveryGuardRow::verdict`], the ONE rule every reader applies to that window.
//!
//! **Why a guard.** The engine books no delivery settlement: a position the venue settles at the expiry stays in the
//! engine's book as a position the venue no longer holds, until an operator confirms the drift. So inside the window
//! no order may OPEN a position on the contract.
//!
//! **The rule is okx's** (its bridge's own per-order check before B3; the owner's ruling of 2026-11-22 makes it every
//! venue's): inside the window an OPENING order is refused and a REDUCE-ONLY order is taken until the
//! expiry itself, so no position is trapped at the delivery; at or after the expiry every order is refused (the
//! instrument no longer trades). The verdict is per ORDER. **The expiry is the contract's ENGINE NAME**, read by ONE
//! pure function, [`DeliveryGuardRow::expiry_of`]: the day is decision 0290's user-name field (`_YYMMDD`, or a deribit
//! option's own `DDMMMYY`), the time of day the row's column (08:00 UTC for binance, bybit, okx and deribit, 00:00 UTC
//! of ibkr's last trading day). [`delivery_verdict`] is the one call over both.
//!
//! **Every roster venue is a NAMED row** (`crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the
//! playbook)"), its `why` saying where its window came from. binance, bybit and okx held 24 h as a constant of their
//! own bridge and ibkr 5 days until B3 moved each here; deribit's 24 h is the owner's ruling of 2026-11-22 (decision
//! 0233 had admitted a dated future through its expiry). The readers are every bridge's send path and its admission
//! (with `reduce_only` set, so only a delivered contract is refused there) and vike-core's window sweep, which cancels
//! our own resting opening orders once the window starts. A `None` row admits a dated contract with no guard; an
//! unknown venue string has none either.

use vike_model::{MS_PER_DAY, MS_PER_HOUR};
use vike_utility::calendar::days_from_civil;

use crate::{deribit_expiry, is_dated, split_perp};

/// 08:00 UTC, the time of day binance, bybit, okx and deribit deliver a dated contract, in milliseconds after 00:00.
const AT_0800_UTC_MS: i64 = 8 * MS_PER_HOUR;

/// One venue's delivery guard: its window, when in the day it delivers, and where it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryGuardRow {
    /// How long before the expiry an OPENING order is refused, in milliseconds; `None`: the venue has no guard.
    pub window_ms: Option<i64>,
    /// UTC time of day the venue delivers, ms after 00:00: binance, bybit, okx, deribit 8 h; ibkr 0 (00:00 UTC of the
    /// last trading day, what its bridge judges); 0 on a row with no window.
    pub delivery_time_of_day_ms: i64,
    /// The venue's options keep their own name (decision 0290) and its `DDMMMYY` is read by [`crate::deribit_expiry`]:
    /// deribit only.
    pub option_day_in_venue_name: bool,
    /// Why, citing the adapter code (a `path`'s `symbol`, never a line number).
    pub why: &'static str,
}

/// What [`DeliveryGuardRow::verdict`] says of one order. Each caller words its own refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryGuardVerdict {
    /// The order may be sent.
    Taken,
    /// An OPENING order inside the window: refused. `left_ms` is the time left to the expiry, above zero.
    OpeningInsideWindow { left_ms: i64 },
    /// The expiry has passed: every order is refused, a reduce-only one too.
    Delivered,
}

impl DeliveryGuardRow {
    /// **The one verdict**: whether an order on a contract expiring at `expiry_ms` may be sent at `now_ms` (epoch
    /// milliseconds both). A row with no window takes every order; otherwise the expiry refuses every order, and the
    /// window an opening one (`reduce_only == false`), exactly as okx's `expiry_refusal` does.
    #[must_use]
    pub const fn verdict(
        &self,
        expiry_ms: i64,
        now_ms: i64,
        reduce_only: bool,
    ) -> DeliveryGuardVerdict {
        let Some(window_ms) = self.window_ms else {
            return DeliveryGuardVerdict::Taken;
        };
        if now_ms >= expiry_ms {
            return DeliveryGuardVerdict::Delivered;
        }
        if !reduce_only && now_ms >= expiry_ms.saturating_sub(window_ms) {
            return DeliveryGuardVerdict::OpeningInsideWindow { left_ms: expiry_ms - now_ms };
        }
        DeliveryGuardVerdict::Taken
    }

    /// The delivery instant of the dated contract `symbol` (an engine user name) names: [`split_perp`], then
    /// [`is_dated`]'s `_YYMMDD`, else (option rows) the `DDMMMYY` of `BASE[_QUOTE]-DDMMMYY-STRIKE-C|P`, at
    /// [`Self::delivery_time_of_day_ms`]. `None`: no window, or the name states no day (perpetual, spot, FX, combo).
    /// Pure, allocation-free.
    #[must_use]
    pub fn expiry_of(&self, symbol: &str) -> Option<i64> {
        self.window_ms?;
        let (name, _) = split_perp(symbol);
        let (year, month, day) = match name.split_once('_') {
            Some((_, yymmdd)) if is_dated(name) => {
                let number = |at: usize| yymmdd.get(at..at + 2)?.parse::<u32>().ok();
                (2000 + i64::from(number(0)?), number(2)?, number(4)?)
            }
            _ if self.option_day_in_venue_name => option_day(name)?,
            _ => return None,
        };
        Some(days_from_civil(year, month, day) * MS_PER_DAY + self.delivery_time_of_day_ms)
    }
}

/// The `DDMMMYY` of a deribit option's own name, `BASE[_QUOTE]-DDMMMYY-STRIKE-C|P` ([`deribit_expiry`]'s reading); a
/// combo (`BTC-CS-29APR22-39300_39600`), a future and a perpetual are none.
fn option_day(name: &str) -> Option<(i64, u32, u32)> {
    let mut parts = name.split('-');
    let (base, expiry, strike, right) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || base.is_empty() || strike.is_empty() || !matches!(right, "C" | "P")
    {
        return None;
    }
    deribit_expiry(expiry)
}

/// **The one call**: the verdict on an order on `symbol` at `venue` at `now_ms`; `Taken` when the venue has no window or
/// the symbol names no dated contract ([`DeliveryGuardRow::expiry_of`]).
#[must_use]
pub fn delivery_verdict(
    venue: &str,
    symbol: &str,
    now_ms: i64,
    reduce_only: bool,
) -> DeliveryGuardVerdict {
    let row = delivery_guard_for(venue);
    row.expiry_of(symbol)
        .map_or(DeliveryGuardVerdict::Taken, |expiry| row.verdict(expiry, now_ms, reduce_only))
}

/// A row with a window, delivering at `delivery_time_of_day_ms` UTC.
const fn window(
    window_ms: i64,
    delivery_time_of_day_ms: i64,
    why: &'static str,
) -> DeliveryGuardRow {
    DeliveryGuardRow {
        window_ms: Some(window_ms),
        delivery_time_of_day_ms,
        option_day_in_venue_name: false,
        why,
    }
}

const fn none(why: &'static str) -> DeliveryGuardRow {
    DeliveryGuardRow {
        window_ms: None,
        delivery_time_of_day_ms: 0,
        option_day_in_venue_name: false,
        why,
    }
}

/// The row of a venue that lists no dated contract.
const NO_DATED_CONTRACT: &str =
    "the venue lists no dated contract the engine admits, so there is no expiry to guard";

/// The delivery-guard row of `venue`; an unknown venue string has no guard.
#[must_use]
pub fn delivery_guard_for(venue: &str) -> DeliveryGuardRow {
    match venue {
        "binance" => window(
            MS_PER_DAY,
            AT_0800_UTC_MS,
            "24 h, the window the bridge held as its own constant before B3: every USDS-M or COIN-M dated order is \
             judged at send, and at admission \
             `crates/bridges/binance/src/symbol_properties.rs`'s `futures_expiry_refusal` refuses only a delivered \
             contract, so a position opened before the window can still close in it",
        ),
        "bybit" => window(
            MS_PER_DAY,
            AT_0800_UTC_MS,
            "24 h, the window the bridge held as its own constant before B3: every dated order is judged at send, \
             and at admission `crates/bridges/bybit/src/symbol_properties.rs`'s `bybit_answer` refuses only a \
             delivered contract, so a position opened before the window can still close in it",
        ),
        "okx" => window(
            MS_PER_DAY,
            AT_0800_UTC_MS,
            "24 h, okx's own window and per-order rule before B3, now every venue's: every order is judged at \
             send, its stop algos and batches included (an opening order refused inside the window, a reduce-only \
             one taken until the expiry), and at admission by \
             `crates/bridges/okx/src/symbol_properties.rs`'s `SymbolPropertiesListSpecOkx`",
        ),
        "deribit" => DeliveryGuardRow {
            option_day_in_venue_name: true,
            ..window(
                MS_PER_DAY,
                AT_0800_UTC_MS,
                "the owner's ruling of 2026-11-22 (\"let it be 24h for deribit also\"), amending decision 0233: \
                 every dated future and option is judged at send and at admission \
                 (`crates/bridges/deribit/src/symbol_properties.rs`'s `SymbolPropertiesListSpecDeribit`), a combo \
                 not at all (its legs name no one day); an option keeps the venue's own name \
                 (`crates/bridges/deribit/src/names.rs`), so its day is the name's `DDMMMYY`",
            )
        },
        "ibkr" => window(
            5 * MS_PER_DAY,
            0,
            "5 days to the last trading day, the window the bridge held as its own constant before B3 (IB's own \
             close-out starts days before it): every dated order is judged at send, and at admission by \
             `crates/bridges/vike-ibkr/src/instrument/any_symbol/forex_future.rs`'s `future_facts`",
        ),
        "ig" => none(
            "`crates/bridges/ig/src/instrument.rs`'s `expiry_refusal` admits a dated epic (`DEC-26`, `30-NOV-26`) \
             with no window: a deal carries the epic's own expiry",
        ),
        "aster" | "hyperliquid" => none(
            "perpetuals and spot only: the bridge's catalog lists no dated future, so there is no expiry to guard",
        ),
        "polymarket" => none(
            "an outcome token resolves, it is not delivered: the bridge holds no dated-future expiry to guard",
        ),
        "oanda" | "fxcm" | "dukascopy" | "ctrader" | "alpaca" => none(NO_DATED_CONTRACT),
        // vike:new-venue:row "{venue}" => none(NO_DATED_CONTRACT), // TODO(new-venue: {venue}): does its bridge admit a dated contract? Then a NAMED window
        _ => none("not a roster venue"),
    }
}

#[path = "venue_delivery_guard_tests.rs"]
#[cfg(test)]
mod venue_delivery_guard_tests;
