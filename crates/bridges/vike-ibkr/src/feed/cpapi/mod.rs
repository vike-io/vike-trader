//! IBKR market data over the **cpapi** backend (decision 0140, "Data on cpapi"):
//! `DataClientBusSpecBackendIbkrCpapi` is the Client Portal backend of
//! `crate::feed::price::DataClientBusSpecIbkr`, beside the socket backend (`crate::feed::socket`), riding the SAME
//! browser-authenticated Client Portal Gateway session the cpapi exec and reconcile use, so a cpapi
//! box opens no ibapi socket and no second session.
//!
//! - **Live quotes and trades:** the gateway WS topic `smd+<conid>+{"fields":[...]}`, dialled by
//!   the same `connect_websocket` the `sor` pump uses, all subscriptions multiplexed on one socket
//!   (`pump`). Decoded by the pure `decode` module.
//! - **Bars:** aggregated from those ticks by the shared `BarAggregator`
//!   (`crates/bridges/vike-ibkr/src/feed/bar.rs`), seeded from REST
//!   `GET /iserver/marketdata/history`.
//! - **First quote:** REST `GET /iserver/marketdata/snapshot`. IBKR requires `/iserver/accounts`
//!   to have been called in the session before a snapshot, so
//!   `DataClientBusSpecBackendIbkrCpapi::connect` calls it (after the `tickle` readiness probe the
//!   exec transport also makes).
//! - **conIds:** the existing `CpapiRest::secdef_search` + `decode_conid`, and an expiry-bearing
//!   contract is REFUSED as ambiguous exactly as
//!   `crates/bridges/vike-ibkr/src/instrument/contract.rs`'s `IbkrContract::conid_search_is_ambiguous`
//!   refuses it on the exec and reconcile lanes (the search cannot name the month). ⚠ For a `CASH`
//!   pair the search is keyed on the BASE currency alone (`EUR`), the same exposure the exec lane
//!   carries; which pair its first row names is unverified, and the market-data smoke prints it.
//! - **No book, no depth:** the accounts hold no depth entitlement (decision 0140, Context 7), so
//!   a book answers `Unsupported` off the declared caps row, as the socket backend's does
//!   (`crates/bridges/vike-ibkr/src/feed/mod.rs`'s `no_book_lane`).
//! - **The delayed label:** read off each row's availability marker straight into
//!   `vike_model::StreamDelay` (`decode`'s `availability_delay`) and sent per stream at ONE site,
//!   `pump`'s `note_delay` — `DataClientSink::data_delay` at first knowledge and on change only. A quote with
//!   no label in force is not delivered: nobody may take it for real time (`pump`'s `deliver`).
//!
//! The only door to this backend is `crate::feed`'s `connect_data_client`, which matches
//! on `config.backend`: a root gets the data connection of the session the start bound, never a second
//! product's. Every wire shape here is UNVERIFIED (IBKR's public docs; `decode`'s module doc has the
//! table) until `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_cpapi_market_data_smoke.rs` has read them.

pub mod decode;

mod lane;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_bridge_core::data_client::bus::DataClientBusSpecKey;
use vike_data::{DataClientError, DataClientSink, DataClientSubscriptionId};

use crate::config::IbkrConfig;
use crate::feed::bar::BarAggregator;
use crate::feed::no_book_lane;
use crate::feed::price::DataClientBusSpecBackendIbkr;
use crate::instrument::contract::{SecType, parse_simplified};
use crate::transport::CpapiRest;
use crate::transport::cpapi::websocket_url_from;
use crate::transport::cpapi_decode::decode_conid;
use crate::transport::error_codes::IbkrError;
use lane::{Book, FeedCpapiCommand, FeedLane};
use vike_venues::IbkrBackend;

/// How long `unsubscribe` waits for the pump to confirm a stream is dropped.
const REMOVE_WAIT: Duration = Duration::from_secs(2);

