//! `crate::instrument::any_symbol`'s tests: the static answers of a currency pair, the basis of every other row, the refusals by
//! name, the wire name each answer is asked by, and the facts a metal or a CFD is admitted on from the venue's terms (`facts_from_terms`). The
//! account's list is the recorded-shape fixture (`crate::test_support::listed`: the 180 instruments the SDK predefines, with
//! the figures the committed SDK table carried, EUR/THB at the server's pip), so every count here is the one the table gave.
//! The terms in the goldens are the VENUE-DOCUMENTED figures (the vendor's order-sizes table: XAU/USD 1 step 1,
//! XAG/USD 50 step 10, USA30.IDX/USD 0.1 step 0.1, COPPER.CMD/USD 20 step 1) on the SDK's own pip scales; the demo
//! account has not stated them yet (`docs/ops/known-limitations-2026-10.md`'s Dukascopy metals and CFDs row).

use std::assert_matches;
use std::sync::OnceLock;

use super::*;
use crate::refusals::DukascopyPropertiesRefusalId;

/// The account's instrument list every test here reads.
fn table() -> &'static [JforexInstrument] {
    static ROWS: OnceLock<Vec<JforexInstrument>> = OnceLock::new();
    ROWS.get_or_init(crate::test_support::listed)
}

/// The two coins of the list (the venue's `CRYPTO_CURR` group), by their wire spelling.
const COINS: [&str; 2] = ["BTCUSD", "ETHUSD"];

/// The fiat currency codes and metals the table's non-coin FOREX and METAL rows are made of.
const LEGS: &[&str] = &[
    "AUD", "BRL", "CAD", "CHF", "CNH", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "ILS", "JPY",
    "MXN", "NOK", "NZD", "PLN", "RON", "RUB", "SEK", "SGD", "THB", "TRY", "USD", "XAG", "XAU",
    "ZAR",
];

fn admitted() -> Vec<&'static str> {
    table()
        .iter()
        .filter(|r| instrument_facts(table(), &r.symbol, "USD").is_ok())
        .map(|r| r.symbol.as_str())
        .collect()
}

fn row(symbol: &str) -> &'static JforexInstrument {
    row_of(table(), symbol).unwrap_or_else(|| panic!("{symbol} is a row"))
}

/// Terms as the sidecar writes them for `symbol`: the row's class and pip scales, the given amounts.
fn terms(symbol: &str, (min, step): (f64, f64)) -> VenueTerms {
    let r = row(symbol);
    VenueTerms {
        symbol: r.symbol.clone(),
        class: match r.kind {
            InstrumentType::Forex => "FOREX",
            InstrumentType::Metal => "METAL",
            InstrumentType::Cfd => "CFD",
        }
        .into(),
        min: Some(min),
        step: Some(step),
        max: Some(5.0e8),
        per_contract: Some(1.0),
        pip: Some(r.pip_value),
        pip_scale: Some(r.pip_scale),
        tick_scale: Some(r.tick_scale),
        quote: Some(r.quote().to_string()),
    }
}

// ---- the static answers: a currency pair ----------------------------------------------------------------------

/// **The whole static set, pinned**: the USD-quoted currency pairs, and nothing else.
#[test]
fn only_the_usd_quoted_currency_pairs_are_admitted_statically() {
    assert_eq!(
        admitted(),
        ["AUDUSD", "EURUSD", "GBPUSD", "NZDUSD"],
        "a row added or lost is a reviewed edit"
    );
}

#[test]
fn a_pair_is_answered_on_the_sdk_grid_and_the_sidecars_quantity_floor() {
    let eur = instrument_facts(table(), "EURUSD", "USD").expect("admitted");
    assert_eq!(eur.tick_size, 0.00001);
    assert_eq!((eur.step_size, eur.min_qty), (1000.0, 1000.0));
    assert_eq!((eur.max_qty, eur.min_notional, eur.contract_size), (0.0, 0.0, 0.0));
    // Asked by the WIRE name exactly (the kept row's, decision 0290): no other spelling is read for the row.
    for other in [" eur/usd ", "EUR/USD", "eurusd"] {
        assert_eq!(
            instrument_facts_refused(table(), other, "USD"),
            Err(DukascopyPropertiesRefusal::NotOnList {
                wire: other.to_string(),
                count: table().len()
            })
        );
    }
}

