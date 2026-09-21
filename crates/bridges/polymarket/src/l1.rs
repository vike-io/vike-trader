//! Polymarket **L1** auth — the ClobAuth EIP-712 signature that BOOTSTRAPS the L2 creds.
//!
//! Two-tier model: **L1 (this)** = a ONE-TIME Ethereum-private-key EIP-712 signature over the
//! fixed `ClobAuth` message, submitted to `/auth/derive-api-key` (or `/auth/api-key` to create) to
//! obtain the L2 `apiKey`/`secret`/`passphrase`. **L2 ([`auth`](super::auth))** = the cheap
//! per-request HMAC used for every order/cancel/query thereafter. The private key is used HERE and
//! never again per request. The ClobAuth domain is `{ClobAuthDomain, "1", chainId 137}` and — unlike
//! the Order domain — carries NO verifyingContract.

use vike_bridge_core::eip712::{
    digest, domain_separator_no_contract, enc_address, enc_string, enc_uint, hash_struct,
    sign_digest_hex,
};

use super::config::PolymarketCreds;

/// The fixed message ClobAuth attests.
const CLOB_AUTH_MESSAGE: &str = "This message attests that I control the given wallet";
/// Polygon.
const POLYGON_CHAIN_ID: u128 = 137;

/// Sign the ClobAuth L1 message → hex signature (bootstraps L2 credential derivation).
pub fn clob_auth_signature(
    private_key: &str,
    address: &str,
    timestamp_secs: i64,
    nonce: u128,
) -> Result<String, String> {
    let dom = domain_separator_no_contract("ClobAuthDomain", "1", POLYGON_CHAIN_ID);
    let struct_hash = hash_struct(
        "ClobAuth(address address,string timestamp,uint256 nonce,string message)",
        &[
            enc_address(address),
            enc_string(&timestamp_secs.to_string()),
            enc_uint(nonce),
            enc_string(CLOB_AUTH_MESSAGE),
        ],
    );
    sign_digest_hex(&digest(&dom, &struct_hash), private_key)
}

/// L1 headers for `GET /auth/derive-api-key` (or `POST /auth/api-key`).
pub fn l1_headers(
    address: &str,
    signature: &str,
    timestamp_secs: i64,
    nonce: u128,
) -> Vec<(String, String)> {
    vec![
        ("POLY_ADDRESS".to_string(), address.to_string()),
        ("POLY_SIGNATURE".to_string(), signature.to_string()),
        ("POLY_TIMESTAMP".to_string(), timestamp_secs.to_string()),
        ("POLY_NONCE".to_string(), nonce.to_string()),
    ]
}

/// The derived L2 credential trio.
///
/// ⚠ All THREE fields are credentials — the api key, the HMAC secret every subsequent order is
/// signed with, and the passphrase — so this type carries a manual [`std::fmt::Debug`], never a
/// derived one. See that impl for why the shape is an allowlist.
#[derive(Clone, PartialEq, Eq)]
pub struct DerivedL2 {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

// Secrets never reach Debug/Display/logs (CLAUDE.md "Credentials & the live gate"). Its sibling
// `PolymarketCreds` one file away has always redacted; this type carried `#[derive(Debug)]` and
// printed the whole trio the moment anything formatted one — a `{:?}` in a log line, a `.unwrap()`
// panic, a failed `assert_eq!`.
//
// An ALLOWLIST — `debug_struct` naming only the fields that are safe — rather than a redacting
// blocklist, because the two fail in opposite directions: a field added to this struct tomorrow is
// OMITTED here (visible as `..`, and someone must choose to add it), whereas a blocklist prints it
// by default and the mistake is silent. What is left is the only non-secret fact about a
// credential: whether it is populated, which is what an operator debugging a mount needs.
impl std::fmt::Debug for DerivedL2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DerivedL2")
            .field("api_key_set", &!self.api_key.is_empty())
            .field("secret_set", &!self.secret.is_empty())
            .field("passphrase_set", &!self.passphrase.is_empty())
            .finish_non_exhaustive()
    }
}

/// Live: derive this L1 key's existing L2 api creds (`GET /auth/derive-api-key`). `timestamp_secs`
/// is the Unix-seconds timestamp signed into the ClobAuth message.
pub fn derive_api_key(
    base: &str,
    private_key: &str,
    address: &str,
    timestamp_secs: i64,
) -> Result<DerivedL2, String> {
    let sig = clob_auth_signature(private_key, address, timestamp_secs, 0)?;
    let agent = crate::egress::agent(); // proxy-aware — Polymarket may be geo/DNS-blocked directly
    let mut req = agent.get(&format!("{base}/auth/derive-api-key"));
    for (k, v) in &l1_headers(address, &sig, timestamp_secs, 0) {
        req = req.header(k, v);
    }
    let mut resp = req.call().map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    parse_derived(status, &text)
}

