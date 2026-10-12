//! IBKR market data over the **cpapi** backend (decision 0140, "Data on cpapi"): `CpapiFeeds`
//! implements `vike_data::DataClient` beside the socket backend's `IbkrFeeds`, riding the SAME
//! browser-authenticated Client Portal Gateway session the cpapi exec and reconcile use, so a cpapi
//! box opens no ibapi socket and no second session.
//!
//! - **Live quotes and trades:** the gateway WS topic `smd+<conid>+{"fields":[...]}`, dialled by
//!   the same `connect_ws` the `sor` pump uses, all subscriptions multiplexed on one socket
//!   (`pump`). Decoded by the pure `decode` module.
//! - **Bars:** aggregated from those ticks by the shared `BarAggregator`
//!   (`crates/bridges/vike-ibkr/src/market_feed/agg.rs`), seeded from REST
//!   `GET /iserver/marketdata/history`.
//! - **First quote:** REST `GET /iserver/marketdata/snapshot`. IBKR requires `/iserver/accounts`
//!   to have been called in the session before a snapshot, so [`CpapiFeeds::connect`] calls it
//!   (after the `tickle` readiness probe the exec transport also makes).
//! - **conIds:** the existing `CpapiRest::secdef_search` + `decode_conid`, and an expiry-bearing
//!   contract is REFUSED as ambiguous exactly as
//!   `crates/bridges/vike-ibkr/src/contract.rs`'s `IbkrContract::conid_search_is_ambiguous`
//!   refuses it on the exec and reconcile lanes (the search cannot name the month). ⚠ For a `CASH`
//!   pair the search is keyed on the BASE currency alone (`EUR`), the same exposure the exec lane
//!   carries; which pair its first row names is unverified, and the market-data smoke prints it.
//! - **No book, no depth:** the accounts hold no depth entitlement (decision 0140, Context 7), so
//!   `subscribe_book`/`subscribe_depth` answer `Unsupported` off the declared caps row, as the
//!   socket feed's do (`crates/bridges/vike-ibkr/src/data_door.rs`'s `no_book_lane`).
//! - **The delayed label:** read off each row's availability marker straight into
//!   `vike_model::DataDelay` (`decode`'s `availability_delay`) and sent per stream at ONE site,
//!   `pump`'s `note_delay` — `LiveDataSink::data_delay` at first knowledge and on change only.
//!
//! The only public door to this client is `crate::data_door`'s `connect_data_client`, which matches
//! on `cfg.backend`: a root gets the data client of the session the mount bound, never a second
//! product's. Every wire shape here is UNVERIFIED (IBKR's public docs; `decode`'s module doc has the
//! table) until `crates/bridges/vike-ibkr/tests/ibkr_cpapi_mktdata_smoke.rs` has read them.

pub mod decode;
mod pump;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};

use crate::agg::BarAggregator;
use crate::config::IbkrConfig;
use crate::contract::{SecType, parse_simplified};
use crate::data_door::no_book_lane;
use crate::error::IbkrError;
use crate::transport::CpapiRest;
use crate::transport::cpapi::ws_url_from;
use crate::transport::cpapi_decode::decode_conid;
use pump::{Book, Cmd, Lane, PumpCtx};
use vike_model::IbkrBackend;

/// How long `unsubscribe` waits for the pump to confirm a stream is dropped.
const REMOVE_WAIT: Duration = Duration::from_secs(2);

