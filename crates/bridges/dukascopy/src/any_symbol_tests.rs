//! `crate::any_symbol`'s tests: the static answers of a currency pair, the basis of every other row, the refusals by
//! name, the wire spelling, and the facts a metal or a CFD is admitted on from the venue's terms (`facts_from_terms`).
//! The terms in the goldens are the VENUE-DOCUMENTED figures (the vendor's order-sizes table: XAU/USD 1 step 1,
//! XAG/USD 50 step 10, USA30.IDX/USD 0.1 step 0.1, COPPER.CMD/USD 20 step 1) on the SDK's own pip scales; the demo
//! account has not stated them yet (`docs/ops/known-limitations-2026-10.md`'s Dukascopy metals and CFDs row).

use super::*;
use crate::jforex_grid::GridBasis;

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

fn row(symbol: &str) -> &'static JforexInstrument {
    row_of(symbol).unwrap_or_else(|| panic!("{symbol} is a row"))
}

/// Terms as the sidecar writes them for `symbol`: the row's class and pip scales, the given amounts.
fn terms(symbol: &str, (min, step): (f64, f64)) -> VenueTerms {
    let r = row(symbol);
    VenueTerms {
        symbol: wire_symbol(r),
        class: match r.kind {
            JforexKind::Forex => "FOREX",
            JforexKind::Metal => "METAL",
            JforexKind::Cfd => "CFD",
        }
        .into(),
        min: Some(min),
        step: Some(step),
        max: Some(5.0e8),
        per_contract: Some(1.0),
        pip: Some(r.pip_value),
        pip_scale: Some(r.pip_scale),
        tick_scale: Some(r.tick_scale),
        quote: Some(quote_currency_of(r).to_string()),
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
    let eur = instrument_facts("EURUSD", "USD").expect("admitted");
    assert_eq!(eur.tick_size, 0.00001);
    assert_eq!((eur.step_size, eur.min_qty), (1000.0, 1000.0));
    assert_eq!((eur.max_qty, eur.min_notional, eur.contract_size), (0.0, 0.0, 0.0));
    // The SDK's slashed pair and any case name the same row.
    assert_eq!(instrument_facts(" eur/usd ", "USD"), Ok(eur));
}

/// **The converting answer is the unconverting one with the quote-currency refusal lifted, and nothing else**
/// (decision 0183): for every row of the table, [`instrument_facts_quoted`] is refused exactly when no ledger money
/// could admit the row, and when it is answered its grid equals the one [`instrument_facts`] gives a ledger in that
/// row's OWN money; and every row the unconverting lane admits on a USD ledger is answered, with the same grid, as
/// quoted in USD.
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
    // 72 FOREX rows less the two coins; every metal and CFD is the venue's terms' (below), the names the table lacks refused
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

/// The admitted pairs are exactly the ones the sidecar round-trips: six letters, which its order path splits to
/// `AAA/BBB` and its fills spell back unsplit.
#[test]
fn an_admitted_pair_is_a_six_letter_pair_the_sidecar_round_trips() {
    for symbol in admitted() {
        let row = row_of(symbol).expect("a row");
        assert!(symbol.len() == 6 && symbol.bytes().all(|b| b.is_ascii_uppercase()), "{symbol}");
        assert_eq!(row.pair, format!("{}/{}", &symbol[..3], &symbol[3..]), "{symbol}");
        assert_eq!(wire_symbol(row), symbol);
    }
}

/// The two coin rows are the ONLY currency-or-metal rows whose legs are not on the known list, so the exclusion cannot
/// silently miss a third.
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
    for r in jforex_instruments() {
        match facts_basis(&wire_symbol(r)) {
            Ok(FactsBasis::Static) => {
                assert_eq!(r.kind, JforexKind::Forex, "{}", r.symbol);
                statics += 1;
            }
            Ok(FactsBasis::VenueTerms(row)) => {
                assert_ne!(row.kind, JforexKind::Forex, "{}", r.symbol);
                assert_eq!(row, r);
                from_terms += 1;
                // and the static answers say so, in the same breath
                let why = instrument_facts(&wire_symbol(r), "USD").expect_err(&r.symbol);
                assert!(why.contains("the venue's own"), "{}: {why}", r.symbol);
                assert!(instrument_facts_quoted(&wire_symbol(r)).is_err(), "{}", r.symbol);
            }
            Err(why) => {
                let cause = if COIN_ROWS.contains(&r.symbol.as_str()) {
                    "a coin typed FOREX"
                } else {
                    "in pence"
                };
                assert!(why.contains(cause), "{}: {why}", r.symbol);
                refused.push(r.symbol.as_str());
            }
        }
    }
    // 72 FOREX rows less the two coins; the two metals and the 106 CFDs less the two quoted in pence; four refused
    assert_eq!((statics, from_terms), (70, 106), "a row added or lost is a reviewed edit");
    assert_eq!(refused, ["BTCUSD", "ETHUSD", "GSKGBGBX", "LSEGBGBX"]);
}

