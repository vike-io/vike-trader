//! **ONE signed JSON transport** for the venues whose params are typed JSON pairs and whose POST carries the EXACT bytes
//! its signer signed (bybit, okx): the seam [`RestTransportJson`], stubbed with canned envelopes in offline tests, and
//! the live [`RestTransportJsonUreq`]. A GET appends the signer's query verbatim; every other method is a POST of the
//! signed body with `Content-Type: application/json`, never re-serialized (it would break the signature).
//!
//! The venue's facts are parameters: its name (the rate gate's and its log's), its [`RestReplySpec`] (the envelope and
//! write rules: [`super::reply`]), the function that writes its log lines (each venue keeps its own `tracing` target),
//! the headers it sends on every request (okx: a browser `User-Agent` block), and the network it talks to with the
//! headers that select it (okx's demo network: the simulated-trading header), which the seam answers
//! ([`RestTransportJson::network`]) for a venue that gates a test-network-only write on it. The venue's 200-with-a-business-error envelope (`retCode`, `code`) is unwrapped by the
//! venue's client, not here.
//!
//! The order-status re-query (`signed_requery`) runs on a short-timeout agent ([`super::REQUERY_TIMEOUT`]) and is
//! exempt from the rate gate, so a throttle can never bring back the stall that agent bounds; every other call rides
//! the gate, which is consulted BEFORE signing so a throttle wait does not age the signed timestamp. A venue that signs
//! on the venue's clock (owner ruling B-4) hands the transport its [`VenueClock`] (`with_venue_clock`), read before the
//! first signed call.

use std::time::Duration;

use serde_json::Value;
use vike_model::VenueNetwork;

use super::reply::{RestReplyNote, RestReplySpec, classify_rest_reply};
use super::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use crate::ratelimit::RateGate;
use crate::signer::{SignerJson, VENUE_CLOCK_READ_TIMEOUT, VenueClock, urlencode};

/// The signed JSON REST seam. Only `signed` is required: a venue without the other calls keeps their defaults, which
/// send nothing and answer in words (code `0`: nothing left this host).
pub trait RestTransportJson {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError>;

    /// [`Self::signed`] for the order-status re-query after an ambiguous submit: a live transport runs it on a SHORT
    /// timeout, so a double timeout (the submit, then the re-query) cannot stall the exec thread. DEFAULT: `signed`.
    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.signed(base_url, path, method, params, signer)
    }

    /// Send an already-serialized JSON body (an ARRAY body: okx's algo and batch endpoints), signed as it is; the raw
    /// envelope comes back, for the caller's per-row read. DEFAULT: refused, nothing sent.
    fn signed_json(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        _body: &str,
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        Err(VenueApiError {
            code: 0,
            msg: format!("{method} {path}: this transport sends no pre-serialized body"),
        })
    }

    /// An UNSIGNED GET. DEFAULT: refused, nothing sent.
    fn public(
        &self,
        _base_url: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        Err(VenueApiError {
            code: 0,
            msg: format!("GET {path}: this transport makes no unsigned call"),
        })
    }

    /// Which network this transport talks to: what a venue gates a write only its test network may take on (okx's
    /// account modes: owner ruling 2026-10-26, no exchange write on a live account). DEFAULT [`VenueNetwork::Mainnet`]:
    /// a transport that does not say is the real network, the one no such write is made on.
    fn network(&self) -> VenueNetwork {
        VenueNetwork::Mainnet
    }
}