/// **The converting answer is the unconverting one with the quote-currency refusal lifted, and nothing else**
/// (decision 0183): for every row of the table, [`instrument_facts_quoted`] is refused exactly when no ledger money
/// could admit the row, and when it is answered its grid equals the one [`instrument_facts`] gives a ledger in that
/// row's OWN money; and every row the unconverting lane admits on a USD ledger is answered, with the same grid, as
/// quoted in USD.
#[test]
fn the_converting_answer_lifts_only_the_quote_currency_refusal() {
    let mut opened = Vec::new();
    for r in table() {
        match instrument_facts_quoted(table(), &r.symbol) {
            Ok((properties, quote)) => {
                assert_eq!(quote, r.pair.rsplit('/').next().unwrap(), "{}", r.symbol);
                assert_eq!(
                    instrument_facts(table(), &r.symbol, &quote),
                    Ok(properties),
                    "{}",
                    r.symbol
                );
                opened.push(r.symbol.as_str());
            }
            Err(why) => {
                // refused for the instrument itself: no ledger money admits it
                for money in ["USD", "EUR", "JPY", "GBP"] {
                    assert_eq!(
                        instrument_facts_refused(table(), &r.symbol, money).unwrap_err(),
                        why,
                        "{}",
                        r.symbol
                    );
                }
            }
        }
    }
    // 72 FOREX rows less the two coins; every metal and CFD is the venue's terms' (below), the names the table lacks refused
    assert_eq!(opened.len(), 70, "{opened:?}");
    for must in ["USDJPY", "EURGBP", "GBPJPY", "AUDUSD", "EURUSD"] {
        assert!(opened.contains(&must), "{must}");
    }
    for never in ["XAUUSD", "XAGUSD", "BTCUSD", "ETHUSD", "AAPL.US/USD", "COPPER.CMD/USD"] {
        assert!(!opened.contains(&never), "{never}");
    }
    let (usd, quote) = instrument_facts_quoted(table(), "EURUSD").expect("admitted");
    assert_eq!(
        (usd.tick_size, usd.step_size, usd.min_qty, quote.as_str()),
        (0.00001, 1000.0, 1000.0, "USD")
    );
    assert_eq!(
        instrument_facts_quoted(table(), "USDJPY").map(|(p, q)| (p.tick_size, q)),
        Ok((0.001, "JPY".to_string()))
    );
}

/// The admitted pairs are exactly the ones the sidecar round-trips: six letters, which its order path splits to
/// `AAA/BBB` and its fills spell back unsplit.
#[test]
fn an_admitted_pair_is_a_six_letter_pair_the_sidecar_round_trips() {
    for symbol in admitted() {
        let row = row_of(table(), symbol).expect("a row");
        assert!(symbol.len() == 6 && symbol.bytes().all(|b| b.is_ascii_uppercase()), "{symbol}");
        assert_eq!(row.pair, format!("{}/{}", &symbol[..3], &symbol[3..]), "{symbol}");
        assert_eq!(row.symbol, symbol);
    }
}

/// The coins are refused by their row's CLASS (the venue's group), and they are the ONLY currency-or-metal rows whose legs
/// are not on the known list, so the class cannot silently miss a coin the list types as a pair.
#[test]
fn the_coin_rows_are_the_only_rows_with_a_leg_that_is_not_a_known_currency_or_metal() {
    let odd: Vec<&str> = table()
        .iter()
        .filter(|r| r.kind != InstrumentType::Cfd)
        .filter(|r| !LEGS.contains(&&r.symbol[..3]) || !LEGS.contains(&&r.symbol[3..]))
        .map(|r| r.symbol.as_str())
        .collect();
    assert_eq!(odd, COINS);
    let coins: Vec<&str> =
        table().iter().filter(|r| r.is_coin()).map(|r| r.symbol.as_str()).collect();
    assert_eq!(coins, COINS, "the class decides, not the name");
}

/// The floor is the sidecar's own `OrderMap.MIN_UNITS`. The Java is read at run time (the public mirror ships no
/// `jforex-bridge/` sources a test may `include_str!`), and a checkout without it says so.
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

