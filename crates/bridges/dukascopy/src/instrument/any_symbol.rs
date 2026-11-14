//! **Which JForex instruments an account may be admitted on the fly, and on what grid** (any-symbol,
//! decisions 0173/0174, and the metals and CFDs): the facts the gatewayhub answers when the node asks about an
//! instrument the account's engine was not mounted on.
//!
//! Every answer reads the ACCOUNT's instrument list, as the sidecar stated it at its login (`crate::instrument::grid`, the
//! `rows` each function takes; the callers read it off the process's bus, `crate::feed::QuoteBus::instrument_list`, and
//! no list yet is "not now"). Nothing about an instrument is compiled in (owner 2026-11-13). Two bases, by the
//! instrument's class ([`facts_basis`]):
//!
//! * a **currency pair** is answered from its row: the price grid is the row's tick scale (the login's pip), the quantity
//!   floor is the sidecar's own (`OrderMap.MIN_UNITS`), and nothing here dials or reads a credential
//!   ([`instrument_facts`], [`instrument_facts_quoted`]);
//! * a **metal or a CFD** is answered from the VENUE's own terms of the instrument ([`facts_from_terms`]): the sidecar
//!   states them on a `terms` line at the instrument's first tick (`crate::instrument::terms`), because the sizes the SDK
//!   holds at login are its class constants, which it "overrides in runtime after instrument subscription". The
//!   converting lane (`crate::ledger`) asks the price lane's bus for the line (`crate::feed::QuoteBus::venue_terms`) and
//!   no line is no admission. Only the converting lane does (a `demo` account with an account-wide exposure ceiling
//!   armed).
//!
//! The module changes no behaviour of the mounted instrument: `grid: None` in `crate::adapter`'s `start_with` is untouched.
//!
//! # The wire spelling (the fill round trip)
//!
//! One instrument is ONE symbol everywhere (`OrderMap.canonicalSymbol`, the node's door, every fill, tick and position
//! line): a currency or metal pair is its six letters without the slash (`EURUSD`, `XAUUSD`, the slashed `EUR/USD` read as
//! the same), and a CFD is the SDK's own slashed name (`COPPER.CMD/USD`, `AAPL.US/USD`, `DEU.IDX/EUR`), the one form
//! `Instrument.fromString` takes. It is NEVER the enum-style name of a CFD (`COPPERCMDUSD`, `AAPLUSUSD`): the sidecar cannot
//! resolve it, so an order placed under it would be refused, and an answer that admitted it would name the instrument
//! differently from the fills that come back. [`facts_basis`] refuses it in words naming the right spelling. The
//! Java side holds the round trip for all 180 predefined instruments (`InstrumentSpellingTest`).
//!
//! # Every refusal, by name
//!
//! * **a name the account's list does not carry** (an instrument the account cannot trade, a misspelling), exactly as an
//!   unknown symbol is refused;
//! * **the enum-style name of a CFD** (above);
//! * **a coin** (a row the venue groups as `CRYPTO_CURR`, which the SDK types `FOREX`): the SDK's 1000-unit FOREX default
//!   is not a statement about a coin's amount unit, and a coin is not a currency pair, a metal or a CFD this venue's terms
//!   were read for;
//! * **a CFD quoted in pence** (`LSE.GB/GBX`, `GSK.GB/GBX`): its price is in hundredths of a pound, and no conversion
//!   pair carries `GBX`, so the booking would be a hundred times off in the dangerous direction;
//! * **a metal or CFD whose terms do not admit it** ([`facts_from_terms`]): the venue states no usable figure, a
//!   different class than its row, more than one unit of the underlying per contract (what a contract is worth per price
//!   point is then unmeasured), a minimum that is not a whole number of increments, or a tick grid that is not the SDK's
//!   (tick scale = pip scale + 1);
//! * **a quote currency other than the ledger's** (a currency pair, [`instrument_facts`]): a position's notional and PnL
//!   would be booked in the wrong money. A converting `demo` account is admitted such an instrument through
//!   [`instrument_facts_quoted`] and the gatewayhub's rate.
//!
//! What no refusal here does: a CFD's trading hours (a stock CFD trades in its exchange's hours, an index in its
//! session: the order is refused by the venue outside them), an ETF CFD's long-only rule (the vendor: "only long (buy)
//! positions are permitted for ETF"; a short is refused by the venue in its own words), margin and leverage (the engine's
//! exposure ceiling is notional), and financing (the SDK states no swap per instrument: a CFD held overnight is financed
//! by the venue and the node books none).
//!
//! # A quote currency other than the ledger's (multi-currency, decision 0183)
//!
//! [`instrument_facts_quoted`] answers for a node that CONVERTS: the quote-currency refusal is lifted, every other one
//! stands, and the instrument's quote currency comes back with the grid so the caller can ask for the rate
//! (`crate::account::quote_rate::quote_rate`: the mid of the conversion pair on the sidecar's own ticks). A metal or a CFD is
//! the same: [`facts_from_terms`] returns the quote currency the terms state, and the caller rates it exactly like a pair's.
//!
//! # The quantity grid
//!
//! A currency pair's minimum and step are both [`SIDECAR_MIN_UNITS`]: the sidecar rejects any pair order under 1000 units,
//! so that is the smallest size the adapter can place, and a step of the same size keeps every quantity a multiple of a unit
//! the venue's own increment divides (measured: 1000 and 1 on every pair the venue answered). A metal's and a CFD's are the
//! venue's own, from the terms line: ounces for a metal (the vendor: 1 oz of XAU/USD, 50 oz of XAG/USD) and contracts for a
//! CFD (the vendor lists 0.1 for USA30.IDX/USD, 20 for COPPER.CMD/USD, 1 for a stock), in the same units as an order's
//! `qty`.

