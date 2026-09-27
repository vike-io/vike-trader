//! Signed blocking REST transport for OKX V5. Exact port of `exec/okx/transport.py`:
//! every request (signed AND public) carries a full browser User-Agent block (Cloudflare
//! 1010/403 dodge); `x-simulated-trading: 1` when simulated (demo). GET appends the
//! signed query VERBATIM from the signer (sign-then-send); POST sends the EXACT signed
//! body bytes. OKX returns HTTP 200 for business errors — the {code,msg,data} envelope
//! is unwrapped by the client (`unwrap_okx`), not here.

use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::{VenueApiError, classify_send_error, read_body_ambiguous};

// The browser UA moved to the crate root ([`crate::BROWSER_UA`]) when the exec/feeds seam gated
// this module: the keyless kline fetch in `crate::data` sends it too and can no longer see a
// module behind `exec`.
use crate::BROWSER_UA;

pub trait OkxTransport {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError>;

    /// POST an already-serialized JSON body — for ARRAY-body endpoints (batch-orders,
    /// cancel-batch-orders) that the flat `signed` param map can't express. Returns the RAW OKX
    /// envelope (NOT `unwrap_okx`'d) so the caller can read per-order `sCode`s. DEFAULT: unsupported
    /// (override only where native batch is wired).
    fn signed_json(
        &self,
        _base_url: &str,
        _path: &str,
        _method: &str,
        _body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        unimplemented!("signed_json (array body) not supported by this transport")
    }

    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError>;

    /// Bounded-timeout twin of `signed` for the audit-T1 order-status re-query. DEFAULT delegates
    /// to `signed`; the live transport runs it on a short-timeout agent.
    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.signed(base_url, path, method, params, signer)
    }
}

pub struct UreqOkxTransport {
    agent: ureq::Agent,
    /// Short-timeout agent for the audit-T1 re-query only (bounds the double-timeout stall).
    requery_agent: ureq::Agent,
    pub simulated: bool,
    /// Optional REST rate gate (net-hardening spec §A). Consulted on `signed`/`signed_json`/
    /// `public` — the T1 `signed_requery` is exempt so a throttle can't reintroduce the stall its
    /// short-timeout agent bounds.
    rate_gate: Option<RateGate>,
}

impl UreqOkxTransport {
    pub fn new(simulated: bool) -> Self {
        UreqOkxTransport {
            // now also sends the shared user-agent (was omitted) — harmless, OKX auths via headers.
            agent: vike_bridge_core::http::blocking_agent(),
            requery_agent: vike_bridge_core::http::blocking_agent_with_timeout(
                std::time::Duration::from_secs(5),
            ),
            simulated,
            rate_gate: None,
        }
    }

    /// Attach a REST rate gate; every `signed`/`signed_json`/`public` call rides it (T1 re-query
    /// stays exempt).
    pub fn with_rate_gate(mut self, gate: RateGate) -> Self {
        self.rate_gate = Some(gate);
        self
    }

    /// Consult the rate gate before a normal request, if one is configured. No-op otherwise.
    fn enter_gate(&self, what: &str) {
        if let Some(gate) = &self.rate_gate {
            gate.proceed_logged("okx", what);
        }
    }

    fn run(
        &self,
        agent: &ureq::Agent,
        url: &str,
        method: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<serde_json::Value, VenueApiError> {
        let sent = if method.eq_ignore_ascii_case("GET") {
            let mut req = agent
                .get(url)
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "en-US,en;q=0.9");
            if self.simulated {
                req = req.header("x-simulated-trading", "1");
            }
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.call()
        } else {
            let mut req = agent
                .post(url)
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "application/json, text/plain, */*")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Content-Type", "application/json");
            if self.simulated {
                req = req.header("x-simulated-trading", "1");
            }
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.send(body.unwrap_or_default())
        };
        // audit T1 (send-failure + body-read classification) is SHARED — see
        // `vike_bridge_core::transport::classify_send_error`. The warn stays here: this venue logs
        // under its own compile-time `target`, which RUST_LOG directives filter on.
        let mut resp = sent.map_err(|e| {
            tracing::warn!(target: "vike_okx::transport", error = %e, "REST request failed");
            classify_send_error(&e)
        })?;
        let status = resp.status().as_u16();
        let text = read_body_ambiguous(&mut resp)?;
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
        if (200..300).contains(&status) {
            return parsed.map_err(|e| VenueApiError { code: 0, msg: format!("bad json: {e}") });
        }
        tracing::warn!(target: "vike_okx::transport", status, "REST request failed");
        Err(http_error(status, parsed))
    }
}