/// The PURE half of [`derive_api_key`]: the credential-endpoint response → the L2 trio, or a
/// diagnostic.
///
/// Split out of the network call so the DIAGNOSTIC is testable without one — this is the
/// `event_mapper` shape the venue-adapter contract asks for, applied to the auth lane. Both of
/// [`ensure_l2`]'s live call sites (`vike_polymarket::mount::live_mount_from_vars` and
/// `vike_polymarket::recon_client::recon_client_from_vars`) log this error at `warn!`, and `warn`
/// survives the hardened `VIKE_LOG_FILE_LEVEL=warn` file filter the logging section of CLAUDE.md
/// recommends — so whatever lands in this string lands in the on-disk log.
///
/// ⚠ **The BODY never goes in the message.** This endpoint's body IS the credential trio, and the
/// arm that used to embed it is precisely the arm a PARTIAL response takes — a body carrying
/// `apiKey` + `secret` but no `passphrase` fails the match, so the two fields the venue did return
/// rode the error string into the log. What replaces the body is the diagnostic an operator
/// actually needs and which is not itself a secret: the HTTP status, and WHICH fields were absent.
fn parse_derived(status: u16, text: &str) -> Result<DerivedL2, String> {
    // The serde error carries a line/column, never the input — see `serde_json::Error`'s Display.
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        format!(
            "derive-api-key HTTP {status}: body is not JSON ({} bytes, not shown): {e}",
            text.len()
        )
    })?;
    let g = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    match (g("apiKey"), g("secret"), g("passphrase")) {
        (Some(api_key), Some(secret), Some(passphrase)) => {
            Ok(DerivedL2 { api_key, secret, passphrase })
        }
        (api_key, secret, passphrase) => {
            let missing: Vec<&str> = [
                ("apiKey", api_key.is_none()),
                ("secret", secret.is_none()),
                ("passphrase", passphrase.is_none()),
            ]
            .into_iter()
            .filter(|(_, absent)| *absent)
            .map(|(name, _)| name)
            .collect();
            Err(format!(
                "derive-api-key HTTP {status}: response is missing {} ({} bytes, not shown — the \
                 body carries whichever credential fields ARE present)",
                missing.join("+"),
                text.len()
            ))
        }
    }
}

