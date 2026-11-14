//! The ONE door to an IBKR market-data connection (decision 0140, "Exec, data and reconcile all use
//! that one product"): [`connect_data_client`] matches on `config.backend` and opens the backend of
//! `crate::feed::price::DataClientBusSpecIbkr` for that product only — the socket's
//! (`crate::feed::socket`'s `DataClientBusSpecBackendIbkrSocket`, ibapi on `data_client_id`) or the
//! Client Portal's (`crate::feed::cpapi`'s `DataClientBusSpecBackendIbkrCpapi`, over its session). A
//! root holding the config the start bound therefore gets the data connection of the same session,
//! never a second product's or tier's.
//!
//! Also home to the refusal both backends answer for a book and for depth ([`no_book_lane`]):
//! the accounts hold no depth entitlement on either product (decision 0140, Context 7), so the
//! declared caps row (`crates/vike-venues/src/venue_caps.rs`'s `IBKR`, one row for both backends) says
//! no book and no depth, and the refusal is driven off that row.

/// The shared bar aggregator: the socket feed folds its 5-second bars through it, the cpapi feed its
/// ticks (decision 0140). It sits at the crate root because both feeds use it, so it compiles with
/// either backend.
pub(crate) mod bar;
/// IBKR market data over the cpapi (Client Portal) session — decision 0140's "Data on cpapi".
/// Public for its pure `decode` module; the backend itself is reached through
/// [`price::DataClientBusSpecIbkr`].
#[cfg(feature = "ibkr-cpapi")]
pub mod cpapi;
/// The price stream for symbols the engine was not mounted on: ONE lazily dialled data client for every symbol
/// (any-symbol, decision 0174). The gatewayhub's credentialed stream of ibkr is this client; the real-time rule
/// that turns its quotes into a price is the tradehub's price tap.
pub mod price;
// No `///` here: the module's own `//!` block is its doc. The socket backend's: its answers are the socket lookup's.
#[cfg(feature = "ibkr-socket")]
pub mod socket;

use std::sync::Arc;

use vike_data::{DataClientError, DataClientSink, DataClientSubscriptionId};
use vike_venues::DataClientVerb;

use crate::config::IbkrConfig;
use crate::feed::price::DataClientBusSpecBackendIbkr;
use crate::transport::error_codes::IbkrError;
use vike_venues::IbkrBackend;

/// Canonical venue string the data backends stamp on the sink.
pub(crate) const VENUE: &str = "ibkr";

/// Open the market-data connection of `config`'s product. `Unavailable` for a backend this build does
/// not compile, or `oauth` (no backend yet); a connect failure is the backend's own error, and the
/// caller keeps the venue data-less.
pub(crate) fn connect_data_client(
    config: &IbkrConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    match config.backend {
        IbkrBackend::Socket => socket_client(config, sink),
        IbkrBackend::Cpapi => cpapi_client(config, sink),
        IbkrBackend::Oauth => Err(IbkrError::Unavailable),
    }
}

#[cfg(feature = "ibkr-socket")]
fn socket_client(
    config: &IbkrConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    Ok(Box::new(crate::feed::socket::DataClientBusSpecBackendIbkrSocket::connect(config, sink)?))
}

#[cfg(not(feature = "ibkr-socket"))]
fn socket_client(
    _config: &IbkrConfig,
    _sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    Err(IbkrError::Unavailable)
}

/// Open the market-data connection for the price stream's supervised RE-DIAL (`crate::feed::price`): the same as
/// [`connect_data_client`], spending the re-dial lane's share of the gateway's hourly data budget
/// (`crate::transport::gateway_gate::DataDialLane::Redial`) on the socket backend. The cpapi backend redials by itself and
/// is never re-dialled from here: it is opened as ever.
pub(crate) fn redial_data_client(
    config: &IbkrConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    match config.backend {
        IbkrBackend::Socket => socket_redial(config, sink),
        IbkrBackend::Cpapi | IbkrBackend::Oauth => connect_data_client(config, sink),
    }
}

