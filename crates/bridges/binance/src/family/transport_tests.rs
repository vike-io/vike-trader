use std::sync::Mutex;

use crate::family::signer::BinanceHmacSigner;
use serde_json::{Value, json};
use vike_secrets::credentials::Credentials;

use super::*;
use crate::transport::ws_auth::build_subscribe_request;

const LOCAL_MS: i64 = 1_700_000_000_000;
const AN_HOUR_MS: i64 = 3_600_000;

fn creds() -> Credentials {
    Credentials {
        api_key: "kkkk1234".to_string().into(),
        api_secret: "s3cr3t".to_string().into(),
        passphrase: None,
    }
}

fn params() -> [(&'static str, String); 2] {
    [("symbol", "BTCUSDT".to_string()), ("quantity", "0.010".to_string())]
}

/// The signed query of a signer on `stamp`, and its headers.
fn signed(stamp: impl Fn() -> i64 + Send + Sync + 'static) -> (String, Vec<(String, String)>) {
    let prepared = BinanceHmacSigner::new(&creds(), stamp).prepare(&params(), "POST", "/x");
    (prepared.query, prepared.headers)
}

/// **The promise of the row.** A clock nobody read, and one read as zero, sign the bytes a signer on the bare local clock
/// signs, and so does the stream's subscribe frame.
#[test]
fn at_zero_skew_the_signed_bytes_are_identical() {
    let read_as_zero = VenueClock::default();
    read_as_zero.read_if_due(0, |_| Ok::<_, String>(0)).unwrap();
    for clock in [VenueClock::default(), read_as_zero] {
        assert_eq!(signed(clock.ms_from(|| LOCAL_MS)), signed(|| LOCAL_MS));
        assert_eq!(
            build_subscribe_request("k", "s", clock.ms_from(|| LOCAL_MS)(), 5000, "id"),
            build_subscribe_request("k", "s", LOCAL_MS, 5000, "id")
        );
        assert_eq!(clock.us_from(|| LOCAL_MS * 1000)(), LOCAL_MS * 1000);
    }
}

/// An offset signs what the signer's own correction signs: the golden of `vike_bridge_core::signer`'s
/// `binance_signs_with_the_offset_clock`, computed independently there. Aster's µs clock moves by the same time.
#[test]
fn an_offset_signs_what_the_signers_own_correction_signed() {
    let clock = VenueClock::default();
    clock.read_if_due(0, |_| Ok::<_, String>(7)).unwrap();
    let (query, _) = signed(clock.ms_from(|| LOCAL_MS));
    assert_eq!(
        query,
        "symbol=BTCUSDT&quantity=0.010&timestamp=1700000000007&recvWindow=5000\
         &signature=a0c6c024253df675f62414d2cc8eb7a4d7805a9be84bcc99617c6943ca445920"
    );
    assert_eq!(clock.us_from(|| LOCAL_MS * 1000)(), LOCAL_MS * 1000 + 7000);
}

/// Answers a signed request with the query its signer prepared.
struct Echo;
impl RestTransport for Echo {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        Ok(json!(signer.prepare(params, method, path).query))
    }
    vike_bridge_core::no_public_call!();
}

/// A host whose clock is `ahead_ms` of this box, or which does not answer, and every clock read it was asked.
struct Host<'a> {
    ahead_ms: Option<i64>,
    asked: &'a Mutex<Vec<String>>,
}
impl RestTransport for Host<'_> {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("a clock read signs nothing: {path}")
    }
    fn public(
        &self,
        base_url: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        self.asked.lock().unwrap().push(format!("{base_url}{path}"));
        match self.ahead_ms {
            Some(ahead) => Ok(json!({"serverTime": vike_model::now_ms() + ahead})),
            None => Err(VenueApiError { code: 503, msg: "no clock".into() }),
        }
    }
}

fn client<'a>(
    clock: &VenueClock,
    ahead_ms: Option<i64>,
    asked: &'a Mutex<Vec<String>>,
) -> RestTransportOnVenueClock<Echo, Host<'a>> {
    RestTransportOnVenueClock {
        inner: Echo,
        reader: Host { ahead_ms, asked },
        clock: clock.clone(),
        time_path: "/time",
    }
}

fn stamp_of(query: &Value) -> i64 {
    let query = query.as_str().unwrap();
    query.split('&').find_map(|p| p.strip_prefix("timestamp=")).unwrap().parse().unwrap()
}

/// The first signed request of a client asks its host once, and from then on each signer and frame on that clock is on
/// the venue's time with no further read, a second client's included.
#[test]
fn one_read_moves_every_signer_and_frame_on_the_clock() {
    let asked = Mutex::new(Vec::new());
    let clock = VenueClock::default();
    let signer = BinanceHmacSigner::new(&creds(), clock.ms());
    let (first, second) =
        (client(&clock, Some(AN_HOUR_MS), &asked), client(&clock, Some(AN_HOUR_MS), &asked));

    let on_the_venue = |stamp: i64| (stamp - vike_model::now_ms() - AN_HOUR_MS).abs() < 60_000;
    assert!(on_the_venue(stamp_of(&first.signed("h", "/a", "GET", &[], &signer).unwrap())));
    assert!(on_the_venue(stamp_of(&first.signed_requery("h", "/a", "GET", &[], &signer).unwrap())));
    assert!(on_the_venue(stamp_of(&second.signed("h", "/a", "POST", &[], &signer).unwrap())));
    assert!(on_the_venue(clock.ms()()));
    assert!(on_the_venue(clock.us()() / 1000));
    assert_eq!(*asked.lock().unwrap(), ["h/time"], "one read, of the client's own path");
}

/// A host with no clock: the request still goes out, on the local clock, and a public read of the client asks nothing.
#[test]
fn a_failed_read_signs_on_the_local_clock() {
    let asked = Mutex::new(Vec::new());
    let clock = VenueClock::default();
    let signer = BinanceHmacSigner::new(&creds(), clock.ms());
    let c = client(&clock, None, &asked);
    let stamp = stamp_of(&c.signed("h", "/a", "GET", &[], &signer).unwrap());
    assert!((stamp - vike_model::now_ms()).abs() < 60_000, "{stamp}");
    assert_eq!(asked.lock().unwrap().len(), 1);
    // Inside the retry window: no second read in front of a signed request.
    c.signed("h", "/a", "GET", &[], &signer).unwrap();
    assert_eq!(asked.lock().unwrap().len(), 1);
}
