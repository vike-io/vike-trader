//! `venue_symbol` — **the DOOR's reading of a symbol a person types (decision 0290): its shape, read with no lookup,
//! then the USER name the venue's KEPT row answers**, and the per-venue rule table (`SYMBOL_RULES`) of what each
//! venue's adapter does with an order's symbol.
//!
//! One instrument has ONE name at every door (the control door's order, a mount, a strategy's leg, an account's stated
//! symbol, the gatewayhub's lease): its user name, `crate::user_name_of_row` of its kept row
//! (`<project>/instruments/<venue>.json`, the datahub's). A door:
//!
//! 1. reads the typed text's SHAPE ([`door_symbol`]): blanks trimmed, ASCII upper case, and the few names refused BY
//!    SHAPE before any lookup because their words say more than "no row" (an option off deribit, deribit's
//!    coin-settled option, hyperliquid's `@107` wire name);
//! 2. resolves it through the kept row ([`door_user_symbol`], over `crate::kept_row_of_user_name`): no list, no row or
//!    two rows is a refusal in words, and no list names its fix ([`no_kept_list_refusal`]);
//! 3. holds the row's canonical user name.
//!
//! The class is the row's, never the name's, and an old spelling (`BTC-PERPETUAL`, `EUR_USD`, `AAPL.SMART.USD`,
//! `CS.D.EURUSD.MINI.IP`, `BTC-USDT-SWAP`, `BTCUSDT.S`) is read by nothing here: it names no row (decision 0117).
//! The venue's own spelling is its bridge's (`crate::wire_name_of_row`). What a door then DOES with the answer
//! (admit, refuse, respell) is the door's. Polymarket is the one venue whose user name is the venue's own (O-13): an
//! outcome token id names itself, and no row is asked (its kept list is a capped page of its markets).

use vike_model::{AssetClass, Instrument};

use crate::refusals::SymbolRefusal;

/// How an `Admits` venue's door reads a symbol (decision 0290: one grammar for every venue but polymarket).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueSymbolSpelling {
    /// Decision 0290's user name, one grammar per class on every venue, resolved through the venue's KEPT row
    /// ([`door_user_symbol`]): FX `USDJPY`, spot `BTCUSDT`, perpetual `BTCUSDT.P`, dated `BTCUSDT_261225`, stock
    /// `AAPL`, stock CFD `AAPL.CFD`, deribit's option its own name.
    UserName,
    /// A Polymarket OUTCOME TOKEN (polymarket): the 20-or-more-digit ERC-1155 id, used unchanged (it is the one name
    /// of the position: fills, positions and every order carry it; decision 0290's O-13). Typed as a market NAME
    /// instead (`slug:Outcome`, the market's address), it is resolved to the id first by the door's
    /// `SymbolAdmitter::resolve` (the gatewayhub reads Gamma), so [`door_symbol`] only ever sees an id. The engine
    /// behind it is mounted on no symbol (the door's `account_wide`).
    OutcomeToken,
}