#[test]
fn the_metals_and_the_stock_and_index_cfds_are_admissible_by_their_wire_spelling() {
    for (spelling, wire) in [
        ("XAUUSD", "XAUUSD"),
        ("xau/usd", "XAUUSD"),
        ("XAG/USD", "XAGUSD"),
        ("COPPER.CMD/USD", "COPPER.CMD/USD"),
        ("copper.cmd/usd", "COPPER.CMD/USD"),
        ("AAPL.US/USD", "AAPL.US/USD"),
        ("DEU.IDX/EUR", "DEU.IDX/EUR"),
        ("BMW.DE/EUR", "BMW.DE/EUR"),
        ("A.US/USD", "A.US/USD"),
    ] {
        let basis = facts_basis(spelling).unwrap_or_else(|e| panic!("{spelling}: {e}"));
        assert_eq!(basis.wire_symbol().as_deref(), Some(wire), "{spelling}");
    }
    assert_eq!(facts_basis("EURUSD"), Ok(FactsBasis::Static));
    assert_eq!(facts_basis("EURUSD").unwrap().wire_symbol(), None);
}

/// **The fill spelling round trip, the door's half**: the enum-style name of a CFD is refused by name (the sidecar
/// cannot resolve it and would stamp the fills under another spelling), with the spelling that works. `AUSUSD` is the
/// enum name of `A.US/USD` and is a six-letter shape, so a pair-shaped guess cannot slip through.
#[test]
fn the_enum_style_name_of_a_cfd_is_refused_with_the_spelling_that_works() {
    for (bad, good) in [
        ("COPPERCMDUSD", "COPPER.CMD/USD"),
        ("AAPLUSUSD", "AAPL.US/USD"),
        ("AUSUSD", "A.US/USD"),
        ("deuidxeur", "DEU.IDX/EUR"),
    ] {
        let why = facts_basis(bad).expect_err(bad);
        assert!(
            why.contains("enum-style name")
                && why.contains(&format!("write the instrument `{good}`")),
            "{bad}: {why}"
        );
        assert!(
            instrument_facts(bad, "USD").is_err() && instrument_facts_quoted(bad).is_err(),
            "{bad}"
        );
    }
    // the wire spelling of every CFD row is its slashed name, and every metal's and pair's its six letters
    for r in jforex_instruments() {
        let wire = wire_symbol(r);
        match r.kind {
            JforexKind::Cfd => assert_eq!(wire, r.pair),
            _ => assert_eq!((wire.len(), wire.contains('/')), (6, false), "{wire}"),
        }
    }
}

#[test]
fn a_cfd_quoted_in_pence_a_coin_and_a_name_the_table_lacks_are_refused_in_words() {
    for pence in ["LSE.GB/GBX", "GSK.GB/GBX"] {
        let why = facts_basis(pence).expect_err(pence);
        assert!(why.contains("quoted in pence") && why.contains("hundred times"), "{pence}: {why}");
    }
    for coin in ["BTCUSD", "ETH/USD"] {
        assert!(facts_basis(coin).expect_err(coin).contains("a coin typed FOREX"), "{coin}");
    }
    let why = facts_basis("USTBOND.TR/USD").expect_err("a CFD the SDK builds at run time");
    assert!(
        why.contains("not one of the 180 instruments") && why.contains("builds at run time"),
        "{why}"
    );
    assert!(facts_basis("").expect_err("empty").contains("not one of"));
    assert!(
        instrument_facts("GBPNOK", "USD")
            .expect_err("GBPNOK")
            .contains("not one of the 180 instruments")
    );
    let why = |s: &str| instrument_facts(s, "USD").expect_err(s);
    assert!(why("USDJPY").contains("quoted in JPY"));
    assert!(why("EURGBP").contains("quoted in GBP"));
    // A ledger in another money admits the pairs quoted in it, and only those.
    assert!(instrument_facts("EURGBP", "GBP").is_ok());
    assert!(instrument_facts("EURUSD", "GBP").is_err());
}

