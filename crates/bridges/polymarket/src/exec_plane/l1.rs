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

use crate::config::PolymarketCreds;

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
// credential: whether it is populated, which is what an operator debugging a start needs.
impl std::fmt::Debug for DerivedL2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DerivedL2")
            .field("api_key_set", &!self.api_key.is_empty())
            .field("secret_set", &!self.secret.is_empty())
            .field("passphrase_set", &!self.passphrase.is_empty())
            .finish_non_exhaustive()
    }
}

/// The two L1-signed credential endpoints, which differ only by verb and path: a `GET` reads the
/// key this wallet already has, a `POST` creates the first one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthCall {
    Derive,
    Create,
}

impl AuthCall {
    fn path(self) -> &'static str {
        match self {
            AuthCall::Derive => "/auth/derive-api-key",
            AuthCall::Create => "/auth/api-key",
        }
    }
    fn label(self) -> &'static str {
        match self {
            AuthCall::Derive => "derive-api-key",
            AuthCall::Create => "api-key (create)",
        }
    }
}

/// One L1-signed call to an auth endpoint → `(HTTP status, raw body)`. The body of these endpoints
/// IS the credential trio, so callers hand it to [`parse_trio`] and never format it themselves.
fn l1_call(
    call: AuthCall,
    base: &str,
    private_key: &str,
    address: &str,
    timestamp_secs: i64,
) -> Result<(u16, String), String> {
    let sig = clob_auth_signature(private_key, address, timestamp_secs, 0)?;
    let agent = crate::egress::agent(); // proxy-aware — Polymarket may be geo/DNS-blocked directly
    let url = format!("{base}{}", call.path());
    let headers = l1_headers(address, &sig, timestamp_secs, 0);
    let mut resp = match call {
        AuthCall::Derive => {
            let mut req = agent.get(&url);
            for (k, v) in &headers {
                req = req.header(k, v);
            }
            req.call()
        }
        AuthCall::Create => {
            let mut req = agent.post(&url);
            for (k, v) in &headers {
                req = req.header(k, v);
            }
            req.send_empty()
        }
    }
    .map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    Ok((status, text))
}

/// Live: derive this L1 key's existing L2 api creds (`GET /auth/derive-api-key`). `timestamp_secs`
/// is the Unix-seconds timestamp signed into the ClobAuth message.
pub fn derive_api_key(
    base: &str,
    private_key: &str,
    address: &str,
    timestamp_secs: i64,
) -> Result<DerivedL2, String> {
    let (status, text) = l1_call(AuthCall::Derive, base, private_key, address, timestamp_secs)?;
    parse_derived(status, &text)
}

/// Live: CREATE this wallet's first L2 api creds (`POST /auth/api-key`, same L1 headers and nonce 0
/// as the derive). A WRITE on the venue's side: it only ever runs from [`ensure_l2_creating`], which
/// the live exec start calls (behind `flags.poly_exec`) after the derive answered "no key yet".
pub(crate) fn create_api_key(
    base: &str,
    private_key: &str,
    address: &str,
    timestamp_secs: i64,
) -> Result<DerivedL2, String> {
    let (status, text) = l1_call(AuthCall::Create, base, private_key, address, timestamp_secs)?;
    parse_trio(AuthCall::Create, status, &text)
}

/// The PURE half of [`derive_api_key`]: the credential-endpoint response → the L2 trio, or a
/// diagnostic.
///
/// Split out of the network call so the DIAGNOSTIC is testable without one — this is the
/// `event_mapper` shape the venue-adapter contract asks for, applied to the auth lane. Both of
/// [`ensure_l2`]'s live call sites (`vike_polymarket::exec_plane::adapter::live_runtime_for_account` and
/// `vike_polymarket::exec_plane::recon_client::recon_client_for_account`) log this error at `warn!`, and `warn`
/// survives the hardened `warn` file level (the `preferences.log_file_level` row) the
/// runbooks recommend — so whatever lands in this string lands in the on-disk log.
///
/// ⚠ **The BODY never goes in the message.** This endpoint's body IS the credential trio, and the
/// arm that used to embed it is precisely the arm a PARTIAL response takes — a body carrying
/// `apiKey` + `secret` but no `passphrase` fails the match, so the two fields the venue did return
/// rode the error string into the log. What replaces the body is the diagnostic an operator
/// actually needs and which is not itself a secret: the HTTP status, and WHICH fields were absent.
fn parse_derived(status: u16, text: &str) -> Result<DerivedL2, String> {
    parse_trio(AuthCall::Derive, status, text)
}