use vike_model::SymbolProperties;

use crate::instrument::grid::{InstrumentType, JforexInstrument};
use crate::instrument::terms::VenueTerms;

/// The smallest currency-pair order the sidecar places, in units (`OrderMap.MIN_UNITS`); a test holds the two equal.
pub const SIDECAR_MIN_UNITS: f64 = 1000.0;

/// The quote currency code of a price in pence (`LSE.GB/GBX`): refused, see the module doc.
const PENCE: &str = "GBX";

/// How the facts of an admissible instrument are had (see the module doc).
#[derive(Debug, Clone, PartialEq)]
pub enum DukascopyPropertiesBasis {
    /// A currency pair: its row's grid and the sidecar's quantity floor ([`instrument_facts_quoted`]).
    Static,
    /// A metal or a CFD: the venue's own terms of the instrument, read from the sidecar ([`facts_from_terms`]).
    VenueTerms(JforexInstrument),
}

impl DukascopyPropertiesBasis {
    /// The symbol the sidecar's terms line, ticks and fills carry for a [`DukascopyPropertiesBasis::VenueTerms`] row (the wire
    /// spelling of the module doc), `None` for a pair.
    #[must_use]
    pub fn wire_symbol(&self) -> Option<String> {
        match self {
            DukascopyPropertiesBasis::Static => None,
            DukascopyPropertiesBasis::VenueTerms(row) => Some(wire_symbol(row)),
        }
    }
}

/// The wire spelling of `row` (`OrderMap.canonicalSymbol`): six letters for a currency or metal pair, the SDK's slashed
/// name for a CFD.
#[must_use]
pub fn wire_symbol(row: &JforexInstrument) -> String {
    if row.kind == InstrumentType::Cfd { row.pair.clone() } else { row.symbol.clone() }
}

/// The row `symbol` names: its wire spelling (`EURUSD`, `COPPER.CMD/USD`), the SDK's slashed name (`EUR/USD`) or a
/// CFD's enum-style name (`COPPERCMDUSD`, which [`admissible_row`] refuses by name), any case.
fn row_of<'a>(rows: &'a [JforexInstrument], symbol: &str) -> Option<&'a JforexInstrument> {
    let key = symbol.trim().to_ascii_uppercase();
    rows.iter().find(|r| {
        r.symbol == key
            || r.pair == key
            || (r.kind == InstrumentType::Cfd && r.history_name() == key)
    })
}

/// The row `symbol` names when it is one this venue may be asked to trade at all, or the operator's sentence for
/// why not: every refusal of the module doc EXCEPT the quote currency's of a currency pair (the caller's rule: the
/// account's own money for [`instrument_facts`], any money for [`instrument_facts_quoted`]) and the venue's terms of a
/// metal or CFD ([`facts_from_terms`]).
fn admissible_row<'a>(
    rows: &'a [JforexInstrument],
    symbol: &str,
) -> Result<&'a JforexInstrument, String> {
    let Some(row) = row_of(rows, symbol) else {
        return Err(format!(
            "dukascopy: `{}` is not on the account's instrument list ({} instruments, as the JForex sidecar stated \
             them at its login), so there is no statement of its price grid and quote currency; it is not an \
             instrument this account can trade",
            symbol.trim(),
            rows.len()
        ));
    };
    let key = symbol.trim().to_ascii_uppercase();
    if row.kind == InstrumentType::Cfd && key != row.pair {
        return Err(format!(
            "dukascopy: `{}` is the SDK's enum-style name of the CFD `{}`, which the sidecar cannot resolve and \
             would stamp its fills with under another spelling; write the instrument `{}`",
            symbol.trim(),
            row.pair,
            row.pair
        ));
    }
    if row.is_coin() {
        let kind = match row.kind {
            InstrumentType::Forex => "FOREX",
            InstrumentType::Metal => "METAL",
            InstrumentType::Cfd => "CFD",
        };
        return Err(format!(
            "dukascopy: `{}` is a coin typed {kind} in the SDK (the venue lists it in {}), and the SDK's FOREX amount \
             defaults are not a statement about a coin's unit",
            row.pair, row.group
        ));
    }
    if row.quote().eq_ignore_ascii_case(PENCE) {
        return Err(format!(
            "dukascopy: `{}` is quoted in pence ({PENCE}, hundredths of a pound) and no conversion pair carries it, \
             so a position in it would be booked a hundred times off; it is not admitted",
            row.pair
        ));
    }
    Ok(row)
}

