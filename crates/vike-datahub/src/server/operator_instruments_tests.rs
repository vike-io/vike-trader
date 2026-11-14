use super::*;

fn facts(tick: f64, quote: &str, class: &str, point_scale: f64) -> StatedInstrumentFacts {
    StatedInstrumentFacts { tick, quote: quote.into(), class: class.into(), point_scale }
}

/// Every stated fact is checked and none is defaulted: the tick, the point value that must be its reciprocal, the
/// quote and the class.
#[test]
fn a_stated_row_holds_its_facts_or_is_refused() {
    let row = stated_row("dukascopy", "AAPL.US/USD", &facts(0.001, "usd", "Share", 1000.0))
        .expect("a row");
    assert_eq!(
        (row.venue.as_str(), row.raw_symbol.as_str(), row.quote.as_str()),
        ("dukascopy", "AAPL.US/USD", "USD")
    );
    assert_eq!(row.properties.tick_size, 0.001);
    assert!(matches!(
        row.contract,
        Contract::Cfd(ContractCfd {
            underlying_kind: ContractCfdUnderlying::Share,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            ..
        })
    ));
    let pair = stated_row("dukascopy", "EURHUF", &facts(0.001, "HUF", "fx", 1000.0)).expect("fx");
    assert_eq!(pair.asset_class(), vike_model::AssetClass::Fx);

    for (bad, word) in [
        (facts(0.0, "USD", "share", 1000.0), "--tick"),
        (facts(f64::NAN, "USD", "share", 1000.0), "--tick"),
        (facts(0.001, "USD", "share", 100.0), "--point-scale"),
        (facts(0.001, "USD", "share", 0.0), "--point-scale"),
        (facts(0.001, "US", "share", 1000.0), "--quote"),
        (facts(0.001, "USD1", "share", 1000.0), "--quote"),
        (facts(0.001, "USD", "", 1000.0), "--class"),
        (facts(0.001, "USD", "equity", 1000.0), "--class"),
    ] {
        let why = stated_row("dukascopy", "X", &bad).expect_err(word);
        assert!(why.contains(word), "{why}");
    }
}

/// The words a symbol may be: the venues' own spellings (`CS.D.EURUSD.MINI.IP`, `COPPER.CMD/USD`), nothing with a
/// space, a quote or a control byte, and nothing past the bound.
#[test]
fn a_symbol_is_bounded_at_the_door() {
    for good in ["EURUSD", "CS.D.EURUSD.MINI.IP", "COPPER.CMD/USD", "BRK_B", "ES-Z26"] {
        assert_eq!(symbol_refusal(good), None, "{good}");
    }
    let long = "X".repeat(SYMBOL_MAX_BYTES + 1);
    for bad in ["", "EUR USD", "EUR\"USD", "EUR\nUSD", long.as_str()] {
        assert!(symbol_refusal(bad).is_some(), "{bad:?}");
    }
}