/// Fill a creds' L2 trio by deriving from its L1 key (no-op if the L2 secret is already set).
pub fn ensure_l2(
    creds: &mut PolymarketCreds,
    base: &str,
    timestamp_secs: i64,
) -> Result<(), String> {
    if !creds.secret.is_empty() {
        return Ok(());
    }
    let d = derive_api_key(base, &creds.private_key, &creds.address, timestamp_secs)?;
    creds.api_key = d.api_key;
    creds.secret = d.secret;
    creds.passphrase = d.passphrase;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clob_auth_signature_deterministic_and_shaped() {
        let pk = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
        let addr = "0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826";
        let a = clob_auth_signature(pk, addr, 1_700_000_000, 0).unwrap();
        assert_eq!(a, clob_auth_signature(pk, addr, 1_700_000_000, 0).unwrap()); // deterministic
        assert!(a.starts_with("0x") && a.len() == 132); // 0x + 65-byte r||s||v
        assert_ne!(a, clob_auth_signature(pk, addr, 1_700_000_001, 0).unwrap());
        // ts binds
    }

    /// **The L2 trio must never print.** `DerivedL2` is the derive endpoint's answer — the api
    /// key, the HMAC secret and the passphrase every subsequent order is signed with — and a
    /// `#[derive(Debug)]` prints all three the moment anything formats one (a `{:?}` in a log
    /// line, a `.unwrap()` panic, an `assert_eq!` failure). Its sibling `PolymarketCreds` one file
    /// away has always redacted; this type did not.
    ///
    /// The assertion is on the UNREDACTED values, not on the message format: it fails if any of
    /// the three appears, whatever spelling the impl chooses.
    #[test]
    fn debug_never_prints_the_derived_l2_trio() {
        const API_KEY: &str = "3f8a1c22-1111-2222-3333-DEADBEEFCAFE";
        const SECRET: &str = "TOP-SECRET-L2-HMAC-MATERIAL-Aq9z";
        const PASSPHRASE: &str = "pass-phrase-Zx81-do-not-print";
        let d = DerivedL2 {
            api_key: API_KEY.to_string(),
            secret: SECRET.to_string(),
            passphrase: PASSPHRASE.to_string(),
        };
        let dbg = format!("{d:?}");
        assert!(!dbg.contains(API_KEY), "api_key leaked into Debug: {dbg}");
        assert!(!dbg.contains(SECRET), "secret leaked into Debug: {dbg}");
        assert!(!dbg.contains(PASSPHRASE), "passphrase leaked into Debug: {dbg}");
        // …and it still says something: which fields are populated is the diagnostic that
        // replaces the values (the `PolymarketCreds` shape).
        assert!(dbg.contains("DerivedL2"), "Debug must still name the type: {dbg}");
        assert!(dbg.contains("secret_set"), "Debug must still report presence: {dbg}");
    }

    /// **A PARTIAL derive response must not carry its credentials into the error.**
    ///
    /// The catch-all arm fires when ANY of the three fields is absent — so a body holding
    /// `apiKey` + `secret` but no `passphrase` takes it, and the two fields the venue DID return
    /// ride the error string verbatim. Both call sites log that at `warn!`, which survives the
    /// hardened `VIKE_LOG_FILE_LEVEL=warn` file filter, so this is the one credential path that
    /// reaches the on-disk log at the level the runbook recommends.
    #[test]
    fn a_partial_derive_response_never_carries_its_credentials_into_the_error() {
        const API_KEY: &str = "3f8a1c22-1111-2222-3333-DEADBEEFCAFE";
        const SECRET: &str = "TOP-SECRET-L2-HMAC-MATERIAL-Aq9z";
        let body = format!("{{\"apiKey\":\"{API_KEY}\",\"secret\":\"{SECRET}\"}}");
        let err = parse_derived(200, &body).unwrap_err();
        assert!(!err.contains(API_KEY), "apiKey leaked into the error: {err}");
        assert!(!err.contains(SECRET), "secret leaked into the error: {err}");
        // Still diagnostic: it names WHICH field was missing, which is what an operator needs and
        // is not itself a secret.
        assert!(err.contains("passphrase"), "the error must name the missing field: {err}");
        assert!(!err.contains("apiKey"), "a PRESENT field must not be named either: {err}");
    }

    /// The same rule for the body that is not JSON at all — a proxy/CDN error page, which is the
    /// realistic shape here because every one of these calls is routed through the SOCKS tunnel and
    /// can be answered by the tunnel rather than by the venue. Such a page routinely echoes the
    /// request, headers included.
    #[test]
    fn a_non_json_derive_response_body_never_reaches_the_error() {
        const ECHOED_HEADER: &str = "POLY_SIGNATURE: 0xdeadbeefc0ffee";
        let body = format!("<html><body>502 Bad Gateway<pre>{ECHOED_HEADER}</pre></body></html>");
        let err = parse_derived(502, &body).unwrap_err();
        assert!(
            !err.contains("0xdeadbeefc0ffee"),
            "the response body leaked into the error: {err}"
        );
        // The status is the diagnostic that replaces the body.
        assert!(err.contains("502"), "the error must still name the HTTP status: {err}");
    }

    #[test]
    fn l1_header_names() {
        let h = l1_headers("0xabc", "0xsig", 1_700_000_000, 0);
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["POLY_ADDRESS", "POLY_SIGNATURE", "POLY_TIMESTAMP", "POLY_NONCE"]);
    }

    /// LIVE validation of the whole ClobAuth L1 path: read the real key, derive the EOA, and hit
    /// Polymarket `/auth/derive-api-key`. Live acceptance == the EIP-712 ClobAuth signing is
    /// byte-correct. Read-only auth (no orders, no funds). Run explicitly:
    /// `cargo test -p vike-polymarket --features polymarket --lib live_derive -- --ignored --nocapture`
    #[test]
    #[ignore = "live: hits Polymarket /auth/derive-api-key with the real key"]
    fn live_derive_api_key() {
        vike_log::test_init();
        let vars = vike_bridge_core::credentials::load_workspace_dotenv();
        let pk = vars.get("POLY_PRIVATE_KEY").expect("POLY_PRIVATE_KEY set").clone();
        let addr = vike_bridge_core::eip712::eth_address_from_private_key(&pk).expect("derive EOA");
        tracing::info!(target: "vike_polymarket::l1", "signer EOA = {addr}");
        let ts =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
                as i64;
        match derive_api_key(super::super::config::CLOB_BASE, &pk, &addr, ts) {
            Ok(d) => {
                let tail = |s: &str| {
                    s.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>()
                };
                tracing::info!(
                    target: "vike_polymarket::l1",
                    "DERIVE OK — ClobAuth L1 accepted. apiKey=…{} secret=…{} pass=…{}",
                    tail(&d.api_key),
                    tail(&d.secret),
                    tail(&d.passphrase)
                );
            }
            Err(e) => panic!("DERIVE FAILED (bad ClobAuth sig or no key for EOA): {e}"),
        }
    }
}
