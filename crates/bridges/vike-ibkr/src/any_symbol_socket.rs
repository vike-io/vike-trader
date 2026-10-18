//! The SOCKET backend of the any-symbol lookup (`crate::any_symbol`): the three requests over one ibapi
//! connection of the logged-in IB Gateway, and the [`IbkrFactsSource`] the gatewayhub asks.
//!
//! # One connection per lookup, through the DATA lane of the gate
//!
//! A lookup dials ONE connection under the account's `data_client_id`, asks the three requests on it, and
//! closes it, all inside the gate's data turn (`GatewayGate::dial_data`):
//!
//! * it **can never delay an order-lane dial** (the exec connect, the grid pre-fetch, the reconcile connect):
//!   refused at once, nothing dialled, while any dial holds the turn or an order-lane dial waits, while the
//!   rebuild schedule is armed, and while the data lane's backoff runs; it gives way the moment an order-lane
//!   dial arrives;
//! * it **spends the data lane's budget**, at most `MAX_ATTEMPTS_PER_HOUR` (6) connects an hour per gateway,
//!   shared with every other data connect: the lane above keeps an answer for ten minutes and a refusal for
//!   thirty seconds, so a name asked twice costs one dial. That floor is the gate's, not this module's, and
//!   it is the price of never touching the order lane;
//! * the connection is **closed inside the turn**, so it never overlaps the grid pre-fetch that shares the
//!   `data_client_id` and no id is held after the answer. The flip side, written down: a data client that
//!   LIVES under that id (`IbkrFeeds`, `HistoricalFetcher`) makes the connect fail (error 326) and the
//!   lookup answers "unavailable"; wiring a price lane must route this lookup over that client's connection.
//!
//! The matching request is paced by the gateway (the research notes say 1 s apart); one lookup makes one,
//! and the gate spaces the dials by `CONNECT_GAP`. A cpapi account is REFUSED by name: the Client Portal
//! has `secdef/search` and `info-and-rules`, neither of which this module reads, and a cpapi box opens no
//! ibapi socket (decision 0140).

use std::time::Duration;

use ibapi::client::blocking::Client;
use ibapi::contracts::{
    Contract, ContractDescription, ContractDetails, Currency, Exchange, MarketRule, SecurityType,
    Symbol,
};

use crate::any_symbol::{
    ContractLookup, DetailsRow, FactsError, MatchRow, PriceBand, ROUTE, StockFacts, checked_name,
    stock_facts,
};
use crate::config::IbkrConfig;
use crate::market_feed::refuse_cpapi;
use crate::transport::gateway_gate::gate_for;

/// How long one lookup may take end to end, connect included. Above the gate's usual 15 s open bound: it is
/// four requests, not one.
pub const LOOKUP_DEADLINE: Duration = Duration::from_secs(25);

/// The lookup over one connected ibapi client.
struct SocketLookup<'a> {
    client: &'a Client,
}

impl ContractLookup for SocketLookup<'_> {
    fn matching_symbols(&self, pattern: &str) -> Result<Vec<MatchRow>, String> {
        let rows = self.client.matching_symbols(pattern).map_err(|e| e.to_string())?;
        Ok(rows.iter().map(match_row).collect())
    }

    fn contract_details(&self, contract: &MatchRow) -> Result<Vec<DetailsRow>, String> {
        let asked = Contract {
            contract_id: contract.conid,
            symbol: Symbol::from(contract.symbol.as_str()),
            security_type: SecurityType::Stock,
            exchange: Exchange::from(ROUTE),
            currency: Currency::from(contract.currency.as_str()),
            ..Contract::default()
        };
        let rows = self.client.contract_details(&asked).map_err(|e| e.to_string())?;
        Ok(rows.iter().map(details_row).collect())
    }

    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String> {
        let rule = self.client.market_rule(id).map_err(|e| e.to_string())?;
        Ok(price_bands(&rule))
    }
}

/// One matching-symbols row in the seam's words.
pub(crate) fn match_row(row: &ContractDescription) -> MatchRow {
    MatchRow {
        conid: row.contract.contract_id,
        symbol: row.contract.symbol.0.clone(),
        sec_type: row.contract.security_type.to_string(),
        primary_exchange: row.contract.primary_exchange.0.clone(),
        currency: row.contract.currency.0.clone(),
    }
}

