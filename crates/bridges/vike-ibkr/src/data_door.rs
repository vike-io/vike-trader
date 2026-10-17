//! The ONE door to an IBKR market-data client (decision 0140, "Exec, data and reconcile all use
//! that one product"): [`connect_data_client`] matches on `cfg.backend` and opens the data client
//! of that product only — the socket backend's `IbkrFeeds` over ibapi on `data_client_id`, or the
//! cpapi backend's `CpapiFeeds` over the Client Portal session. A root holding the config the start
//! bound therefore gets the data client of the same session, never a second product's or tier's.
//!
//! Also home to the refusal both clients answer for the book and depth verbs ([`no_book_lane`]):
//! the accounts hold no depth entitlement on either product (decision 0140, Context 7), so the
//! declared caps row (`crates/vike-venues/src/venue_caps.rs`'s `IBKR`, one row for both backends) says
//! no book and no depth, and the refusal is driven off that row.

use std::sync::Arc;

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_venues::LiveVerb;

use crate::config::IbkrConfig;
use crate::error::IbkrError;
use vike_venues::IbkrBackend;

/// Canonical venue string the data clients stamp on the sink.
pub(crate) const VENUE: &str = "ibkr";

/// Open the market-data client of `cfg`'s product. `Unavailable` for a backend this build does not
/// compile, or `oauth` (no backend yet); a connect failure is the backend's own error, and the
/// caller keeps the venue data-less.
pub fn connect_data_client(
    cfg: &IbkrConfig,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, IbkrError> {
    match cfg.backend {
        IbkrBackend::Socket => socket_client(cfg, sink),
        IbkrBackend::Cpapi => cpapi_client(cfg, sink),
        IbkrBackend::Oauth => Err(IbkrError::Unavailable),
    }
}

#[cfg(feature = "ibkr-socket")]
fn socket_client(
    cfg: &IbkrConfig,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, IbkrError> {
    Ok(Box::new(crate::market_feed::IbkrFeeds::connect(cfg, sink)?))
}

#[cfg(not(feature = "ibkr-socket"))]
fn socket_client(
    _cfg: &IbkrConfig,
    _sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, IbkrError> {
    Err(IbkrError::Unavailable)
}

#[cfg(feature = "ibkr-cpapi")]
fn cpapi_client(
    cfg: &IbkrConfig,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, IbkrError> {
    Ok(Box::new(crate::cpapi_feed::CpapiFeeds::connect(cfg, sink)?))
}

#[cfg(not(feature = "ibkr-cpapi"))]
fn cpapi_client(
    _cfg: &IbkrConfig,
    _sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, IbkrError> {
    Err(IbkrError::Unavailable)
}

/// What both IBKR data clients answer for `subscribe_book` (`verb` = [`LiveVerb::Book`]) and
/// `subscribe_depth` ([`LiveVerb::Depth`]): the declared caps row refuses first
/// (`vike_data::require_live_verb`), and the client refuses after it regardless — no IBKR data
/// client holds a depth pump any more, so a row flipped back to `true` without one would still not
/// open a `reqMktDepth` the account cannot hold.
pub(crate) fn no_book_lane(verb: LiveVerb) -> Result<SubscriptionId, LiveDataError> {
    vike_data::require_live_verb(VENUE, verb)?;
    Err(LiveDataError::Unsupported(
        "ibkr: the accounts hold no market-depth entitlement (decision 0140)",
    ))
}

/// Offline doubles the data clients' unit tests share: a config built through the real loader (so
/// a field added to `IbkrConfig` needs no edit here) and a sink that records every call.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use vike_data::LiveDataSink;
    use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
    use vike_model::{AccountTier, DataDelay, StreamStatus};
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    use vike_secrets::venue_setting::VenueSettings;

    use crate::config::{IbkrConfig, load_ibkr_config_from};
    use vike_venues::IbkrBackend;

    /// A demo config on `backend` whose gateway is a closed loopback port, so a test that reached
    /// the network by mistake fails fast instead of finding a real gateway.
    pub(crate) fn offline_cfg(backend: IbkrBackend) -> IbkrConfig {
        let fields = CredentialFields::from_pairs([("ACCOUNT", "DU0000000")]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let mut cfg = load_ibkr_config_from(AccountTier::Demo, &creds, &VenueSettings::default())
            .expect("an account alone is a loadable config");
        cfg.backend = backend;
        cfg.cpapi_url = "https://127.0.0.1:1".into();
        cfg.port = 1;
        cfg
    }

    /// One recorded sink call, as a short word plus what the tests read.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum Call {
        Seed(String, usize),
        Close(String, Bar),
        Forming(String, Bar),
        BarCloseTick(f64),
        Quote(String, QuoteTick),
        Trade(String, TradeTick),
        Status(String, String, StreamStatus),
        Delay(String, String, DataDelay),
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

    impl LiveDataSink for RecSink {
        fn seed_bars(&self, _v: &str, _s: &str, interval: &str, bars: Vec<Bar>) {
            self.push(Call::Seed(interval.into(), bars.len()));
        }
        fn close_bar(&self, _v: &str, _s: &str, interval: &str, bar: Bar) {
            self.push(Call::Close(interval.into(), bar));
        }
        fn forming_bar(&self, _v: &str, _s: &str, interval: &str, bar: Bar) {
            self.push(Call::Forming(interval.into(), bar));
        }
        fn mark_tick(&self, _v: &str, _s: &str, _px: f64, _ts: i64) {
            panic!("an IBKR cpapi stream carries no venue mark");
        }
        fn bar_close_tick(&self, _v: &str, _s: &str, px: f64, _ts: i64) {
            self.push(Call::BarCloseTick(px));
        }
        fn quote(&self, _v: &str, symbol: &str, quote: QuoteTick) {
            self.push(Call::Quote(symbol.into(), quote));
        }
        fn trade(&self, _v: &str, symbol: &str, trade: TradeTick) {
            self.push(Call::Trade(symbol.into(), trade));
        }
        fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {
            panic!("no IBKR data client serves a book (decision 0140)");
        }
        fn stream_status(&self, _v: &str, symbol: &str, stream: &str, status: StreamStatus) {
            self.push(Call::Status(symbol.into(), stream.into(), status));
        }
        fn data_delay(&self, _v: &str, symbol: &str, stream: &str, delay: DataDelay) {
            self.push(Call::Delay(symbol.into(), stream.into(), delay));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{RecSink, offline_cfg};
    use super::*;

    /// Book and depth are refused on BOTH backends: the shared refusal answers `Unsupported` for
    /// both verbs, and both backends' declared rows say no book and no depth.
    #[test]
    fn book_and_depth_are_unsupported_on_both_backends() {
        for verb in [LiveVerb::Book, LiveVerb::Depth] {
            assert!(
                matches!(no_book_lane(verb), Err(LiveDataError::Unsupported(_))),
                "{verb:?} must be Unsupported"
            );
        }
        for backend in [IbkrBackend::Socket, IbkrBackend::Cpapi] {
            let live = crate::CAPS.live_data; // ONE row for both backends
            assert!(!live.book && !live.depth, "{backend:?} declares no book and no depth");
            assert!(live.bars && live.quotes && live.trades, "{backend:?} keeps the L1 lanes");
        }
    }

    /// `oauth` has no backend: the door opens nothing and touches no network.
    #[test]
    fn the_door_opens_nothing_for_oauth() {
        let sink: Arc<dyn LiveDataSink> = RecSink::arc();
        let opened = connect_data_client(&offline_cfg(IbkrBackend::Oauth), sink);
        assert!(matches!(opened, Err(IbkrError::Unavailable)));
    }
}
