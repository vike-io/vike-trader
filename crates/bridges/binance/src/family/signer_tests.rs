//! Golden signed output of [`BinanceHmacSigner`], computed independently (Python `hmac`) over a fixed clock plus an
//! offset, and its `Debug` line: the bytes `vike_bridge_core::signer` pinned before the signer moved here, unchanged.

use vike_bridge_core::signer::{Signer, VenueClock};
use vike_secrets::credentials::Credentials;

use super::BinanceHmacSigner;

const LOCAL_MS: i64 = 1_700_000_000_000;

/// The signed query for `symbol=BTCUSDT&quantity=0.010` stamped 7 ms after [`LOCAL_MS`].
const SIGNED_AT_PLUS_7: &str = "symbol=BTCUSDT&quantity=0.010&timestamp=1700000000007&recvWindow=5000\
     &signature=a0c6c024253df675f62414d2cc8eb7a4d7805a9be84bcc99617c6943ca445920";

fn creds() -> Credentials {
    Credentials::for_test("kkkk1234", "s3cr3t", Some("pp"))
}

fn signed(stamp: impl Fn() -> i64 + Send + Sync + 'static) -> String {
    BinanceHmacSigner::new(&creds(), stamp)
        .prepare(
            &[("symbol", "BTCUSDT".to_string()), ("quantity", "0.010".to_string())],
            "POST",
            "/x",
        )
        .query
}

#[test]
fn binance_signs_with_the_offset_clock() {
    let s = BinanceHmacSigner::new(&creds(), || LOCAL_MS);
    s.set_offset_ms(7);
    let p = s.prepare(
        &[("symbol", "BTCUSDT".to_string()), ("quantity", "0.010".to_string())],
        "POST",
        "/x",
    );
    assert_eq!(p.query, SIGNED_AT_PLUS_7);
    assert_eq!(p.headers, vec![("X-MBX-APIKEY".to_string(), "kkkk1234".to_string())]);
    assert_eq!(format!("{s:?}"), "BinanceHmacSigner(key=***1234, recv_window=5000)");
}

#[test]
fn at_zero_skew_the_signed_bytes_are_identical() {
    let read_as_zero = VenueClock::default();
    read_as_zero.read_if_due(0, |_| Ok::<_, ()>(0)).unwrap();
    for clock in [VenueClock::default(), read_as_zero] {
        assert_eq!(signed(clock.ms_from(|| LOCAL_MS)), signed(|| LOCAL_MS));
    }
}

/// The golden of `binance_signs_with_the_offset_clock`, which corrects by `set_offset_ms(7)`: a clock read as 7 ms
/// ahead signs it.
#[test]
fn an_offset_signs_what_the_signers_own_correction_signs() {
    let clock = VenueClock::default();
    clock.read_if_due(0, |_| Ok::<_, ()>(7)).unwrap();
    assert_eq!(signed(clock.ms_from(|| LOCAL_MS)), SIGNED_AT_PLUS_7);
}