/// Map a NON-2xx response (status + body-parse attempt) to the caller-visible error. The venue's
/// own `{code}` wins when it carries an i64 (OKX sends it as a JSON string); an ABSENT code falls
/// back to the HTTP status (`i64::from(status)`), and so does a present-but-unparseable or
/// wrong-typed one. **Never `code: 0` here**: `0` is the workspace's definite PRE-SEND sentinel
/// (`vike_bridge_core::transport::classify_send_error` — DNS/connect/TLS, nothing left this host),
/// and response HEADERS already arrived, so the venue demonstrably received the request. A `0`
/// would let `ErrorKind::classify` file a delivered request under `Network` (retryable,
/// safe-to-synthesize-a-reject) — provably false. The status-as-code range (100..600) is disjoint
/// from OKX business codes (≥ 10000), so the fallback stays unambiguous downstream.
fn http_error(status: u16, parsed: Result<serde_json::Value, serde_json::Error>) -> VenueApiError {
    match parsed {
        Ok(body) => {
            if let Some(code) = body.get("code") {
                let code = match code {
                    serde_json::Value::String(s) => s.parse::<i64>().unwrap_or(i64::from(status)),
                    serde_json::Value::Number(n) => n.as_i64().unwrap_or(i64::from(status)),
                    _ => i64::from(status),
                };
                VenueApiError {
                    code,
                    msg: body.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                }
            } else {
                VenueApiError { code: i64::from(status), msg: body.to_string() }
            }
        }
        Err(_) => VenueApiError { code: i64::from(status), msg: "http error".to_string() },
    }
}

impl OkxTransport for UreqOkxTransport {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        // Gate BEFORE signing so a throttle wait doesn't age the OK-ACCESS-TIMESTAMP (matches the
        // generic UreqTransport ordering).
        self.enter_gate(method);
        let prepared = signer.prepare(params, method, path);
        let url = if method.eq_ignore_ascii_case("GET") && !prepared.query.is_empty() {
            format!("{base_url}{path}?{}", prepared.query) // signed query VERBATIM
        } else {
            format!("{base_url}{path}")
        };
        self.run(&self.agent, &url, method, &prepared.headers, prepared.body.as_deref())
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        let prepared = signer.prepare(params, method, path);
        let url = if method.eq_ignore_ascii_case("GET") && !prepared.query.is_empty() {
            format!("{base_url}{path}?{}", prepared.query)
        } else {
            format!("{base_url}{path}")
        };
        // Same request as `signed`, but on the short-timeout agent (audit T1 stall bound).
        self.run(&self.requery_agent, &url, method, &prepared.headers, prepared.body.as_deref())
    }

    fn signed_json(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        body: &str,
        signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.enter_gate(method); // gate before signing (see `signed`)
        let prepared = signer.prepare_json(body, method, path);
        let url = format!("{base_url}{path}");
        self.run(&self.agent, &url, method, &prepared.headers, prepared.body.as_deref())
    }

    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        let url = if params.is_empty() {
            format!("{base_url}{path}")
        } else {
            format!("{base_url}{path}?{}", vike_bridge_core::signer::urlencode(params))
        };
        self.enter_gate("GET");
        self.run(&self.agent, &url, "GET", &[], None)
    }
}

/// OKX two-level envelope unwrap (the client's `unwrap`): top-level code != "0" raises
/// (preferring a per-order data[0].sCode when present); success + data[0].sCode != "0"
/// ALSO raises (partial-batch per-order error). Returns `data` (a list).
pub fn unwrap_okx(resp: serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
    let code_str = match resp.get("code") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => "0".to_string(),
    };
    let data = resp.get("data").cloned().unwrap_or(serde_json::json!([]));
    let scode =
        data.as_array().and_then(|d| d.first()).and_then(|d0| d0.get("sCode")).map(|s| match s {
            serde_json::Value::String(x) => x.clone(),
            other => other.to_string(),
        });
    if code_str != "0" {
        if let Some(sc) = &scode
            && sc != "0"
        {
            return Err(per_order_error(&data, sc));
        }
        return Err(VenueApiError {
            code: code_str.parse::<i64>().unwrap_or(0),
            msg: resp.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        });
    }
    if let Some(sc) = &scode
        && sc != "0"
    {
        return Err(per_order_error(&data, sc));
    }
    Ok(data)
}

