//! **`Request::OperatorInstruments`: the instruments the operator adds or removes BY HAND**, one at a time
//! (`vike_secrets::operator_instrument`, one row per instrument). The two cases the owner named (2026-11-13): an
//! instrument a venue's list does not carry (a Dukascopy US stock CFD), and a pair of a venue with no list at all
//! (IG); and every IBKR instrument (decision 0290's O-10: a stock, a forex pair, a dated future, its wire name as
//! the symbol and its class, base and quote stated). Nothing is hardcoded: every symbol is what the operator typed,
//! at run time.
//!
//! * **Facts come from the venue when it can answer** — [`gateway_lookup`]: the gatewayhub's credentialed lookup over
//!   a session it ALREADY holds (`vike_gatewayhub_client::catalog::operator_lookup`; only ig answers one), its chosen
//!   row stored and the forms it passed over named in the answer.
//! * **Where it cannot, the operator states them** ([`StatedInstrumentFacts`]): the tick, the quote, the class and the
//!   `.bi5` point value, all required and checked ([`stated_row`]); the venue is asked nothing.
//! * **Every reader of the kept lists reads them**: after the change the rows are folded into the venue's kept list
//!   (`crate::instrument_lists::keep_operator_rows`, `with_operator_rows`' one rule: the venue's own row wins), and
//!   every refresh folds them back, so a refresh never erases a row added by hand.
//! * **A row the venue's kept list already carries is refused**: the venue's own row is what every reader reads. The
//!   one exception is a CFD the venue gives NO name (dukascopy's `USA500.IDX/USD`, decision 0290's O-6): a hand row
//!   that states its base is stored and NAMES the venue's row (`crate::instrument_lists::lent_naming`). Added again
//!   with another base it replaces itself and renames the row in that one add; removed, it leaves the row unnamed at
//!   once (`crate::instrument_lists::with_operator_rows` takes every lent name back before it lends again).
//!
//! Remote text is bounded here, at the trust boundary ([`OPERATOR_INSTRUMENTS_MAX`], [`SYMBOL_MAX_BYTES`]). It writes
//! no other table and touches no store but the settings database and `<project>/instruments`.

use std::path::Path;
use std::sync::Arc;