/// The 103 placeholder CFDs and the three with a real pip are alike (the table's pip is no statement for the first, and
/// for the others the venue's terms are still the one source), and a metal too.
#[test]
fn a_cfd_is_judged_on_the_venues_terms_whatever_pip_the_table_holds() {
    let kinds: Vec<GridBasis> =
        ["AAPL.US/USD", "COPPER.CMD/USD"].iter().map(|n| row(n).basis).collect();
    assert_eq!(kinds, [GridBasis::CfdPlaceholder, GridBasis::SdkConstant]);
    for name in ["COPPER.CMD/USD", "GAS.CMD/USD", "PRT.IDX/EUR", "AAPL.US/USD", "XAU/USD"] {
        assert!(matches!(facts_basis(name), Ok(FactsBasis::VenueTerms(_))), "{name}");
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
    let (idx, quote) =
        facts_from_terms(row("DEU.IDX/EUR"), &terms("DEU.IDX/EUR", (0.1, 0.1))).expect("index");
    assert_eq!(
        (idx.min_qty, idx.step_size, idx.tick_size, quote.as_str()),
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
    for r in jforex_instruments() {
        let Ok(FactsBasis::VenueTerms(row)) = facts_basis(&wire_symbol(r)) else { continue };
        let (props, quote) = facts_from_terms(row, &terms(&r.symbol, (1.0, 1.0)))
            .unwrap_or_else(|e| panic!("{}: {e}", r.symbol));
        assert_eq!(props.tick_size, r.tick_size(), "{}", r.symbol);
        assert_eq!(quote, r.pair.rsplit('/').next().unwrap(), "{}", r.symbol);
    }
}

/// **Nothing is guessed**: each thing the venue's terms can get wrong refuses the instrument, with the figure named.
#[test]
fn terms_that_do_not_admit_an_instrument_refuse_it_in_words() {
    let gold = row("XAUUSD");
    let good = terms("XAUUSD", (1.0, 1.0));
    let refuse = |mutate: &dyn Fn(&mut VenueTerms), cause: &str| {
        let mut t = good.clone();
        mutate(&mut t);
        let why = facts_from_terms(gold, &t).expect_err(cause);
        assert!(why.contains(cause) && why.contains("XAU/USD"), "{cause}: {why}");
    };
    refuse(&|t| t.min = None, "no usable minimum amount");
    refuse(&|t| t.min = Some(0.0), "no usable minimum amount");
    refuse(&|t| t.step = Some(f64::NAN), "no usable amount increment");
    refuse(&|t| t.per_contract = None, "no usable amount per contract");
    refuse(&|t| t.pip = Some(-0.01), "no usable pip");
    // more than one unit of the underlying per contract: a contract's worth per point is unmeasured
    refuse(&|t| t.per_contract = Some(100.0), "100 units of the underlying per contract");
    refuse(&|t| t.per_contract = Some(0.5), "0.5 units of the underlying per contract");
    refuse(&|t| t.class = "CFD".into(), "states it as a `CFD`");
    refuse(&|t| t.class = String::new(), "states it as a ``");
    refuse(&|t| t.pip_scale = None, "no pip scale or tick scale");
    refuse(&|t| t.tick_scale = None, "no pip scale or tick scale");
    refuse(&|t| t.tick_scale = Some(2), "tick scale 2");
    refuse(&|t| t.tick_scale = Some(13), "tick scale 13");
    // 1.5 is not a whole number of increments of 1.0 ... and 50 is not of 30
    refuse(&|t| t.min = Some(1.5), "not a whole number of the increment");
    refuse(
        &|t| {
            t.min = Some(50.0);
            t.step = Some(30.0)
        },
        "not a whole number of the increment",
    );
    refuse(
        &|t| {
            t.min = Some(10.0);
            t.max = Some(5.0)
        },
        "largest order 5 is below its smallest 10",
    );
    refuse(
        &|t| t.quote = Some("EUR".into()),
        "states the quote currency EUR and the table has USD",
    );
    // no maximum stated is no maximum (the engine's own ceilings judge a size); an absent quote reads the table's
    let mut open = good.clone();
    (open.max, open.quote) = (None, None);
    let (props, quote) = facts_from_terms(gold, &open).expect("no maximum is fine");
    assert_eq!((props.max_qty, quote.as_str()), (0.0, "USD"));
    // a stated quote in another case is the same currency
    let mut lower = good;
    lower.quote = Some(" usd ".into());
    assert!(facts_from_terms(gold, &lower).is_ok());
}
