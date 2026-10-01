//! cTrader Open API OAuth2 (authorization-code + refresh) — the pure token-response parser and
//! authorize-URL builder are network-free and unit-tested; `exchange_code`/`refresh` are the thin
//! blocking `ureq` calls over them. Ports the exact endpoint/params/JSON shape proven live by
//! `scratchpad/ctrader_catcher.py` (see `.superpowers/sdd/task-2-brief.md`): the token endpoint
//! returns camelCase JSON (`accessToken`/`refreshToken`/`tokenType`/`expiresIn`) or an error object
//! (`errorCode`/`description`) — cTrader's Open API docs, not a Python-app oracle (no Python twin
//! for this venue).

use std::fmt;

/// Base URL for cTrader's OAuth2 app endpoints (authorize + token).
const OPENAPI_BASE: &str = "https://openapi.ctrader.com/apps";

/// An OAuth2 access/refresh token pair. `Debug` is manually implemented to redact both secrets —
/// never let a token leak into a log line or panic message.
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// OAuth failure: either the token endpoint returned an explicit `{errorCode, description}` body,
/// a malformed/incomplete JSON response, or a transport error. Never carries a secret.
#[derive(Debug)]
pub enum OAuthError {
    /// The venue returned a structured error (e.g. `ACCESS_DENIED`).
    Venue { error_code: String, description: String },
    /// The response parsed as JSON but lacked the fields a token response requires.
    MalformedResponse(String),
    /// The HTTP/transport layer failed (network, TLS, non-2xx status, etc).
    Transport(String),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OAuthError::Venue { error_code, description } => {
                write!(f, "cTrader OAuth error {error_code}: {description}")
            }
            OAuthError::MalformedResponse(msg) => write!(f, "malformed token response: {msg}"),
            OAuthError::Transport(msg) => write!(f, "OAuth transport error: {msg}"),
        }
    }
}

impl std::error::Error for OAuthError {}