/// What a venue's adapter does with the symbol an order names. What the door does with each rule is its own
/// (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `plan_submit_symbol`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueSymbolRule {
    /// Signs on the order's symbol: the door admits it.
    Admits(VenueSymbolSpelling),
    /// Signs on the engine's mounted symbol: the door refuses any other, for this reason. ⚠ No roster
    /// venue has this row today (deribit's was the last): the variant is kept because
    /// `just new-venue`'s classification hint offers it to the next adapter.
    MountedOnly(&'static str),
    /// Reads the order's own symbol already; not this gate's. ⚠ No roster venue has this row today (polymarket's was
    /// the last, until it became `Admits`): the variant is kept because `just new-venue`'s classification hint offers
    /// it, and `per_order_refusal`'s fences stay for the next venue that holds it.
    PerOrder,
    /// Reads the order's own symbol, but the ENGINE cannot judge a symbol it was not started on (its
    /// grid, multiplier or price are the mounted symbol's): the door refuses a foreign symbol, for this
    /// reason, until admission supports the venue. Deleting the row (to `PerOrder` or `Admits`) opens it.
    /// ⚠ No roster venue has this row today (fxcm's was the last, until its price lane): the variant is kept
    /// because `just new-venue`'s classification hint offers it. ⚠ NO TEST HOLDS WHAT IT DOES any more (the test
    /// that did went with the last row, and a rule cannot be injected into `SYMBOL_RULES` from outside): whoever
    /// gives a venue this row again writes one first (`per_order_refusal`'s `PerOrderUnsafe` arm).
    PerOrderUnsafe(&'static str),
}

/// The rule of every roster venue. `every_roster_venue_has_a_symbol_rule` fails on a venue in none,
/// so a new venue is classified by whoever reads its order builder.
// vike:new-venue:note do NOT add a row here by hand-copying one: read the new venue's ORDER BUILDER (does it sign on the order's symbol or on the one its client was bound to?), then add `("{venue}", VenueSymbolRule::PerOrder)` if it reads the order's symbol AND the engine judges a foreign symbol soundly (grid, multiplier, a price source), `VenueSymbolRule::PerOrderUnsafe("<why>")` if it reads the symbol but the engine cannot, or `VenueSymbolRule::MountedOnly("<why>")` if it does not. `Admits` is only for a venue whose adapter has a per-symbol router AND whose `TradeLane::symbol_facts` answers (the gatewayhub), and it also needs a `VenueSymbolSpelling` (`UserName`, decision 0290's grammar through the venue's kept row, unless its user name is the venue's own id).
pub const SYMBOL_RULES: &[(&str, VenueSymbolRule)] = &[
    ("binance", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("aster", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("bybit", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("okx", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("deribit", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("hyperliquid", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("alpaca", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("ctrader", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("ig", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("oanda", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("fxcm", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("ibkr", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
    ("polymarket", VenueSymbolRule::Admits(VenueSymbolSpelling::OutcomeToken)),
    ("dukascopy", VenueSymbolRule::Admits(VenueSymbolSpelling::UserName)),
];

/// The spelling of an `Admits` venue, `None` for a venue no rule admits symbols for.
fn admits(venue: &str) -> Option<VenueSymbolSpelling> {
    match SYMBOL_RULES.iter().find(|(v, _)| *v == venue) {
        Some((_, VenueSymbolRule::Admits(spelling))) => Some(*spelling),
        _ => None,
    }
}

/// **The SHAPE of `raw` at `venue`'s door, read with no lookup**: `None` for a venue whose rule admits no symbol by
/// name (no `Admits` row); else the typed text trimmed and upper-cased (a polymarket token kept as written, a deribit
/// option in the venue's own spelling, its decimal strike's lower-case `d`), or the refusal of a name refused by its
/// shape alone (an option off deribit, a deribit coin-settled option, hyperliquid's `@107` wire name, a character no
/// symbol has). Whether it names an instrument is the kept row's to say ([`door_user_symbol`]).
#[must_use]
pub fn door_symbol(venue: &str, raw: &str) -> Option<Result<String, String>> {
    Some(match admits(venue)? {
        VenueSymbolSpelling::OutcomeToken => canonical_outcome_token(raw),
        VenueSymbolSpelling::UserName => typed_shape(venue, raw),
    })
}

/// [`door_symbol`]'s user-name arm: the shape refusals, then the typed text upper-cased.
fn typed_shape(venue: &str, raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if venue == "deribit"
        && let Some(option) = deribit_option(raw, text)
    {
        return option;
    }
    if let Some(why) = option_by_shape(venue, raw, text) {
        return Err(why);
    }
    if venue == "hyperliquid" && text.starts_with('@') {
        return Err(SymbolRefusal::WireNameTyped { raw: raw.to_string() }.to_string());
    }
    let upper = text.to_ascii_uppercase();
    if upper.is_empty()
        || upper.len() > 64
        || !upper.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-/".contains(&b))
    {
        return Err(SymbolRefusal::NotASymbol { raw: raw.to_string() }.to_string());
    }
    Ok(upper)
}

/// **An OPTION written as the venue writes one, refused by its shape** on a venue that trades none (decision 0290's
/// O-3: options are named, and traded, on deribit only), in words that say why rather than "no row": binance and
/// aster's EAPI shape (`BTC-261225-85000-C`), bybit's (`BTC-31OCT26-75500-C-USDT`), okx's
/// (`BTC-USD-261106-76000-C`) and ibkr's (`AAPL.SMART.USD.OPT...`). `None` for every other shape.
fn option_by_shape(venue: &str, raw: &str, text: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let dashed: Vec<&str> = upper.split('-').collect();
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let word = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let right = |p: &str| matches!(p, "C" | "P");
    let option = match (venue, dashed.as_slice()) {
        ("binance" | "aster", &[underlying, expiry, strike, side]) => {
            word(underlying) && expiry.len() == 6 && digits(expiry) && digits(strike) && right(side)
        }
        ("bybit", parts) => parts.len() > 2,
        ("okx", &[a, b, c, d, side]) => [a, b, c, d].into_iter().all(word) && right(side),
        ("ibkr", _) => upper.split('.').nth(3) == Some("OPT"),
        _ => false,
    };
    option.then(|| {
        SymbolRefusal::OptionNotTraded { raw: raw.to_string(), venue: venue.to_string() }
            .to_string()
    })
}

/// **A deribit OPTION in the venue's own name**, the one class whose user name is the venue's (decision 0290's O-3):
/// `BASE_QUOTE-DDMMMYY-STRIKE-C|P`, upper case but a decimal strike's `d` (`XRP_USDC-27MAR26-2d5-C`), on a day the
/// calendar has (`vike_venues::deribit_expiry`). A coin-settled option (`BTC-27MAR26-60000-C`: its premium is coin, which a
/// one-currency ledger would book as dollars) is refused by name. `None` for a name of no option's shape: it is read
/// as any user name.
fn deribit_option(raw: &str, text: &str) -> Option<Result<String, String>> {
    let upper = text.to_ascii_uppercase();
    let parts: Vec<&str> = upper.split('-').collect();
    let &[base, expiry, strike, cp @ ("C" | "P")] = parts.as_slice() else { return None };
    let word =
        |p: &str| !p.is_empty() && p.len() <= 16 && p.bytes().all(|b| b.is_ascii_alphanumeric());
    let strike_ok = strike.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && strike.bytes().last().is_some_and(|b| b.is_ascii_digit())
        && strike.bytes().all(|b| b.is_ascii_digit() || b == b'D')
        && strike.bytes().filter(|b| *b == b'D').count() <= 1;
    let base_ok = base.split('_').count() <= 2 && base.split('_').all(word);
    if !(base_ok && vike_venues::deribit_expiry(expiry).is_some() && strike_ok) {
        return Some(Err(SymbolRefusal::NotADeribitOption { raw: raw.to_string() }.to_string()));
    }
    if !base.contains('_') {
        return Some(Err(SymbolRefusal::CoinSettledOption {
            raw: raw.to_string(),
            base: base.to_string(),
            expiry: expiry.to_string(),
            strike: strike.to_string(),
            cp: cp.to_string(),
        }
        .to_string()));
    }
    Some(Ok(format!("{base}-{expiry}-{}-{cp}", strike.to_ascii_lowercase())))
}

/// **Why `venue` keeps no list a door could read a name off, and the fix in the operator's words**: the datahub's
/// refresh (`vike-cli data catalog refresh`) for a venue it lists (a public list, or one read over the session of
/// the venue's running account: `crate::CatalogAvailability::is_served`), else the operator's own row
/// (`vike-cli data instruments add`) for a venue that publishes no list (ibkr, ig, alpaca; decision 0290's O-10).
#[must_use]
pub fn no_kept_list_refusal(venue: &str) -> String {
    let fix = if crate::catalog_availability(venue).is_served() {
        format!("run `vike-cli data catalog refresh --venue {venue}`")
    } else {
        format!("add the instrument: `vike-cli data instruments add --venue {venue} <name>`")
    };
    SymbolRefusal::NoKeptList { venue: venue.to_string(), fix }.to_string()
}

/// **The USER name `raw` names on `venue`, or why it names none** (decision 0290): [`door_symbol`]'s shape, then the
/// ONE row of `kept` (the venue's kept list; rows of other venues are skipped) whose user name it is, ignoring ASCII
/// case, and that row's canonical user name. The control door's reading of an order's symbol. A polymarket token
/// names itself and asks no row.
///
/// # Errors
/// A venue no rule admits symbols for, a name refused by its shape, no list of `venue` in `kept`
/// ([`no_kept_list_refusal`]), no row or two rows answering it (`crate::kept_row_of_user_name`'s words), and a row of
/// a class the door does not admit by name (a deribit spot pair): each in words naming `raw`.
pub fn door_user_symbol(venue: &str, raw: &str, kept: &[Instrument]) -> Result<String, String> {
    let Some(read) = door_symbol(venue, raw) else {
        return Err(no_spelling(venue, raw));
    };
    let shape = read?;
    if admits(venue) == Some(VenueSymbolSpelling::OutcomeToken) {
        return Ok(shape);
    }
    if !kept.iter().any(|r| r.venue == venue) {
        return Err(SymbolRefusal::NoListHere {
            raw: raw.to_string(),
            venue: venue.to_string(),
            no_list: no_kept_list_refusal(venue),
        }
        .to_string());
    }
    let row = crate::kept_row_of_user_name(kept, venue, &shape)?;
    if venue == "deribit" && row.asset_class() == AssetClass::CryptoSpot {
        return Err(SymbolRefusal::DeribitSpotPair { raw: raw.to_string() }.to_string());
    }
    crate::user_name_of_row(row)
}

/// Why `raw` cannot be read on `venue`: no rule declares a spelling for it.
fn no_spelling(venue: &str, raw: &str) -> String {
    SymbolRefusal::NoSpelling { raw: raw.to_string(), venue: venue.to_string() }.to_string()
}

/// **The engine symbol `raw` names on `venue`, or why it names none**: the reading `vike-cli account
/// set-symbol` makes of an account's stated symbol before anything is written, with no network, and the node's
/// reading of a stated symbol at boot. [`door_user_symbol`] over `kept` (the venue's kept list), so the CLI refuses
/// what the door would, in the door's words, and stores the canonical user name.
///
/// ONE exception, so a refusal never blocks the session that cures it: a venue whose list is read over the session of
/// its RUNNING account (`crate::CatalogAvailability::HeldSession`: oanda, ctrader, dukascopy, fxcm, ig) and of which
/// `kept` holds no row yet answers the name's shape ([`door_symbol`]): its account must start for its list to exist.
/// Its first start, and an operator's refresh after it, keep the list; the next reading resolves the name.
///
/// # Errors
/// [`door_user_symbol`]'s, each in words naming `raw` and the venue.
pub fn venue_engine_symbol(venue: &str, raw: &str, kept: &[Instrument]) -> Result<String, String> {
    let listed_by_its_session =
        crate::catalog_availability(venue) == crate::CatalogAvailability::HeldSession;
    if listed_by_its_session && !kept.iter().any(|r| r.venue == venue) {
        return door_symbol(venue, raw).unwrap_or_else(|| Err(no_spelling(venue, raw)));
    }
    door_user_symbol(venue, raw, kept)
}

/// **Why `raw` is refused where a symbol is TAKEN AS WRITTEN** (a mount, a strategy's leg, the gatewayhub's lease): it
/// names a kept row of `venue` whose user name is spelled ANOTHER way (`btcusdt.p` is `BTCUSDT.P` on bybit, `eurusd`
/// is `EURUSD` on oanda), so the same instrument would carry two names. Such a door refuses rather than respelling,
/// because what it takes as written (a strategy's own orders, an engine's journal) keeps the name it was given. Read
/// over this process's kept list (`crate::kept_list`). `None` when it is written exactly so, or when nothing judges it:
/// a venue with no rule, a blank, no list kept, a name no row answers (the bridge, or the lease's start, refuses what
/// it cannot sign). ibkr and oanda are judged like every venue (their names are user names now).
#[must_use]
pub fn respelled_symbol(venue: &str, raw: &str) -> Option<String> {
    let rows = crate::kept_list(venue).unwrap_or_default();
    respelled_in(venue, raw, &rows)
}

/// [`respelled_symbol`] over `kept` rather than this process's list.
fn respelled_in(venue: &str, raw: &str, kept: &[Instrument]) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    let canonical = door_user_symbol(venue, raw, kept).ok()?;
    (canonical != raw).then(|| {
        SymbolRefusal::Respelled { raw: raw.to_string(), canonical, venue: venue.to_string() }
            .to_string()
    })
}

/// The most digits an outcome token id has: an ERC-1155 id is a uint256, 78 decimal digits at most.
const OUTCOME_TOKEN_MAX_DIGITS: usize = 78;

/// Whether `text` is an outcome token id: 20 or more decimal digits and no more than a uint256 holds. The ONE
/// outcome-token test: the polymarket bridge, the node's control door, the CLI's one-shot order and the backtest's kept
/// rows all read it, so no surface takes a 79-digit text another refuses. Its bounds are pinned beside it
/// (`venue_symbol_tests.rs`'s `an_outcome_token_is_twenty_to_seventy_eight_digits`).
#[must_use]
pub fn is_outcome_token(text: &str) -> bool {
    (20..=OUTCOME_TOKEN_MAX_DIGITS).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_digit())
}

/// **A Polymarket symbol is an outcome's TOKEN id**, used unchanged: the digits are the venue's own name for the
/// position (every order, fill and holding carries it), so there is nothing to translate and nothing to upper-case.
/// What an operator may type instead, a market name, never reaches this function: the door's `SymbolAdmitter::resolve` turned it
/// into the id before the planner canonicalised. So a text that is not an id here is a surface with no source to
/// resolve it, or a name no source could, and is refused with the spellings that work.
fn canonical_outcome_token(raw: &str) -> Result<String, String> {
    let token = raw.trim();
    if is_outcome_token(token) {
        return Ok(token.to_string());
    }
    Err(SymbolRefusal::NotAnOutcomeToken { raw: raw.to_string() }.to_string())
}

/// **The row of `symbol` on `venue` in `rows`, a KEPT instrument list** (`<project>/instruments/<venue>.json`, the
/// datahub's): the row spelled exactly so, else the ONE row whose user name `symbol` is (`crate::kept_row_of_user_name`:
/// `EURUSD` finds fxcm's `EUR/USD`). `None` when the list carries no such row, or two rows that one name cannot tell
/// apart: an ambiguous answer is no answer, never a pick. A grammar, never a guess: no row is made up for a name the
/// list does not carry. ⚠ Superseded by the translator (`crate::kept_row_of_user_name`, `crate::kept_row_of_store_name`):
/// SYM-NAMES step 2's backtest group moves its readers and deletes it.
#[must_use]
pub fn kept_row<'a>(rows: &'a [Instrument], venue: &str, symbol: &str) -> Option<&'a Instrument> {
    if let Some(exact) = rows.iter().find(|r| r.venue == venue && r.raw_symbol == symbol) {
        return Some(exact);
    }
    crate::kept_row_of_user_name(rows, venue, symbol).ok()
}

/// **The DATASET name dukascopy's keyless `.bi5` datafeed files the kept row `raw_symbol` under**: the row's name with
/// its `/` and `.` dropped (`AAPL.US/USD` is `AAPLUSUSD`, `EUR/USD` is `EURUSD`, `COPPER.CMD/USD` is `COPPERCMDUSD`).
/// It is the vendor's folder name, which a `.bi5` archive import is handed and the datafeed's tick fetch asks by, and
/// NO store name (decision 0290: a series is filed under the row's user name, `crate::user_name_of_row`, as the store
/// spells it, `vike_paths::store_path::store_name_of_user_name`). The one rule
/// both directions read: [`bi5_dataset_row`] (a dataset to its row) and the dukascopy bridge naming a row's history.
#[must_use]
pub fn bi5_dataset_name(raw_symbol: &str) -> String {
    raw_symbol.replace(['/', '.'], "")
}

/// **The kept dukascopy row of the `.bi5` dataset `dataset`** (the vendor's folder name, `AAPLUSUSD`): the ONE row of
/// `rows` on dukascopy whose [`bi5_dataset_name`] is `dataset` and that states a price step, else the ONE such row
/// whatever its step. A row that states no step (zero) is never a scale, so beside one the dataset's one row that
/// states a step answers (an operator row spelled otherwise beside a step-less venue row, DUK-6), and a lone
/// step-less row is still found, for its reader to refuse in its own words. `None` for no such row or for two: an
/// ambiguous answer is no answer. The datahub's `.bi5` import and the dukascopy bridge's HTTP lane both read it.
#[must_use]
pub fn bi5_dataset_row<'a>(rows: &'a [Instrument], dataset: &str) -> Option<&'a Instrument> {
    let of_dataset =
        |r: &&Instrument| r.venue == "dukascopy" && bi5_dataset_name(&r.raw_symbol) == dataset;
    let states_step =
        |r: &&Instrument| r.properties.tick_size.is_finite() && r.properties.tick_size > 0.0;
    only_one(rows.iter().filter(of_dataset).filter(states_step))
        .or_else(|| only_one(rows.iter().filter(of_dataset)))
}

/// The one item of `rows`, `None` for none or for two.
fn only_one<'a>(mut rows: impl Iterator<Item = &'a Instrument>) -> Option<&'a Instrument> {
    let first = rows.next()?;
    rows.next().is_none().then_some(first)
}

#[path = "venue_symbol_tests.rs"]
#[cfg(test)]
mod venue_symbol_tests;