#[cfg(feature = "ibkr-socket")]
fn socket_redial(
    config: &IbkrConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    use crate::feed::socket::DataClientBusSpecBackendIbkrSocket;
    use crate::transport::gateway_gate::DataDialLane;
    use crate::transport::socket::OPEN_DEADLINE;
    let socket = DataClientBusSpecBackendIbkrSocket::connect_in(
        config,
        sink,
        OPEN_DEADLINE,
        DataDialLane::Redial,
    )?;
    Ok(Box::new(socket))
}

#[cfg(not(feature = "ibkr-socket"))]
fn socket_redial(
    _config: &IbkrConfig,
    _sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    Err(IbkrError::Unavailable)
}

/// May the price stream's supervisor re-dial RIGHT NOW? `Ok` or how long to wait before asking again. Asked before
/// every dial and spending nothing, so a dial that could not have happened costs the supervisor neither a spent attempt
/// nor a backoff step: on the socket backend the gateway's port must answer (a closed one is the daily logoff or a
/// restart: probed every `PROBE_EVERY`) and the gate must allow a data dial of the re-dial lane
/// (`GatewayGate::data_ready`). A connection the session already holds live (`ibapi` reconnected it itself, or a
/// lookup dialled it) needs no dial at all: the re-dial JOINS it, so it is ready whatever the budget says. The cpapi
/// client is never re-dialled from here.
pub(crate) fn redial_ready(config: &IbkrConfig) -> Result<(), std::time::Duration> {
    match config.backend {
        IbkrBackend::Socket => socket_ready(config),
        IbkrBackend::Cpapi | IbkrBackend::Oauth => Ok(()),
    }
}

#[cfg(feature = "ibkr-socket")]
fn socket_ready(config: &IbkrConfig) -> Result<(), std::time::Duration> {
    use crate::transport::gateway_gate::{DataDialLane, gate_for, port_answers};
    if crate::feed::price::session::session_for(config).live().is_some() {
        return Ok(());
    }
    let addr = format!("{}:{}", config.host, config.port);
    if !port_answers(&addr) {
        return Err(crate::feed::price::redial::PROBE_EVERY);
    }
    gate_for(&addr).data_ready(DataDialLane::Redial)
}

#[cfg(not(feature = "ibkr-socket"))]
fn socket_ready(_config: &IbkrConfig) -> Result<(), std::time::Duration> {
    Ok(())
}

#[cfg(feature = "ibkr-cpapi")]
fn cpapi_client(
    config: &IbkrConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    Ok(Box::new(crate::feed::cpapi::DataClientBusSpecBackendIbkrCpapi::connect(config, sink)?))
}

