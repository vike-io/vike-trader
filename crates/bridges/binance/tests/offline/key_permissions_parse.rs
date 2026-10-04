//! Pure parse + offline fetch tests for binance API-key permission introspection — NO network.
//! Bodies are shaped like Binance's real `GET /sapi/v1/account/apiRestrictions` response. Proves:
//! `enableWithdrawals` true/false -> `can_withdraw`; the full field map; a missing field -> Unknown
//! (`None`); malformed JSON -> `Err`; the end-to-end probe over a canned transport; and that the
//! shared bridge-core withdraw policy REFUSES a fetched withdraw-enabled key, ALLOWS a trade-only
//! key, and does NOT refuse an Unknown (fail-open on introspection).

use std::sync::Mutex;

use vike_binance::key_permissions::{
    BinanceKeyPermissionProbe, key_permission_probe, parse_api_restrictions,
};
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::key_permissions::{KeyPermissionProbe, WithdrawGate, withdraw_gate};
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// --- pure parser ------------------------------------------------------------------------------

/// A withdraw-ENABLED key: `enableWithdrawals: true` maps to `can_withdraw: Some(true)`.
#[test]
fn parses_withdraw_enabled() {
    let body = r#"{"ipRestrict":true,"enableWithdrawals":true,"enableSpotAndMarginTrading":true}"#;
    let perms = parse_api_restrictions(body).unwrap();
    assert_eq!(perms.can_withdraw, Some(true));
    assert_eq!(perms.can_trade, Some(true));
    assert_eq!(perms.ip_restricted, Some(true));
}

/// The full documented body shape (withdraw off, trade on, no IP restriction) maps every field.
#[test]
fn parses_full_restrictions_body() {
    let body = r#"{
        "ipRestrict": false,
        "createTime": 1623840271000,
        "enableWithdrawals": false,
        "enableInternalTransfer": true,
        "permitsUniversalTransfer": true,
        "enableVanillaOptions": false,
        "enableReading": true,
        "enableFutures": false,
        "enableMargin": false,
        "enableSpotAndMarginTrading": true,
        "tradingAuthorityExpirationTime": 1628985600000
    }"#;
    let perms = parse_api_restrictions(body).unwrap();
    assert_eq!(perms.can_withdraw, Some(false));
    assert_eq!(perms.can_trade, Some(true));
    assert_eq!(perms.ip_restricted, Some(false));
}

/// A body missing `enableWithdrawals` -> `None` (Unknown), NOT an error and NOT a false positive.
#[test]
fn missing_field_is_unknown() {
    let body = r#"{"enableSpotAndMarginTrading":true}"#;
    let perms = parse_api_restrictions(body).unwrap();
    assert_eq!(perms.can_withdraw, None);
    assert_eq!(perms.can_trade, Some(true));
    assert_eq!(perms.ip_restricted, None);
}

/// Malformed JSON is a hard `Err`, never a panic.
#[test]
fn malformed_json_is_err() {
    assert!(parse_api_restrictions("not json").is_err());
}

// --- policy over parsed permissions -----------------------------------------------------------

/// The bridge-core withdraw policy refuses a KNOWN withdraw-capable binance key, allows a trade-only
/// key, and does not refuse an Unknown — the whole point of STEP-1.
#[test]
fn policy_refuses_withdraw_allows_trade_only_ignores_unknown() {
    let withdraw =
        parse_api_restrictions(r#"{"enableWithdrawals":true,"enableSpotAndMarginTrading":true}"#)
            .unwrap();
    assert_eq!(withdraw_gate(&withdraw, false), WithdrawGate::Refuse);

    let trade_only =
        parse_api_restrictions(r#"{"enableWithdrawals":false,"enableSpotAndMarginTrading":true}"#)
            .unwrap();
    assert_eq!(withdraw_gate(&trade_only, false), WithdrawGate::Allow);

    let unknown = parse_api_restrictions(r#"{"enableReading":true}"#).unwrap();
    assert_eq!(withdraw_gate(&unknown, false), WithdrawGate::Allow);
}

// --- offline end-to-end fetch (stub transport, no network) ------------------------------------

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Returns a canned body on `signed` and records the path it was asked for.
struct Canned {
    body: String,
    seen_path: Mutex<Option<String>>,
}
impl RestTransport for Canned {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        *self.seen_path.lock().unwrap() = Some(path.to_string());
        Ok(serde_json::from_str::<serde_json::Value>(&self.body).unwrap())
    }
    fn public(
        &self,
        _base: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        unreachable!("apiRestrictions is a signed call")
    }
}

/// A transport that fails, to prove a fetch error surfaces as `Err`.
struct Failing;
impl RestTransport for Failing {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        Err(VenueApiError { code: 401, msg: "invalid api key".to_string() })
    }
    fn public(
        &self,
        _base: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        unreachable!("apiRestrictions is a signed call")
    }
}

/// The probe hits the signed `apiRestrictions` path and round-trips the canned body into
/// KeyPermissions the policy then refuses.
#[test]
fn probe_fetches_and_maps_over_stub_transport() {
    let transport = Canned {
        body: r#"{"ipRestrict":false,"enableWithdrawals":true,"enableSpotAndMarginTrading":true}"#
            .to_string(),
        seen_path: Mutex::new(None),
    };
    let probe = BinanceKeyPermissionProbe::new(NullSigner, transport, "https://api.binance.com");
    let perms = probe.fetch_key_permissions().unwrap();
    assert_eq!(perms.can_withdraw, Some(true));
    assert_eq!(withdraw_gate(&perms, false), WithdrawGate::Refuse);
    assert_eq!(
        probe.transport.seen_path.lock().unwrap().as_deref(),
        Some("/sapi/v1/account/apiRestrictions")
    );
}

/// A transport failure surfaces as `Err` (the step-2 caller decides fail-open vs fail-closed).
#[test]
fn probe_surfaces_fetch_error() {
    let probe = BinanceKeyPermissionProbe::new(NullSigner, Failing, "https://api.binance.com");
    assert!(probe.fetch_key_permissions().is_err());
}

/// The live-stack factory builds a concrete probe without touching the network (construction only).
#[test]
fn factory_builds_concrete_probe() {
    let creds =
        Credentials { api_key: "k".to_string(), api_secret: "s".to_string(), passphrase: None };
    let probe = key_permission_probe(&creds, "https://api.binance.com");
    assert_eq!(probe.base_url, "https://api.binance.com");
}
