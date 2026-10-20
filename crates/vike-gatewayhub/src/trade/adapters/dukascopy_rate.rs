//! `dukascopy_rate` — the ONE place a Dukascopy conversion becomes the rate and the quote currency a facts
//! answer carries (decision 0183), so the account's currency, the mounted pair's money and the orientation each
//! have one home and one test (`dukascopy_rate_tests.rs`).
//!
//! The rate source is the JForex sidecar's own TICKS of the conversion pair (`vike_dukascopy::home_conversion`,
//! over the process's price lane `vike_dukascopy::quotes::process_bus`): a JPY-quoted pair on a USD account is
//! priced by `USDJPY`, whose mid is yen per dollar, so the rate is its INVERSE (about `1/150`, never `150`); a
//! GBP-quoted one by `GBPUSD`, whose mid is the rate itself (about `1.35`). The account's currency is what the
//! sidecar's `ready` line states (`IAccount.getAccountCurrency()`).
//!
//! # The ledger's currency, and the pair the engine is mounted on
//!
//! The engine's mounted pair (`mount_symbol`: the symbol the account's engine was STARTED on, which a labelled
//! account may choose, so it is the start's and not the wiring table's `EURUSD`) is booked at par: the engine knows
//! no rate for the symbol it was started on. That is right only when the pair is quoted in the account's own
//! currency. A sidecar that states another money (a EUR account on a `EURUSD` mount, a USD account on a `USDJPY`
//! mount, where the mounted pair is booked in yen) is **no ledger currency at all** ([`ledger_of`]'s
//! [`Ledger::Elsewhere`]): nothing is converted AND no symbol beyond the mounted one is admitted, in words naming the
//! stated currency ([`quoted_facts`]), because a symbol booked at par or converted into a ledger whose mounted pair
//! is in another money would be two inconsistent treatments of one book.
//!
//! # When it does NOT convert
//!
//! A jar from before the field, a sidecar not attached, a currency that is no code: each leaves the facts exactly
//! what the unconverting lane answers ([`quoted_facts`]'s `plain`), so a USD-quoted pair is admitted as before and a
//! foreign-quoted one is refused by the old fence's words, EXCEPT while no sidecar is attached at all (it is
//! starting or restarting): that refusal is "not now" ([`FACTS_RATE_UNAVAILABLE`]), not a verdict on the symbol the
//! node remembers. Nothing here ever answers `1.0` for "I do not know".

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_dukascopy::home_conversion::{RateError, quote_rate};
use vike_dukascopy::quotes::{QuoteBus, is_not_subscribed};
use vike_gatewayhub_client::proto::{FACTS_RATE_UNAVAILABLE, WireSymbolFacts};

use super::assumed_ccy::{ASSUMED_CCY, CcyRead};

/// How often one symbol's "its conversion pair could not be subscribed" is said ([`say_not_subscribed`]).
const NOT_SUBSCRIBED_SAID_EVERY: Duration = Duration::from_secs(3600);

/// When each symbol's "its conversion pair could not be subscribed" was last said, so a node that re-asks a held
/// symbol's rate every few seconds says it once an hour and not once a refresh. Bounded by the SDK table (a
/// symbol is a row of it by the time a rate is asked), so it never grows past a couple of hundred entries.
#[derive(Default)]
pub(crate) struct SaidAt(Mutex<HashMap<String, Instant>>);

