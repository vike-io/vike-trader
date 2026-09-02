//! Signed blocking REST transport for Bybit V5. Exact port of `exec/bybit/transport.py`:
//! GET appends the signed query to the URL; POST sends the EXACT signed JSON body bytes
//! with Content-Type: application/json — never re-serialized, or X-BAPI-SIGN won't match.
//! Bybit returns HTTP 200 for both success and business errors; the {retCode,retMsg,
//! result} envelope is unwrapped by the CLIENT's `unwrap` (raising on retCode != 0),
//! not here. Non-2xx bodies with a retCode become typed errors.

use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::{classify_send_error, read_body_ambiguous, VenueApiError};

/// Bybit REST seam (typed params — Bybit JSON bodies carry ints/bools). Stubbed with
/// canned envelopes in offline tests; [`UreqBybitTransport`] live.
pub trait BybitTransport {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError>;

    /// Bounded-timeout twin of `signed` for the audit-T1 order-status re-query. DEFAULT delegates
    /// to `signed` (offline stubs unchanged); the live transport runs it on a short-timeout agent.
    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.signed(base_url, path, method, params, signer)
    }
}

pub struct UreqBybitTransport {
    agent: ureq::Agent,
    /// Short-timeout agent for the audit-T1 re-query only (bounds the double-timeout stall).
    requery_agent: ureq::Agent,
    /// Optional REST rate gate (net-hardening spec §A). Consulted on `signed` only — the T1
    /// `signed_requery` is exempt so a throttle can't reintroduce the stall its short-timeout
    /// agent bounds.
    rate_gate: Option<RateGate>,
}

impl Default for UreqBybitTransport {
    fn default() -> Self {
        UreqBybitTransport {
            agent: vike_bridge_core::http::blocking_agent(),
            requery_agent: vike_bridge_core::http::blocking_agent_with_timeout(
                std::time::Duration::from_secs(5),
            ),
            rate_gate: None,
        }
    }
}

impl UreqBybitTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a REST rate gate; every `signed` call then rides it (T1 re-query stays exempt).
    pub fn with_rate_gate(mut self, gate: RateGate) -> Self {
        self.rate_gate = Some(gate);
        self
    }

    /// Consult the rate gate before a normal request, if one is configured. No-op otherwise.
    fn enter_gate(&self, what: &str) {
        if let Some(gate) = &self.rate_gate {
            gate.proceed_logged("bybit", what);
        }
    }
}

impl UreqBybitTransport {
    fn run(
        &self,
        agent: &ureq::Agent,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        let prepared = signer.prepare(params, method);
        let sent = if method.eq_ignore_ascii_case("GET") {
            let url = if prepared.query.is_empty() {
                format!("{base_url}{path}")
            } else {
                format!("{base_url}{path}?{}", prepared.query)
            };
            let mut req = agent.get(&url);
            for (k, v) in &prepared.headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.call()
        } else {
            let mut req =
                agent.post(&format!("{base_url}{path}")).header("Content-Type", "application/json");
            for (k, v) in &prepared.headers {
                req = req.header(k.as_str(), v.as_str());
            }
            // the EXACT signed bytes — never re-serialize
            req.send(&prepared.body.clone().unwrap_or_default()[..])
        };
        // audit T1 (send-failure + body-read classification) is SHARED — see
        // `vike_bridge_core::transport::classify_send_error`. The warn stays here: this venue logs
        // under its own compile-time `target`, which RUST_LOG directives filter on.
        let mut resp = sent.map_err(|e| {
            tracing::warn!(target: "vike_bybit::transport", error = %e, "REST request failed");
            classify_send_error(&e)
        })?;
        let status = resp.status().as_u16();
        let text = read_body_ambiguous(&mut resp)?;
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
        if (200..300).contains(&status) {
            return parsed.map_err(|e| VenueApiError { code: 0, msg: format!("bad json: {e}") });
        }
        tracing::warn!(target: "vike_bybit::transport", status, "REST request failed");
        match parsed {
            Ok(body) => {
                if let Some(code) = body.get("retCode").and_then(|c| c.as_i64()) {
                    Err(VenueApiError {
                        code,
                        msg: body.get("retMsg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                    })
                } else {
                    Err(VenueApiError { code: i64::from(status), msg: body.to_string() })
                }
            }
            Err(_) => Err(VenueApiError { code: i64::from(status), msg: "http error".to_string() }),
        }
    }
}

impl BybitTransport for UreqBybitTransport {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.enter_gate(method);
        self.run(&self.agent, base_url, path, method, params, signer)
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, serde_json::Value)],
        signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.run(&self.requery_agent, base_url, path, method, params, signer)
    }
}

/// Client-side envelope unwrap: retCode != 0 raises; success returns `result`.
pub fn unwrap_envelope(resp: serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
    let code = resp.get("retCode").and_then(|c| c.as_i64()).unwrap_or(0);
    if code != 0 {
        return Err(VenueApiError {
            code,
            msg: resp.get("retMsg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        });
    }
    Ok(resp.get("result").cloned().unwrap_or(serde_json::json!({})))
}

#[cfg(test)]
mod rate_gate_wiring {
    use super::*;
    use std::time::Duration;
    use vike_bridge_core::credentials::Credentials;
    use vike_bridge_core::ratelimit::RateGate;

    // A closed localhost port: connection-refused is immediate (no DNS/network), so the send fails
    // fast — the assertion is purely on the GATE, which is consulted BEFORE the send.
    const DEAD: &str = "http://127.0.0.1:1";

    fn signer() -> BybitV5Signer {
        let creds = Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None };
        BybitV5Signer::new(&creds, || 0)
    }

    #[test]
    fn signed_consumes_the_gate() {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = UreqBybitTransport::new().with_rate_gate(gate.clone());
        let _ = t.signed(DEAD, "/x", "GET", &[], &signer());
        assert!(!gate.try_proceed(), "the signed call took the gate's only slot");
    }

    #[test]
    fn t1_requery_is_exempt_from_the_gate() {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = UreqBybitTransport::new().with_rate_gate(gate.clone());
        let _ = t.signed_requery(DEAD, "/x", "GET", &[], &signer());
        assert!(gate.try_proceed(), "the T1 re-query must not ride the gate");
    }

    #[test]
    fn no_gate_configured_is_a_passthrough() {
        let t = UreqBybitTransport::new();
        let _ = t.signed(DEAD, "/x", "GET", &[], &signer());
    }
}
