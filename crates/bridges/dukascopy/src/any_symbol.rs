//! **Which JForex instruments an account may be admitted on the fly, and on what grid** (any-symbol,
//! decisions 0173/0174): the facts the gatewayhub answers when the node asks about an instrument the
//! account's engine was not mounted on.
//!
//! Every fact is STATIC and offline: the price grid is `crate::jforex_grid`'s committed SDK table, the
//! quantity floor is the sidecar's own (`OrderMap.MIN_UNITS`), and nothing here logs in, dials or reads a
//! credential. The module changes no behaviour of the mounted instrument: `grid: None` in
//! `crate::adapter`'s `start_with` is untouched.
//!
//! # What is admitted
//!
//! A `FOREX` row of the table whose QUOTE currency is the ledger's ([`instrument_facts`]'s
//! `ledger_currency`; nothing converts HERE, decision 0170: a node that converts a quote currency, decision
//! 0183, is answered by [`instrument_facts_quoted`], which the gatewayhub's `dukascopy_rate` calls and which
//! prices a row in another money from the sidecar's own ticks): `AUDUSD`, `EURUSD`, `GBPUSD`, `NZDUSD` on a
//! USD ledger. That is the only kind of row whose spelling the sidecar round-trips (a `METAL` row does too, but
//! is refused for its unit, below): it takes `EURUSD` (six letters become `EUR/USD`) and stamps every fill
//! and position line with the enum name (`OrderMap.canonicalSymbol`), which is the same six letters.
//!
//! # Every refusal, by name
//!
//! * **a name the table does not carry** (a run-time instrument built by `Instrument.fromString`, a
//!   misspelling): no grid statement exists for it;
//! * **a CFD whose pip is the SDK's blanket placeholder** (103 of 106 CFD rows, pip 0.01): the number says
//!   nothing about the instrument's price grid;
//! * **a CFD with a real pip** (`COPPER.CMD/USD`, `GAS.CMD/USD`, `PRT.IDX/EUR`): its grid is a statement,
//!   but the sidecar stamps its fills `COPPERCMDUSD` (the enum name) and resolves any name that is not six
//!   letters through `Instrument.fromString`, so the name an order is placed under and the name its fill
//!   comes back under differ, and the engine would never match the fill (whether the bare enum name is
//!   accepted by `fromString` is UNMEASURED);
//! * **the two crypto rows typed `FOREX`** (`BTCUSD`, `ETHUSD`): the SDK's 1000-unit FOREX default is not a
//!   statement about a coin's amount unit;
//! * **the two metals** (`XAUUSD`, `XAGUSD`): the floor below is 1000 UNITS of the instrument. The unit IS
//!   documented: the vendor's order-amounts wiki page states a metal's amount in ounces, with a venue minimum of
//!   1 oz of XAU/USD and 50 oz of XAG/USD, so the sidecar's own 1000-unit floor is 1000 oz (the smallest XAUUSD
//!   order would be ~2.4M USD), and nothing in the facts guards a notional;
//! * **a quote currency other than the ledger's**: a position's notional and PnL would be booked in the wrong
//!   money, and overstated PnL feeds the margin-call sweep and the drawdown latch. (Refused by THIS function;
//!   a converting `demo` account is admitted such a pair through the quoted path above, with a rate.)
//!
//! ⚠ **STOP-AND-REPORT: the metals stay refused until a `demo` order proves a metal path.** The unit is the
//! vendor's word, not a measurement of this adapter: nothing has placed a metal order through the sidecar. The
//! proof that lifts it: one demo order on XAU/USD at the smallest amount the sidecar places, then the fill (or the
//! position line) and the account's balance and margin read back, showing the `venue terms` line's minimum and
//! amount per contract and how many ounces 1000 is on this account. Only then may a metal be admitted, on a
//! quantity grid of its own (sized from the venue's terms, never from the 1000 below).
//!
//! # A quote currency other than the ledger's (multi-currency, decision 0183)
//!
//! [`instrument_facts_quoted`] answers for a node that CONVERTS: the last refusal above is lifted, every other one
//! stands, and the instrument's quote currency comes back with the grid so the caller can ask for the rate
//! (`crate::home_conversion::quote_rate`: the mid of the conversion pair on the sidecar's own ticks). Which
//! instruments that opens is the SDK table's FOREX rows quoted in a currency that has a pair against the account's
//! (`USDJPY`, `EURGBP`, `GBPJPY`, ...): the metals, the coins and every CFD stay refused by their own words.
//!
//! # The quantity grid
//!
//! Both the minimum and the step are [`SIDECAR_MIN_UNITS`]: the sidecar rejects any order under 1000 units
//! for EVERY instrument, so that is the smallest size the adapter can place, and a step of the same size
//! keeps every quantity a multiple of a unit the venue's own increment divides (the SDK default increment is
//! 1 and the vendor wiki's figure is 1, neither observed on an order). That is the currency pairs' grid; a
//! metal has none here (refused above).

