//! **IBKR's symbol facts, as the gatewayhub's per-key bounds ask them** (`docs/ops/any-symbol-remaining-venues-plan.md`,
//! "ibkr"): the venue's half of `SymbolPropertiesCacheKeyed<S>` ([`SymbolPropertiesKeyedSpecIbkr`]) and the ONE place a converting
//! answer is assembled from its two reads of different ages ([`quoted_facts`], [`quoted_class_facts`]).
//!
//! IBKR has no instrument list to keep: a name is resolved by asking the account's logged-in gateway
//! ([`crate::instrument::any_symbol::socket::IbkrPropertiesSource`]: a symbol search, the contract details and the market rules on one data
//! connection). So the gatewayhub bounds the lookups per key (a keep time per name, a single flight, [`FETCH_BUDGET`]
//! lookups per [`FETCH_WINDOW`], [`MAX_NAMES`] names tracked), for one account. Under the budget the gateway's gate allows
//! six data connects an hour per gateway and the lookup lane may spend four of them, not counting up to a ceiling of
//! lookups the gateway answered at once, which is the harder floor
//! (`crates/bridges/vike-ibkr/src/transport/gateway_gate.rs`'s `MAX_REFUNDS_PER_HOUR`). The fences (a US stock by name,
//! the account's currency, one conId, the grid) are [`crate::instrument::any_symbol`]'s.
//!
//! # A converting answer
//!
//! The rate is **ledger-currency units per ONE unit of the stock's QUOTE currency**, the engine's orientation
//! (`SymbolAdmission::quote_rate`): a euro is worth more than a dollar (`1.10`), a yen a fraction of one (`1/150`, never
//! `150`). `vike_model::AccountPerQuote` carries that direction in its type and this module only reads it out. An answer
//! is assembled from two reads of different ages, kept apart on purpose:
//!
//! * the stock's GRID (tick, lot, minimums), kept by the gatewayhub's per-key bounds for ten minutes like every IBKR
//!   answer;
//! * the RATE, asked of the gateway source on EVERY ask and never kept here (the node reuses it for its own 30 s and
//!   re-reads it for a held position about once a minute): a rate served from the facts' keep time would be ten minutes
//!   old on arrival, and the engine calls a rate stale at 120 s. The source reuses one forex read per CURRENCY for
//!   [`crate::instrument::any_symbol::socket::RATE_KEEP`] (30 s), so the ten symbols a refresh pass walks in one currency cost one
//!   read, not ten.
//!
//! The grid and the rate of one ask ride ONE gateway connection ([`crate::instrument::any_symbol::socket::FactsScope`]): the closures
//! handed to [`quoted_facts`] share it, so a new foreign stock costs the data lane one dial, not two.
//!
//! **Fail closed**: a name that is no stock name, a grid that could not be had, a base currency the account summary does
//! not state, a currency with no forex pair, a minor-unit currency, a FOREIGN stock whose contract price magnifier is not 1
//! and a quote that is not a rate (delayed, one-sided, crossed, wide) are each a refusal in words. A rate the gateway could
//! not give is a failure of the RATE ([`SymbolPropertiesError::RateUnavailable`]), so the node does not remember it as a
//! verdict on the symbol. There is no path that answers `1.0` for "I do not know": the only answer without a rate is a
//! stock quoted in the account's own base currency, which needs no conversion.

use std::time::{Duration, Instant};

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesAnswer, SymbolPropertiesError, SymbolPropertiesKeyedError,
    SymbolPropertiesKeyedSpec,
};
use vike_model::{DEFAULT_CURRENCY, same_currency};

use crate::account::quote_rate::minor_unit_refusal;
use crate::instrument::any_symbol::socket::{IbkrPropertiesClass, RateAnswer};
use crate::instrument::any_symbol::{
    IbkrPropertiesError, IbkrPropertiesStock, parse_stock_name, price_magnifier_refusal,
};
use crate::refusals::IbkrSymbolRefusal;

