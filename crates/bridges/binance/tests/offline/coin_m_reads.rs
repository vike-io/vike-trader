//! **A COIN-M (`dapi`) mount's reconcile reads, over the public seams only** (offline: a scripted
//! `RestTransport`, canned bodies, no network). The code before the COIN-M mount read the USDT row of a COIN-M
//! wallet answer (none: `None`) and left the account's modes alone on the COIN-M demo book.
//!
//! * the wallet of a `BTCUSD_PERP` reconcile client is the BTC row of `/dapi/v1/balance`, the coin cash decision
//!   0258 adopts (the body is the venue's documented shape);
//! * a linear perp client of the same body still answers the USDT row: `None` here, as before;
//! * the COIN-M DEMO book is one the bridge writes modes on (each book keeps its own), the mainnet one is not.

use serde_json::{Value, json};
use vike_binance::ReconcileClientBinance;
use vike_binance::execution::BinanceUrls;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_execution::recon::ReconcileClient;
use vike_model::AccountTier;
use vike_venues::venue_urls::VenueUrls;

/// The addresses of an account at `tier`, from the release's `venue_url` seed.
fn hosts(tier: AccountTier) -> BinanceUrls {
    BinanceUrls::resolve(&VenueUrls::seed("binance"), tier)
        .expect("the seed holds every binance row")
}

/// A `/dapi/v1/balance` answer: one row per asset (the venue's documented shape).
fn balance_body() -> Value {
    json!([
        {"accountAlias": "SgsR", "asset": "BTC", "balance": "0.00250000", "withdrawAvailable": "0.00250000",
         "crossWalletBalance": "0.00241969", "crossUnPnl": "0.00000000", "availableBalance": "0.00241969",
         "updateTime": 1_592_468_353_979_i64},
        {"accountAlias": "SgsR", "asset": "ETH", "balance": "1.50000000", "withdrawAvailable": "1.5",
         "crossWalletBalance": "1.5", "crossUnPnl": "0", "availableBalance": "1.5",
         "updateTime": 1_592_468_353_979_i64}
    ])
}

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Answers any balance read with [`balance_body`]; anything else is a finding.
struct Wallet;
impl RestTransport for Wallet {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        assert!(path.ends_with("/balance"), "a wallet read only: {path}");
        Ok(balance_body())
    }
    fn public(&self, _b: &str, path: &str, _p: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("a wallet read reads nothing public: {path}")
    }
}

#[test]
fn a_coin_m_reconcile_client_reports_its_settle_coins_wallet() {
    let demo = hosts(AccountTier::Demo);
    let btc = ReconcileClientBinance::perp(NullSigner, Wallet, demo.dapi_rest, "BTCUSD_PERP");
    assert_eq!(btc.fetch_balance(), Ok(Some(0.0025)), "the BTC row's `balance`, the coin cash");
    let eth = ReconcileClientBinance::perp(NullSigner, Wallet, demo.dapi_rest, "ETHUSD_PERP");
    assert_eq!(eth.fetch_balance(), Ok(Some(1.5)), "each contract's own coin");
    // A USDⓈ-M client reads the USDT row, which this answer does not carry: no figure, as before.
    let linear = ReconcileClientBinance::perp(NullSigner, Wallet, demo.fapi_rest, "BTCUSDT");
    assert_eq!(linear.fetch_balance(), Ok(None));
}

#[test]
fn the_coin_m_demo_host_is_one_the_bridge_writes_modes_on() {
    let demo = hosts(AccountTier::Demo);
    assert_eq!(demo.futures_rest("BTCUSD_PERP"), "https://demo-dapi.binance.com");
    assert_eq!(
        vike_binance::execution::rest::coin_m::margining_of("BTCUSD_PERP"),
        vike_model::Margining::Inverse,
        "a COIN-M name is the COIN-M book"
    );
    assert!(
        vike_binance::account::modes::writes_modes(demo.ty),
        "COIN-M keeps its own position mode"
    );
    let live = hosts(AccountTier::Live);
    assert_eq!(live.futures_rest("BTCUSD_PERP"), "https://dapi.binance.com");
    assert!(!vike_binance::account::modes::writes_modes(live.ty), "never on mainnet");
}

/// Answers every signed read with the venue error `code`.
struct Failing(i64);
impl RestTransport for Failing {
    fn signed(
        &self,
        _base_url: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        Err(VenueApiError { code: self.0, msg: "scripted".to_string() })
    }
    fn public(&self, _b: &str, path: &str, _p: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("a wallet read reads nothing public: {path}")
    }
}

/// **D12: a failed balance read (the startup credential probe's) carries binance's OWN classification**, a linear
/// and a COIN-M mount alike: `-1007` (the backend never answered) is outcome unknown and `-1008` (overloaded) not now,
/// so neither reads as a refused key; `-2015` (an invalid key) and `-1022` (a bad signature) are refusals.
#[test]
fn a_failed_balance_read_carries_binances_own_outcome() {
    use vike_utility::errors::FailureOutcome;
    let demo = hosts(AccountTier::Demo);
    for (code, outcome) in [
        (-1007, FailureOutcome::OutcomeUnknown),
        (-1008, FailureOutcome::NotNow),
        (-2015, FailureOutcome::Refused),
        (-1022, FailureOutcome::Refused),
    ] {
        for (base, symbol) in [(demo.fapi_rest, "BTCUSDT"), (demo.dapi_rest, "BTCUSD_PERP")] {
            let client = ReconcileClientBinance::perp(NullSigner, Failing(code), base, symbol);
            let failed = client.fetch_balance().expect_err("the venue answered an error");
            assert_eq!(failed.outcome, outcome, "{symbol} {code}: {failed}");
        }
    }
}
