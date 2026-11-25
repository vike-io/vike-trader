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
//! # The WIRE name (decision 0290)
//!
//! Every function here takes the instrument's WIRE name, the sidecar's spelling of its kept row
//! (`crate::instrument::names`: `EURUSD`, `XAUUSD`, `AAPL.US/USD`), and finds the row of the account's list stated under
//! exactly that name. The node asks by the USER name (`AAPL.CFD`); its caller (the gatewayhub's dukascopy facts) turns it
//! into the wire through the kept row first (`crate::instrument::names::facts_wire_symbol`), so no spelling of a name is
//! read here: a name of no kept row (an old spelling, the SDK's enum-style `AAPLUSUSD`, an index or commodity CFD the
//! operator wrote no name for) never reaches these functions.
//!
//! # Every refusal, by name
//!
//! * **a name the account's list does not carry** (an instrument the account cannot trade, a kept row the login does
//!   not list), exactly as an unknown symbol is refused;
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

use vike_math::powers::power_of_ten;
use vike_model::SymbolProperties;
use vike_utility::errors::CarriedRefusal;

use crate::instrument::grid::{InstrumentType, JforexInstrument};
use crate::instrument::terms::VenueTerms;
use crate::refusals::DukascopyPropertiesRefusal;

/// The smallest currency-pair order the sidecar places, in units (`OrderMap.MIN_UNITS`); a test holds the two equal.
pub const SIDECAR_MIN_UNITS: f64 = 1000.0;

/// How the facts of an admissible instrument are had (see the module doc).
#[derive(Debug, Clone, PartialEq)]
pub enum DukascopyPropertiesBasis {
    /// A currency pair: its row's grid and the sidecar's quantity floor ([`instrument_facts_quoted`]).
    Static,
    /// A metal or a CFD: the venue's own terms of the instrument, read from the sidecar ([`facts_from_terms`]).
    VenueTerms(JforexInstrument),
}

impl DukascopyPropertiesBasis {
    /// The symbol the sidecar's terms line, ticks and fills carry for a [`DukascopyPropertiesBasis::VenueTerms`] row (its
    /// wire name), `None` for a pair.
    #[must_use]
    pub fn wire_symbol(&self) -> Option<String> {
        match self {
            DukascopyPropertiesBasis::Static => None,
            DukascopyPropertiesBasis::VenueTerms(row) => Some(row.symbol.clone()),
        }
    }
}

/// The row of the account's list stated under the wire name `wire` (the module doc), exactly so.
fn row_of<'a>(rows: &'a [JforexInstrument], wire: &str) -> Option<&'a JforexInstrument> {
    rows.iter().find(|r| r.symbol == wire)
}

/// The row `wire` names when it is one this venue may be asked to trade at all, or the operator's sentence for
/// why not: every refusal of the module doc EXCEPT the quote currency's of a currency pair (the caller's rule: the
/// account's own money for [`instrument_facts`], any money for [`instrument_facts_quoted`]) and the venue's terms of a
/// metal or CFD ([`facts_from_terms`]).
fn admissible_row<'a>(
    rows: &'a [JforexInstrument],
    wire: &str,
) -> Result<&'a JforexInstrument, DukascopyPropertiesRefusal> {
    let Some(row) = row_of(rows, wire) else {
        return Err(DukascopyPropertiesRefusal::NotOnList {
            wire: wire.to_string(),
            count: rows.len(),
        });
    };
    if row.is_coin() {
        let kind = match row.kind {
            InstrumentType::Forex => "FOREX",
            InstrumentType::Metal => "METAL",
            InstrumentType::Cfd => "CFD",
        };
        return Err(DukascopyPropertiesRefusal::Coin {
            pair: row.pair.clone(),
            kind,
            group: row.group.clone(),
        });
    }
    if let Some(unit) = vike_model::MinorUnit::of_minor(row.quote()) {
        // The account's list quotes `GBX` alone; any other minor unit of the table is refused the same way.
        let unit_words = match unit.major {
            "GBP" => format!("pence ({}, hundredths of a pound)", unit.minor),
            major => format!("{} (hundredths of {major})", unit.minor),
        };
        return Err(DukascopyPropertiesRefusal::MinorUnit { pair: row.pair.clone(), unit_words });
    }
    Ok(row)
}

/// The sentence for a metal or a CFD asked of the STATIC answers: its amounts are the venue's own, which only the
/// converting lane reads from the sidecar.
fn needs_the_venues_terms(row: &JforexInstrument) -> DukascopyPropertiesRefusal {
    let class = if row.kind == InstrumentType::Metal { "metal" } else { "CFD" };
    DukascopyPropertiesRefusal::NeedsVenueTerms { pair: row.pair.clone(), class }
}

/// The row `wire` names for a STATIC answer: a currency pair, or the sentence for why not.
fn static_row<'a>(
    rows: &'a [JforexInstrument],
    wire: &str,
) -> Result<&'a JforexInstrument, DukascopyPropertiesRefusal> {
    let row = admissible_row(rows, wire)?;
    if row.kind == InstrumentType::Forex { Ok(row) } else { Err(needs_the_venues_terms(row)) }
}