/// [`parse_derived`] for either credential endpoint (the create answers the same trio).
///
/// What an operator learns from a failure: the HTTP status, WHICH fields were absent and, when the
/// venue answered with a plain `{"error": "..."}` object that carries NO credential field, that one
/// sentence ([`venue_error_sentence`]) — it is the venue's own explanation ("Could not derive api
/// key!" reads very differently from a clock-skew refusal) and is not itself a secret.
fn parse_trio(call: AuthCall, status: u16, text: &str) -> Result<DerivedL2, String> {
    let what = call.label();
    // The serde error carries a line/column, never the input — see `serde_json::Error`'s Display.
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        format!("{what} HTTP {status}: body is not JSON ({} bytes, not shown): {e}", text.len())
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
            let venue_says = venue_error_sentence(&v)
                .map(|s| format!(" — the venue says: \"{s}\""))
                .unwrap_or_default();
            Err(format!(
                "{what} HTTP {status}: response is missing {} ({} bytes, not shown — the \
                 body carries whichever credential fields ARE present){venue_says}",
                missing.join("+"),
                text.len()
            ))
        }
    }
}

/// The venue's own one-sentence explanation out of a plain `{"error": "..."}` reply, bounded, and
/// ONLY when the object holds no credential field at all: a body that carries any of the trio is
/// never quoted, whatever else it says (the rule [`parse_derived`] was written under).
fn venue_error_sentence(v: &serde_json::Value) -> Option<String> {
    const MAX_CHARS: usize = 160;
    if ["apiKey", "secret", "passphrase"].iter().any(|k| v.get(k).is_some()) {
        return None;
    }
    let s = v.get("error")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(MAX_CHARS).collect())
}

/// Does a derive answer say THIS WALLET HAS NO API KEY YET (as opposed to "I could not reach or
/// authenticate you")? Only that answer may lead to a create: an HTTP 400 whose `error` sentence
/// says the venue "could not derive" a key. A 401 (bad signature, clock skew), a 403 (region), a
/// 429, a 5xx, an unreadable body and a transport failure are NOT "no key", and creating a key on
/// any of them would be a write made on a guess.
///
/// ⚠ The sentence is the py-clob-client behaviour as documented (`Could not derive api key!`, HTTP
/// 400) and has NOT been observed in this repo against the live venue: a wording that differs makes
/// this answer `false` and the start refuses in words exactly as it did before this existed. The
/// first live run on a wallet with no key is what confirms it.
pub(crate) fn derive_says_no_key(status: u16, text: &str) -> bool {
    status == 400
        && serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| venue_error_sentence(&v))
            .is_some_and(|s| s.to_ascii_lowercase().contains("could not derive"))
}

/// Fill a creds' L2 trio by deriving from its L1 key (no-op if the L2 secret is already set).
///
/// READ-ONLY on the venue: a wallet with no API key gets an error here. The one start that may
/// create the key is `ensure_l2_creating` (crate-private: the live exec start's).
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

/// Where a start's L2 trio came from — what the live-mount line reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum L2Origin {
    /// The account's own `API_KEY`/`SECRET`/`PASSPHRASE` fields were already filled.
    Supplied,
    /// `GET /auth/derive-api-key` returned the key the wallet already had.
    Derived,
    /// The derive said the wallet has no key, and `POST /auth/api-key` created it.
    Created,
}