use vike_model::SymbolProperties;

use crate::jforex_grid::{GridBasis, JforexInstrument, JforexKind, jforex_instruments};

/// The smallest order the sidecar places, in units (`OrderMap.MIN_UNITS`); a test holds the two equal.
pub const SIDECAR_MIN_UNITS: f64 = 1000.0;

/// The two table rows typed `FOREX` that are coins: the SDK's FOREX defaults (a 1000-unit minimum, a tick
/// scale of 1) are not statements about a coin's amount unit.
const COIN_ROWS: [&str; 2] = ["BTCUSD", "ETHUSD"];

/// The row `symbol` names: its enum spelling (`EURUSD`, `COPPERCMDUSD`) or the SDK's slashed pair
/// (`EUR/USD`, `COPPER.CMD/USD`), any case.
fn row_of(symbol: &str) -> Option<&'static JforexInstrument> {
    let key = symbol.trim().to_ascii_uppercase();
    jforex_instruments().iter().find(|r| r.symbol == key || r.pair == key)
}

/// The row `symbol` names when it is one this venue may be asked to trade at all, or the operator's sentence for
/// why not: every refusal of the module doc EXCEPT the quote currency's (the caller's rule: the account's own
/// money for [`instrument_facts`], any money for [`instrument_facts_quoted`]).
fn admissible_row(symbol: &str) -> Result<&'static JforexInstrument, String> {
    let Some(row) = row_of(symbol) else {
        return Err(format!(
            "dukascopy: `{symbol}` is not one of the {} instruments the committed JForex SDK table \
             carries, so there is no statement of its price grid (an instrument the SDK builds at run \
             time has none)",
            jforex_instruments().len()
        ));
    };
    match (row.kind, row.basis) {
        (JforexKind::Cfd, GridBasis::CfdPlaceholder) => {
            return Err(format!(
                "dukascopy: `{}` is a CFD whose pip in the SDK is its blanket default ({}), not a \
                 statement of its price grid, so no order can be rounded on it",
                row.pair, row.pip_value
            ));
        }
        (JforexKind::Cfd, GridBasis::SdkConstant) => {
            return Err(format!(
                "dukascopy: `{}` is a CFD: the sidecar stamps its fills `{}` (the enum name), which is \
                 not the name the order is placed under, so a fill would never match the symbol the \
                 engine trades; only a six-letter currency or metal pair round-trips",
                row.pair, row.symbol
            ));
        }
        _ => {}
    }
    if row.kind == JforexKind::Metal {
        return Err(format!(
            "dukascopy: `{}`: a metal's amount is in ounces (the venue's minimum is 1 oz of XAU/USD, 50 oz of \
             XAG/USD) and the sidecar's own floor is 1000 units, i.e. 1000 oz (~2.4M USD per minimum order \
             for gold); refused until a demo order proves a metal path",
            row.pair
        ));
    }
    if COIN_ROWS.contains(&row.symbol.as_str()) {
        return Err(format!(
            "dukascopy: `{}` is a coin typed FOREX in the SDK, and the SDK's FOREX amount defaults \
             are not a statement about a coin's unit",
            row.pair
        ));
    }
    Ok(row)
}

/// The currency a row is quoted in (`USD` for `EUR/USD`).
fn quote_currency_of(row: &JforexInstrument) -> &str {
    row.pair.rsplit('/').next().unwrap_or_default()
}