/// Parse a token endpoint response body. Tries the error shape first (an `errorCode` field is
/// unambiguous), then the token shape; anything else is `MalformedResponse`. Pure/no I/O.
///
/// ⚠ **Read through a `serde_json::Value` map rather than a `#[derive(Deserialize)]` struct, and
/// that is load-bearing.** The struct form carried `#[serde(rename = "accessToken", alias =
/// "access_token")]` — a defensive alias for "in case a future response ever ships the other
/// convention". cTrader ships BOTH CONVENTIONS IN ONE BODY, so the two spellings collapsed onto one
/// field and serde rejected the whole response with `duplicate field 'accessToken'`. Measured live
/// 2026-08-19: a freshly granted authorization code was exchanged, the venue answered 200, and the
/// parser refused it — the defensive alias was the entire failure, and the code is single-use, so
/// every attempt cost a fresh human consent click.
///
/// A `serde_json::Map` takes the LAST value for a repeated key instead of erroring, which makes
/// this immune to both shapes of the defect at once: the same key twice, and two spellings of the
/// same field. camelCase is preferred where both are present, because that is the documented
/// Open API convention and the snake_case leg was only ever a fallback.
pub fn parse_token_response(json: &str) -> Result<Token, OAuthError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| OAuthError::MalformedResponse(format!("JSON parse: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| OAuthError::MalformedResponse("response is not a JSON object".into()))?;

    // The error shape first: an `errorCode` field is unambiguous.
    let str_of = |k: &str| obj.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
    if let Some(error_code) = str_of("errorCode").or_else(|| str_of("error_code")) {
        return Err(OAuthError::Venue {
            error_code,
            description: str_of("description").unwrap_or_default(),
        });
    }

    let access_token = str_of("accessToken")
        .or_else(|| str_of("access_token"))
        .ok_or_else(|| OAuthError::MalformedResponse("missing accessToken".into()))?;
    let refresh_token = str_of("refreshToken")
        .or_else(|| str_of("refresh_token"))
        .ok_or_else(|| OAuthError::MalformedResponse("missing refreshToken".into()))?;
    let expires_in = obj
        .get("expiresIn")
        .or_else(|| obj.get("expires_in"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| OAuthError::MalformedResponse("missing expiresIn".into()))?;
    Ok(Token { access_token, refresh_token, expires_in })
}

/// Minimal percent-encoder for query-string values (RFC 3986 unreserved set kept literal;
/// everything else escaped). No new dependency — the value set here (URLs, client ids, tokens)
/// is small and ASCII-safe, so byte-wise percent-encoding is sufficient.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build one `key=value` query-string segment with a percent-encoded value.
fn qs_pair(key: &str, value: &str) -> String {
    format!("{key}={}", percent_encode(value))
}

/// Build the browser-facing authorize URL (`GET /apps/auth`) the user opens to grant access.
pub fn build_authorize_url(client_id: &str, redirect_uri: &str, scope: &str) -> String {
    format!(
        "{OPENAPI_BASE}/auth?{}&{}&{}",
        qs_pair("client_id", client_id),
        qs_pair("redirect_uri", redirect_uri),
        qs_pair("scope", scope),
    )
}

/// Build the token-endpoint URL for a given `grant_type`, with the shared params plus the
/// grant-specific one (`code` for `authorization_code`, `refresh_token` for `refresh_token`).
fn build_token_url(
    grant_type: &str,
    client_id: &str,
    client_secret: &str,
    redirect_uri: Option<&str>,
    code_or_refresh_key: &str,
    code_or_refresh_value: &str,
) -> String {
    let mut url = format!(
        "{OPENAPI_BASE}/token?{}&{}&{}",
        qs_pair("grant_type", grant_type),
        qs_pair(code_or_refresh_key, code_or_refresh_value),
        qs_pair("client_id", client_id),
    );
    if let Some(redirect_uri) = redirect_uri {
        url.push('&');
        url.push_str(&qs_pair("redirect_uri", redirect_uri));
    }
    url.push('&');
    url.push_str(&qs_pair("client_secret", client_secret));
    url
}

/// One blocking GET against the token endpoint, returning the parsed [`Token`] or an
/// [`OAuthError`]. Shared by `exchange_code`/`refresh` — the two grant types differ only in the
/// URL built for them.
///
/// Uses `vike_bridge_core::http::blocking_agent()`, whose agent config sets
/// `http_status_as_error(false)` — ureq 3.3's default agent treats any non-2xx status as an
/// `Err` and discards the body, which would make the token endpoint's `400 {errorCode,
/// description}` body unreachable. With that agent, ANY status (2xx or not) is read as a body
/// and handed to `parse_token_response`, so venue error bodies parse correctly; only genuine
/// transport failures (DNS/connect/TLS) map to `OAuthError::Transport`.
fn fetch_token(url: &str) -> Result<Token, OAuthError> {
    let agent = vike_bridge_core::http::blocking_agent();
    let mut resp = agent.get(url).call().map_err(|e| OAuthError::Transport(e.to_string()))?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| OAuthError::Transport(format!("read body: {e}")))?;
    parse_token_response(&body)
}

/// Exchange an authorization `code` (captured from the redirect) for a [`Token`].
pub fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<Token, OAuthError> {
    let url = build_token_url(
        "authorization_code",
        client_id,
        client_secret,
        Some(redirect_uri),
        "code",
        code,
    );
    fetch_token(&url)
}

/// Refresh an expiring [`Token`] using its `refresh_token`. No `redirect_uri` is required for
/// this grant type per cTrader's Open API.
pub fn refresh(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Token, OAuthError> {
    let url = build_token_url(
        "refresh_token",
        client_id,
        client_secret,
        None,
        "refresh_token",
        refresh_token,
    );
    fetch_token(&url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_reserved_chars() {
        assert_eq!(percent_encode("http://localhost:5033/"), "http%3A%2F%2Flocalhost%3A5033%2F");
        assert_eq!(percent_encode("trading"), "trading");
    }

    #[test]
    fn token_url_builds_expected_shape() {
        let url = build_token_url(
            "authorization_code",
            "cid",
            "secret",
            Some("http://localhost:5033/"),
            "code",
            "abc",
        );
        assert!(
            url.starts_with("https://openapi.ctrader.com/apps/token?grant_type=authorization_code")
        );
        assert!(url.contains("code=abc"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A5033%2F"));
        assert!(url.contains("client_secret=secret"));
    }

    #[test]
    fn refresh_url_omits_redirect_uri() {
        let url = build_token_url("refresh_token", "cid", "secret", None, "refresh_token", "rt");
        assert!(!url.contains("redirect_uri"));
        assert!(url.contains("refresh_token=rt"));
    }
}

#[cfg(test)]
mod duplicate_field_tests {
    use super::*;

    /// THE regression, in the venue's own shape: cTrader answers a token exchange with BOTH the
    /// camelCase and the snake_case spelling of each field in one body. The struct form's
    /// `#[serde(alias)]` collapsed the pair onto one field and serde refused the response with
    /// `duplicate field 'accessToken'` — a 200 from the venue turned into a hard failure, and
    /// because an authorization code is single-use, each attempt burned a human consent click.
    #[test]
    fn a_body_carrying_both_spellings_parses_and_prefers_camel_case() {
        let json = r#"{"accessToken":"at-camel","refreshToken":"rt-camel","expiresIn":2628000,
                       "access_token":"at-snake","refresh_token":"rt-snake","expires_in":11}"#;
        let t = parse_token_response(json).expect("both spellings in one body must parse");
        assert_eq!(t.access_token, "at-camel", "camelCase is the documented convention and wins");
        assert_eq!(t.refresh_token, "rt-camel");
        assert_eq!(t.expires_in, 2_628_000);
    }

    /// The other shape of the same defect: the SAME key repeated. A JSON object takes the last
    /// value rather than erroring, which is what makes this parser immune to both at once.
    #[test]
    fn a_repeated_key_takes_the_last_value_instead_of_failing() {
        let json =
            r#"{"accessToken":"first","accessToken":"second","refreshToken":"r","expiresIn":7}"#;
        let t = parse_token_response(json).expect("a repeated key must not fail the parse");
        assert_eq!(t.access_token, "second");
    }

    /// A snake_case-ONLY body still works — the fallback the alias was originally added for.
    #[test]
    fn a_snake_case_only_body_still_parses() {
        let json = r#"{"access_token":"a","refresh_token":"r","expires_in":9}"#;
        let t = parse_token_response(json).expect("snake_case only must parse");
        assert_eq!(t.access_token, "a");
        assert_eq!(t.expires_in, 9);
    }

    /// The error shape still wins over the token shape, and still carries no secret.
    #[test]
    fn an_error_body_is_reported_as_a_venue_error() {
        let json = r#"{"errorCode":"ACCESS_DENIED","description":"no"}"#;
        match parse_token_response(json) {
            Err(OAuthError::Venue { error_code, description }) => {
                assert_eq!(error_code, "ACCESS_DENIED");
                assert_eq!(description, "no");
            }
            other => panic!("expected a Venue error, got {other:?}"),
        }
    }

    /// A body that is valid JSON but not an object is malformed, not a panic.
    #[test]
    fn a_non_object_body_is_malformed_not_a_panic() {
        assert!(matches!(parse_token_response("[1,2,3]"), Err(OAuthError::MalformedResponse(_))));
    }
}