/// Lookups allowed per [`FETCH_WINDOW`], per account.
pub const FETCH_BUDGET: usize = 6;

/// See [`FETCH_BUDGET`].
pub const FETCH_WINDOW: Duration = Duration::from_secs(60);

/// How many names are tracked at once.
pub const MAX_NAMES: usize = 64;

/// How long to wait between two asks inside the caller's bound on the whole read ([`read_base_currency`]'s `total`: the
/// gatewayhub's one start-path wait, `crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `CURRENCY_READ_WAIT`).
/// The data lane of the gateway's gate refuses a dial at once
/// while another dial holds its turn (another account of the same gateway starting) or the previous one ended less
/// than the connect gap ago, so a first refusal is the ordinary case and not an outage.
pub const BASE_PAUSE: Duration = Duration::from_secs(3);

/// Why a forex pair or a dated future `name` counting its money in `money` is not admitted for a node that converts
/// nothing, or `None`. That node's ledger is `vike_model::DEFAULT_CURRENCY`: the ledger is one currency and nothing
/// converts (decision 0170), the balance seed reads the CASH of the mounted contract's currency, and the account's base
/// currency is NOT read on this path (its `$LEDGER` vocabulary is unobserved), so it is the owner's default, not a
/// lookup. A node that converts (demo with an exposure ceiling) reads the base currency of the account summary instead.
#[must_use]
pub fn ledger_refusal(name: &str, money: &str) -> Option<IbkrSymbolRefusal> {
    (!same_currency(money, DEFAULT_CURRENCY)).then(|| IbkrSymbolRefusal::LedgerCurrency {
        name: name.to_string(),
        money: money.to_string(),
    })
}

/// **IBKR's half of the per-key bounds.** The first kind of ask is a stock name; the second a forex pair or a dated
/// future (`crate::instrument::any_symbol::forex_future`, decision 0234) under the same bounds. A stock's answer is the grid alone
/// (a share: `Linear`, 1.0, the contract value stays absent), and the contract's price magnifier is kept beside it as
/// the note, NOT judged here: this is the plain path's answer too, and it admits what it always did ([`quoted_facts`]
/// judges it). A forex pair is linear in units of its base and states no margin rate; a future is answered with its
/// contract value (the multiplier) and the margin rate the venue's what-if orders state. The facts are for the name
/// that was asked: the lookup refuses any other spelling of a future with the canonical one.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesKeyedSpecIbkr;

/// A refusal is a verdict; an unreachable gateway is "not now".
fn keyed(error: IbkrPropertiesError) -> SymbolPropertiesKeyedError {
    match error {
        IbkrPropertiesError::Refused(why) => SymbolPropertiesKeyedError::Refused(why.carried()),
        IbkrPropertiesError::Unavailable(why) => {
            SymbolPropertiesKeyedError::Unavailable(why.carried())
        }
    }
}

impl SymbolPropertiesKeyedSpec for SymbolPropertiesKeyedSpecIbkr {
    const VENUE: &'static str = "ibkr";
    const KEYS: &'static str = "names";
    const LOOKUPS: &'static str = "lookups";
    const MAX_KEYS: usize = MAX_NAMES;
    const BUDGET: usize = FETCH_BUDGET;
    const WINDOW: Duration = FETCH_WINDOW;
    const REFRESH_RESERVE: usize = 0;

    type Fetched = Result<IbkrPropertiesStock, IbkrPropertiesError>;
    type FetchedOther = Result<IbkrPropertiesClass, IbkrPropertiesError>;
    /// The contract's `priceMagnifier`.
    type Note = i32;

    fn answer<A: SymbolPropertiesAnswer>(
        fetched: Self::Fetched,
    ) -> Result<(A, Option<i32>), SymbolPropertiesKeyedError> {
        let facts = fetched.map_err(keyed)?;
        Ok((A::of(facts.properties, None, None, None, None), Some(facts.price_magnifier)))
    }