#[cfg(not(feature = "ibkr-cpapi"))]
fn cpapi_client(
    _config: &IbkrConfig,
    _sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClientBusSpecBackendIbkr>, IbkrError> {
    Err(IbkrError::Unavailable)
}

/// What both IBKR data backends answer for a book (`verb` = [`DataClientVerb::Book`]), and what depth
/// ([`DataClientVerb::Depth`]) would be answered: the declared caps row refuses first
/// (`vike_data::require_live_verb`), and the backend refuses after it regardless — no IBKR data
/// backend holds a depth pump any more, so a row flipped back to `true` without one would still not
/// open a `reqMktDepth` the account cannot hold.
pub(crate) fn no_book_lane(
    verb: DataClientVerb,
) -> Result<DataClientSubscriptionId, DataClientError> {
    vike_data::require_live_verb(VENUE, verb)?;
    Err(DataClientError::Unsupported(
        "ibkr: the accounts hold no market-depth entitlement (decision 0140)",
    ))
}

/// Offline doubles the data clients' unit tests share: a config built through the real loader (so
/// a field added to `IbkrConfig` needs no edit here) and a sink that records every call.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use vike_data::DataClientSink;
    use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
    use vike_model::{AccountTier, StreamDelay, StreamStatus};
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    use vike_secrets::venue_setting::VenueSettings;

    use crate::config::{IbkrConfig, load_ibkr_config_from};
    use vike_venues::IbkrBackend;

    /// A demo config on `backend` whose gateway is a closed loopback port, so a test that reached
    /// the network by mistake fails fast instead of finding a real gateway.
    pub(crate) fn offline_config(backend: IbkrBackend) -> IbkrConfig {
        let fields = CredentialFields::from_pairs([("ACCOUNT", "DU0000000")]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let seeded =
            VenueSettings::default().with_urls(vike_venues::venue_urls::VenueUrls::seed("ibkr"));
        let mut config = load_ibkr_config_from(AccountTier::Demo, &creds, &seeded)
            .expect("an account alone is a loadable config");
        config.backend = backend;
        config.cpapi_url = "https://127.0.0.1:1".into();
        config.port = 1;
        config
    }

    /// One recorded sink call, as a short word plus what the tests read.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum Call {
        Seed(String, usize),
        Close(String, Bar),
        Forming(String, Bar),
        BarCloseTick(f64),
        Quote(String, TickQuote),
        Trade(String, TickTrade),
        Status(String, String, StreamStatus),
        Delay(String, String, StreamDelay),
    }

    #[derive(Default)]
    pub(crate) struct RecSink {
        pub(crate) calls: Mutex<Vec<Call>>,
    }

    impl RecSink {
        pub(crate) fn arc() -> Arc<RecSink> {
            Arc::new(RecSink::default())
        }
        pub(crate) fn take(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
        fn push(&self, c: Call) {
            self.calls.lock().unwrap().push(c);
        }
    }

    impl DataClientSink for RecSink {
        fn seed_bars(&self, _v: &str, _s: &str, interval: &str, bars: Vec<Bar>) {
            self.push(Call::Seed(interval.into(), bars.len()));
        }
        fn close_bar(&self, _v: &str, _s: &str, interval: &str, bar: Bar) {
            self.push(Call::Close(interval.into(), bar));
        }
        fn forming_bar(&self, _v: &str, _s: &str, interval: &str, bar: Bar) {
            self.push(Call::Forming(interval.into(), bar));
        }
        fn mark_tick(&self, _v: &str, _s: &str, _price: f64, _ts: i64) {
            panic!("an IBKR cpapi stream carries no venue mark");
        }
        fn bar_close_tick(&self, _v: &str, _s: &str, px: f64, _ts: i64) {
            self.push(Call::BarCloseTick(px));
        }
        fn quote(&self, _v: &str, symbol: &str, quote: TickQuote) {
            self.push(Call::Quote(symbol.into(), quote));
        }
        fn trade(&self, _v: &str, symbol: &str, trade: TickTrade) {
            self.push(Call::Trade(symbol.into(), trade));
        }
        fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {
            panic!("no IBKR data client serves a book (decision 0140)");
        }
        fn stream_status(&self, _v: &str, symbol: &str, stream: &str, status: StreamStatus) {
            self.push(Call::Status(symbol.into(), stream.into(), status));
        }
        fn data_delay(&self, _v: &str, symbol: &str, stream: &str, delay: StreamDelay) {
            self.push(Call::Delay(symbol.into(), stream.into(), delay));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{RecSink, offline_config};
    use super::*;

    /// Book and depth are refused on BOTH backends: the shared refusal answers `Unsupported` for
    /// both verbs, and both backends' declared rows say no book and no depth.
    #[test]
    fn book_and_depth_are_unsupported_on_both_backends() {
        for verb in [DataClientVerb::Book, DataClientVerb::Depth] {
            assert!(
                matches!(no_book_lane(verb), Err(DataClientError::Unsupported(_))),
                "{verb:?} must be Unsupported"
            );
        }
        for backend in [IbkrBackend::Socket, IbkrBackend::Cpapi] {
            let live = crate::CAPABILITIES.live_data; // ONE row for both backends
            assert!(!live.book && !live.depth, "{backend:?} declares no book and no depth");
            assert!(live.bars && live.quotes && live.trades, "{backend:?} keeps the L1 lanes");
        }
    }

    /// `oauth` has no backend: the door opens nothing and touches no network.
    #[test]
    fn the_door_opens_nothing_for_oauth() {
        let sink: Arc<dyn DataClientSink> = RecSink::arc();
        let opened = connect_data_client(&offline_config(IbkrBackend::Oauth), sink);
        assert!(matches!(opened, Err(IbkrError::Unavailable)));
    }
}