/// One contract-details row in the seam's words.
pub(crate) fn details_row(row: &ContractDetails) -> DetailsRow {
    DetailsRow {
        conid: row.contract.contract_id,
        symbol: row.contract.symbol.0.clone(),
        sec_type: row.contract.security_type.to_string(),
        currency: row.contract.currency.0.clone(),
        multiplier: row.contract.multiplier.clone(),
        min_tick: row.min_tick,
        size_increment: row.size_increment,
        min_size: row.min_size,
        market_rule_ids: row.market_rule_ids.clone(),
    }
}

/// A market rule's bands in the seam's words.
pub(crate) fn price_bands(rule: &MarketRule) -> Vec<PriceBand> {
    rule.price_increments
        .iter()
        .map(|b| PriceBand { low_edge: b.low_edge, increment: b.increment })
        .collect()
}

/// What the gatewayhub asks for the facts of one stock of ONE account's gateway session.
#[derive(Debug)]
pub struct IbkrFactsSource {
    cfg: IbkrConfig,
}

impl IbkrFactsSource {
    /// A source over `cfg` (the account's resolved connection: gateway, ids, account).
    #[must_use]
    pub fn new(cfg: IbkrConfig) -> Self {
        Self { cfg }
    }

    /// Whether this source was built for `cfg`'s gateway, client ids and account, so the caller keeps it
    /// across asks and builds a new one when the account's rows change.
    #[must_use]
    pub fn serves(&self, cfg: &IbkrConfig) -> bool {
        let mine = &self.cfg;
        mine.backend == cfg.backend
            && mine.host == cfg.host
            && mine.port == cfg.port
            && mine.account == cfg.account
            && mine.client_id == cfg.client_id
            && mine.data_client_id == cfg.data_client_id
    }

    /// The facts of the stock `name` (`TICKER.SMART.CCY`) in an account whose money is `account_currency`.
    ///
    /// # Errors
    /// [`FactsError::Refused`] for a name or backend this lookup does not admit (decided before any dial),
    /// and every verdict of [`stock_facts`]; [`FactsError::Unavailable`] when the data lane of the gate
    /// refused the dial or the gateway did not answer within [`LOOKUP_DEADLINE`].
    pub fn facts(&self, name: &str, account_currency: &str) -> Result<StockFacts, FactsError> {
        self.facts_within(name, account_currency, LOOKUP_DEADLINE)
    }

