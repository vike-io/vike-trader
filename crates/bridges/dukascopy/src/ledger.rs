//! `ledger` — the ONE place a Dukascopy conversion becomes the rate and the quote currency a facts answer carries
//! (decision 0183), so the account's currency, the mounted pair's money and the orientation each have one home and one
//! test (`ledger_tests.rs`). The gatewayhub asks it for a node's `TradeUp::SymbolProperties` and words what it refuses
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`); this module names no wire.
//!
//! The rate source is the JForex sidecar's own TICKS of the conversion pair ([`crate::account::quote_rate`], over the
//! process's price lane [`crate::feed::process_bus`]): a JPY-quoted pair on a USD account is priced by `USDJPY`, whose
//! mid is yen per dollar, so the rate is its INVERSE (about `1/150`, never `150`); a GBP-quoted one by `GBPUSD`, whose
//! mid is the rate itself (about `1.35`). The account's currency is what the sidecar's `ready` line states
//! (`IAccount.getAccountCurrency()`).
//!
//! # The ledger's currency, and the pair the engine is mounted on
//!
//! The engine's mounted pair (`mount_symbol`: the symbol the account's engine was STARTED on, which a labelled
//! account may choose, so it is the start's and not the wiring table's `EURUSD`) is booked at par unless the engine
//! is told otherwise. That is right when the pair is quoted in the account's own currency ([`Ledger::Anchored`]).
//! A sidecar that states another money (a EUR account on a `EURUSD` mount, a USD account on a `USDJPY` mount, where
//! the mounted pair is quoted in yen) is [`Ledger::Elsewhere`], and since dukascopy's mount converts
//! (`crates/vike-venues/src/venue_currency.rs`'s `venue_currency_for`: `OtherSymbolsAndMount`, decision 0183's
//! mounted-symbol amendment, the same mechanism as FXCM's) the ledger IS
//! the stated money: the engine DECLARES its mount quoted in its own money and books it through a rate like every
//! other symbol, so this lane answers the mounted pair too (`quoted_facts`: quote `USD`, the rate of EUR per dollar
//! on a EUR account) and every other pair converts into the stated money. Only a mount that is NO six-letter pair
//! (`money` is `None`: nothing says what it is quoted in) stays refused, in words naming the stated currency,
//! because the node never declares a mount whose money it cannot read off the name. Every converted answer states
//! the ledger it is expressed in (the answer's `account_currency`), so the node's door refuses it against an engine
//! anchored to another currency (an ASSUMED USD the start fell back on while the sidecar was not up).
//!
//! # When it does NOT convert
//!
//! A jar from before the field, a sidecar not attached, a currency that is no code: each leaves the facts exactly
//! what the unconverting lane answers ([`quoted_facts`]'s `plain`), so a USD-quoted pair is admitted as before and a
//! foreign-quoted one is refused by the old fence's words, EXCEPT while no sidecar is attached at all (it is
//! starting or restarting): that refusal is "not now" (a failure of the RATE), not a verdict on the symbol the
//! node remembers. Nothing here ever answers `1.0` for "I do not know".

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::symbol_properties::SymbolPropertiesError;
use vike_model::{
    AccountCurrency, AccountTier, DEFAULT_CURRENCY, QuoteRate, QuoteRateError, SymbolProperties,
};
use vike_utility::errors::CarriedRefusal;

use crate::account::quote_rate::quote_rate;
use crate::feed::QuoteBus;
use crate::instrument::any_symbol::{
    DukascopyPropertiesBasis, facts_basis, facts_from_terms, instrument_facts_quoted,
};
use crate::reconcile::VENUE;
use crate::refusals::DukascopyPropertiesRefusal;

// A node that does NOT convert a quote currency is answered in `vike_model::DEFAULT_CURRENCY` (the gatewayhub's plain
// lane): the ledger is one currency and nothing converts there (decision 0170), a demo account opened in another
// currency is UNMEASURED there, and the door admits an instrument only when it is quoted in the money of the engine's own
// symbol as well. A node that DOES convert (decision 0183) is answered from the currency the sidecar's `ready` line
// states ([`quoted_facts`]), and falls back to that plain answer when the jar states none.

/// **Why an account of `tier` is looked up for nothing**: only a `demo` account is answered (the sidecar logs into a
/// demo server only; a `live` account never runs). `None` for a `demo` account.
#[must_use]
pub fn tier_refusal(tier: AccountTier) -> Option<CarriedRefusal> {
    (tier != AccountTier::Demo).then(|| DukascopyPropertiesRefusal::TierNotDemo.carried())
}

/// How often one symbol's "its conversion pair could not be subscribed" is said ([`say_not_subscribed`]).
const NOT_SUBSCRIBED_SAID_EVERY: Duration = Duration::from_secs(3600);

/// When each symbol's "its conversion pair could not be subscribed" was last said, so a node that re-asks a held
/// symbol's rate every few seconds says it once an hour and not once a refresh. Bounded by the account's instrument
/// list (a symbol is a row of it by the time a rate is asked), so it never grows past the list's size.
#[derive(Default)]
struct SaidAt(Mutex<HashMap<String, Instant>>);

impl SaidAt {
    /// Whether `symbol` is due to be said at `now` (`every` after the last time), and, when it is, that it was.
    fn due(&self, symbol: &str, now: Instant, every: Duration) -> bool {
        let mut said = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if said.get(symbol).is_some_and(|at| now.saturating_duration_since(*at) < every) {
            return false;
        }
        said.insert(symbol.to_string(), now);
        true
    }
}

static NOT_SUBSCRIBED_SAID: LazyLock<SaidAt> = LazyLock::new(SaidAt::default);

/// **WARN, once an hour per symbol: the conversion pair of `symbol`'s quote currency could not be subscribed.** The
/// sidecar has eight places and the data lane's price streams may hold all of them, so a pair for a rate can be
/// refused; the node then gets no rate for `symbol`, and a position it HOLDS in `symbol` keeps the last rate it
/// was given only for the engine's TTL (120 s): after that no new order in it is admitted, while closing it and
/// valuing it carry on. Without this line the only trace is the refusal text the node folds into its own log.
fn say_not_subscribed(symbol: &str, quote: &str, ledger: &str, why: &str) {
    if !NOT_SUBSCRIBED_SAID.due(symbol, Instant::now(), NOT_SUBSCRIBED_SAID_EVERY) {
        return;
    }
    tracing::warn!(
        venue = "dukascopy", %symbol, quote_currency = %quote, ledger = %ledger,
        "dukascopy: the rate of `{symbol}`'s quote currency {quote} in {ledger} could not be had because the \
         conversion pair could not be subscribed ({why}). A position held in `{symbol}` keeps the last rate it \
         was given for 120 s and then has none: no new order in it is admitted until a place frees (stop a price \
         stream), while closing it and valuing it carry on. Said once an hour per symbol"
    );
}

/// What the sidecar's statement and the engine's mount say about the money the ledger counts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ledger {
    /// The sidecar states no currency: `attached` says whether a sidecar is up at all (a jar from before the field
    /// is attached and states none; a sidecar that is starting or restarting is not attached).
    Unstated {
        /// Whether a sidecar is up at all.
        attached: bool,
    },
    /// The mounted pair is quoted in this currency, which the sidecar states ([`AccountCurrency::Stated`]) or the
    /// owner's USD default fills in for a jar that states none ([`AccountCurrency::Assumed`], [`ledger_assuming`]):
    /// the ledger is anchored to it.
    Anchored(AccountCurrency),
    /// The sidecar states `stated`, but the mount does not count its money in it (`mounted` is the start's
    /// symbol, as the caller spelled it; `money` is its quote currency, `None` when it is no six-letter pair). With a
    /// `money` the ledger is still `stated` (the node declares the mount quoted in `money` and rates it); without
    /// one the lane refuses every symbol beyond the mount ([`DukascopyPropertiesRefusal::MountNotAPair`]).
    Elsewhere {
        /// The currency the sidecar states.
        stated: String,
        /// The start's symbol, as the caller spelled it.
        mounted: String,
        /// The mount's quote currency; `None` when it is no six-letter pair.
        money: Option<String>,
    },
}

/// What the ledger of the account mounted on `mount_symbol` counts in: see [`Ledger`].
#[must_use]
pub(crate) fn ledger_of(bus: &QuoteBus, mount_symbol: &str) -> Ledger {
    let Some(stated) = bus.account_currency() else {
        return Ledger::Unstated { attached: bus.sidecar_attached() };
    };
    ledger_for(stated, mount_symbol)
}

/// **The money the engine mounted on `mount_symbol` counts in**: the quote of its KEPT row when that is a fiat code,
/// else the last three letters of a six-letter name, else none. The node's own reading of a mount's money
/// (`vike-tradehub-core`'s `engine/assemble.rs`'s `mount_money`), so the lane and the node never read two moneys for one
/// mount; the wallet frame reads it too (`crate::account::ready_balance`).
pub(crate) fn mount_money(mount_symbol: &str) -> Option<String> {
    if let Some(quote) = vike_catalog::kept_row_of(VENUE, mount_symbol)
        .and_then(|row| vike_model::fiat_code(&row.quote))
    {
        return Some(quote.to_string());
    }
    let name = mount_symbol.trim().to_ascii_uppercase();
    (name.len() == 6 && name.bytes().all(|b| b.is_ascii_uppercase())).then(|| name[3..].to_string())
}

/// What the ledger of the account mounted on `mount_symbol` counts in, given the money `stated` for the account (by
/// the sidecar, or ASSUMED: [`ledger_assuming`]).
fn ledger_for(stated: String, mount_symbol: &str) -> Ledger {
    let Some(money) = mount_money(mount_symbol) else {
        return Ledger::Elsewhere { stated, mounted: mount_symbol.to_string(), money: None };
    };
    if money == stated {
        Ledger::Anchored(AccountCurrency::Stated(stated))
    } else {
        let mounted = mount_symbol.trim().to_ascii_uppercase();
        Ledger::Elsewhere { stated, mounted, money: Some(money) }
    }
}

/// [`ledger_of`] with the owner's USD default (decision 0183, amended 2026-10-19). A jar from before the field that is
/// ATTACHED and states no currency is read as the ruling's USD ([`AccountCurrency::Assumed`]) when the mounted pair is
/// quoted in it, so the account lists, converts and anchors like any other; where the mount is quoted in another money
/// the assumption cannot anchor it and the answer stays [`Ledger::Unstated`]. A sidecar that is not attached
/// (starting, restarting, not armed) is "not now" and is never assumed over, and a currency the sidecar STATES is
/// passed through untouched.
#[must_use]
pub(crate) fn ledger_assuming(bus: &QuoteBus, mount_symbol: &str) -> Ledger {
    match ledger_of(bus, mount_symbol) {
        Ledger::Unstated { attached: true } => {
            match ledger_for(DEFAULT_CURRENCY.to_string(), mount_symbol) {
                Ledger::Anchored(_) => Ledger::Anchored(AccountCurrency::Assumed {
                    why: "dukascopy: the sidecar states no currency for the account (its jar predates the field)"
                        .to_string(),
                }),
                _ => Ledger::Unstated { attached: true },
            }
        }
        other => other,
    }
}

/// **The account's currency for a start's reply** (the gatewayhub's `assumed_currency`): the currency the sidecar STATES,
/// whether or not the mounted pair is quoted in it (a EUR account on the `EURUSD` mount is EUR: the node's mount fence
/// declines an anchor the mount cannot carry, in its own words, exactly as it does for oanda, ctrader and ig stating
/// EUR, and [`quoted_facts`] still refuses every other symbol for it), the assumed USD for an attached jar that predates
/// the field and whose mounted pair is quoted in it (the owner's default, its why in words), and `None` when the sidecar
/// is not attached or the assumption could not anchor the mount.
#[must_use]
pub fn currency_read(bus: &QuoteBus, mount_symbol: &str) -> Option<AccountCurrency> {
    match ledger_assuming(bus, mount_symbol) {
        Ledger::Anchored(currency) => Some(currency),
        Ledger::Elsewhere { stated, .. } => Some(AccountCurrency::Stated(stated)),
        Ledger::Unstated { .. } => None,
    }
}

/// The grid and quote currency of `symbol`: a currency pair's from its row of the account's instrument list, a metal's
/// or a CFD's from the VENUE's own terms of it, which the sidecar states at the instrument's first tick
/// (`QuoteBus::venue_terms`: it subscribes the instrument and waits). A refusal is the instrument's own sentence; the
/// list or the terms not being readable NOW (no list stated yet, no sidecar, a jar without them, a closed market, no
/// place to subscribe) is a failure of the lookup ([`SymbolPropertiesError::ListUnavailable`]) so the node does not
/// remember it as a verdict on the symbol.
fn grid_and_quote(
    bus: &QuoteBus,
    wait: Duration,
    symbol: &str,
) -> Result<(SymbolProperties, String), SymbolPropertiesError> {
    let rows =
        bus.instrument_list().map_err(|why| SymbolPropertiesError::ListUnavailable(why.into()))?;
    match facts_basis(&rows, symbol).map_err(refused)? {
        DukascopyPropertiesBasis::Static => instrument_facts_quoted(&rows, symbol).map_err(refused),
        DukascopyPropertiesBasis::VenueTerms(row) => {
            let terms = bus.venue_terms(&row.symbol, wait).map_err(|why| {
                let unread = DukascopyPropertiesRefusal::TermsUnread {
                    pair: row.pair.clone(),
                    why: why.to_string(),
                };
                SymbolPropertiesError::ListUnavailable(unread.carried())
            })?;
            facts_from_terms(&row, &terms).map_err(refused)
        }
    }
}

/// A verdict on the symbol, in the shared seam's words.
fn refused(refusal: DukascopyPropertiesRefusal) -> SymbolPropertiesError {
    SymbolPropertiesError::Refused(refusal.carried())
}

/// **The facts of `symbol` for a node that CONVERTS a quote currency** (the gatewayhub's `symbol_facts_quoted`):
///
/// * an instrument quoted in the ledger's currency is answered with the grid and the ledger, no rate;
/// * one quoted in ANOTHER currency is answered with the grid, the rate of the conversion pair's mid from the
///   sidecar's own ticks (stamped with the tick's own time, its quote currency inside), and the ledger;
/// * a metal or a CFD is answered on the venue's own terms of it ([`grid_and_quote`]), quoted in the currency those
///   terms state and converted like any other;
/// * every other refusal of the unconverting lane (a coin, a CFD in pence, a name the list lacks) stands, in the same
///   words; and no rate is no answer: the instrument is not admitted.
///
/// `answer` writes a converted answer (the grid, the rate when one is needed, the ledger it is expressed in), so the
/// caller's own answer type carries it. When the sidecar states no currency the answer is `plain()`'s, unchanged (the
/// module doc; its refusal is passed through as it came, kind and words), except that a
/// refusal of the currency fence while NO sidecar is attached is "not now". When it states one that is not the
/// mount's money ([`Ledger::Elsewhere`]) the ledger is the stated money all the same and the mounted pair is
/// answered like any other symbol, with its quote currency and the rate into the stated money (the node books
/// the mount through it); only a mount that is no pair refuses every symbol, in words naming it. `mount_symbol` is
/// the symbol the engine was started on; `symbol` is the instrument's WIRE name, the caller's translation of the node's
/// user name through its kept row (`crate::instrument::names::facts_wire_symbol`); `wait` is how long a conversion pair
/// that is not priced yet may take to send its first tick (production: [`crate::feed::RATE_WAIT`]).
///
/// # Errors
/// The operator's sentence. A rate that could not be had NOW is [`SymbolPropertiesError::RateUnavailable`] so the
/// node does not remember it as a verdict on the symbol; a pair no single conversion pair prices is a verdict on the
/// symbol.
pub fn quoted_facts<T>(
    bus: &QuoteBus,
    wait: Duration,
    mount_symbol: &str,
    symbol: &str,
    plain: impl FnOnce() -> Result<T, SymbolPropertiesError>,
    answer: impl FnOnce(SymbolProperties, Option<QuoteRate>, String) -> T,
) -> Result<T, SymbolPropertiesError> {
    let ledger = match ledger_assuming(bus, mount_symbol) {
        Ledger::Anchored(currency) => currency.code().to_string(),
        Ledger::Unstated { attached } => {
            tracing::debug!(
                venue = "dukascopy", %symbol, attached,
                "the sidecar states no currency for the account; answering as the unconverting lane does"
            );
            let answer = plain();
            // The fence's refusal of a pair quoted in another currency is the unconverting lane's word for "the
            // ledger is USD"; while the sidecar is not even attached (it is starting, or restarting) the ledger is
            // simply not known YET, which the node must not remember as a verdict on the symbol.
            if !attached
                && answer.is_err()
                && bus.instrument_list().is_ok_and(|rows| {
                    instrument_facts_quoted(&rows, symbol).is_ok()
                        || matches!(
                            facts_basis(&rows, symbol),
                            Ok(DukascopyPropertiesBasis::VenueTerms(_))
                        )
                })
            {
                let not_yet =
                    DukascopyPropertiesRefusal::LedgerNotKnownYet { symbol: symbol.to_string() };
                return Err(SymbolPropertiesError::RateUnavailable(not_yet.carried()));
            }
            return answer;
        }
        // The mount is quoted in another money than the stated one: the ledger is the stated money and the node
        // books the mount through the rate this lane answers for it (decision 0183, the mounted-symbol amendment).
        Ledger::Elsewhere { stated, money: Some(_), .. } => stated,
        Ledger::Elsewhere { stated, mounted, money: None } => {
            // Not an instrument the list admits is still the list's refusal: the instrument's own words come first.
            let rows = bus
                .instrument_list()
                .map_err(|why| SymbolPropertiesError::ListUnavailable(why.into()))?;
            facts_basis(&rows, symbol).map_err(refused)?;
            return Err(refused(DukascopyPropertiesRefusal::MountNotAPair {
                symbol: symbol.to_string(),
                stated,
                mounted,
            }));
        }
    };
    let (properties, quote) = grid_and_quote(bus, wait, symbol)?;
    if quote == ledger {
        return Ok(answer(properties, None, ledger));
    }
    let rate = quote_rate(bus, &quote, &ledger, wait).map_err(|no| match no.error {
        QuoteRateError::NoPair(why) => SymbolPropertiesError::Refused(why.into()),
        QuoteRateError::Unavailable(why) | QuoteRateError::BadQuote(why) => {
            if no.pair_not_subscribed {
                say_not_subscribed(symbol, &quote, &ledger, &why);
            }
            let no_rate = DukascopyPropertiesRefusal::NoRate {
                quote: quote.clone(),
                ledger: ledger.clone(),
                why,
            };
            SymbolPropertiesError::RateUnavailable(no_rate.carried())
        }
    })?;
    Ok(answer(properties, Some(rate), ledger))
}

#[path = "ledger_tests.rs"]
#[cfg(test)]
mod ledger_tests;