    fn answer_other<A: SymbolPropertiesAnswer>(
        fetched: Self::FetchedOther,
    ) -> Result<A, SymbolPropertiesKeyedError> {
        Ok(match fetched.map_err(keyed)? {
            IbkrPropertiesClass::Forex(f) => A::of(f.properties, None, None, None, None),
            IbkrPropertiesClass::Future(f) => {
                A::of(f.properties, Some(f.contract_value), f.margin_rate, None, None)
            }
        })
    }

    /// A stock name and a forex or futures name never share a spelling, so the two kinds share one key space.
    fn other_key(name: &str) -> String {
        name.to_string()
    }
}

/// The rate's refusal: a verdict as it stands, a gateway that could not give it as a failure of the rate. Its words
/// name the account's currency when `base` knows it ("no rate for JPY in EUR", as the other converting venues word
/// it); `base` is asked only now because the failed read may have stated it, and a read that failed before it did
/// leaves the currency alone.
fn rate_refusal(
    currency: &str,
    base: impl FnOnce() -> Option<String>,
    why: IbkrPropertiesError,
) -> SymbolPropertiesError {
    match why {
        IbkrPropertiesError::Refused(why) => SymbolPropertiesError::Refused(why.carried()),
        IbkrPropertiesError::Unavailable(why) => {
            let (currency, why) = (currency.to_string(), why.to_string());
            let refusal = match base() {
                Some(base) => IbkrSymbolRefusal::NoRateIn { currency, base, why },
                None => IbkrSymbolRefusal::NoRate { currency, why },
            };
            SymbolPropertiesError::RateUnavailable(refusal.carried())
        }
    }
}

/// The facts of the stock `name` (`TICKER.SMART.CCY`, upper case) for an account that converts a quote currency: its grid
/// from the keep (`grid`: the gatewayhub's kept lookup, its refusal `E` passed back as it came), the price magnifier
/// noted beside it (`magnifier`, asked AFTER `grid`, so a kept grid's note is no older than the keep), and its rate read
/// just now by `rate_for` (given the name's currency). `base` is the account's currency as the source knows it, asked
/// only when the rate could not be had (`IbkrPropertiesSource::remembered_base`).
///
/// # Errors
/// `grid`'s refusal as it came; every verdict as [`SymbolPropertiesError::Refused`] and a rate the gateway could not
/// give as [`SymbolPropertiesError::RateUnavailable`], into `E`.
pub fn quoted_facts<A: SymbolPropertiesAnswer, E: From<SymbolPropertiesError>>(
    name: &str,
    grid: impl FnOnce() -> Result<A, E>,
    magnifier: impl FnOnce() -> Option<i32>,
    rate_for: impl FnOnce(&str) -> Result<RateAnswer, IbkrPropertiesError>,
    base: impl FnOnce() -> Option<String>,
) -> Result<A, E> {
    // Before the budget is spent and before any dial: a name that is not a stock name is refused in the
    // words the plain path uses.
    let currency = parse_stock_name(name)
        .map_err(|why| SymbolPropertiesError::Refused(why.carried()))?
        .currency;
    // A name in a currency this adapter refuses by name (pence, agorot, cents: `minor_unit_refusal`) is refused
    // BEFORE its grid is fetched, in the words the rate read gives: the grid costs a data dial of a scarce hourly
    // budget, and it was spent on every such name every ten minutes for an answer that was always the same.
    if let Some(why) = minor_unit_refusal(&currency) {
        return Err(SymbolPropertiesError::Refused(why.carried()).into());
    }
    let wire = grid()?;
    let answer = rate_for(&currency).map_err(|why| rate_refusal(&currency, base, why))?;
    let Some((rate, _pair)) = answer.rate else { return Ok(wire) };
    // A FOREIGN stock (a rate exists exactly when its currency is not the base): the unit its prices are
    // reported in must be known. The contract's `priceMagnifier` is that statement, `1` for whole units; a `0`
    // the gateway left unset is not one. A stock in the base currency (no rate) and the plain path never get
    // here, so an account that converts nothing admits what it always did. A magnifier that was not recorded
    // (it always is, by the keyed answer) refuses too: no price in an unknown unit is booked.
    let refusal = match magnifier() {
        Some(magnifier) => price_magnifier_refusal(name, &currency, magnifier),
        None => Some(IbkrSymbolRefusal::PriceMagnifierNotRead { name: name.to_string() }),
    };
    if let Some(why) = refusal {
        return Err(SymbolPropertiesError::Refused(why.carried()).into());
    }
    // The ledger currency the rate is expressed in, as THIS read found it: the node refuses a rate that is
    // not in the currency its engine was anchored to.
    Ok(wire.with_rate(rate, answer.base))
}