/// The grid of an admissible row: the SDK's price step and the sidecar's quantity floor.
fn grid_of(row: &JforexInstrument) -> Result<SymbolProperties, String> {
    let tick_size = row.tick_size();
    if !tick_size.is_finite() || tick_size <= 0.0 {
        return Err(format!(
            "dukascopy: `{}`: the table's tick scale gives no price step",
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
/// operator's sentence for why not. See the module doc for what is admitted and every refusal.
///
/// # Errors
/// One sentence per refusal, naming the instrument and the cause.
pub fn instrument_facts(symbol: &str, ledger_currency: &str) -> Result<SymbolProperties, String> {
    let row = admissible_row(symbol)?;
    let quote = quote_currency_of(row);
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
/// refusals (a CFD, a coin, a metal, a name the table lacks), but an instrument quoted in ANOTHER currency than
/// the account's is not refused for that alone. Returns the grid and the instrument's quote currency (upper-case),
/// and the caller compares it with the account's: equal = nothing to convert, otherwise the rate is
/// `crate::home_conversion::quote_rate`'s, and no rate is no admission.
///
/// # Errors
/// As [`instrument_facts`], minus the quote-currency refusal.
pub fn instrument_facts_quoted(symbol: &str) -> Result<(SymbolProperties, String), String> {
    let row = admissible_row(symbol)?;
    Ok((grid_of(row)?, quote_currency_of(row).to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fiat currency codes and metals the table's non-coin FOREX and METAL rows are made of.
    const LEGS: &[&str] = &[
        "AUD", "BRL", "CAD", "CHF", "CNH", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "ILS", "JPY",
        "MXN", "NOK", "NZD", "PLN", "RON", "RUB", "SEK", "SGD", "THB", "TRY", "USD", "XAG", "XAU",
        "ZAR",
    ];

    fn admitted() -> Vec<&'static str> {
        jforex_instruments()
            .iter()
            .filter(|r| instrument_facts(&r.symbol, "USD").is_ok())
            .map(|r| r.symbol.as_str())
            .collect()
    }

    /// **The whole admitted set, pinned**: the USD-quoted currency pairs, and nothing else.
    #[test]
    fn only_the_usd_quoted_currency_pairs_are_admitted() {
        assert_eq!(
            admitted(),
            ["AUDUSD", "EURUSD", "GBPUSD", "NZDUSD"],
            "a row added or lost is a reviewed edit"
        );
    }

    /// **The metals are refused by name until a demo order proves a metal path** (module doc, STOP-AND-REPORT): the
    /// unit is the vendor's documented ounce, and 1000 units of gold through the sidecar would be 1000 oz.
    #[test]
    fn the_metals_are_refused_until_a_demo_order_proves_a_metal_path() {
        for name in ["XAUUSD", "XAGUSD", "XAU/USD", "xag/usd"] {
            let why = instrument_facts(name, "USD").expect_err(name);
            assert!(why.contains("a metal's amount is in ounces"), "{name}: {why}");
            assert!(
                why.contains("1000 oz (~2.4M USD per minimum order for gold)"),
                "{name}: {why}"
            );
        }
        // A metal is refused whatever the ledger's money: the unit is the question, not the quote.
        assert!(
            instrument_facts("XAUUSD", "EUR").expect_err("XAUUSD").contains("a metal's amount")
        );
    }

    #[test]
    fn a_pair_is_answered_on_the_sdk_grid_and_the_sidecars_quantity_floor() {
        let eur = instrument_facts("EURUSD", "USD").expect("admitted");
        assert_eq!(eur.tick_size, 0.00001);
        assert_eq!((eur.step_size, eur.min_qty), (1000.0, 1000.0));
        assert_eq!((eur.max_qty, eur.min_notional, eur.contract_size), (0.0, 0.0, 0.0));
        // The SDK's slashed pair and any case name the same row.
        assert_eq!(instrument_facts(" eur/usd ", "USD"), Ok(eur));
    }

    /// Every row of the table is judged, and every refusal names its cause: nothing is admitted by omission.
    #[test]
    fn every_other_row_is_refused_with_its_own_reason() {
        for r in jforex_instruments() {
            let Err(why) = instrument_facts(&r.symbol, "USD") else { continue };
            let cause = match (r.kind, r.basis) {
                (JforexKind::Cfd, GridBasis::CfdPlaceholder) => "blanket default",
                (JforexKind::Cfd, GridBasis::SdkConstant) => "stamps its fills",
                (JforexKind::Metal, _) => "a metal's amount is in ounces",
                _ if COIN_ROWS.contains(&r.symbol.as_str()) => "a coin typed FOREX",
                _ => "nothing converts",
            };
            assert!(why.contains(cause), "{}: {why}", r.symbol);
        }
        let why = |s: &str| instrument_facts(s, "USD").expect_err(s);
        assert!(why("USDJPY").contains("quoted in JPY"));
        assert!(why("EURGBP").contains("quoted in GBP"));
        assert!(why("COPPER.CMD/USD").contains("COPPERCMDUSD"));
        assert!(why("AAPLUSUSD").contains("blanket default"));
        assert!(why("GBPNOK").contains("not one of the 180 instruments"), "{}", why("GBPNOK"));
        assert!(why("").contains("not one of"));
        // A ledger in another money admits the pairs quoted in it, and only those.
        assert!(instrument_facts("EURGBP", "GBP").is_ok());
        assert!(instrument_facts("EURUSD", "GBP").is_err());
    }

    /// **The converting answer is the unconverting one with the quote-currency refusal lifted, and nothing else**
    /// (decision 0183): for every row of the table, [`instrument_facts_quoted`] is refused exactly when no
    /// ledger money could admit the row, and when it is answered its grid equals the one
    /// [`instrument_facts`] gives a ledger in that row's OWN money; and every row the unconverting lane admits on
    /// a USD ledger is answered, with the same grid, as quoted in USD.
    #[test]
    fn the_converting_answer_lifts_only_the_quote_currency_refusal() {
        let mut opened = Vec::new();
        for r in jforex_instruments() {
            match instrument_facts_quoted(&r.symbol) {
                Ok((props, quote)) => {
                    assert_eq!(quote, r.pair.rsplit('/').next().unwrap(), "{}", r.symbol);
                    assert_eq!(instrument_facts(&r.symbol, &quote), Ok(props), "{}", r.symbol);
                    opened.push(r.symbol.as_str());
                }
                Err(why) => {
                    // refused for the instrument itself: no ledger money admits it
                    for money in ["USD", "EUR", "JPY", "GBP"] {
                        assert_eq!(
                            instrument_facts(&r.symbol, money).unwrap_err(),
                            why,
                            "{}",
                            r.symbol
                        );
                    }
                }
            }
        }
        // 72 FOREX rows less the two coins; the metals, every CFD and the names the table lacks stay refused
        assert_eq!(opened.len(), 70, "{opened:?}");
        for must in ["USDJPY", "EURGBP", "GBPJPY", "AUDUSD", "EURUSD"] {
            assert!(opened.contains(&must), "{must}");
        }
        for never in ["XAUUSD", "XAGUSD", "BTCUSD", "ETHUSD", "AAPLUSUSD", "COPPERCMDUSD"] {
            assert!(!opened.contains(&never), "{never}");
        }
        let (usd, quote) = instrument_facts_quoted("EURUSD").expect("admitted");
        assert_eq!(
            (usd.tick_size, usd.step_size, usd.min_qty, quote.as_str()),
            (0.00001, 1000.0, 1000.0, "USD")
        );
        // the SDK's slashed spelling and any case are the same row, as for the unconverting lane
        assert_eq!(instrument_facts_quoted(" usd/jpy "), instrument_facts_quoted("USDJPY"));
        assert_eq!(
            instrument_facts_quoted("USDJPY").map(|(p, q)| (p.tick_size, q)),
            Ok((0.001, "JPY".to_string()))
        );
    }

    /// The admitted rows are exactly the ones the sidecar round-trips: six letters, which its order path
    /// splits to `AAA/BBB` and its fills spell back unsplit.
    #[test]
    fn an_admitted_row_is_a_six_letter_pair_the_sidecar_round_trips() {
        for symbol in admitted() {
            let row = row_of(symbol).expect("a row");
            assert!(
                symbol.len() == 6 && symbol.bytes().all(|b| b.is_ascii_uppercase()),
                "{symbol}"
            );
            assert_eq!(row.pair, format!("{}/{}", &symbol[..3], &symbol[3..]), "{symbol}");
        }
    }

    /// The two coin rows are the ONLY currency-or-metal rows whose legs are not on the known list, so the
    /// exclusion cannot silently miss a third.
    #[test]
    fn the_coin_rows_are_the_only_rows_with_a_leg_that_is_not_a_known_currency_or_metal() {
        let odd: Vec<&str> = jforex_instruments()
            .iter()
            .filter(|r| r.kind != JforexKind::Cfd)
            .filter(|r| !LEGS.contains(&&r.symbol[..3]) || !LEGS.contains(&&r.symbol[3..]))
            .map(|r| r.symbol.as_str())
            .collect();
        assert_eq!(odd, COIN_ROWS);
    }

    /// The floor is the sidecar's own `OrderMap.MIN_UNITS`. The Java is read at run time (the public mirror
    /// ships no `jforex-bridge/` sources a test may `include_str!`), and a checkout without it says so.
    #[test]
    fn the_quantity_floor_is_the_sidecars_min_units() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("jforex-bridge/src/main/java/vike/jforex/OrderMap.java");
        let Ok(java) = std::fs::read_to_string(&path) else {
            eprintln!("SKIPPED: {} is not in this checkout", path.display());
            return;
        };
        let line = java
            .lines()
            .find(|l| l.contains("double MIN_UNITS"))
            .expect("OrderMap.java declares MIN_UNITS");
        let value = line.split('=').nth(1).and_then(|v| v.split(';').next()).map(str::trim);
        assert_eq!(value.and_then(|v| v.parse::<f64>().ok()), Some(SIDECAR_MIN_UNITS), "{line}");
    }
}
