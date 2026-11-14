//! The crate's own test helpers (`test-support`, never in a normal build; the `vike-ctrader` idiom): the
//! SDK's canonical test order, the signer a user-signed signature recovers to, and the raw testnet
//! `/exchange` POST. Unit tests reach them as `crate::test_support`, the `tests/` binaries through the
//! self dev-dependency. None reads the environment or the credential store.

use std::sync::{Arc, Mutex};

use vike_bridge_core::http::blocking_agent;
use vike_bridge_core::transport::read_body_ambiguous;

use vike_venues::venue_urls::{UrlChannel, UrlProtocol, VenueUrls};

use crate::signing::action::{
    Action, HyperliquidBuilderFee, LimitParams, OrderAction, OrderKind, OrderWire,
};
use crate::signing::{Signature, eip712};
use crate::transport::HyperliquidTransport;
use crate::transport::consts::{EXCHANGE_PATH, VENUE};
use vike_model::VenueNetwork;

/// `network`'s `trade` REST base on the release's rows (`VENUE_URL_SEED`): what a start on that
/// network dials, for a test or a smoke that builds a client by hand.
pub fn seed_rest(network: VenueNetwork) -> String {
    crate::config::base_url(network, &VenueUrls::seed(VENUE), UrlChannel::Trade, UrlProtocol::Rest)
        .expect("the seed holds hyperliquid's rest rows")
        .to_string()
}

/// A transport on `network`'s seed rows ([`seed_rest`]), with a fresh gate.
pub fn seed_transport(network: VenueNetwork) -> HyperliquidTransport {
    HyperliquidTransport::new(network, &seed_rest(network))
}

/// `network`'s exec addresses on the release's rows, as a start resolves them.
pub fn seed_execution_urls(network: VenueNetwork) -> crate::config::HyperliquidUrls {
    crate::config::HyperliquidUrls::of(network, &VenueUrls::seed(VENUE))
        .expect("the seed holds hyperliquid's exec rows")
}

/// `activeAssetData`'s answer for ETH holding a `kind` (`cross`/`isolated`) leverage of `value`.
pub fn held(kind: &str, value: u64) -> serde_json::Value {
    serde_json::json!({"user":"0xmaster","coin":"ETH","leverage":{"type":kind,"value":value},
           "maxTradeSzs":["1.0","1.0"],"availableToTrade":["8.8","8.8"],"markPx":"3000.0"})
}

/// The Ethereum address `signature` recovers to over `digest`: reassemble `r‖s`, recover the verifying
/// key, derive `keccak256(pubkey[1..])[12..]`. A LOCAL round-trip proof that a user-signed action is
/// signed by the key it claims; it does not prove the VENUE computes the same digest.
pub fn recovered_address(signature: &Signature, digest: &[u8; 32]) -> String {
    use k256::ecdsa::{RecoveryId, Signature as EcdsaSig, VerifyingKey};
    let r = hex::decode(signature.r.strip_prefix("0x").unwrap()).unwrap();
    let s = hex::decode(signature.s.strip_prefix("0x").unwrap()).unwrap();
    let mut rs = [0u8; 64];
    rs[..32].copy_from_slice(&r);
    rs[32..].copy_from_slice(&s);
    let ecdsa_sig = EcdsaSig::from_slice(&rs).expect("valid r,s");
    let recid = RecoveryId::from_byte(signature.v - 27).expect("v ∈ {27,28}");
    let vk = VerifyingKey::recover_from_prehash(digest, &ecdsa_sig, recid)
        .expect("recovery succeeds for a signature we just produced");
    let point = vk.to_sec1_point(false); // 0x04 ‖ X ‖ Y
    format!("0x{}", hex::encode(&eip712::keccak256(&point.as_bytes()[1..])[12..]))
}

/// The SDK's canonical test order: asset 1, BUY 3.5 at `limit_px` as a `tif` limit, no cloid, `na`
/// grouping, `builder` attached. The price and TIF spellings are each caller's (they are signed bytes).
pub fn sdk_order(limit_px: &str, tif: &str, builder: Option<HyperliquidBuilderFee>) -> Action {
    Action::Order(OrderAction {
        orders: vec![OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: limit_px.to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: OrderKind::Limit(LimitParams { tif: tif.to_string() }),
            cloid: None,
        }],
        grouping: "na".to_string(),
        builder,
    })
}

/// A loopback `/info` (`http://127.0.0.1:<port>`) that reads each request whole, answers it with
/// `answer(its body)` (a whole HTTP reply), and records every body it was sent. No venue is called.
pub fn info_server(
    answer: impl Fn(&str) -> &'static str + Send + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::Builder::new()
        .name("hyperliquid-loopback-info".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let (mut raw, mut buffer) = (Vec::new(), [0u8; 4096]);
                // The headers, then as many body bytes as they announce.
                let body = loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    raw.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        if n == 0 {
                            break String::new();
                        }
                        continue;
                    };
                    let want = head
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= want || n == 0 {
                        break body.to_string();
                    }
                };
                log.lock().unwrap().push(body.clone());
                let _ = stream.write_all(answer(&body).as_bytes());
            }
        })
        .expect("spawn the loopback /info");
    (format!("http://{addr}"), seen)
}

/// POST raw `/exchange` bytes to TESTNET and read the verdict back as (status, VERBATIM body).
///
/// A bare agent rather than `HyperliquidTransport::post`, which is `pub(crate)` and parses the body
/// into a `Value`: a certification records the venue's reply exactly as sent, and a probe or a negative
/// control posts bytes the production API deliberately offers no way to build. Same rustls agent and
/// same body reader the real transport uses, so only the bytes differ.
pub fn post_testnet_exchange(payload: &[u8]) -> (u16, String) {
    let agent = blocking_agent();
    let exchange = format!("{}{EXCHANGE_PATH}", seed_rest(VenueNetwork::Testnet));
    let mut response = agent
        .post(&exchange)
        .header("Content-Type", "application/json")
        .send(payload)
        .expect("testnet /exchange is reachable");
    let status = response.status().as_u16();
    let text = read_body_ambiguous(&mut response).expect("response body reads");
    (status, text)
}