/// [`ensure_l2`] for the LIVE EXEC start only: derive, and when (and only when) the venue says this
/// wallet has no API key yet ([`derive_says_no_key`]), create it and use that.
///
/// This is the one place the bridge WRITES a credential on the venue, and it is reachable only
/// through the exec start (`crate::exec_plane::adapter::live_runtime_for_account`), which
/// `flags.poly_exec` and a live-tier account already gate; the reconcile-only start keeps
/// [`ensure_l2`] and so can never create anything. The created trio lives in this process's memory
/// only: nothing here writes the account's stored fields (the next start derives the same key).
pub(crate) fn ensure_l2_creating(
    creds: &mut PolymarketCreds,
    base: &str,
    timestamp_secs: i64,
) -> Result<L2Origin, String> {
    if !creds.secret.is_empty() {
        return Ok(L2Origin::Supplied);
    }
    let (status, text) =
        l1_call(AuthCall::Derive, base, &creds.private_key, &creds.address, timestamp_secs)?;
    let (d, origin) = match parse_derived(status, &text) {
        Ok(d) => (d, L2Origin::Derived),
        Err(e) if derive_says_no_key(status, &text) => {
            tracing::warn!(
                target: "vike_polymarket::l1",
                "polymarket: this wallet has no API key yet ({e}) — creating one (POST /auth/api-key)"
            );
            let made = create_api_key(base, &creds.private_key, &creds.address, timestamp_secs)
                .map_err(|why| {
                    format!("this wallet has no API key and creating one failed: {why}")
                })?;
            (made, L2Origin::Created)
        }
        Err(e) => return Err(e),
    };
    creds.api_key = d.api_key;
    creds.secret = d.secret;
    creds.passphrase = d.passphrase;
    Ok(origin)
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
    /// hardened `warn` file level, so this is the one credential path that
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

    // --- create-if-missing, against a loopback CLOB ------------------------------------------

    use crate::exec_plane::test_support::{FakeServer, Reply, declare_direct_egress};

    const PK: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
    const ADDR: &str = "0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826";
    const NO_KEY: &str = r#"{"error":"Could not derive api key!"}"#;
    const TRIO: &str = r#"{"apiKey":"k-1","secret":"c2VjcmV0","passphrase":"p-1"}"#;

    fn creds() -> PolymarketCreds {
        PolymarketCreds {
            private_key: PK.to_string(),
            address: ADDR.to_string(),
            ..Default::default()
        }
    }

    /// A loopback CLOB: `derive` answers the GET, `create` answers the POST.
    fn auth_server(derive: (u16, &'static str), create: (u16, &'static str)) -> FakeServer {
        FakeServer::start(move |req| match (req.method.as_str(), req.path()) {
            ("GET", "/auth/derive-api-key") => Reply::text(derive.0, derive.1),
            ("POST", "/auth/api-key") => Reply::text(create.0, create.1),
            _ => Reply::text(404, "{}"),
        })
    }

    /// Only the venue's "this wallet has no API key" answer reads as such: a refusal of the signature,
    /// a region block, a rate limit, a gateway error and an unreadable body are NOT "no key", and
    /// creating a key on any of them would be a write made on a guess.
    #[test]
    fn only_the_no_key_answer_may_lead_to_a_create() {
        assert!(derive_says_no_key(400, NO_KEY));
        assert!(derive_says_no_key(400, r#"{"error":"could not derive api key"}"#));
        for (status, body) in [
            (400, r#"{"error":"Invalid nonce"}"#),
            (400, r#"{"error":"Invalid L1 Request headers"}"#),
            (400, "Could not derive api key!"),
            (400, r#"{"message":"Could not derive api key!"}"#),
            (401, NO_KEY),
            (403, r#"{"error":"Trading restricted in your region"}"#),
            (429, r#"{"error":"Too many requests"}"#),
            (502, "<html>Bad Gateway</html>"),
            (200, TRIO),
            (200, ""),
        ] {
            assert!(!derive_says_no_key(status, body), "{status} {body}");
        }
        // an answer that carries a credential field is never quoted, so never matched
        assert!(!derive_says_no_key(400, r#"{"error":"Could not derive api key!","secret":"s"}"#));
    }

    /// The venue's own sentence reaches the operator on a failure — and only when the body holds no
    /// credential field at all.
    #[test]
    fn a_plain_error_sentence_is_quoted_and_a_credential_bearing_body_never_is() {
        let err = parse_derived(400, NO_KEY).unwrap_err();
        assert!(err.contains("the venue says: \"Could not derive api key!\""), "{err}");
        assert!(err.contains("derive-api-key HTTP 400"), "{err}");
        let err = parse_derived(401, r#"{"error":"Unauthorized"}"#).unwrap_err();
        assert!(err.contains("the venue says: \"Unauthorized\""), "{err}");
        // a partial trio: the error field is NOT quoted either
        let leaky = r#"{"apiKey":"SECRETKEYVALUE","error":"half a response"}"#;
        let err = parse_derived(200, leaky).unwrap_err();
        assert!(!err.contains("SECRETKEYVALUE") && !err.contains("half a response"), "{err}");
        // bounded
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(1000));
        assert!(parse_derived(400, &long).unwrap_err().len() < 400);
    }

    /// Red on base: before this a wallet with no key could only ever fail the start.
    #[test]
    fn a_wallet_with_no_key_gets_one_created_and_the_trio_is_used() {
        declare_direct_egress();
        let server = auth_server((400, NO_KEY), (200, TRIO));
        let mut c = creds();
        let origin = ensure_l2_creating(&mut c, &server.base, 1_700_000_000).expect("created");
        assert_eq!(origin, L2Origin::Created);
        assert_eq!(
            (c.api_key.as_str(), c.secret.as_str(), c.passphrase.as_str()),
            ("k-1", "c2VjcmV0", "p-1")
        );

        let seen = server.seen();
        assert_eq!(seen.len(), 2, "one derive, then one create: {seen:?}");
        assert_eq!((seen[0].method.as_str(), seen[0].path()), ("GET", "/auth/derive-api-key"));
        assert_eq!((seen[1].method.as_str(), seen[1].path()), ("POST", "/auth/api-key"));
        // the create carries the same L1 headers (the wallet signs the ClobAuth message), no body
        for r in &seen {
            assert_eq!(r.header("poly_address"), Some(ADDR));
            assert_eq!(r.header("poly_nonce"), Some("0"));
            assert_eq!(r.header("poly_timestamp"), Some("1700000000"));
            assert_eq!(r.header("poly_signature").map(str::len), Some(132));
        }
        assert!(seen[1].body.is_empty(), "the create sends no body");
        assert!(seen.iter().all(|r| !r.headers.iter().any(|(_, v)| v.contains(&PK[2..]))));
    }

    #[test]
    fn a_wallet_that_has_a_key_derives_it_and_creates_nothing() {
        declare_direct_egress();
        let server = auth_server((200, TRIO), (500, "must not be asked"));
        let mut c = creds();
        assert_eq!(ensure_l2_creating(&mut c, &server.base, 1).unwrap(), L2Origin::Derived);
        assert_eq!(c.api_key, "k-1");
        assert_eq!(server.seen().len(), 1, "a derive that works is the only request");
    }

    /// Every OTHER derive failure ends the start in words and creates NOTHING.
    #[test]
    fn a_derive_failure_that_is_not_no_key_creates_nothing() {
        declare_direct_egress();
        for (status, body) in [
            (401, r#"{"error":"Invalid L1 Request headers"}"#),
            (403, r#"{"error":"Trading restricted in your region"}"#),
            (400, r#"{"error":"Invalid nonce"}"#),
            (429, r#"{"error":"Too many requests"}"#),
            (502, "<html>Bad Gateway</html>"),
        ] {
            let server = auth_server((status, body), (200, TRIO));
            let mut c = creds();
            let why = ensure_l2_creating(&mut c, &server.base, 1).unwrap_err();
            assert!(why.contains(&format!("HTTP {status}")), "{status}: {why}");
            assert_eq!(server.seen().len(), 1, "{status}: no create was sent");
            assert!(c.secret.is_empty(), "no credential was set");
        }
        // an unreachable CLOB: not "no key" either
        let mut c = creds();
        assert!(
            ensure_l2_creating(&mut c, "http://127.0.0.1:1", 1).unwrap_err().contains("network")
        );
    }

    #[test]
    fn a_create_that_fails_says_so_and_sets_nothing() {
        declare_direct_egress();
        let server = auth_server((400, NO_KEY), (400, r#"{"error":"Could not create api key"}"#));
        let mut c = creds();
        let why = ensure_l2_creating(&mut c, &server.base, 1).unwrap_err();
        assert!(why.contains("no API key and creating one failed"), "{why}");
        assert!(
            why.contains("Could not create api key") && why.contains("api-key (create) HTTP 400"),
            "{why}"
        );
        assert!(c.secret.is_empty());
        // a create that answers without a full trio is a failure too, and never quotes the half it got
        let server = auth_server((400, NO_KEY), (200, r#"{"apiKey":"HALF-A-KEY"}"#));
        let why = ensure_l2_creating(&mut creds(), &server.base, 1).unwrap_err();
        assert!(why.contains("missing secret+passphrase") && !why.contains("HALF-A-KEY"), "{why}");
    }

    /// Supplied credentials short-circuit before any request, and the READ-ONLY `ensure_l2` (the
    /// recon-only start's) never POSTs, even for a wallet with no key.
    #[test]
    fn supplied_credentials_cost_no_request_and_the_read_only_path_never_creates() {
        declare_direct_egress();
        let server = auth_server((400, NO_KEY), (200, TRIO));
        let mut supplied = PolymarketCreds { secret: "already".to_string(), ..creds() };
        assert_eq!(ensure_l2_creating(&mut supplied, &server.base, 1).unwrap(), L2Origin::Supplied);
        assert!(server.seen().is_empty());

        let mut c = creds();
        let why = ensure_l2(&mut c, &server.base, 1).unwrap_err();
        assert!(
            why.contains("Could not derive api key!"),
            "the venue's sentence reaches the operator: {why}"
        );
        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "GET", "the read-only path sent no create");
    }

    /// LIVE validation of the whole ClobAuth L1 path: read the real key, derive the EOA, and hit
    /// Polymarket `/auth/derive-api-key`. Live acceptance == the EIP-712 ClobAuth signing is
    /// byte-correct. Read-only auth (no orders, no funds). Run explicitly:
    /// `cargo test -p vike-polymarket --features polymarket --lib live_derive -- --ignored --nocapture`
    #[test]
    #[ignore = "live: hits Polymarket /auth/derive-api-key with the real key"]
    fn live_derive_api_key() {
        vike_log::test_init();
        crate::egress::egress_tests::declare_from_the_settings_database(
            std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
        );
        // The DEFAULT polymarket account's `PRIVATE_KEY` field — the account at tier `live`, the
        // only tier this venue has.
        let dir = vike_secrets::smoke_directory();
        let account = dir.credentials_for(
            crate::VENUE,
            &vike_model::accounts::account_keys::AccountLabel::Default,
            vike_model::AccountTier::Live,
        );
        let pk = account.get("PRIVATE_KEY").expect("the live account's PRIVATE_KEY").to_string();
        let addr = vike_bridge_core::eip712::eth_address_from_private_key(&pk).expect("derive EOA");
        tracing::info!(target: "vike_polymarket::l1", "signer EOA = {addr}");
        let ts = vike_model::time::clock::now_secs();
        match derive_api_key(crate::config::CLOB_BASE, &pk, &addr, ts) {
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