/// The blocking ureq [`RestTransportJson`] (rustls, the shared agents of `crate::http`: no redirect followed).
pub struct RestTransportJsonUreq {
    agent: ureq::Agent,
    requery_agent: ureq::Agent,
    venue: &'static str,
    reply: &'static RestReplySpec,
    note: fn(RestReplyNote<'_>),
    headers: Vec<(&'static str, &'static str)>,
    network: VenueNetwork,
    rate_gate: Option<RateGate>,
    clock: Option<JsonClock>,
}

/// What [`RestTransportJsonUreq::with_venue_clock`] keeps.
struct JsonClock {
    clock: VenueClock,
    /// A keyless agent of its own, bounded at [`VENUE_CLOCK_READ_TIMEOUT`], that spends no order window.
    reader: ureq::Agent,
    time_path: &'static str,
    server_ms: fn(&Value) -> Option<i64>,
}

impl RestTransportJsonUreq {
    /// `venue`'s transport: replies classified by `reply`, its log lines written by `note`.
    pub fn new(
        venue: &'static str,
        reply: &'static RestReplySpec,
        note: fn(RestReplyNote<'_>),
    ) -> Self {
        RestTransportJsonUreq {
            agent: crate::http::blocking_agent(),
            requery_agent: crate::http::blocking_agent_with_timeout(super::REQUERY_TIMEOUT),
            venue,
            reply,
            note,
            headers: Vec::new(),
            network: VenueNetwork::Mainnet,
            rate_gate: None,
            clock: None,
        }
    }

    /// **Owner ruling B-4 (2026-11-11): sign on the venue's clock.** Before a signed call (`signed`, `signed_requery`,
    /// `signed_json`) the transport reads `clock` if a read is due ([`VenueClock::read_if_due`]: once, or again a
    /// minute after a failure): a keyless `GET {base_url}{time_path}` whose answer `server_ms` reads. The signers built
    /// on `clock.ms()` then stamp the venue's time. A read that fails is warned and the call signs on the offset held,
    /// the local clock until one answers. Without this, the signers stamp what they were built with (bybit and okx: the
    /// local clock, before they apply B-4).
    pub fn with_venue_clock(
        mut self,
        clock: &VenueClock,
        time_path: &'static str,
        server_ms: fn(&Value) -> Option<i64>,
    ) -> Self {
        self.clock = Some(JsonClock {
            clock: clock.clone(),
            reader: crate::http::blocking_agent_with_timeout(VENUE_CLOCK_READ_TIMEOUT),
            time_path,
            server_ms,
        });
        self
    }

    /// Read the venue's clock from `base_url` if [`Self::with_venue_clock`] gave one and a read is due.
    fn sync(&self, base_url: &str) {
        let Some(c) = &self.clock else { return };
        let read = c.clock.read_if_due(vike_model::now_ms(), |local| {
            let body =
                self.run(&c.reader, &format!("{base_url}{}", c.time_path), "GET", &[], None)?;
            (c.server_ms)(&body).map(|server| server - local).ok_or_else(|| VenueApiError {
                code: E_TIMEOUT_AMBIGUOUS,
                msg: format!("GET {}: the answer carries no server time", c.time_path),
            })
        });
        if let Err(e) = read {
            tracing::warn!(
                venue = self.venue, base_url, path = c.time_path, code = e.code, msg = %e.msg,
                "server time read failed; signing on the offset held, the local clock until a read answers"
            );
        }
    }

    /// Send `headers` on every request, ahead of `Content-Type` and the signer's own.
    pub fn with_headers(mut self, headers: &[(&'static str, &'static str)]) -> Self {
        self.headers.extend_from_slice(headers);
        self
    }

    /// The headers [`Self::with_headers`] and [`Self::with_network`] added.
    pub fn headers(&self) -> &[(&'static str, &'static str)] {
        &self.headers
    }

    /// Talk to `network`, sending `headers` on every request to select it, for a venue whose one host serves both
    /// networks (okx: the simulated-trading header). One call, so the transport's answer ([`RestTransportJson::network`])
    /// and the request it makes cannot disagree. Without it the transport answers [`VenueNetwork::Mainnet`].
    pub fn with_network(
        mut self,
        network: VenueNetwork,
        headers: &[(&'static str, &'static str)],
    ) -> Self {
        self.network = network;
        self.with_headers(headers)
    }

    /// The ordinary agent (every call but the re-query) gives up after `timeout`, not the shared 30 s: for a read that
    /// blocks a thread nothing else can run on.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.agent = crate::http::blocking_agent_with_timeout(timeout);
        self
    }

    /// Ride `gate` on every call but the re-query. `Clone` one gate across a venue's clients so they share one window.
    pub fn with_rate_gate(mut self, gate: RateGate) -> Self {
        self.rate_gate = Some(gate);
        self
    }

    /// The attached gate, if any: what the clients built for other symbols share.
    pub fn rate_gate(&self) -> Option<&RateGate> {
        self.rate_gate.as_ref()
    }

    fn enter_gate(&self, what: &str) {
        if let Some(gate) = &self.rate_gate {
            gate.proceed_logged(self.venue, what);
        }
    }

    fn signed_url(base_url: &str, path: &str, method: &str, query: &str) -> String {
        if method.eq_ignore_ascii_case("GET") && !query.is_empty() {
            format!("{base_url}{path}?{query}") // the signed query VERBATIM
        } else {
            format!("{base_url}{path}")
        }
    }

    fn run(
        &self,
        agent: &ureq::Agent,
        url: &str,
        method: &str,
        signed_headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<Value, VenueApiError> {
        let get = method.eq_ignore_ascii_case("GET");
        let sent = if get {
            let mut req = agent.get(url);
            for (k, v) in &self.headers {
                req = req.header(*k, *v);
            }
            for (k, v) in signed_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.call()
        } else {
            let mut req = agent.post(url);
            for (k, v) in &self.headers {
                req = req.header(*k, *v);
            }
            req = req.header("Content-Type", "application/json");
            for (k, v) in signed_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.send(body.unwrap_or_default()) // the EXACT signed bytes
        };
        classify_rest_reply(sent, !get, self.reply, self.note)
    }
}

impl RestTransportJson for RestTransportJsonUreq {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.sync(base_url);
        self.enter_gate(method);
        let prepared = signer.prepare(params, method, path);
        let url = Self::signed_url(base_url, path, method, &prepared.query);
        self.run(&self.agent, &url, method, &prepared.headers, prepared.body.as_deref())
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.sync(base_url);
        let prepared = signer.prepare(params, method, path);
        let url = Self::signed_url(base_url, path, method, &prepared.query);
        self.run(&self.requery_agent, &url, method, &prepared.headers, prepared.body.as_deref())
    }

    fn signed_json(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        body: &str,
        signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.sync(base_url);
        self.enter_gate(method);
        let prepared = signer.prepare_body(body, method, path);
        self.run(
            &self.agent,
            &format!("{base_url}{path}"),
            method,
            &prepared.headers,
            prepared.body.as_deref(),
        )
    }

    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        let url = if params.is_empty() {
            format!("{base_url}{path}")
        } else {
            format!("{base_url}{path}?{}", urlencode(params))
        };
        self.enter_gate("GET");
        self.run(&self.agent, &url, "GET", &[], None)
    }

    fn network(&self) -> VenueNetwork {
        self.network
    }
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod json_tests;