impl SaidAt {
    /// Whether `symbol` is due to be said at `now` (`every` after the last time), and, when it is, that it was.
    pub(crate) fn due(&self, symbol: &str, now: Instant, every: Duration) -> bool {
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
        venue = "dukascopy", %symbol, quote_ccy = %quote, ledger = %ledger,
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
    Unstated { attached: bool },
    /// The sidecar states this currency and the mounted pair is quoted in it: the ledger is anchored to it.
    Anchored(String),
    /// The sidecar states `stated`, but the mount does not count its money in it (`mounted` is the start's
    /// symbol, as the caller spelled it; `money` is its quote currency, `None` when it is no six-letter pair).
    Elsewhere { stated: String, mounted: String, money: Option<String> },
}

/// What the ledger of the account mounted on `mount_symbol` counts in: see [`Ledger`].
pub(crate) fn ledger_of(bus: &QuoteBus, mount_symbol: &str) -> Ledger {
    let Some(stated) = bus.account_currency() else {
        return Ledger::Unstated { attached: bus.sidecar_attached() };
    };
    ledger_for(stated, mount_symbol)
}

/// What the ledger of the account mounted on `mount_symbol` counts in, given the money `stated` for the account (by
/// the sidecar, or ASSUMED: [`ledger_assuming`]).
fn ledger_for(stated: String, mount_symbol: &str) -> Ledger {
    let mounted = mount_symbol.trim().to_ascii_uppercase();
    if mounted.len() != 6 || !mounted.bytes().all(|b| b.is_ascii_uppercase()) {
        return Ledger::Elsewhere { stated, mounted: mount_symbol.to_string(), money: None };
    }
    let money = mounted[3..].to_string();
    if money == stated {
        Ledger::Anchored(stated)
    } else {
        Ledger::Elsewhere { stated, mounted, money: Some(money) }
    }
}

/// [`ledger_of`] with the owner's USD default (decision 0183, amended 2026-10-19): `(ledger, assumed)`. A jar from
/// before the field that is ATTACHED and states no currency is read as the ruling's USD (`assumed == true`) when
/// the mounted pair is quoted in it, so the account lists, converts and anchors like any other; where the mount is
/// quoted in another money the assumption cannot anchor it and the answer stays [`Ledger::Unstated`]. A sidecar
/// that is not attached (starting, restarting, not armed) is "not now" and is never assumed over, and a currency the
/// sidecar STATES is passed through untouched.
pub(crate) fn ledger_assuming(bus: &QuoteBus, mount_symbol: &str) -> (Ledger, bool) {
    match ledger_of(bus, mount_symbol) {
        Ledger::Unstated { attached: true } => {
            match ledger_for(ASSUMED_CCY.to_string(), mount_symbol) {
                anchored @ Ledger::Anchored(_) => (anchored, true),
                _ => (Ledger::Unstated { attached: true }, false),
            }
        }
        other => (other, false),
    }
}

/// **The account's currency for a start's reply** (`assumed_ccy`): the currency the sidecar STATES, whether or not
/// the mounted pair is quoted in it (a EUR account on the `EURUSD` mount is EUR: the node's mount fence declines an
/// anchor the mount cannot carry, in its own words, exactly as it does for oanda, ctrader and ig stating EUR, and
/// this lane's [`quoted_facts`] still refuses every other symbol for it), silence from an attached jar that predates
/// the field and whose mounted pair is quoted in the assumed USD (the owner's default), and nothing to say
/// (`Declined`) when the sidecar is not attached or the assumption could not anchor the mount.
pub(crate) fn ccy_read(bus: &QuoteBus, mount_symbol: &str) -> CcyRead {
    match ledger_assuming(bus, mount_symbol) {
        (Ledger::Anchored(ccy), false) | (Ledger::Elsewhere { stated: ccy, .. }, _) => {
            CcyRead::Stated(ccy)
        }
        (Ledger::Anchored(_), true) => CcyRead::Silent(
            "dukascopy: the sidecar states no currency for the account (its jar predates the field)"
                .to_string(),
        ),
        (Ledger::Unstated { .. }, _) => CcyRead::Declined,
    }
}

/// The refusal of any symbol beyond the mounted one on an account whose stated money is not its mount's
/// ([`Ledger::Elsewhere`]): words naming the stated currency and the mount. A verdict on the account until it is
/// mounted again, so it is not worded as a failure of the rate.
fn elsewhere_refusal(symbol: &str, stated: &str, mounted: &str, money: Option<&str>) -> String {
    let booked = money.map_or_else(
        || "which is not a pair, so the money it is booked in is not known".to_string(),
        |money| format!("quoted in {money}, which the ledger books at par"),
    );
    format!(
        "dukascopy: this account's money is {stated} but the engine is mounted on {mounted}, {booked}; its \
         numbers are not {stated}, so `{symbol}` and every other symbol beyond the mounted one are NOT admitted \
         for it. Use an account mounted on a pair quoted in {stated}"
    )
}

/// **The facts of `symbol` for a node that CONVERTS a quote currency** (`TradeLane::symbol_facts_quoted`):
///
/// * an instrument quoted in the ledger's currency is answered exactly as `plain` would (the grid, no rate);
/// * one quoted in ANOTHER currency is answered with the grid, its quote currency, and the rate of the
///   conversion pair's mid from the sidecar's own ticks, stamped with the tick's own time;
/// * every refusal of the unconverting lane (a CFD, a coin, a metal, a name the table lacks) stands, in the same
///   words; and no rate is no answer: the instrument is not admitted.
///
/// When the sidecar states no currency the answer is `plain()`'s, unchanged (the module doc), except that a
/// refusal of the currency fence while NO sidecar is attached is "not now". When it states one that is not the
/// mount's money ([`Ledger::Elsewhere`]) every symbol is refused in words naming it. `mount_symbol` is the
/// symbol the engine was started on; `wait` is how long a conversion pair that is not priced yet may take to
/// send its first tick (production: `vike_dukascopy::quotes::RATE_WAIT`).
///
/// # Errors
/// The operator's sentence. A rate that could not be had NOW is worded as a failure of the rate
/// ([`FACTS_RATE_UNAVAILABLE`]) so the node does not remember it as a verdict on the symbol; a pair no single
/// conversion pair prices is a verdict on the symbol.
pub(crate) fn quoted_facts(
    bus: &QuoteBus,
    wait: Duration,
    mount_symbol: &str,
    symbol: &str,
    plain: impl FnOnce() -> Result<WireSymbolFacts, String>,
) -> Result<WireSymbolFacts, String> {
    let ledger = match ledger_assuming(bus, mount_symbol).0 {
        Ledger::Anchored(ccy) => ccy,
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
                && vike_dukascopy::any_symbol::instrument_facts_quoted(symbol).is_ok()
            {
                return Err(format!(
                    "dukascopy: {FACTS_RATE_UNAVAILABLE}: the JForex sidecar is not up (it is starting or \
                     restarting, or this account is not armed), so the ledger's currency is not known yet \
                     and `{symbol}` cannot be placed against it"
                ));
            }
            return answer;
        }
        Ledger::Elsewhere { stated, mounted, money } => {
            // Not a pair the table knows is still the table's refusal: the instrument's own words come first.
            vike_dukascopy::any_symbol::instrument_facts_quoted(symbol)?;
            return Err(elsewhere_refusal(symbol, &stated, &mounted, money.as_deref()));
        }
    };
    let (properties, quote) = vike_dukascopy::any_symbol::instrument_facts_quoted(symbol)?;
    if quote == ledger {
        return Ok(WireSymbolFacts { properties, ..WireSymbolFacts::default() });
    }
    let rate = quote_rate(bus, &quote, &ledger, wait).map_err(|e| match e {
        RateError::NoPair(why) => why,
        RateError::Unavailable(why) => {
            if is_not_subscribed(&why) {
                say_not_subscribed(symbol, &quote, &ledger, &why);
            }
            format!("dukascopy: {FACTS_RATE_UNAVAILABLE}: no rate for {quote} in {ledger} ({why})")
        }
    })?;
    Ok(WireSymbolFacts {
        properties,
        quote_ccy: Some(rate.quote_ccy),
        quote_rate: Some(rate.account_per_quote.get()),
        rate_as_of_ms: Some(rate.as_of_ms),
        ..WireSymbolFacts::default()
    })
}

#[path = "dukascopy_rate_tests.rs"]
#[cfg(test)]
mod dukascopy_rate_tests;