// ---- the basis of every row, and the refusals by name ----------------------------------------------------------

/// **Every row of the table is judged, and every refusal names its cause**: nothing is admitted by omission. A row is
/// answered statically (a currency pair), from the venue's terms (a metal or a CFD) or refused (a coin, a CFD in pence).
#[test]
fn every_row_has_a_basis_or_a_refusal_with_its_own_reason() {
    let (mut statics, mut from_terms, mut refused) = (0, 0, Vec::new());
    for r in table() {
        match facts_basis(table(), &r.symbol) {
            Ok(DukascopyPropertiesBasis::Static) => {
                assert_eq!(r.kind, InstrumentType::Forex, "{}", r.symbol);
                statics += 1;
            }
            Ok(DukascopyPropertiesBasis::VenueTerms(row)) => {
                assert_ne!(row.kind, InstrumentType::Forex, "{}", r.symbol);
                assert_eq!(&row, r);
                from_terms += 1;
                // and the static answers say so, in the same breath
                let why = instrument_facts_refused(table(), &r.symbol, "USD").expect_err(&r.symbol);
                assert_eq!(why.id(), DukascopyPropertiesRefusalId::NeedsVenueTerms, "{why}");
                assert!(instrument_facts_quoted(table(), &r.symbol).is_err(), "{}", r.symbol);
            }
            Err(why) => {
                let cause = if COINS.contains(&r.symbol.as_str()) {
                    DukascopyPropertiesRefusalId::Coin
                } else {
                    DukascopyPropertiesRefusalId::MinorUnit
                };
                assert_eq!(why.id(), cause, "{}: {why}", r.symbol);
                refused.push(r.symbol.as_str());
            }
        }
    }
    // 72 FOREX rows less the two coins; the two metals and the 106 CFDs less the two quoted in pence; four refused
    assert_eq!((statics, from_terms), (70, 106), "a row added or lost is a reviewed edit");
    // (a CFD's row is spelled with its wire name, the SDK's slashed one)
    assert_eq!(refused, ["BTCUSD", "ETHUSD", "GSK.GB/GBX", "LSE.GB/GBX"]);
}

#[test]
fn the_metals_and_the_stock_and_index_cfds_are_admissible_by_their_wire_name() {
    for wire in [
        "XAUUSD",
        "XAGUSD",
        "COPPER.CMD/USD",
        "AAPL.US/USD",
        "DEU.IDX/EUR",
        "BMW.DE/EUR",
        "A.US/USD",
    ] {
        let basis = facts_basis(table(), wire).unwrap_or_else(|e| panic!("{wire}: {e}"));
        assert_eq!(basis.wire_symbol().as_deref(), Some(wire), "{wire}");
    }
    assert_eq!(facts_basis(table(), "EURUSD"), Ok(DukascopyPropertiesBasis::Static));
    assert_eq!(facts_basis(table(), "EURUSD").unwrap().wire_symbol(), None);
}

/// **No other spelling of a row is read** (decision 0290: the caller asks by the kept row's wire name): the SDK's
/// enum-style name of a CFD (`AUSUSD`, the enum name of `A.US/USD`, is a six-letter shape, so a pair-shaped guess cannot
/// slip through), the slashed spelling of a pair or metal and another case are on no row of the list.
#[test]
fn no_other_spelling_of_a_row_is_read() {
    for bad in ["COPPERCMDUSD", "AAPLUSUSD", "AUSUSD", "deuidxeur", "XAU/USD", "copper.cmd/usd"] {
        let why = facts_basis(table(), bad).expect_err(bad);
        assert_eq!(why.id(), DukascopyPropertiesRefusalId::NotOnList, "{bad}: {why}");
        assert!(
            instrument_facts(table(), bad, "USD").is_err()
                && instrument_facts_quoted(table(), bad).is_err(),
            "{bad}"
        );
    }
    // the wire name of every row is the sidecar's spelling of the SDK's name (`crate::instrument::names`' one rule)
    for r in table() {
        assert_eq!(crate::instrument::names::sidecar_spelling(&r.pair), r.symbol, "{}", r.pair);
    }
}

