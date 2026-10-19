use std::io::Read;

use vike_data::{DataClient, LiveDataSink};
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::{
    FetchFailure, FetchRows, FetchWhat, OptionsCall, OptionsFeedSpec, OptionsReply,
};

use super::*;
use crate::data::{DataLane, Holder};
use crate::rest::{
    CatalogTraits, FetchCtx, OptionsFeedHandle, OptionsFeedSink, RestFacts, RestLane,
};
use crate::session::ConnectionScope;

struct NullData;

impl DataLane for NullData {
    fn venues(&self) -> Vec<String> {
        Vec::new()
    }

    fn open(
        &self,
        venue: &str,
        _holder: &Holder,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Err(format!("null lane: no client for {venue}"))
    }
}

struct NullRest;

impl RestLane for NullRest {
    fn facts(&self) -> RestFacts {
        RestFacts::default()
    }

    fn fetch(
        &self,
        _scope: &ConnectionScope,
        _what: &FetchWhat,
        _credentialed: bool,
        _ctx: &FetchCtx,
    ) -> Result<FetchRows, FetchFailure> {
        Err(FetchFailure::Refused("null lane".into()))
    }

    fn catalog_describe(&self, _s: &ConnectionScope, _v: &str) -> Result<CatalogTraits, String> {
        Err("null lane".into())
    }

    fn options(&self, _s: &ConnectionScope, _c: &OptionsCall) -> Result<OptionsReply, String> {
        Err("null lane".into())
    }

    fn open_options_feed(
        &self,
        _s: &ConnectionScope,
        _spec: &OptionsFeedSpec,
        _sink: Arc<dyn OptionsFeedSink>,
    ) -> Result<Box<dyn OptionsFeedHandle>, String> {
        Err("null lane".into())
    }

    fn resolve_family(
        &self,
        _s: &ConnectionScope,
        _v: &str,
        _f: &str,
        _now: i64,
    ) -> Result<Vec<String>, String> {
        Err("null lane".into())
    }

    fn credential_presence(&self, _venue: &str) -> CredentialPresence {
        CredentialPresence::NotNeeded
    }
}

fn lanes() -> Lanes {
    Lanes { data: Arc::new(NullData), rest: Arc::new(NullRest) }
}

fn keys() -> HubKeys {
    HubKeys::new(b"observe".to_vec(), b"control".to_vec())
}

#[test]
fn a_daemon_without_keys_refuses_to_start() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let err = serve(
        listener,
        HubKeys::new(Vec::new(), Vec::new()),
        lanes(),
        ServerConfig::PRODUCTION,
        Arc::new(AtomicBool::new(false)),
    )
    .expect_err("no keys, no start");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("no hub key"), "{err}");
}

#[test]
fn a_daemon_refuses_a_public_listener_a_root_bound_itself() {
    let listener = TcpListener::bind("0.0.0.0:0").expect("wildcard binds in a test");
    let err = serve(
        listener,
        keys(),
        lanes(),
        ServerConfig::PRODUCTION,
        Arc::new(AtomicBool::new(false)),
    )
    .expect_err("a public listener");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn the_stop_flag_ends_a_blocked_accept_and_the_port_closes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("accept-test".into())
            .spawn(move || serve(listener, keys(), lanes(), ServerConfig::PRODUCTION, stop))
            .expect("spawn")
    };
    // It is serving: a connection is accepted and answered by the handshake (a Welcome frame follows
    // a Hello); here it is enough that the socket accepts.
    let mut probe = TcpStream::connect(addr).expect("serving");
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let result = server.join().expect("join");
    assert!(result.is_ok(), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "the stop took {:?}", started.elapsed());
    // The idle connection was interrupted by the stop: it reads EOF rather than hanging.
    probe.set_read_timeout(Some(Duration::from_secs(2))).expect("timeout");
    let mut byte = [0u8; 1];
    assert!(matches!(probe.read(&mut byte), Ok(0) | Err(_)));
    // And the listener is gone.
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err());
}