/// The facts of the forex pair or future `name` (upper case) for an account that converts a quote currency: its grid
/// from the keep (`class`, its refusal passed back as it came), and, when the money it counts in (`currency`: the
/// pair's quote, a future's settlement currency) is not the account's base, the rate read just now by `rate_for`
/// (decision 0183, as [`quoted_facts`] does for a stock, `base` included).
///
/// ⚠ A pair's rate is the IDEALPRO quote of a pair, and the paper account streams none through the API (`[10089]`,
/// 2026-10-30), so on it a pair or a future in another currency than the base is refused as "the rate could not be
/// fetched" while one in the base's own currency (`EUR.USD` on a USD account) needs no rate.
///
/// # Errors
/// As [`quoted_facts`]'.
pub fn quoted_class_facts<A: SymbolPropertiesAnswer, E: From<SymbolPropertiesError>>(
    currency: &str,
    class: impl FnOnce() -> Result<A, E>,
    rate_for: impl FnOnce(&str) -> Result<RateAnswer, IbkrPropertiesError>,
    base: impl FnOnce() -> Option<String>,
) -> Result<A, E> {
    if let Some(why) = minor_unit_refusal(currency) {
        return Err(SymbolPropertiesError::Refused(why.carried()).into());
    }
    let wire = class()?;
    let answer = rate_for(currency).map_err(|why| rate_refusal(currency, base, why))?;
    Ok(match answer.rate {
        Some((rate, _pair)) => wire.with_rate(rate, answer.base),
        None => wire,
    })
}

/// The account's base currency, asked until `total` is spent with `pause` between asks: `Some` the moment the venue
/// states it, `None` when it does not within `total` (the caller then falls back to the currency this process read
/// before, if any; else the account is left unanchored and a stock of another currency stays refused). `total` is the
/// WHOLE of what an adapter's start spends here, dial and retries together: the start holds the account's adapter lock
/// while it waits and the node waits for its reply, so a longer read would turn a slow gateway into a failed start. `read` is
/// handed the time left, so no ask outlives the bound. A refusal (a cpapi account, a summary that names no single
/// currency) is final and is not retried; an `Unavailable` answer is said once, at the end.
pub fn read_base_currency(
    mut read: impl FnMut(Duration) -> Result<String, IbkrPropertiesError>,
    total: Duration,
    pause: Duration,
) -> Option<String> {
    let end = Instant::now() + total;
    let mut tries = 0_u32;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        tries += 1;
        match read(left) {
            Ok(base) => return Some(base),
            Err(IbkrPropertiesError::Refused(why)) => {
                tracing::debug!("ibkr: the account's base currency is not read: {why}");
                return None;
            }
            Err(IbkrPropertiesError::Unavailable(why)) => {
                // Another ask only fits when the pause and something after it do.
                if end.saturating_duration_since(Instant::now()) <= pause {
                    tracing::warn!(
                        "ibkr: the account's base currency could not be read in {} s ({tries} tries: \
                         {why})",
                        total.as_secs()
                    );
                    return None;
                }
                std::thread::sleep(pause);
            }
        }
    }
}