/// **What the sidecar's one rule for a slashed name rests on** (`crate::instrument::names`' `sidecar_spelling`, the
/// Java `OrderMap.canonicalSymbol`: the slash of a pair of two three-letter codes is dropped, any other name keeps it):
/// every CFD of the table carries a dot, and every other row is a three-by-three pair, so a pair or a metal is six
/// letters on the wire and a CFD its slashed name.
#[test]
fn every_slashed_name_of_the_table_is_a_pair_of_three_letter_codes_or_carries_a_dot() {
    for r in table() {
        let (code, currency) = r.pair.split_once('/').expect("a slashed name");
        assert_eq!(code.contains('.'), r.kind == InstrumentType::Cfd, "{}", r.pair);
        if r.kind != InstrumentType::Cfd {
            assert_eq!((code.len(), currency.len()), (3, 3), "{}", r.pair);
        }
    }
}

/// **The wire spelling is the sidecar's, by one rule** (DUK-7): `crate::instrument::names::sidecar_spelling` of every
/// row's SDK name is the symbol the sidecar wrote for it (the line's `symbol`, `OrderMap.canonicalSymbol`'s output), the
/// row is found under exactly that wire name, and a coin of a four-letter code keeps its slash as the Java's
/// `[A-Z]{3}/[A-Z]{3}` does. That the Java still spells by the rule is `names_tests.rs`'s `the_rust_rule_is_the_java_rule`.
#[test]
fn the_wire_spelling_is_the_sidecars() {
    use crate::instrument::names::sidecar_spelling;
    for r in table() {
        assert_eq!(sidecar_spelling(&r.pair), r.symbol, "{}", r.pair);
        assert_eq!(row_of(table(), &r.symbol).map(|hit| hit.pair.as_str()), Some(r.pair.as_str()));
    }
    for (sdk, wire) in [
        ("EUR/USD", "EURUSD"),
        ("XAU/USD", "XAUUSD"),
        ("DOGE/USD", "DOGE/USD"),
        ("COPPER.CMD/USD", "COPPER.CMD/USD"),
        ("EURUSD", "EURUSD"),
    ] {
        assert_eq!(sidecar_spelling(sdk), wire, "{sdk}");
    }
}

#[test]
fn a_cfd_quoted_in_pence_a_coin_and_a_name_the_table_lacks_are_refused_in_words() {
    for pence in ["LSE.GB/GBX", "GSK.GB/GBX"] {
        let why = facts_basis(table(), pence).expect_err(pence);
        assert_matches!(
            &why,
            DukascopyPropertiesRefusal::MinorUnit { pair, unit_words }
                if pair == pence && unit_words.starts_with("pence ("),
            "{pence}: {why}"
        );
    }
    for coin in ["BTCUSD", "ETHUSD"] {
        let why = facts_basis(table(), coin).expect_err(coin);
        assert_matches!(
            &why,
            DukascopyPropertiesRefusal::Coin { kind: "FOREX", .. },
            "{coin}: {why}"
        );
    }
    let not_on_list =
        |wire: &str| DukascopyPropertiesRefusal::NotOnList { wire: wire.to_string(), count: 180 };
    assert_eq!(facts_basis(table(), "USTBOND.TR/USD"), Err(not_on_list("USTBOND.TR/USD")));
    assert_eq!(facts_basis(table(), ""), Err(not_on_list("")));
    assert_eq!(instrument_facts_refused(table(), "GBPNOK", "USD"), Err(not_on_list("GBPNOK")));
    // no list at all is no admission, never a default
    assert_eq!(
        facts_basis(&[], "EURUSD"),
        Err(DukascopyPropertiesRefusal::NotOnList { wire: "EURUSD".to_string(), count: 0 })
    );
    let quoted_in = |pair: &str, quote: &str| DukascopyPropertiesRefusal::QuoteNotLedger {
        pair: pair.to_string(),
        quote: quote.to_string(),
        ledger: "USD".to_string(),
    };
    assert_eq!(
        instrument_facts_refused(table(), "USDJPY", "USD"),
        Err(quoted_in("USD/JPY", "JPY"))
    );
    assert_eq!(
        instrument_facts_refused(table(), "EURGBP", "USD"),
        Err(quoted_in("EUR/GBP", "GBP"))
    );
    // A ledger in another money admits the pairs quoted in it, and only those.
    assert!(instrument_facts(table(), "EURGBP", "GBP").is_ok());
    assert!(instrument_facts(table(), "EURUSD", "GBP").is_err());
}