use vike_datahub_client::catalog::{
    OperatorInstrumentOutcome, OperatorInstrumentRow, OperatorInstrumentsReply,
    StatedInstrumentClass, StatedInstrumentFacts,
};
use vike_datahub_client::proto::Response;
use vike_model::{
    Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
    ContractEquity, ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractFx,
    Instrument, Margining, SymbolProperties,
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

/// **The row the operator's stated facts make**, or why they do not hold: a tick above zero, a three-letter quote
/// (a coin's: three to five letters, a token's code such as `USDT`), a known class, and a point value that is the
/// tick's reciprocal (a `.bi5` reader takes the one off the other, `vike_bi5`'s `Bi5PriceScale::from_tick_size`, and
/// nothing here may let them disagree).
///
/// Each class states what its user name is spelled from (decision 0290; the rows of a venue that publishes no list,
/// ibkr and alpaca, O-10): an `equity` is a share, its ticker the stated `base`, else its symbol (`AAPL`, `BRK.B`;
/// ibkr's `AAPL.SMART.USD` states `--base AAPL`); a `future` an exchange-dated future on its `expiry` day, its root the
/// `base`, required (`ES_261218`); a `crypto_spot` a coin added under the venue's pair, its base the pair less its
/// quote (`BTC/USD` is `BTCUSD`); a pair may state its `base` too (`USDJPY`).
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
    let quote = vike_model::currency_code(&facts.quote).unwrap_or_default();
    let class = facts.class.trim().to_ascii_lowercase();
    // `None`: not a stated class, so a rolling CFD's underlying or a refusal (the arm below).
    let stated = StatedInstrumentClass::from_word(&class);
    let longest = if stated == Some(StatedInstrumentClass::CryptoSpot) { 5 } else { 3 };
    if !((3..=longest).contains(&quote.len()) && quote.bytes().all(|b| b.is_ascii_alphabetic())) {
        return Err(format!(
            "--quote `{:.16}` is not a three-letter currency code (a coin's: three to five letters)",
            facts.quote
        ));
    }
    let stated_base = facts.base.as_deref().map(|b| b.trim().to_ascii_uppercase());
    if let Some(base) = &stated_base
        && !(!base.is_empty()
            && base.len() <= 32
            && base.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'))
    {
        return Err(format!("--base `{base:.32}` is not a ticker, root or currency code"));
    }
    if facts.expiry.is_some() != (stated == Some(StatedInstrumentClass::Future)) {
        return Err(
            "--expiry is a future's last trading day (YYYY-MM-DD), stated with --asset-class future and only with it"
                .to_string(),
        );
    }
    let (base, contract) = match stated {
        Some(StatedInstrumentClass::Fx) => (
            stated_base.unwrap_or_default(),
            Contract::Fx(ContractFx {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            }),
        ),
        // A share's ticker is its symbol unless the venue spells it otherwise (ibkr's `AAPL.SMART.USD`: `--base AAPL`).
        Some(StatedInstrumentClass::Equity) => (
            stated_base.unwrap_or_else(|| symbol.to_string()),
            Contract::Equity(ContractEquity { exchange: None, currency: Some(quote.clone()) }),
        ),
        Some(StatedInstrumentClass::Future) => {
            let root = stated_base.ok_or_else(|| {
                "--asset-class future states its --base (the root its user name is spelled from, `ES` of `ES_261218`; \
                 decision 0290)"
                    .to_string()
            })?;
            let expiry = facts.expiry.as_deref().unwrap_or_default();
            vike_model::parse_ymd(expiry).map_err(|why| format!("--expiry: {why}"))?;
            let expiry_ms =
                vike_model::parse_date_label(expiry).map_err(|why| format!("--expiry: {why}"))?;
            let future = ContractFuture {
                margining: Margining::Linear,
                settle: quote.clone(),
                value_currency: Some(quote.clone()),
                expiry_ms,
                tenor: None,
                delivery: ContractFutureDelivery::Unknown,
                family: ContractFutureFamily::Traditional,
            };
            (root, Contract::Future(future))
        }
        Some(StatedInstrumentClass::CryptoSpot) => {
            let base = symbol
                .strip_suffix(quote.as_str())
                .and_then(|pair| pair.strip_suffix('/'))
                .filter(|base| !base.is_empty())
                .ok_or_else(|| {
                    format!(
                        "a coin is added under the venue's pair `BASE/{quote}` (alpaca's `BTC/USD`), not \
                         `{symbol:.32}`"
                    )
                })?;
            (base.to_string(), Contract::CryptoSpot)
        }
        // The class `fx` is a pair (the arm above), so a CFD's `Fx` underlying is never stated by its word and the
        // refusal leaves it out.
        None => {
            let underlying_kind = ContractCfdUnderlying::from_word(&class).ok_or_else(|| {
                let cfd: Vec<&str> = ContractCfdUnderlying::ALL
                    .iter()
                    .filter(|u| **u != ContractCfdUnderlying::Fx)
                    .map(|u| u.word())
                    .collect();
                format!(
                    "--asset-class `{class:.16}` is not one of {}, or a CFD's {}",
                    StatedInstrumentClass::WORDS.join(", "),
                    cfd.join(", ")
                )
            })?;
            (
                stated_base.unwrap_or_default(),
                Contract::Cfd(ContractCfd {
                    underlying_kind,
                    underlying: None,
                    product: ContractCfdProduct::Cfd,
                    expiry: ContractCfdExpiry::Rolling,
                    margin_rate: None,
                    swap_long: None,
                    swap_short: None,
                }),
            )
        }
    };
    Ok(Instrument::new(venue, symbol, base, quote, contract)
        .with_properties(SymbolProperties { tick_size: tick, ..SymbolProperties::default() }))
}

/// Whether the KEPT row `kept` of `venue` and the row being `added` name one instrument by a rule a reader of the kept
/// list finds a row with (DUK-8): the same USER name (decision 0290: `kept`'s stored one, `vike_catalog::user_name_of_row`,
/// and `added_user`, the added row's spelled once by the add; dukascopy's `EUR/USD` and a hand row `EURUSD` are both
/// `EURUSD`; two of IG's epics of one pair are both its user name, which a reader refuses as two rows) or, on
/// dukascopy, the same `.bi5` dataset (`vike_catalog::bi5_dataset_name`: `AAPLUSUSD` is `AAPL.US/USD`), which the
/// archive import finds its ONE row by.
fn same_instrument(
    venue: &str,
    kept: &Instrument,
    added: &Instrument,
    added_user: Option<&str>,
) -> bool {
    added_user
        .is_some_and(|name| vike_catalog::user_name_of_row(kept).is_ok_and(|user| user == name))
        || (venue == "dukascopy"
            && vike_catalog::bi5_dataset_name(&kept.raw_symbol)
                == vike_catalog::bi5_dataset_name(&added.raw_symbol))
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
             instead (--tick, --quote, --asset-class, --point-scale)"
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
    let (listing, by_hand) = crate::instrument_lists::split_operator_rows(listed);
    let listing_rows = listing.as_ref().map_or(&[][..], |l| l.instruments.as_slice());
    let listed_row = |raw: &str| listing_rows.iter().find(|i| i.raw_symbol == raw);
    let today = vike_model::time::epoch_ms_to_utc_date(vike_model::now_ms());
    let mut outcomes = Vec::new();
    for asked in add {
        let symbol = asked.trim().to_ascii_uppercase();
        let done = row_of(doors, venue, &symbol, stated).and_then(|(row, mut words)| {
            if let Some(listed) = listed_row(&row.raw_symbol) {
                // The one row a hand row may stand beside: a CFD the venue gives no name (`lent_naming`), which it names.
                let Some(vike_model::InstrumentNaming::User(user)) =
                    crate::instrument_lists::lent_naming(listed, &row)
                else {
                    return Err(format!(
                        "{venue}: {} is on the venue's own kept list already, which is what every reader reads; nothing \
                         was added",
                        row.raw_symbol
                    ));
                };
                words = match vike_catalog::user_name_of_row(listed) {
                    // A name an earlier hand row lent: this row replaces that one, so the name changes in this one add.
                    Ok(was) if crate::instrument_lists::is_lent(listed) => format!(
                        "{venue}: {} was named {was} by hand; it is named {user} now (the venue's row keeps its \
                         other facts)",
                        row.raw_symbol
                    ),
                    _ => format!(
                        "{venue}: {} is on the venue's own kept list with no user name; it is named {user} from the \
                         base stated (the venue's row keeps its other facts)",
                        row.raw_symbol
                    ),
                };
            }
            // Another spelling of a row already kept (the venue's, or one added by hand under another name) would be a
            // SECOND row of one instrument, and a reader that finds a row by the door's spelling then finds two and none.
            let raw = row.raw_symbol.as_str();
            // The add spells its new row's name ONCE (decision 0290's writer); the kept rows' are read as stored.
            let added_user = match vike_catalog::spell_naming_of_row(&row) {
                vike_model::InstrumentNaming::User(user) => Some(user),
                vike_model::InstrumentNaming::Unnamed(_) => None,
            };
            if let Some(kept) = listing_rows
                .iter()
                .chain(&by_hand)
                .find(|i| i.raw_symbol != raw && same_instrument(venue, i, &row, added_user.as_deref()))
            {
                return Err(format!(
                    "{venue}: {raw} names the instrument the kept list already carries as {}, which is what every \
                     reader reads; nothing was added",
                    kept.raw_symbol
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