/// The sentence for a metal or a CFD asked of the STATIC answers: its amounts are the venue's own, which only the
/// converting lane reads from the sidecar.
fn needs_the_venues_terms(row: &JforexInstrument) -> String {
    let class = if row.kind == InstrumentType::Metal { "metal" } else { "CFD" };
    format!(
        "dukascopy: `{}` is a {class}: its amounts and its price grid are the venue's own, which the JForex sidecar \
         states once the instrument is subscribed, and this answer has no sidecar to read them from; it is admitted \
         only for a `demo` account with an account-wide exposure ceiling armed (the converting lane) on a sidecar \
         jar that states the venue's terms",
        row.pair
    )
}

/// The row `symbol` names for a STATIC answer: a currency pair, or the sentence for why not.
fn static_row<'a>(
    rows: &'a [JforexInstrument],
    symbol: &str,
) -> Result<&'a JforexInstrument, String> {
    let row = admissible_row(rows, symbol)?;
    if row.kind == InstrumentType::Forex { Ok(row) } else { Err(needs_the_venues_terms(row)) }
}

/// Which basis the facts of `symbol` are had on, or the operator's sentence for why the symbol is not admissible at all
/// (every refusal of the module doc except the quote currency's and the venue's terms, which need the ledger and the
/// sidecar). `rows` is the account's instrument list.
///
/// # Errors
/// One sentence per refusal, naming the instrument and the cause.
pub fn facts_basis(
    rows: &[JforexInstrument],
    symbol: &str,
) -> Result<DukascopyPropertiesBasis, String> {
    let row = admissible_row(rows, symbol)?;
    Ok(if row.kind == InstrumentType::Forex {
        DukascopyPropertiesBasis::Static
    } else {
        DukascopyPropertiesBasis::VenueTerms(row.clone())
    })
}

/// The grid of an admissible currency pair: its row's price step and the sidecar's quantity floor.
fn grid_of(row: &JforexInstrument) -> Result<SymbolProperties, String> {
    let tick_size = row.tick_size();
    if !tick_size.is_finite() || tick_size <= 0.0 {
        return Err(format!(
            "dukascopy: `{}`: the list's tick scale gives no price step",
            row.pair
        ));
    }
    Ok(SymbolProperties {
        tick_size,
        step_size: SIDECAR_MIN_UNITS,
        min_qty: SIDECAR_MIN_UNITS,
        ..SymbolProperties::default()
    })
}

/// The facts of `symbol` as an account whose money is `ledger_currency` may be admitted it, or the
/// operator's sentence for why not. `rows` is the account's instrument list. See the module doc for what is admitted
/// and every refusal.
///
/// # Errors
/// One sentence per refusal, naming the instrument and the cause.
pub fn instrument_facts(
    rows: &[JforexInstrument],
    symbol: &str,
    ledger_currency: &str,
) -> Result<SymbolProperties, String> {
    let row = static_row(rows, symbol)?;
    let quote = row.quote();
    if !quote.eq_ignore_ascii_case(ledger_currency) {
        return Err(format!(
            "dukascopy: `{}` is quoted in {quote} and this account's money is {ledger_currency} (the \
             ledger is one currency and nothing converts); only {ledger_currency}-quoted instruments \
             are admitted",
            row.pair
        ));
    }
    grid_of(row)
}