/// A CFD is judged on the venue's terms whatever pip its row holds (its sizes are the venue's, read after subscription),
/// and a metal too.
#[test]
fn a_cfd_is_judged_on_the_venues_terms_whatever_pip_the_list_holds() {
    let pips: Vec<f64> =
        ["AAPL.US/USD", "COPPER.CMD/USD"].iter().map(|n| row(n).pip_value).collect();
    assert_eq!(pips, [0.01, 0.001]);
    for name in ["COPPER.CMD/USD", "GAS.CMD/USD", "PRT.IDX/EUR", "AAPL.US/USD", "XAUUSD"] {
        assert!(
            matches!(facts_basis(table(), name), Ok(DukascopyPropertiesBasis::VenueTerms(_))),
            "{name}"
        );
    }
}

// ---- the facts a metal or a CFD is admitted on: the venue's own terms -------------------------------------------

/// **The goldens**: what the venue's documented figures give on the SDK's own pip scales.
#[test]
fn a_metal_and_a_cfd_are_answered_from_the_venues_terms_in_the_units_of_a_qty() {
    // XAU/USD: 1 oz, step 1; the SDK's pip 0.01 / tick scale 3
    let (gold, quote) =
        facts_from_terms(row("XAUUSD"), &terms("XAUUSD", (1.0, 1.0))).expect("gold");
    assert_eq!(
        (gold.tick_size, gold.min_qty, gold.step_size, gold.max_qty),
        (0.001, 1.0, 1.0, 5.0e8)
    );
    assert_eq!((gold.min_notional, gold.contract_size, quote.as_str()), (0.0, 0.0, "USD"));
    // XAG/USD: 50 oz, step 10 (the minimum is five increments)
    let (silver, _) =
        facts_from_terms(row("XAGUSD"), &terms("XAGUSD", (50.0, 10.0))).expect("silver");
    assert_eq!((silver.min_qty, silver.step_size), (50.0, 10.0));
    // a commodity CFD: 20 contracts, step 1, on the SDK's real pip of 0.001 (tick scale 4)
    let (copper, _) =
        facts_from_terms(row("COPPER.CMD/USD"), &terms("COPPER.CMD/USD", (20.0, 1.0)))
            .expect("copper");
    assert_eq!((copper.min_qty, copper.step_size, copper.tick_size), (20.0, 1.0, 0.0001));
    // an index CFD quoted in euros: 0.1 step 0.1 (a fractional step is a whole number of increments)
    let (index, quote) =
        facts_from_terms(row("DEU.IDX/EUR"), &terms("DEU.IDX/EUR", (0.1, 0.1))).expect("index");
    assert_eq!(
        (index.min_qty, index.step_size, index.tick_size, quote.as_str()),
        (0.1, 0.1, 0.001, "EUR")
    );
    // a stock CFD quoted in euros keeps its quote currency for the rate
    let (_, quote) =
        facts_from_terms(row("BMW.DE/EUR"), &terms("BMW.DE/EUR", (1.0, 1.0))).expect("stock");
    assert_eq!(quote, "EUR");
}

/// Every metal and CFD row is answerable from terms shaped like its own table row (the SDK defaults, which the venue
/// may override): no row is refused by the reading itself for lack of a figure the SDK always has.
#[test]
fn every_admissible_metal_and_cfd_row_reads_from_default_shaped_terms() {
    for r in table() {
        let Ok(DukascopyPropertiesBasis::VenueTerms(row)) = facts_basis(table(), &r.symbol) else {
            continue;
        };
        let (properties, quote) = facts_from_terms(&row, &terms(&r.symbol, (1.0, 1.0)))
            .unwrap_or_else(|e| panic!("{}: {e}", r.symbol));
        assert_eq!(properties.tick_size, r.tick_size(), "{}", r.symbol);
        assert_eq!(quote, r.pair.rsplit('/').next().unwrap(), "{}", r.symbol);
    }
}