    /// [`Self::facts`] under a `within` bound (a test shortens it).
    pub(crate) fn facts_within(
        &self,
        name: &str,
        account_currency: &str,
        within: Duration,
    ) -> Result<StockFacts, FactsError> {
        checked_name(name, account_currency)?;
        refuse_cpapi(&self.cfg, "the any-symbol lookup")
            .map_err(|why| FactsError::Refused(why.to_string()))?;
        let cfg = self.cfg.clone();
        let (name, currency) = (name.to_string(), account_currency.to_string());
        let gate = gate_for(&format!("{}:{}", cfg.host, cfg.port));
        let answer = gate.dial_data("any-symbol lookup", within, move || {
            let addr = format!("{}:{}", cfg.host, cfg.port);
            let client = Client::connect(&addr, cfg.data_client_id).map_err(|e| {
                crate::error::IbkrError::Connect(format!(
                    "any-symbol lookup {addr} (client_id={}): {e}",
                    cfg.data_client_id
                ))
            })?;
            let facts = stock_facts(&SocketLookup { client: &client }, &name, &currency);
            // Closed inside the turn: the id is free before the next dial asks for it.
            client.disconnect();
            Ok(facts)
        });
        match answer {
            Ok(facts) => facts,
            Err(why) => Err(FactsError::Unavailable(why.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ibapi::contracts::PriceIncrement;
    use vike_venues::IbkrBackend;

    use super::*;
    use crate::transport::socket::socket_tests::{QUICK, TEST_BOUND, silent_gateway};

    fn description(conid: i32, symbol: &str, kind: SecurityType, ccy: &str) -> ContractDescription {
        ContractDescription {
            contract: Contract {
                contract_id: conid,
                symbol: Symbol::from(symbol),
                security_type: kind,
                primary_exchange: Exchange::from("NASDAQ"),
                currency: Currency::from(ccy),
                ..Contract::default()
            },
            derivative_security_types: vec!["OPT".into()],
        }
    }

    /// The ibapi rows reach the seam in IB's own words: the secType as its wire code, the exchange and
    /// currency as text, the optional size rules kept optional, the rule ids kept as sent.
    #[test]
    fn ibapi_rows_map_into_the_seams_words() {
        let row = match_row(&description(265_598, "AAPL", SecurityType::Stock, "USD"));
        assert_eq!(
            row,
            MatchRow {
                conid: 265_598,
                symbol: "AAPL".into(),
                sec_type: "STK".into(),
                primary_exchange: "NASDAQ".into(),
                currency: "USD".into(),
            }
        );
        assert_eq!(match_row(&description(1, "ES", SecurityType::Future, "USD")).sec_type, "FUT");

        let details = ContractDetails {
            contract: Contract {
                contract_id: 265_598,
                symbol: Symbol::from("AAPL"),
                security_type: SecurityType::Stock,
                currency: Currency::from("USD"),
                multiplier: String::new(),
                ..Contract::default()
            },
            min_tick: 0.01,
            min_size: None,
            size_increment: Some(1.0),
            market_rule_ids: vec!["26".into(), "26".into()],
            ..ContractDetails::default()
        };
        let row = details_row(&details);
        assert_eq!(
            (row.conid, row.sec_type.as_str(), row.currency.as_str()),
            (265_598, "STK", "USD")
        );
        assert_eq!((row.min_tick, row.min_size, row.size_increment), (0.01, None, Some(1.0)));
        assert_eq!(row.market_rule_ids, ["26", "26"]);

        let rule = MarketRule {
            market_rule_id: 26,
            price_increments: vec![
                PriceIncrement { low_edge: 0.0, increment: 0.0001 },
                PriceIncrement { low_edge: 1.0, increment: 0.01 },
            ],
        };
        assert_eq!(
            price_bands(&rule),
            [
                PriceBand { low_edge: 0.0, increment: 0.0001 },
                PriceBand { low_edge: 1.0, increment: 0.01 }
            ]
        );
    }

    /// A name the grammar or the currency fence refuses never dials: the gateway here is a closed port.
    #[test]
    fn a_refused_name_dials_nothing() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg);
        let began = Instant::now();
        for name in ["AAPL", "EUR.IDEALPRO.USD", "VOD.SMART.GBP"] {
            let got = source.facts_within(name, "USD", TEST_BOUND);
            assert!(matches!(got, Err(FactsError::Refused(_))), "{name}: {got:?}");
        }
        assert!(began.elapsed() < QUICK, "refused without dialling: {:?}", began.elapsed());
    }

    /// A cpapi account is refused by name and nothing is dialled (a cpapi box opens no ibapi socket).
    #[test]
    fn a_cpapi_account_is_refused_by_name() {
        let (_gateway, mut cfg) = silent_gateway();
        cfg.backend = IbkrBackend::Cpapi;
        let got = IbkrFactsSource::new(cfg).facts_within("AAPL.SMART.USD", "USD", TEST_BOUND);
        let Err(FactsError::Refused(why)) = got else { panic!("expected a refusal, got {got:?}") };
        assert!(why.contains("cpapi"), "{why}");
    }

    /// A gateway that takes the connection and never answers ends in "unavailable" within the bound.
    #[test]
    fn a_silent_gateway_is_unavailable_within_the_bound() {
        let (_gateway, cfg) = silent_gateway();
        let began = Instant::now();
        let got = IbkrFactsSource::new(cfg).facts_within("AAPL.SMART.USD", "USD", TEST_BOUND);
        assert!(matches!(got, Err(FactsError::Unavailable(_))), "{got:?}");
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
    }

    /// The source is kept while the account's connection is the same and replaced when it changes.
    #[test]
    fn a_source_serves_its_own_connection_only() {
        let (_gateway, cfg) = silent_gateway();
        let source = IbkrFactsSource::new(cfg.clone());
        assert!(source.serves(&cfg));
        for change in [
            |c: &mut IbkrConfig| c.port += 1,
            |c: &mut IbkrConfig| c.host = "<host>".into(),
            |c: &mut IbkrConfig| c.account = "DU1".into(),
            |c: &mut IbkrConfig| c.data_client_id += 1,
            |c: &mut IbkrConfig| c.client_id += 5,
        ] {
            let mut other = cfg.clone();
            change(&mut other);
            assert!(!source.serves(&other));
        }
    }
}
