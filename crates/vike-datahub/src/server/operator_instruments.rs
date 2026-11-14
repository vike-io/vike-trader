//! **`Request::OperatorInstruments`: the instruments the operator adds or removes BY HAND**, one at a time
//! (`vike_secrets::operator_instrument`, one row per instrument). The two cases the owner named (2026-11-13): an
//! instrument a venue's list does not carry (a Dukascopy US stock CFD), and a pair of a venue with no list at all
//! (IG). Nothing is hardcoded: every symbol is what the operator typed, at run time.
//!
//! * **Facts come from the venue when it can answer** — [`gateway_lookup`]: the gatewayhub's credentialed lookup over
//!   a session it ALREADY holds (`vike_gatewayhub_client::catalog::operator_lookup`; only ig answers one), its chosen
//!   row stored and the forms it passed over named in the answer.
//! * **Where it cannot, the operator states them** ([`StatedInstrumentFacts`]): the tick, the quote, the class and the
//!   `.bi5` point value, all required and checked ([`stated_row`]); the venue is asked nothing.
//! * **Every reader of the kept lists reads them**: after the change the rows are folded into the venue's kept list
//!   (`crate::instrument_lists::keep_operator_rows`, `with_operator_rows`' one rule: the venue's own row wins), and
//!   every refresh folds them back, so a refresh never erases a row added by hand.
//! * **A row the venue's kept list already carries is refused**: the venue's own row is what every reader reads.
//!
//! Remote text is bounded here, at the trust boundary ([`OPERATOR_INSTRUMENTS_MAX`], [`SYMBOL_MAX_BYTES`]). It writes
//! no other table and touches no store but the settings database and `<project>/instruments`.

use std::path::Path;
use std::sync::Arc;

use vike_datahub_client::catalog::{
    OperatorInstrumentOutcome, OperatorInstrumentRow, OperatorInstrumentsReply,
    StatedInstrumentFacts,
};
use vike_datahub_client::proto::Response;
use vike_model::{
    Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
    ContractFx, Instrument, SymbolProperties,
};
use vike_secrets::CredentialStore;
use vike_secrets::operator_instrument::{
    OperatorInstrument, delete_operator_instrument, read_operator_instruments,
    write_operator_instrument,
};

/// How a symbol is looked up on its venue: `(venue, symbol)` to the chosen row FIRST and the forms passed over after
/// it, or the reason in words.
pub type InstrumentLookup =
    Arc<dyn Fn(&str, &str) -> Result<Vec<Instrument>, String> + Send + Sync>;

/// The production lookup: the gatewayhub's credentialed lookup over `link`.
#[must_use]
pub fn gateway_lookup(link: vike_gatewayhub_client::client::GatewayLink) -> InstrumentLookup {
    Arc::new(move |venue: &str, symbol: &str| {
        vike_gatewayhub_client::catalog::operator_lookup(&link, venue, symbol)
    })
}

/// The most symbols one request may add and remove together.
pub(crate) const OPERATOR_INSTRUMENTS_MAX: usize = 64;
/// The longest symbol, in bytes.
pub(crate) const SYMBOL_MAX_BYTES: usize = 64;

/// The answer of a datahub with no settings store.
const NO_STORE: &str =
    "this datahub was started with no settings store, so it holds no instruments added by hand";

/// What the verb reads and writes through.
pub(crate) struct OperatorInstrumentsDoors<'a> {
    /// `<project>/instruments`.
    pub(crate) instruments: Option<&'a Path>,
    pub(crate) settings: Option<&'a CredentialStore>,
    pub(crate) lookup: Option<&'a InstrumentLookup>,
}

/// Why `symbol` is not a word this verb takes, or `None`.
fn symbol_refusal(symbol: &str) -> Option<String> {
    let ok = !symbol.is_empty()
        && symbol.len() <= SYMBOL_MAX_BYTES
        && symbol
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'/' | b'_' | b'-'));
    (!ok).then(|| {
        format!(
            "`{symbol:.32}` is not a symbol (1 to {SYMBOL_MAX_BYTES} letters, digits, `.`, `/`, `_`, `-`)"
        )
    })
}

/// **The row the operator's stated facts make**, or why they do not hold: a tick above zero, a three-letter quote,
/// a known class, and a point value that is the tick's reciprocal (a `.bi5` reader takes the one off the other,
/// `vike_bi5`'s `Bi5PriceScale::from_tick_size`, and nothing here may let them disagree).
pub(crate) fn stated_row(
    venue: &str,
    symbol: &str,
    facts: &StatedInstrumentFacts,
) -> Result<Instrument, String> {
    let tick = facts.tick;
    if !(tick.is_finite() && tick > 0.0) {
        return Err(format!("--tick {tick} is not a price step above zero"));
    }
    let point = facts.point_scale;
    if !(point.is_finite() && point >= 1.0 && (point * tick - 1.0).abs() <= 1e-9) {
        return Err(format!(
            "--point-scale {point} is not the reciprocal of --tick {tick} (a .bi5 price is points divided by \
             1/tick, so the two must agree)"
        ));
    }
    let quote = facts.quote.trim().to_ascii_uppercase();
    if !(quote.len() == 3 && quote.bytes().all(|b| b.is_ascii_alphabetic())) {
        return Err(format!("--quote `{:.16}` is not a three-letter currency code", facts.quote));
    }
    let class = facts.class.trim().to_ascii_lowercase();
    let contract = if class == "fx" {
        Contract::Fx(ContractFx {
            pip_size: None,
            lot_size: None,
            swap_long: None,
            swap_short: None,
        })
    } else {
        let underlying_kind: ContractCfdUnderlying =
            serde_json::from_value(serde_json::Value::String(class.clone())).map_err(|_| {
                format!(
                    "--class `{class:.16}` is not one of fx, share, index, metal, commodity, bond, crypto, other"
                )
            })?;
        Contract::Cfd(ContractCfd {
            underlying_kind,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        })
    };
    Ok(Instrument::new(venue, symbol, "", quote, contract)
        .with_properties(SymbolProperties { tick_size: tick, ..SymbolProperties::default() }))
}

