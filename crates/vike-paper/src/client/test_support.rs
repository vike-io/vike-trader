//! Helpers shared by the `client` unit-test modules: the OHLC `bar` builder and `drain`.

use vike_execution::ExecutionClient;
use vike_marketdata::Bar;
use vike_model::events::Event;

use super::ExecutionClientPaper;

/// A symbol-less OHLC bar: no volume, no funding, no bid/ask.
pub(super) fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Drain every queued event, in emit order.
pub(super) fn drain(c: &mut ExecutionClientPaper) -> Vec<Event> {
    std::iter::from_fn(|| c.poll_events()).collect()
}

/// **The operator's ibkr rows a paper test reads** (decision 0290's O-10: IBKR lists nothing, so its kept list is the
/// operator's rows), held as this process's ibkr kept list: the stock `AAPL`, the euro stock `SAP`, the pair
/// `USDJPY` and the dollar future `ES_261218` (its multiplier the venue's alone). Every test of this binary holds these
/// same rows.
pub(super) fn hold_ibkr_rows() {
    use vike_model::{
        Contract, ContractEquity, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
        ContractFx, Instrument, Margining,
    };
    let future = ContractFuture {
        margining: Margining::Linear,
        settle: "USD".into(),
        value_currency: None,
        expiry_ms: 1_797_580_800_000, // 2026-12-18 08:00 UTC
        tenor: None,
        delivery: ContractFutureDelivery::Unknown,
        family: ContractFutureFamily::Traditional,
    };
    let stock = |wire: &str, ticker: &str, currency: &str| {
        let equity = ContractEquity { exchange: None, currency: Some(currency.into()) };
        Instrument::new("ibkr", wire, ticker, currency, Contract::Equity(equity))
    };
    vike_catalog::hold_kept_list(
        "ibkr",
        vec![
            stock("AAPL.SMART.USD", "AAPL", "USD"),
            stock("SAP.SMART.EUR", "SAP", "EUR"),
            Instrument::new(
                "ibkr",
                "USD.JPY.IDEALPRO",
                "USD",
                "JPY",
                Contract::Fx(ContractFx::default()),
            ),
            Instrument::new(
                "ibkr",
                "ES.CME.USD.FUT.20261218",
                "ES",
                "USD",
                Contract::Future(future),
            ),
        ],
    );
}