/// The running pump: its command lane, stop flag and thread.
struct FeedCpapiHandle {
    commands: Sender<FeedCpapiCommand>,
    stop: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

/// **The Client Portal backend of the price stream** (`crate::feed::price::DataClientBusSpecIbkr`, which dials it
/// through `crate::feed`'s `connect_data_client`): IBKR market data over the CP Gateway session. The pump thread
/// (and its WS) starts with the first subscription, so a backend that only refuses opens nothing.
pub(crate) struct DataClientBusSpecBackendIbkrCpapi {
    sink: Arc<dyn DataClientSink>,
    /// The caller-side REST lane: conId resolution at subscribe.
    rest: CpapiRest,
    cpapi_url: String,
    account: String,
    /// symbol → conId, resolved once per symbol.
    conids: HashMap<String, i64>,
    next_id: u64,
    pump: Option<FeedCpapiHandle>,
}

impl DataClientBusSpecBackendIbkrCpapi {
    /// Open the market-data connection on a cpapi config: refuse any other backend, then probe the
    /// session (`tickle`: gateway up and browser-authenticated) and call `/iserver/accounts`, the
    /// precondition IBKR documents for every snapshot.
    pub(crate) fn connect(
        config: &IbkrConfig,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<DataClientBusSpecBackendIbkrCpapi, IbkrError> {
        if config.backend != IbkrBackend::Cpapi {
            return Err(IbkrError::Unsupported(format!(
                "the cpapi market-data client rides a Client Portal session; this config's \
                 backend is {:?}",
                config.backend
            )));
        }
        let feeds = DataClientBusSpecBackendIbkrCpapi::unconnected(config, sink);
        feeds.rest.tickle().map_err(|e| IbkrError::Connect(format!("cpapi tickle: {}", e.msg)))?;
        feeds
            .rest
            .accounts()
            .map_err(|e| IbkrError::Connect(format!("cpapi /iserver/accounts: {}", e.msg)))?;
        Ok(feeds)
    }

    /// The backend with no network touched (the tests' constructor, and `connect`'s first step).
    fn unconnected(
        config: &IbkrConfig,
        sink: Arc<dyn DataClientSink>,
    ) -> DataClientBusSpecBackendIbkrCpapi {
        DataClientBusSpecBackendIbkrCpapi {
            sink,
            rest: CpapiRest::new(&config.cpapi_url, &config.account),
            cpapi_url: config.cpapi_url.clone(),
            account: config.account.clone(),
            conids: HashMap::new(),
            next_id: 0,
            pump: None,
        }
    }

    /// Resolve `symbol` to a conId and its secType, refusing what cpapi cannot pin.
    fn resolve(&mut self, symbol: &str) -> Result<(i64, SecType), DataClientError> {
        let contract = parse_simplified(symbol)
            .map_err(|why| DataClientError::Subscribe(format!("ibkr cpapi {symbol}: {why}")))?;
        if contract.conid_search_is_ambiguous() {
            return Err(DataClientError::Subscribe(format!(
                "ibkr cpapi cannot pin {symbol}: its identity includes an expiry, and cpapi's \
                 secdef/search is keyed on symbol+secType alone — the conId it would return could \
                 be another contract month. Use the socket backend for this contract."
            )));
        }
        if let Some(conid) = contract.con_id {
            return Ok((conid, contract.sec_type));
        }
        if let Some(conid) = self.conids.get(symbol).copied() {
            return Ok((conid, contract.sec_type));
        }
        let body = self
            .rest
            .secdef_search(&contract.symbol, contract.sec_type.as_ibkr_code())
            .map_err(|e| {
                DataClientError::Subscribe(format!("ibkr cpapi secdef_search {symbol}: {}", e.msg))
            })?;
        let conid = decode_conid(&body).ok_or_else(|| {
            DataClientError::Subscribe(format!("ibkr cpapi: no conId for {symbol}"))
        })?;
        self.conids.insert(symbol.to_string(), conid);
        Ok((conid, contract.sec_type))
    }

    /// The pump's command lane, starting the pump on first use, and again after a `begin_shutdown` ended it: that
    /// call ends the pump and not this backend, whose session stays usable (a tradehub that restarts sends it, and its
    /// successor subscribes here; `crates/vike-gatewayhub/src/server/stream.rs`'s module doc, "Draining").
    fn pump(&mut self) -> Result<&Sender<FeedCpapiCommand>, DataClientError> {
        if self.pump.as_ref().is_some_and(|pump| pump.stop.load(Ordering::Relaxed)) {
            self.shutdown();
        }
        if self.pump.is_none() {
            let (commands, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let websocket_url = websocket_url_from(&self.cpapi_url);
            let rest = CpapiRest::new(&self.cpapi_url, &self.account);
            let book = Book::new(Arc::clone(&self.sink));
            let stopped = Arc::clone(&stop);
            let join = std::thread::Builder::new()
                .name("ibkr-cpapi-feed".into())
                .spawn(move || {
                    vike_execution::affinity::pin_current_thread(
                        vike_execution::affinity::Role::MarketData,
                        crate::feed::VENUE,
                    );
                    lane::run(&websocket_url, &rest, book, &rx, &stopped)
                })
                .map_err(|e| DataClientError::Subscribe(format!("spawn ibkr cpapi feed: {e}")))?;
            self.pump = Some(FeedCpapiHandle { commands, stop, join });
        }
        Ok(&self.pump.as_ref().expect("pump just started").commands)
    }

    fn subscribe(
        &mut self,
        symbol: &str,
        lane: impl FnOnce(SecType) -> FeedLane,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        let (conid, sec_type) = self.resolve(symbol)?;
        let id = DataClientSubscriptionId(self.next_id);
        let cmd =
            FeedCpapiCommand::Add { id, conid, symbol: symbol.to_string(), lane: lane(sec_type) };
        self.pump()?
            .send(cmd)
            .map_err(|_| DataClientError::Subscribe("ibkr cpapi feed thread is gone".into()))?;
        self.next_id += 1;
        Ok(id)
    }
}

impl DataClientBusSpecBackendIbkr for DataClientBusSpecBackendIbkrCpapi {
    /// A bars key takes any interval the shared parser reads: ticks aggregate into any bar length.
    /// The history seed needs an exact endpoint bar (`decode`'s `history_bar_param`); without one the
    /// stream runs unseeded, with a warning. A book is refused off the declared row.
    fn want(
        &mut self,
        key: &DataClientBusSpecKey,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        match key {
            DataClientBusSpecKey::Quotes(symbol) => self.subscribe(symbol, |_| FeedLane::Quotes),
            DataClientBusSpecKey::Trades(symbol) => self.subscribe(symbol, |_| FeedLane::Trades),
            DataClientBusSpecKey::Bars { symbol, interval } => {
                let Some(interval_ms) = vike_marketdata::interval::bar_step_ms(interval) else {
                    return Err(DataClientError::Subscribe(format!(
                        "ibkr cpapi bars: interval {interval:?} is not a bar length"
                    )));
                };
                let interval = interval.clone();
                self.subscribe(symbol, move |sec_type| FeedLane::Bars {
                    interval,
                    interval_ms,
                    agg: BarAggregator::new(interval_ms),
                    from_mid: sec_type == SecType::Cash,
                })
            }
            DataClientBusSpecKey::Book(_) => no_book_lane(vike_venues::DataClientVerb::Book),
        }
    }
    fn unwant(&mut self, id: DataClientSubscriptionId) {
        let Some(pump) = &self.pump else { return };
        let (done, ack) = mpsc::channel();
        if pump.commands.send(FeedCpapiCommand::Remove { id, done }).is_ok() {
            let _ = ack.recv_timeout(REMOVE_WAIT);
        }
    }
    fn begin_shutdown(&mut self) {
        if let Some(pump) = &self.pump {
            pump.stop.store(true, Ordering::Relaxed);
        }
    }
    fn shutdown(&mut self) {
        if let Some(pump) = self.pump.take() {
            pump.stop.store(true, Ordering::Relaxed);
            drop(pump.commands);
            let _ = pump.join.join();
        }
    }
}

impl Drop for DataClientBusSpecBackendIbkrCpapi {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "feeds_tests.rs"]
mod feeds_tests;