/// The row of `symbol`, looked up or stated, and the sentence saying what was taken.
fn row_of(
    doors: &OperatorInstrumentsDoors<'_>,
    venue: &str,
    symbol: &str,
    stated: Option<&StatedInstrumentFacts>,
) -> Result<(Instrument, String), String> {
    if let Some(facts) = stated {
        let row = stated_row(venue, symbol, facts)?;
        return Ok((
            row,
            format!("{venue}: {symbol} added with the facts stated (the venue was asked nothing)"),
        ));
    }
    let lookup = doors.lookup.ok_or_else(|| {
        format!(
            "this datahub holds no gatewayhub link to look `{symbol}` up on {venue} with; state its facts \
             instead (--tick, --quote, --class, --point-scale)"
        )
    })?;
    let rows = lookup(venue, symbol)?;
    let Some(row) = rows.first().filter(|r| r.venue == venue && !r.raw_symbol.is_empty()).cloned()
    else {
        return Err(format!(
            "{venue}: the venue answered no row for `{symbol}`; nothing was added"
        ));
    };
    let passed: Vec<&str> = rows[1..].iter().map(|r| r.raw_symbol.as_str()).collect();
    let mut words = format!("{venue}: {symbol} is {} ({})", row.raw_symbol, row.description);
    if !passed.is_empty() {
        words.push_str(&format!("; passed over: {}", passed.join(", ")));
    }
    Ok((row, words))
}

/// `Request::OperatorInstruments` (the module doc).
pub(crate) fn operator_instruments_verb(
    doors: &OperatorInstrumentsDoors<'_>,
    venue: &str,
    add: &[String],
    remove: &[String],
    stated: Option<&StatedInstrumentFacts>,
) -> Response {
    let Some(settings_dir) = doors
        .settings
        .and_then(CredentialStore::location)
        .map(|at| at.settings_dir().to_path_buf())
    else {
        return Response::Error(NO_STORE.to_string());
    };
    let changing = !add.is_empty() || !remove.is_empty();
    if changing {
        if !vike_model::VENUES.contains(&venue) {
            return Response::Error(format!(
                "`{venue:.32}` is not a roster venue; nothing was changed"
            ));
        }
        if add.len() + remove.len() > OPERATOR_INSTRUMENTS_MAX {
            return Response::Error(format!(
                "one request adds and removes at most {OPERATOR_INSTRUMENTS_MAX} instruments; nothing was changed"
            ));
        }
        if let Some(why) = add.iter().chain(remove).find_map(|s| symbol_refusal(s.trim())) {
            return Response::Error(format!("{why}; nothing was changed"));
        }
        if doors.instruments.is_none() {
            return Response::Error(
                "this datahub resolved no project folder, so it keeps no instrument lists to add to; nothing was \
                 changed"
                    .to_string(),
            );
        }
    }
    let listed = doors.instruments.and_then(|dir| vike_catalog::read_kept_list(dir, venue));
    let (listing, _) = crate::instrument_lists::split_operator_rows(listed);
    let on_the_list = |raw: &str| {
        listing.as_ref().is_some_and(|l| l.instruments.iter().any(|i| i.raw_symbol == raw))
    };
    let today = vike_model::time::epoch_ms_to_utc_date(vike_model::now_ms());
    let mut outcomes = Vec::new();
    for asked in add {
        let symbol = asked.trim().to_ascii_uppercase();
        let done = row_of(doors, venue, &symbol, stated).and_then(|(row, words)| {
            if on_the_list(&row.raw_symbol) {
                return Err(format!(
                    "{venue}: {} is on the venue's own kept list already, which is what every reader reads; nothing \
                     was added",
                    row.raw_symbol
                ));
            }
            let entry = OperatorInstrument { asked: symbol.clone(), added: today.clone(), instrument: row };
            write_operator_instrument(&settings_dir, &entry)
                .map(|()| words)
                .map_err(|e| format!("{venue}: {symbol} was not stored: {e}"))
        });
        outcomes.push(OperatorInstrumentOutcome { symbol: asked.clone(), done });
    }
    for asked in remove {
        let name = asked.trim().to_ascii_uppercase();
        let done = match delete_operator_instrument(&settings_dir, venue, &name) {
            Ok(true) => Ok(format!("{venue}: {name} removed")),
            Ok(false) => Err(format!(
                "{venue}: no instrument added by hand is named {name}; nothing was removed"
            )),
            Err(e) => Err(format!("{venue}: {name} was not removed: {e}")),
        };
        outcomes.push(OperatorInstrumentOutcome { symbol: asked.clone(), done });
    }
    let list_error = match doors.instruments {
        Some(dir) if outcomes.iter().any(|o| o.done.is_ok()) => {
            crate::instrument_lists::keep_operator_rows(dir, &settings_dir, venue).err()
        }
        _ => None,
    };
    match read_operator_instruments(&settings_dir) {
        Ok(rows) => Response::OperatorInstruments(OperatorInstrumentsReply {
            outcomes,
            rows: rows
                .into_iter()
                .map(|r| OperatorInstrumentRow {
                    asked: r.asked,
                    added: r.added,
                    instrument: r.instrument,
                })
                .collect(),
            list_error,
        }),
        Err(e) => Response::Error(format!("the instruments added by hand could not be read: {e}")),
    }
}

#[cfg(test)]
#[path = "operator_instruments_tests.rs"]
mod operator_instruments_tests;