/// Which basis the facts of the instrument of wire name `wire` are had on, or the operator's sentence for why it is not
/// admissible at all (every refusal of the module doc except the quote currency's and the venue's terms, which need the
/// ledger and the sidecar). `rows` is the account's instrument list.
///
/// # Errors
/// One sentence per refusal, naming the instrument and the cause.
pub fn facts_basis(
    rows: &[JforexInstrument],
    wire: &str,
) -> Result<DukascopyPropertiesBasis, DukascopyPropertiesRefusal> {
    let row = admissible_row(rows, wire)?;
    Ok(if row.kind == InstrumentType::Forex {
        DukascopyPropertiesBasis::Static
    } else {
        DukascopyPropertiesBasis::VenueTerms(row.clone())
    })
}

/// The grid of an admissible currency pair: its row's price step and the sidecar's quantity floor.
fn grid_of(row: &JforexInstrument) -> Result<SymbolProperties, DukascopyPropertiesRefusal> {
    let tick_size = row.tick_size();
    if !tick_size.is_finite() || tick_size <= 0.0 {
        return Err(DukascopyPropertiesRefusal::NoPriceStep { pair: row.pair.clone() });
    }
    Ok(SymbolProperties {
        tick_size,
        step_size: SIDECAR_MIN_UNITS,
        min_qty: SIDECAR_MIN_UNITS,
        ..SymbolProperties::default()
    })
}

/// The facts of the instrument of wire name `wire` as an account whose money is `ledger_currency` may be admitted it,
/// or the operator's sentence for why not. `rows` is the account's instrument list. See the module doc for what is
/// admitted and every refusal.
///
/// # Errors
/// One sentence per refusal, naming the instrument and the cause: a [`DukascopyPropertiesRefusal`]'s text, which the
/// gatewayhub's facts door carries as its `String`.
pub fn instrument_facts(
    rows: &[JforexInstrument],
    wire: &str,
    ledger_currency: &str,
) -> Result<SymbolProperties, CarriedRefusal> {
    instrument_facts_refused(rows, wire, ledger_currency).map_err(|refusal| refusal.carried())
}

/// [`instrument_facts`] with its refusal typed.
pub(crate) fn instrument_facts_refused(
    rows: &[JforexInstrument],
    wire: &str,
    ledger_currency: &str,
) -> Result<SymbolProperties, DukascopyPropertiesRefusal> {
    let row = static_row(rows, wire)?;
    let quote = row.quote();
    if !quote.eq_ignore_ascii_case(ledger_currency) {
        return Err(DukascopyPropertiesRefusal::QuoteNotLedger {
            pair: row.pair.clone(),
            quote: quote.to_string(),
            ledger: ledger_currency.to_string(),
        });
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
    wire: &str,
) -> Result<(SymbolProperties, String), DukascopyPropertiesRefusal> {
    let row = static_row(rows, wire)?;
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
) -> Result<(SymbolProperties, String), DukascopyPropertiesRefusal> {
    let name = row.pair.clone();
    let class = match row.kind {
        InstrumentType::Forex => "FOREX",
        InstrumentType::Metal => "METAL",
        InstrumentType::Cfd => "CFD",
    };
    if !terms.class.eq_ignore_ascii_case(class) {
        return Err(DukascopyPropertiesRefusal::TermsClass {
            name,
            stated: terms.class.clone(),
            class,
        });
    }
    let usable =
        |figure: Option<f64>, what: &'static str| -> Result<f64, DukascopyPropertiesRefusal> {
            figure.filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| {
                DukascopyPropertiesRefusal::TermsNotUsable { name: name.clone(), what, figure }
            })
        };
    let min = usable(terms.min, "minimum amount")?;
    let step = usable(terms.step, "amount increment")?;
    let per_contract = usable(terms.per_contract, "amount per contract")?;
    let pip = usable(terms.pip, "pip")?;
    if (per_contract - 1.0).abs() > 1e-9 {
        return Err(DukascopyPropertiesRefusal::PerContract { name, per_contract });
    }
    let (Some(pip_scale), Some(tick_scale)) = (terms.pip_scale, terms.tick_scale) else {
        return Err(DukascopyPropertiesRefusal::TermsNoScale { name });
    };
    if tick_scale != pip_scale + 1 || tick_scale > 12 {
        return Err(DukascopyPropertiesRefusal::TermsGrid { name, pip_scale, tick_scale, pip });
    }
    // `10^-tick_scale`, the literal's bits, as `JforexInstrument::tick_size` (the scale is at most 12 here).
    let tick_size = power_of_ten(-(tick_scale as i32)).unwrap_or(f64::NAN);
    let steps = min / step;
    if (steps - steps.round()).abs() > 1e-9 * steps.max(1.0) {
        return Err(DukascopyPropertiesRefusal::MinNotWhole { name, min, step });
    }
    let max_qty = match terms.max {
        Some(max) if max.is_finite() && max > 0.0 => {
            if max < min {
                return Err(DukascopyPropertiesRefusal::MaxBelowMin { name, max, min });
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
        return Err(DukascopyPropertiesRefusal::TermsQuote {
            name,
            stated: stated.to_string(),
            quote,
        });
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
