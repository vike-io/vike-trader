//! alpaca's kept list for a test process, which reads no project folder (decision 0290's O-10: alpaca's kept list is
//! the operator's rows): the shares `AAPL` and `NVDA` and the coin `BTC/USD`, as `vike-cli data instruments add` would
//! add them.

use vike_model::{Contract, ContractEquity, Instrument};

/// Hold the shares `AAPL` and `NVDA` and the coin `BTC/USD` (user name `BTCUSD`) as alpaca's kept list in this process.
pub fn hold_kept_rows() {
    let share = |ticker: &str| {
        Instrument::new(
            "alpaca",
            ticker,
            ticker,
            "USD",
            Contract::Equity(ContractEquity::default()),
        )
    };
    vike_catalog::hold_kept_list(
        "alpaca",
        vec![
            share("AAPL"),
            share("NVDA"),
            Instrument::new("alpaca", "BTC/USD", "BTC", "USD", Contract::CryptoSpot),
        ],
    );
}
