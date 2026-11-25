use super::{off_default_contracts, off_dollar_ledgers};
use vike_execution::{CoreSnapshot, VenueBlock};

/// The snapshot's blocks name the off-dollar ledgers: the dollar group (an EMPTY `ledger_currency`) names none.
#[test]
fn the_off_dollar_ledgers_are_the_blocks_whose_group_is_not_the_dollar() {
    let block = |venue: &str, currency: &str| VenueBlock {
        venue: venue.to_string(),
        ledger_currency: currency.into(),
        ..VenueBlock::default()
    };
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    assert!(off_dollar_ledgers(&snapshot).is_empty());
    snapshot.portfolio.venues =
        vec![block("binance", ""), block("fxcm", "EUR"), block("fxcm", "EUR")];
    assert_eq!(off_dollar_ledgers(&snapshot), vec![("fxcm".to_string(), "EUR".to_string())]);
}

/// Every non-default row of every block, each `(venue, symbol)` once; a default row and a block with none name
/// nothing.
#[test]
fn the_off_default_contracts_are_the_non_default_rows_each_once() {
    use vike_model::ContractValue;
    let values = [
        ("BTC-PERPETUAL".to_string(), ContractValue::inverse(10.0)),
        ("ETH-PERPETUAL".to_string(), ContractValue::linear(1.0)),
    ];
    let deribit = VenueBlock {
        venue: "deribit".to_string(),
        contract_values: std::sync::Arc::new(values.into_iter().collect()),
        ..VenueBlock::default()
    };
    let mut snapshot = CoreSnapshot::empty("deribit", "BTC-PERPETUAL");
    assert!(off_default_contracts(&snapshot).is_empty());
    snapshot.portfolio.venues = vec![deribit.clone(), deribit, VenueBlock::default()];
    assert_eq!(
        off_default_contracts(&snapshot),
        vec![("deribit".into(), "BTC-PERPETUAL".into(), ContractValue::inverse(10.0))]
    );
}