/// The running pump: its command lane, stop flag and thread.
struct PumpHandle {
    cmds: Sender<Cmd>,
    stop: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

/// IBKR market data over the CP Gateway session. The pump thread (and its WS) starts with the
/// first subscription, so a client that only refuses verbs opens nothing.
pub(crate) struct CpapiFeeds {
    sink: Arc<dyn LiveDataSink>,
    /// The caller-side REST lane: conId resolution at subscribe.
    rest: CpapiRest,
    cpapi_url: String,
    account: String,
    /// symbol → conId, resolved once per symbol.
    conids: HashMap<String, i64>,
    next_id: u64,
    pump: Option<PumpHandle>,
}

impl CpapiFeeds {
    /// Open the market-data client on a cpapi config: refuse any other backend, then probe the
    /// session (`tickle`: gateway up and browser-authenticated) and call `/iserver/accounts`, the
    /// precondition IBKR documents for every snapshot.
    pub(crate) fn connect(
        cfg: &IbkrConfig,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<CpapiFeeds, IbkrError> {
        if cfg.backend != IbkrBackend::Cpapi {
            return Err(IbkrError::Unsupported(format!(
                "the cpapi market-data client rides a Client Portal session; this config's \
                 backend is {:?}",
                cfg.backend
            )));
        }
        let feeds = CpapiFeeds::unconnected(cfg, sink);
        feeds.rest.tickle().map_err(|e| IbkrError::Connect(format!("cpapi tickle: {}", e.msg)))?;
        feeds
            .rest
            .accounts()
            .map_err(|e| IbkrError::Connect(format!("cpapi /iserver/accounts: {}", e.msg)))?;
        Ok(feeds)
    }

    /// The client with no network touched (the tests' constructor, and `connect`'s first step).
    fn unconnected(cfg: &IbkrConfig, sink: Arc<dyn LiveDataSink>) -> CpapiFeeds {
        CpapiFeeds {
            sink,
            rest: CpapiRest::new(&cfg.cpapi_url, &cfg.account),
            cpapi_url: cfg.cpapi_url.clone(),
            account: cfg.account.clone(),
            conids: HashMap::new(),
            next_id: 0,
            pump: None,
        }
    }

    /// Resolve `symbol` to a conId and its secType, refusing what cpapi cannot pin.
    fn resolve(&mut self, symbol: &str) -> Result<(i64, SecType), LiveDataError> {
        let contract = parse_simplified(symbol)
            .map_err(|why| LiveDataError::Subscribe(format!("ibkr cpapi {symbol}: {why}")))?;
        if contract.conid_search_is_ambiguous() {
            return Err(LiveDataError::Subscribe(format!(
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
            .secdef_search(&contract.symbol, contract.sec_type.as_ib_code())
            .map_err(|e| {
                LiveDataError::Subscribe(format!("ibkr cpapi secdef_search {symbol}: {}", e.msg))
            })?;
        let conid = decode_conid(&body).ok_or_else(|| {
            LiveDataError::Subscribe(format!("ibkr cpapi: no conId for {symbol}"))
        })?;
        self.conids.insert(symbol.to_string(), conid);
        Ok((conid, contract.sec_type))
    }

    /// The pump's command lane, starting the pump on first use.
    fn pump(&mut self) -> Result<&Sender<Cmd>, LiveDataError> {
        if self.pump.is_none() {
            let (cmds, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let ctx = PumpCtx {
                ws_url: ws_url_from(&self.cpapi_url),
                rest: CpapiRest::new(&self.cpapi_url, &self.account),
                book: Book::new(Arc::clone(&self.sink)),
                cmds: rx,
                stop: Arc::clone(&stop),
            };
            let join = std::thread::Builder::new()
                .name("ibkr-cpapi-feed".into())
                .spawn(move || pump::run(ctx))
                .map_err(|e| LiveDataError::Subscribe(format!("spawn ibkr cpapi feed: {e}")))?;
            self.pump = Some(PumpHandle { cmds, stop, join });
        }
        Ok(&self.pump.as_ref().expect("pump just started").cmds)
    }

    fn subscribe(
        &mut self,
        symbol: &str,
        lane: impl FnOnce(SecType) -> Lane,
    ) -> Result<SubscriptionId, LiveDataError> {
        let (conid, sec_type) = self.resolve(symbol)?;
        let id = SubscriptionId(self.next_id);
        let cmd = Cmd::Add { id, conid, symbol: symbol.to_string(), lane: lane(sec_type) };
        self.pump()?
            .send(cmd)
            .map_err(|_| LiveDataError::Subscribe("ibkr cpapi feed thread is gone".into()))?;
        self.next_id += 1;
        Ok(id)
    }
}

impl DataClient for CpapiFeeds {
    /// Any interval the shared parser reads: ticks aggregate into any bar length. The history
    /// seed needs an exact endpoint bar (`decode`'s `history_bar_param`); without one the stream
    /// runs unseeded, with a warning.
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let Some(interval_ms) =
            vike_marketdata::interval::interval_ms(interval).filter(|ms| *ms > 0)
        else {
            return Err(LiveDataError::Subscribe(format!(
                "ibkr cpapi bars: interval {interval:?} is not a bar length"
            )));
        };
        let interval = interval.to_string();
        self.subscribe(symbol, move |sec_type| Lane::Bars {
            interval,
            interval_ms,
            agg: BarAggregator::new(interval_ms),
            from_mid: sec_type == SecType::Cash,
        })
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(symbol, |_| Lane::Quotes)
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(symbol, |_| Lane::Trades)
    }
    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        no_book_lane(vike_model::LiveVerb::Book)
    }
    fn subscribe_depth(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        no_book_lane(vike_model::LiveVerb::Depth)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        let Some(pump) = &self.pump else { return };
        let (done, ack) = mpsc::channel();
        if pump.cmds.send(Cmd::Remove { id, done }).is_ok() {
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
            drop(pump.cmds);
            let _ = pump.join.join();
        }
    }
}

impl Drop for CpapiFeeds {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "feeds_tests.rs"]
mod feeds_tests;
