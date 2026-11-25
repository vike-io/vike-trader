//! The crate's own test helpers (`test-support`, never in a normal build; the `vike-ctrader` idiom): the
//! SDK's canonical test order, the signer a user-signed signature recovers to, and the raw testnet
//! `/exchange` POST. Unit tests reach them as `crate::test_support`, the `tests/` binaries through the
//! self dev-dependency. None reads the environment or the credential store.

use std::sync::{Arc, Mutex};

use vike_bridge_core::transport::read_body_ambiguous;
use vike_transport::http::blocking_agent;

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
    format!("0x{}", vike_utility::hex::hex_lower(&eip712::keccak256(&point.as_bytes()[1..])[12..]))
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

/// **The kept rows the datahub's writer writes for a listing** (decision 0290, the owner's ruling «Б» of 2026-11-16):
/// the catalog's rows of `meta` + `spot_meta` and of each builder dex `(its meta, its perpDexs position, its name)`,
/// listed with no kept list, each named as the writer names it (`vike_catalog::spelled`). The ONE place a test's
/// kept rows are spelled; production code reads what a writer stored.
pub fn written(
    meta: &serde_json::Value,
    spot_meta: &serde_json::Value,
    dexes: &[(serde_json::Value, u32, &str)],
) -> Vec<vike_model::Instrument> {
    use crate::instrument::symbology::Symbology;
    let mut listing = Symbology::from_meta_named_by(meta, spot_meta, None);
    for (dex_meta, position, name) in dexes {
        listing.extend_with_perp_dex(dex_meta, *position, name, spot_meta);
    }
    vike_catalog::spelled(crate::catalog::instruments_from_symbology(&listing))
}

/// **The kept hyperliquid rows the crate's tests read** (decision 0290): [`written`] for a `meta` of the perpetuals
/// `BTC` (asset 0), `ETH` (1), `kPEPE` (2) and `HPOS`, `CASHCAT`, `RLB`, `STRICT`, `NOCROSS`, `OLD`, `JUNK`, `FUTURE`,
/// `kNEW`, a `spotMeta` of the pairs `PURR/USDC` (coin `PURR/USDC`, asset 10000) and `HYPE/USDC` (coin `@107`, asset
/// 10107), the USDC builder dexes `xyz` (position 1: `xyz:TSLA` at asset 110000, `xyz:AAPL`), `test` (`test:ABC`,
/// `test:XYZ`, `test:X`, `test:ISO`, `test:GONE`), `vntls` (`vntls:AAPL`) and `flx` (`flx:GOLD`), and the USDT0 dex
/// `cash` (`cash:TSLA`). User names `BTCUSDC.P`, `KPEPEUSDC.P`, `PURRUSDC`, `HYPEUSDC`, `XYZ:TSLAUSDC.P`,
/// `CASH:TSLAUSDT0.P` and so on: every name this crate's tests look a listed instrument up by.
pub fn kept_rows() -> Vec<vike_model::Instrument> {
    let listed = |coins: &[&str]| -> Vec<serde_json::Value> {
        coins
            .iter()
            .map(|coin| serde_json::json!({"name": coin, "szDecimals": 2, "maxLeverage": 10}))
            .collect()
    };
    let meta = serde_json::json!({"universe": listed(&[
        "BTC", "ETH", "kPEPE", "HPOS", "CASHCAT", "RLB", "STRICT", "NOCROSS", "OLD", "JUNK", "FUTURE",
        "kNEW",
    ])});
    let spot = serde_json::json!({
        "tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "PURR", "szDecimals": 0, "index": 1},
            {"name": "HYPE", "szDecimals": 2, "index": 150},
            {"name": "USDT0", "szDecimals": 2, "index": 268}
        ],
        "universe": [
            {"name": "PURR/USDC", "tokens": [1, 0], "index": 0},
            {"name": "@107", "tokens": [150, 0], "index": 107}
        ]
    });
    let dex = |coins: &[&str], collateral: u32| serde_json::json!({"universe": listed(coins), "collateralToken": collateral});
    written(
        &meta,
        &spot,
        &[
            (dex(&["xyz:TSLA", "xyz:AAPL"], 0), 1, "xyz"),
            (dex(&["test:ABC", "test:XYZ", "test:X", "test:ISO", "test:GONE"], 0), 2, "test"),
            (dex(&["vntls:AAPL"], 0), 3, "vntls"),
            (dex(&["flx:GOLD"], 0), 4, "flx"),
            (dex(&["cash:TSLA"], 268), 5, "cash"),
        ],
    )
}

/// **Hold [`kept_rows`] as this process's hyperliquid kept list** (`vike_catalog::hold_kept_list`), once. Every test
/// of this crate that needs a kept list holds THIS one (a `Symbology` built in a unit test holds it itself): the list
/// is process-wide and the tests run as threads, so a second call changes nothing.
pub fn hold_kept_rows() {
    static HELD: std::sync::Once = std::sync::Once::new();
    HELD.call_once(|| vike_catalog::hold_kept_list(VENUE, kept_rows()));
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