fn per_order_error(data: &serde_json::Value, scode: &str) -> VenueApiError {
    VenueApiError {
        code: scode.parse::<i64>().unwrap_or(0),
        msg: data
            .as_array()
            .and_then(|d| d.first())
            .and_then(|d0| d0.get("sMsg"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

#[cfg(test)]
mod http_error_mapping {
    use super::*;

    fn parse(text: &str) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(text)
    }

    #[test]
    fn venue_string_code_wins_over_the_status() {
        let e = http_error(429, parse(r#"{"code": "50011", "msg": "Requests too frequent"}"#));
        assert_eq!(e.code, 50011);
        assert_eq!(e.msg, "Requests too frequent");
    }

    #[test]
    fn venue_numeric_code_wins_over_the_status() {
        let e = http_error(400, parse(r#"{"code": 51000, "msg": "Parameter error"}"#));
        assert_eq!(e.code, 51000);
        assert_eq!(e.msg, "Parameter error");
    }

    /// The fix: a PRESENT but wrong-typed `code` must fall back to the HTTP status like the absent
    /// case — never the pre-send `code: 0` sentinel (headers arrived, so "nothing left this host"
    /// is provably false; a `0` would classify a delivered request as safe-to-reject `Network`).
    #[test]
    fn wrong_typed_code_falls_back_to_the_status_not_the_pre_send_sentinel() {
        let e = http_error(503, parse(r#"{"code": true, "msg": "upstream unhappy"}"#));
        assert_ne!(e.code, 0, "code 0 is the definite PRE-SEND sentinel — false post-headers");
        assert_eq!(e.code, 503);
        assert_eq!(e.msg, "upstream unhappy");
    }

    /// Same fix, the non-numeric-string shape.
    #[test]
    fn non_numeric_string_code_falls_back_to_the_status() {
        let e = http_error(500, parse(r#"{"code": "oops", "msg": "broken"}"#));
        assert_ne!(e.code, 0);
        assert_eq!(e.code, 500);
        assert_eq!(e.msg, "broken");
    }

    /// Same fix, a numeric that does not fit an i64 (float): status, not 0.
    #[test]
    fn non_i64_number_code_falls_back_to_the_status() {
        let e = http_error(500, parse(r#"{"code": 1.5, "msg": "x"}"#));
        assert_eq!(e.code, 500);
    }

    // The two pre-existing fallback branches, pinned unchanged:

    #[test]
    fn absent_code_falls_back_to_the_status() {
        let e = http_error(404, parse(r#"{"note": "no okx envelope"}"#));
        assert_eq!(e.code, 404);
        assert_eq!(e.msg, r#"{"note":"no okx envelope"}"#);
    }

    #[test]
    fn unparseable_body_falls_back_to_the_status() {
        let e = http_error(502, parse("<html>bad gateway</html>"));
        assert_eq!(e.code, 502);
        assert_eq!(e.msg, "http error");
    }
}

#[cfg(test)]
mod rate_gate_wiring {
    use super::*;
    use std::time::Duration;
    use vike_bridge_core::credentials::Credentials;
    use vike_bridge_core::ratelimit::RateGate;

    // A closed localhost port: connection-refused is immediate (no DNS/network), so the send fails
    // fast — the assertion is purely on the GATE, consulted BEFORE the send.
    const DEAD: &str = "http://127.0.0.1:1";

    fn signer() -> OkxV5Signer {
        let creds = Credentials {
            api_key: "k".into(),
            api_secret: "s".into(),
            passphrase: Some("p".into()),
        };
        OkxV5Signer::new(&creds, || 0)
    }

    fn gated() -> (RateGate, UreqOkxTransport) {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = UreqOkxTransport::new(false).with_rate_gate(gate.clone());
        (gate, t)
    }

    #[test]
    fn signed_consumes_the_gate() {
        let (gate, t) = gated();
        let _ = t.signed(DEAD, "/x", "GET", &[], &signer());
        assert!(!gate.try_proceed(), "signed took the gate's only slot");
    }

    #[test]
    fn signed_json_consumes_the_gate() {
        let (gate, t) = gated();
        let _ = t.signed_json(DEAD, "/x", "POST", "{}", &signer());
        assert!(!gate.try_proceed(), "signed_json took the gate's only slot");
    }

    #[test]
    fn public_consumes_the_gate() {
        let (gate, t) = gated();
        let _ = t.public(DEAD, "/x", &[]);
        assert!(!gate.try_proceed(), "public took the gate's only slot");
    }

    #[test]
    fn t1_requery_is_exempt_from_the_gate() {
        let (gate, t) = gated();
        let _ = t.signed_requery(DEAD, "/x", "GET", &[], &signer());
        assert!(gate.try_proceed(), "the T1 re-query must not ride the gate");
    }

    #[test]
    fn no_gate_configured_is_a_passthrough() {
        let t = UreqOkxTransport::new(false);
        let _ = t.public(DEAD, "/x", &[]);
    }
}