/// [`instrument_facts`] for a node that CONVERTS a quote currency (decision 0183): the same grid and the same
/// refusals (a coin, a name the list lacks, a metal or a CFD, which need the venue's terms: [`facts_from_terms`]), but a
/// currency pair quoted in ANOTHER currency than the account's is not refused for that alone. Returns the grid and the
/// instrument's quote currency (upper-case), and the caller compares it with the account's: equal = nothing to convert,
/// otherwise the rate is `crate::account::quote_rate::quote_rate`'s, and no rate is no admission.
///
/// # Errors
/// As [`instrument_facts`], minus the quote-currency refusal.
pub fn instrument_facts_quoted(
    rows: &[JforexInstrument],
    symbol: &str,
) -> Result<(SymbolProperties, String), String> {
    let row = static_row(rows, symbol)?;
    Ok((grid_of(row)?, row.quote().to_ascii_uppercase()))
}

/// **The facts of a metal or a CFD from the VENUE's own terms of it**: the grid the door answers (`tick_size` from the
/// tick scale, `min_qty`, `step_size` and `max_qty` in the units of an order's `qty`) and the quote currency, or the
/// sentence that refuses the instrument. Nothing is guessed: a term the sidecar did not state is a refusal, and so is
/// everything the engine could not book soundly (module doc, "Every refusal"). The caller has already judged the
/// symbol ([`facts_basis`]) and read the terms off the bus (`QuoteBus::venue_terms`).
///
/// # Errors
/// One sentence per refusal, naming the instrument and the figure.
pub fn facts_from_terms(
    row: &JforexInstrument,
    terms: &VenueTerms,
) -> Result<(SymbolProperties, String), String> {
    let name = row.pair.as_str();
    let class = match row.kind {
        InstrumentType::Forex => "FOREX",
        InstrumentType::Metal => "METAL",
        InstrumentType::Cfd => "CFD",
    };
    if !terms.class.eq_ignore_ascii_case(class) {
        return Err(format!(
            "dukascopy: `{name}`: the sidecar states it as a `{}` and the account's list has it as a `{class}`; it \
             is not admitted until the two agree",
            terms.class
        ));
    }
    let usable = |figure: Option<f64>, what: &str| -> Result<f64, String> {
        figure.filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| {
            format!(
                "dukascopy: `{name}`: the venue's terms state no usable {what} ({figure:?}), so it is not admitted on them"
            )
        })
    };
    let min = usable(terms.min, "minimum amount")?;
    let step = usable(terms.step, "amount increment")?;
    let per_contract = usable(terms.per_contract, "amount per contract")?;
    let pip = usable(terms.pip, "pip")?;
    if (per_contract - 1.0).abs() > 1e-9 {
        return Err(format!(
            "dukascopy: `{name}`: the venue states {per_contract} units of the underlying per contract; what one contract \
             is worth per price point is unmeasured (no demo fill has shown it), so only an instrument stated at one \
             per contract is admitted"
        ));
    }
    let (Some(pip_scale), Some(tick_scale)) = (terms.pip_scale, terms.tick_scale) else {
        return Err(format!(
            "dukascopy: `{name}`: the venue's terms state no pip scale or tick scale, so its price grid is not known"
        ));
    };
    if tick_scale != pip_scale + 1 || tick_scale > 12 {
        return Err(format!(
            "dukascopy: `{name}`: the venue states pip scale {pip_scale} and tick scale {tick_scale} (pip {pip}); the \
             SDK's tick is a tenth of a pip, so this is not a grid the engine can round to"
        ));
    }
    // 1e-N parses to the nearest f64, the same value as the source literal (as `JforexInstrument::tick_size`).
    let tick_size: f64 = format!("1e-{tick_scale}").parse().unwrap_or(f64::NAN);
    let steps = min / step;
    if (steps - steps.round()).abs() > 1e-9 * steps.max(1.0) {
        return Err(format!(
            "dukascopy: `{name}`: the minimum {min} is not a whole number of the increment {step}, so the venue's \
             sizes ({min}, {min} + {step}, ...) are not multiples of a step the engine can round to"
        ));
    }
    let max_qty = match terms.max {
        Some(max) if max.is_finite() && max > 0.0 => {
            if max < min {
                return Err(format!(
                    "dukascopy: `{name}`: the venue's largest order {max} is below its smallest {min}"
                ));
            }
            max
        }
        _ => 0.0, // no usable maximum stated: the engine's own ceilings judge a size
    };
    let stated = terms.quote.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let quote = row.quote().to_ascii_uppercase();
    if let Some(stated) = stated
        && !stated.eq_ignore_ascii_case(&quote)
    {
        return Err(format!(
            "dukascopy: `{name}`: the sidecar states the quote currency {stated} and its list row has {quote}; it is not \
             admitted until the two agree"
        ));
    }
    Ok((
        SymbolProperties {
            tick_size,
            step_size: step,
            min_qty: min,
            max_qty,
            ..SymbolProperties::default()
        },
        quote,
    ))
}

#[cfg(test)]
#[path = "any_symbol_tests.rs"]
mod any_symbol_tests;