/// **Nothing is guessed**: each thing the venue's terms can get wrong refuses the instrument, with the figure named.
#[test]
fn terms_that_do_not_admit_an_instrument_refuse_it_in_words() {
    let gold = row("XAUUSD");
    let good = terms("XAUUSD", (1.0, 1.0));
    let refused = |mutate: &dyn Fn(&mut VenueTerms)| {
        let mut t = good.clone();
        mutate(&mut t);
        facts_from_terms(gold, &t).expect_err("refused")
    };
    let name = || "XAU/USD".to_string();
    let not_usable =
        |what, figure| DukascopyPropertiesRefusal::TermsNotUsable { name: name(), what, figure };
    assert_eq!(refused(&|t| t.min = None), not_usable("minimum amount", None));
    assert_eq!(refused(&|t| t.min = Some(0.0)), not_usable("minimum amount", Some(0.0)));
    assert_matches!(
        refused(&|t| t.step = Some(f64::NAN)),
        DukascopyPropertiesRefusal::TermsNotUsable { what: "amount increment", figure: Some(f), .. } if f.is_nan()
    );
    assert_eq!(refused(&|t| t.per_contract = None), not_usable("amount per contract", None));
    assert_eq!(refused(&|t| t.pip = Some(-0.01)), not_usable("pip", Some(-0.01)));
    // more than one unit of the underlying per contract: a contract's worth per point is unmeasured
    for per_contract in [100.0, 0.5] {
        assert_eq!(
            refused(&|t| t.per_contract = Some(per_contract)),
            DukascopyPropertiesRefusal::PerContract { name: name(), per_contract }
        );
    }
    for stated in ["CFD", ""] {
        assert_eq!(
            refused(&|t| t.class = stated.into()),
            DukascopyPropertiesRefusal::TermsClass {
                name: name(),
                stated: stated.to_string(),
                class: "METAL"
            }
        );
    }
    assert_eq!(
        refused(&|t| t.pip_scale = None),
        DukascopyPropertiesRefusal::TermsNoScale { name: name() }
    );
    assert_eq!(
        refused(&|t| t.tick_scale = None),
        DukascopyPropertiesRefusal::TermsNoScale { name: name() }
    );
    for tick_scale in [2, 13] {
        assert_eq!(
            refused(&|t| t.tick_scale = Some(tick_scale)),
            DukascopyPropertiesRefusal::TermsGrid {
                name: name(),
                pip_scale: gold.pip_scale,
                tick_scale,
                pip: gold.pip_value
            }
        );
    }
    // 1.5 is not a whole number of increments of 1.0 ... and 50 is not of 30
    assert_eq!(
        refused(&|t| t.min = Some(1.5)),
        DukascopyPropertiesRefusal::MinNotWhole { name: name(), min: 1.5, step: 1.0 }
    );
    assert_eq!(
        refused(&|t| {
            t.min = Some(50.0);
            t.step = Some(30.0)
        }),
        DukascopyPropertiesRefusal::MinNotWhole { name: name(), min: 50.0, step: 30.0 }
    );
    assert_eq!(
        refused(&|t| {
            t.min = Some(10.0);
            t.max = Some(5.0)
        }),
        DukascopyPropertiesRefusal::MaxBelowMin { name: name(), max: 5.0, min: 10.0 }
    );
    assert_eq!(
        refused(&|t| t.quote = Some("EUR".into())),
        DukascopyPropertiesRefusal::TermsQuote {
            name: name(),
            stated: "EUR".to_string(),
            quote: "USD".to_string()
        }
    );
    // no maximum stated is no maximum (the engine's own ceilings judge a size); an absent quote reads the table's
    let mut open = good.clone();
    (open.max, open.quote) = (None, None);
    let (properties, quote) = facts_from_terms(gold, &open).expect("no maximum is fine");
    assert_eq!((properties.max_qty, quote.as_str()), (0.0, "USD"));
    // a stated quote in another case is the same currency
    let mut lower = good;
    lower.quote = Some(" usd ".into());
    assert!(facts_from_terms(gold, &lower).is_ok());
}
