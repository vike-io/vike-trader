//! Signed blocking REST transport (e.g. Binance's shape: signed query in the URL for every
//! method, no body). Exact port of `exec/binance/transport.py` semantics: a
//! `{"code":-XXXX,"msg":"..."}` non-2xx body becomes a typed [`VenueApiError`];
//! non-JSON error bodies (WAF/HTML) become a generic (http-status, reason) error;
//! network failures map to code 0.

use crate::ratelimit::RateGate;
use crate::signer::{Signer, urlencode};

/// A venue order/account error, normalized to (code, msg). Twin of
/// `exec/crypto_client.py::VenueApiError` / `BinanceApiError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueApiError {
    pub code: i64,
    pub msg: String,
}

/// Ambiguous transport failure (audit T1): a timeout AFTER the request may have reached the venue,
/// so the venue MAY have accepted the order. Distinct from `code: 0` (a definite pre-send failure —
/// DNS/connect/TLS — where the order never reached the venue). A submit that sees this MUST re-query
/// order status, never emit a terminal `OrderRejected` (which would leave a phantom position).
pub const E_TIMEOUT_AMBIGUOUS: i64 = i64::MIN;

impl std::fmt::Display for VenueApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "venue error {}: {}", self.code, self.msg)
    }
}

impl std::error::Error for VenueApiError {}

impl VenueApiError {
    /// This error's DEFAULT [`ErrorKind`] — the single principled source for retry-vs-abort at the
    /// transport boundary (audit br3), replacing the per-consumer ad-hoc inspection of the raw
    /// `code`. Pure function of `{code, msg}`; see [`ErrorKind::classify`] for the mapping.
    pub fn kind(&self) -> ErrorKind {
        ErrorKind::classify(self.code, &self.msg)
    }

    /// Per-venue override hook: `f` is consulted FIRST and, when it returns `Some(kind)`, wins;
    /// `None` falls back to the central [`kind`](Self::kind) default. This is how a venue keeps its
    /// own nuance (e.g. remapping a shared HTTP 403 to [`ErrorKind::RateLimited`] for a CDN IP-block,
    /// as Bybit fronts REST with Cloudflare) WITHOUT forking the baseline classification, which stays
    /// central. Per-adapter adoption of this hook is a follow-up — this PR only makes it available.
    pub fn kind_with(&self, f: impl FnOnce(&VenueApiError) -> Option<ErrorKind>) -> ErrorKind {
        f(self).unwrap_or_else(|| self.kind())
    }
}

/// A coarse, venue-neutral classification of a transport-boundary failure (audit br3). Collapses the
/// raw venue `{code, msg}` / HTTP status behind [`VenueApiError`] into a small principled set so
/// retry-vs-abort decisions have ONE source instead of the ad-hoc per-venue code lists otherwise
/// duplicated across the adapters. The DEFAULT mapping ([`ErrorKind::classify`]) lives here; a venue
/// may refine it via [`VenueApiError::kind_with`] without moving the baseline off-center.
///
/// SEEDED from the pre-existing 3-way REST severity split (see [`VenueApiError`] /
/// [`E_TIMEOUT_AMBIGUOUS`]): `code == 0` → [`Network`](Self::Network) (a definite pre-send failure),
/// [`E_TIMEOUT_AMBIGUOUS`] → [`Timeout`](Self::Timeout) (the audit-T1 "must re-query, never reject"
/// path, preserved as its OWN kind so it can't regress into a blind retry/reject), everything else →
/// an HTTP-status / venue-code mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Throttled — HTTP 429/418 or a venue rate-limit code. Retryable (honor `Retry-After`).
    RateLimited,
    /// Authentication / signature / permission failure — HTTP 401/403 or a venue key/sign code.
    /// Terminal: retrying without fixing credentials just repeats it.
    Auth,
    /// Malformed / rejected request — an HTTP 400-class status or a venue param/filter/precision
    /// code. Terminal: the request is wrong, not the moment.
    InvalidRequest,
    /// The referenced entity is absent — HTTP 404 or a venue "unknown / does-not-exist order" code.
    NotFound,
    /// The venue itself failed — HTTP 5xx or a venue internal/unavailable code. Retryable.
    ServerError,
    /// A definite PRE-SEND transport failure (DNS/connect/TLS), `code == 0`: the request never
    /// reached the venue. Retryable; for an order SUBMIT it is ALSO safe to reject (nothing landed) —
    /// which is exactly what distinguishes it from [`Timeout`](Self::Timeout).
    Network,
    /// An AMBIGUOUS timeout ([`E_TIMEOUT_AMBIGUOUS`]): the request may have reached the venue, so the
    /// order MAY be live. The caller MUST re-query status and NEVER synthesize a terminal reject
    /// (audit T1). Kept a distinct kind precisely so this path can't be collapsed into a blind retry.
    Timeout,
    /// The order was refused for lack of margin / wallet balance (a venue "insufficient balance"
    /// code). Terminal for THIS request and semantically distinct from [`InvalidRequest`](Self::InvalidRequest):
    /// the request was well-formed, the ACCOUNT could not back it — a sizing/risk signal, not a bug.
    ///
    /// ADDITIVE (venue-taxonomy lane): [`ErrorKind::classify`] NEVER returns this, so every
    /// pre-existing caller is byte-identical. Only the opt-in per-venue tables
    /// ([`crate::error_kind`]) produce it.
    InsufficientFunds,
    /// The venue is in a scheduled maintenance / upgrade window (OKX 50001/50004, Bybit 10016
    /// "service is restarting", Binance -1016). Retryable, but on a LONG backoff — unlike
    /// [`ServerError`](Self::ServerError) this is expected to last minutes, not milliseconds.
    ///
    /// ADDITIVE, same as [`InsufficientFunds`](Self::InsufficientFunds): unreachable from
    /// [`ErrorKind::classify`], produced only by the opt-in per-venue tables.
    VenueMaintenance,
    /// Unclassified — no default rule matched. Treated conservatively (non-retryable) until a venue
    /// override or a follow-up broadens the seed. This IS the "default Fatal" safe posture: an
    /// unrecognized failure is never retried and never silently tolerated
    /// (see [`is_terminal`](Self::is_terminal)).
    Unknown,
}

impl ErrorKind {
    /// The DEFAULT, venue-neutral classification of a `{code, msg}` pair. `code` carries the
    /// [`VenueApiError`] overload, decoded in order: the two sentinels ([`E_TIMEOUT_AMBIGUOUS`] and
    /// `0`) FIRST, then the HTTP-status-as-code range (`100..600`, produced by the transport when an
    /// error body has no venue `{code}`), then the seeded venue business codes; `msg` is a
    /// last-resort textual signal for the otherwise-[`Unknown`](Self::Unknown) bucket.
    pub fn classify(code: i64, msg: &str) -> ErrorKind {
        // audit T1 FIRST: the ambiguous-timeout sentinel is its OWN kind and nothing may shadow it.
        if code == E_TIMEOUT_AMBIGUOUS {
            return ErrorKind::Timeout;
        }
        // Definite pre-send failure (DNS/connect/TLS): the request never reached the venue.
        if code == 0 {
            return ErrorKind::Network;
        }
        // HTTP-status-as-code: the transport stores `i64::from(status)` when an error body carries no
        // venue `{code}`. Venue business codes never fall in this range (Binance is negative;
        // Bybit/OKX are ≥ 10000), so for transport-produced errors the split is unambiguous.
        if (100..600).contains(&code) {
            return ErrorKind::from_http_status(code as u16);
        }
        // Seeded venue business codes (Binance/Bybit/OKX), else a conservative msg-text fallback.
        classify_venue_code(code).unwrap_or_else(|| {
            let m = msg.to_ascii_lowercase();
            if m.contains("too many request") || m.contains("rate limit") {
                ErrorKind::RateLimited
            } else {
                ErrorKind::Unknown
            }
        })
    }

    /// Map a raw HTTP status to a kind — a reusable building block (a venue can call it from a
    /// [`VenueApiError::kind_with`] override). NOTE: HTTP 403 defaults to [`Auth`](Self::Auth), its
    /// standard meaning; a venue that fronts REST with an IP-block-on-403 CDN (e.g. Bybit behind
    /// Cloudflare) can override 403 → [`RateLimited`](Self::RateLimited).
    pub fn from_http_status(status: u16) -> ErrorKind {
        match status {
            401 | 403 => ErrorKind::Auth,
            404 => ErrorKind::NotFound,
            418 | 429 => ErrorKind::RateLimited, // 418 = Binance IP auto-ban; 429 = Too Many Requests
            400..=499 => ErrorKind::InvalidRequest, // other 4xx: bad request / params
            500..=599 => ErrorKind::ServerError,
            _ => ErrorKind::Unknown, // 1xx/2xx/3xx are not errors here
        }
    }

    /// The audit-T1 must-re-query signal: an ambiguous outcome where the venue MAY have accepted the
    /// request, so the caller MUST re-query status and NEVER synthesize a terminal reject. True for
    /// EXACTLY [`Timeout`](Self::Timeout) — deliberately NOT [`Network`](Self::Network), which is a
    /// definite pre-send failure (nothing landed) and so is safe to reject.
    pub fn must_requery(self) -> bool {
        matches!(self, ErrorKind::Timeout)
    }

    /// Whether the REQUEST itself is safe to retry as-is (idempotent transient): rate-limit,
    /// venue-server error, and definite pre-send network failures. Auth / InvalidRequest / NotFound
    /// are terminal; an ambiguous [`Timeout`](Self::Timeout) is NOT blind-retryable (it must
    /// [`must_requery`](Self::must_requery) instead, to avoid a double-submit);
    /// [`Unknown`](Self::Unknown) is treated as non-retryable (conservative).
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::RateLimited
                | ErrorKind::ServerError
                | ErrorKind::Network
                | ErrorKind::VenueMaintenance
        )
    }

    /// The complement of the two non-terminal dispositions: `true` when this kind means "stop —
    /// re-issuing as-is cannot help". Terminal kinds are the ones an order submit may answer with a
    /// synthesized `OrderRejected`; the rest either retry ([`is_retryable`](Self::is_retryable)) or
    /// re-query ([`must_requery`](Self::must_requery)).
    ///
    /// [`Unknown`](Self::Unknown) is terminal BY DESIGN — the safe posture for an unrecognized venue
    /// code is to abort the request, never to loop on it.
    pub fn is_terminal(self) -> bool {
        !self.is_retryable() && !self.must_requery()
    }

    /// How long a caller should wait before re-issuing a [`is_retryable`](Self::is_retryable) kind,
    /// as a coarse multiplier on the caller's own base backoff. Maintenance windows last minutes, so
    /// they get a much longer wait than an ordinary 5xx blip. `0` for kinds that must not be retried.
    ///
    /// Advisory only — no existing caller consults it, so it changes no behavior on its own.
    pub fn backoff_scale(self) -> u32 {
        match self {
            ErrorKind::VenueMaintenance => 20,
            ErrorKind::RateLimited => 4,
            ErrorKind::ServerError | ErrorKind::Network => 1,
            _ => 0,
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::Auth => "auth",
            ErrorKind::InvalidRequest => "invalid_request",
            ErrorKind::NotFound => "not_found",
            ErrorKind::ServerError => "server_error",
            ErrorKind::Network => "network",
            ErrorKind::Timeout => "timeout",
            ErrorKind::InsufficientFunds => "insufficient_funds",
            ErrorKind::VenueMaintenance => "venue_maintenance",
            ErrorKind::Unknown => "unknown",
        })
    }
}

/// The seeded venue-business-code table behind [`ErrorKind::classify`]. Representative, NOT
/// exhaustive: it covers the in-repo-documented rate-limit codes plus common Binance/Bybit/OKX
/// order/auth codes. Codes are disjoint across venues (Binance is negative; Bybit/OKX are ≥ 10000),
/// so one flat match is unambiguous. `None` → no rule matched.
///
/// NOT the WS-handshake transient set — see [`ws_ack_is_transient`], which the per-venue `ws_auth`
/// matchers delegate to instead. Folding the two together (this table's older doc proposed it)
/// turns out NOT to be behavior-preserving: this bucket's REST-only throttle codes would silently
/// promote three fatal handshake rejections to reconnect loops. Kept apart deliberately.
fn classify_venue_code(code: i64) -> Option<ErrorKind> {
    match code {
        // rate limit (retryable): Binance -1003/-1015, Bybit 10006/10018, OKX 50011/50061
        -1003 | -1015 | 10006 | 10018 | 50011 | 50061 => Some(ErrorKind::RateLimited),
        // auth/key/signature (terminal): Binance -2014/-2015/-1022, Bybit 10003/10004/10005, OKX 50111/50113
        -2014 | -2015 | -1022 | 10003 | 10004 | 10005 | 50111 | 50113 => Some(ErrorKind::Auth),
        // not found / unknown order: Binance -2011/-2013, Bybit 110001, OKX 51603
        -2011 | -2013 | 110001 | 51603 => Some(ErrorKind::NotFound),
        // server / venue down (retryable): Binance -1000/-1001/-1016, Bybit 10016, OKX 50001
        -1000 | -1001 | -1016 | 10016 | 50001 => Some(ErrorKind::ServerError),
        // bad request / params / filters (terminal): Binance -1013/-1100/-1102/-1111/-2010, Bybit 10001, OKX 51000
        -1013 | -1100 | -1102 | -1111 | -2010 | 10001 | 51000 => Some(ErrorKind::InvalidRequest),
        _ => None,
    }
}

/// A `ureq` SEND failure → [`VenueApiError`], audit-T1 classified. **The ONE site for that rule**:
/// a TIMEOUT may have reached the venue (the order MAY be live) so it carries the ambiguous
/// [`E_TIMEOUT_AMBIGUOUS`] sentinel and the caller MUST re-query; every other send failure
/// (DNS/connect/TLS) definitely never left this host, so it carries `code: 0` and a submit may
/// safely synthesize a terminal reject.
///
/// Hoisted from the byte-identical copies in [`UreqTransport::run`] and the bybit/okx/hyperliquid
/// transports (dedup F1): the classification is safety-critical (a wrong answer either strands a
/// phantom position or double-submits an order) and was drifting across hand-maintained twins. The
/// CALLER keeps its own `tracing::warn!` — each venue logs under its own target/venue field, and
/// those differ deliberately (`RUST_LOG` directives filter on them).
pub fn classify_send_error(e: &ureq::Error) -> VenueApiError {
    VenueApiError {
        // audit T1: a send TIMEOUT may have reached the venue (ambiguous); DNS/connect/TLS did not.
        code: if matches!(e, ureq::Error::Timeout(_)) { E_TIMEOUT_AMBIGUOUS } else { 0 },
        msg: format!("network error: {e}"),
    }
}

/// Read a response body to text; a read failure is ALWAYS the ambiguous [`E_TIMEOUT_AMBIGUOUS`]
/// (audit T1) — the response HEADERS already arrived, so the venue demonstrably RECEIVED the
/// request and may have acted on it. Never `code: 0` here: that would license a terminal reject
/// for an order that might be live.
///
/// The read-half twin of [`classify_send_error`], hoisted from the same four copies (dedup F1).
pub fn read_body_ambiguous(
    resp: &mut ureq::http::Response<ureq::Body>,
) -> Result<String, VenueApiError> {
    resp.body_mut().read_to_string().map_err(|e| VenueApiError {
        code: E_TIMEOUT_AMBIGUOUS,
        msg: format!("network error: {e}"),
    })
}

/// Is `code` a venue business code that a private-WS handshake ack should treat as **TRANSIENT**
/// (close + reconnect with backoff) rather than a genuine auth failure (surface and stop)?
///
/// ONE list, delegated to by the binance/bybit/okx `ws_auth` ack matchers, which each kept a
/// private 2-code copy (dedup F5). Per-venue codes are disjoint (Binance is negative; Bybit/OKX are
/// ≥ 10000), so one flat union is EXACT per venue — each only ever sees its own:
/// - Binance `-1021` (timestamp outside recvWindow), `-1003` (too many requests)
/// - Bybit `10002` (request timestamp outside the recv window), `10006` (too many visits)
/// - OKX `50011` (rate limit), `50102` (timestamp expired)
///
/// Deliberately NOT [`classify_venue_code`]'s [`RateLimited`](ErrorKind::RateLimited) bucket, though
/// it overlaps it: the WS handshake is a DIFFERENT channel from REST with its own code vocabulary,
/// and that bucket also carries REST-only throttle codes (Binance `-1015` too-many-orders, Bybit
/// `10018`, OKX `50061`) which have never been handshake-transient here. Folding those in would turn
/// a fatal handshake rejection into a silent reconnect loop on three LIVE order streams — a
/// behavior change a dedup does not license. (That, not an oversight, is why the codes stay listed
/// here rather than derived: `classify_venue_code`'s own doc proposes the fold-in, but it cannot be
/// done behavior-preservingly. Taking the broadening deliberately is a separate decision.)
///
/// An absent (`0` — every matcher's `unwrap_or(0)` on a malformed frame) or unknown code is NOT
/// transient, so a genuinely broken session still surfaces instead of reconnect-looping forever.
pub fn ws_ack_is_transient(code: i64) -> bool {
    matches!(code, -1021 | -1003 | 10002 | 10006 | 50011 | 50102)
}

/// The REST seam — stubbed with canned JSON in offline tests, [`UreqTransport`] live.
pub trait RestTransport {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError>;

    /// UNSIGNED GET (exchangeInfo / time / ticker).
    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError>;

    /// Signed request for the audit-T1 order-status **re-query** on an ambiguous submit timeout.
    /// Identical semantics to [`signed`](RestTransport::signed) but a live transport runs it on a
    /// SHORT timeout, so a double-timeout (the submit ~30s, then the requery) can't stall the
    /// single-writer core toward ~60s. DEFAULT delegates to `signed` — offline stubs need no change,
    /// and any transport without a bounded agent simply reuses its normal timeout.
    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.signed(base_url, path, method, params, signer)
    }
}

/// Blocking ureq transport (rustls — no OpenSSL). Non-2xx statuses are read for their
/// body (e.g. Binance carries the real error there) instead of erroring at the HTTP layer.
pub struct UreqTransport {
    agent: ureq::Agent,
    /// Short-timeout agent for the audit-T1 order-status re-query only, so a double-timeout can't
    /// stall the core toward ~60s. Built once; the re-query is off the hot path.
    requery_agent: ureq::Agent,
    /// The venue this transport instance is wired to (e.g. "binance"), stamped onto the
    /// diagnostic logs below. `UreqTransport` is a generic-shaped (Binance-style) transport that
    /// more than one venue can construct, so the venue name is caller-supplied here rather than
    /// hardcoded — see `run`'s log calls. (Before this crate was extracted (crate-reorg Phase 3,
    /// D4) the log `target` was a fixed venue-and-crate-qualified string literal naming binance
    /// specifically, which was already stale for any non-binance caller and would be outright
    /// wrong post-extraction. `tracing`'s `target:` must be a compile-time constant per callsite —
    /// it cannot vary per instance — so the venue name is carried as a structured `venue` field
    /// instead.)
    venue: &'static str,
    /// Optional REST rate gate (net-hardening spec §A). Consulted on the NORMAL request paths
    /// (`signed`/`public`) only — the audit-T1 `signed_requery` is deliberately exempt so a
    /// throttle can never reintroduce the ~60s stall its short-timeout agent exists to bound.
    rate_gate: Option<RateGate>,
}

impl UreqTransport {
    pub fn new(venue: &'static str) -> Self {
        UreqTransport {
            agent: crate::http::blocking_agent(),
            requery_agent: crate::http::blocking_agent_with_timeout(
                std::time::Duration::from_secs(5),
            ),
            venue,
            rate_gate: None,
        }
    }

    /// Like [`new`](Self::new), but wired to a CALLER-BUILT `ureq::Agent` for BOTH the normal and
    /// the audit-T1 re-query lanes. The reason this exists: [`new`](Self::new) builds a PLAIN agent
    /// with no proxy, which is wrong for a geo-blocked venue (Polymarket) whose every other lane
    /// routes through a SOCKS tunnel via its own proxied agent. Passing that same proxied agent here
    /// lets a REST warmup (e.g. the market feed's tick-size lookup) share the egress instead of
    /// dialing direct and hitting the very geo-block the proxy clears. The one agent is used for both
    /// fields — the two-agent split in [`new`](Self::new) exists only to bound the signed audit-T1
    /// re-query's timeout, and a proxied public-GET-only caller never walks that path, so a single
    /// agent is correct here.
    pub fn with_agent(venue: &'static str, agent: ureq::Agent) -> Self {
        UreqTransport { agent: agent.clone(), requery_agent: agent, venue, rate_gate: None }
    }

    /// Attach a REST rate gate. Every `signed`/`public` call then rides it (blocking on throttle,
    /// with a single warn) with zero call-site changes; the T1 re-query stays exempt. `Clone` the
    /// same gate across a venue's transport + backfill so they share one window.
    pub fn with_rate_gate(mut self, gate: RateGate) -> Self {
        self.rate_gate = Some(gate);
        self
    }

    /// Consult the rate gate before a normal request, if one is configured. No-op otherwise.
    fn enter_gate(&self, what: &str) {
        if let Some(gate) = &self.rate_gate {
            gate.proceed_logged(self.venue, what);
        }
    }

    fn run(
        &self,
        agent: &ureq::Agent,
        url: &str,
        method: &str,
        headers: &[(String, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        // e.g. Binance signs the query string — every method sends an EMPTY body.
        // (ureq types GET/DELETE vs POST/PUT builders differently; unify on the Result.)
        let sent = match method {
            "GET" | "DELETE" => {
                let mut req = if method == "GET" { agent.get(url) } else { agent.delete(url) };
                for (k, v) in headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                req.call()
            }
            "POST" | "PUT" => {
                let mut req = if method == "POST" { agent.post(url) } else { agent.put(url) };
                for (k, v) in headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                req.send_empty()
            }
            other => {
                return Err(VenueApiError { code: 0, msg: format!("unsupported method {other}") });
            }
        };
        // `venue` (caller-supplied at construction, see the struct doc) identifies which venue
        // hit this generically-shaped transport — constructed by each venue's REST client / smoke
        // with its own venue name.
        let mut resp = sent.map_err(|e| {
            tracing::warn!(venue = self.venue, error = %e, "REST request failed");
            classify_send_error(&e)
        })?;
        let status = resp.status().as_u16();
        let text = read_body_ambiguous(&mut resp)?;
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
        if (200..300).contains(&status) {
            return parsed.map_err(|e| VenueApiError { code: 0, msg: format!("bad json: {e}") });
        }
        // error status: prefer the venue's {code, msg} body
        tracing::warn!(venue = self.venue, status, "REST request failed");
        match parsed {
            Ok(body) => {
                if let Some(code) = body.get("code").and_then(|c| c.as_i64()) {
                    Err(VenueApiError {
                        code,
                        msg: body.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                    })
                } else {
                    Err(VenueApiError { code: i64::from(status), msg: body.to_string() })
                }
            }
            Err(_) => Err(VenueApiError { code: i64::from(status), msg: "http error".to_string() }),
        }
    }
}

impl RestTransport for UreqTransport {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.enter_gate(method);
        let prepared = signer.prepare(params, method, path);
        let url = format!("{base_url}{path}?{}", prepared.query);
        self.run(&self.agent, &url, method, &prepared.headers)
    }

    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        self.enter_gate("GET");
        let url = if params.is_empty() {
            format!("{base_url}{path}")
        } else {
            format!("{base_url}{path}?{}", urlencode(params))
        };
        self.run(&self.agent, &url, "GET", &[])
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        // Same request as `signed`, but on the short-timeout agent (audit T1 stall bound).
        // DELIBERATELY NOT rate-gated: this rare re-query exists to BOUND latency after a
        // timeout, so a blocking throttle here would defeat its purpose. The normal paths carry
        // the venue budget; exempting the re-query costs ~one extra call in a throttled window.
        let prepared = signer.prepare(params, method, path);
        let url = format!("{base_url}{path}?{}", prepared.query);
        self.run(&self.requery_agent, &url, method, &prepared.headers)
    }
}

#[cfg(test)]
mod rate_gate_wiring {
    use super::*;
    use crate::ratelimit::RateGate;
    use crate::signer::PreparedRequest;
    use std::time::Duration;

    struct NoopSigner;
    impl Signer for NoopSigner {
        fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
            PreparedRequest::default()
        }
    }

    // A closed localhost port: connection-refused is immediate and needs no DNS/network, so the
    // send fails fast. We assert on the GATE, which is consulted BEFORE the send.
    const DEAD: &str = "http://127.0.0.1:1";

    #[test]
    fn normal_public_call_consumes_the_gate() {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = UreqTransport::new("test").with_rate_gate(gate.clone());
        let _ = t.public(DEAD, "/x", &[]); // fails at the network; the slot is already taken
        assert!(!gate.try_proceed(), "the public call took the gate's only slot before sending");
    }

    #[test]
    fn t1_requery_is_exempt_from_the_gate() {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = UreqTransport::new("test").with_rate_gate(gate.clone());
        let _ = t.signed_requery(DEAD, "/x", "GET", &[], &NoopSigner);
        assert!(gate.try_proceed(), "the latency-bounded T1 re-query must NOT ride the gate");
    }

    #[test]
    fn no_gate_configured_is_a_passthrough() {
        // without with_rate_gate, calls just pass through — no panic, no gating.
        let t = UreqTransport::new("test");
        let _ = t.public(DEAD, "/x", &[]);
    }
}

#[cfg(test)]
mod error_kind_tests {
    use super::*;

    /// The non-regression guard for audit T1: the ambiguous-timeout sentinel must classify to its
    /// OWN kind (never Network / Unknown), and that kind is the must-re-query (never blind-retry,
    /// never reject) path.
    #[test]
    fn ambiguous_timeout_stays_its_own_kind() {
        let e = VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "network error: timed out".into() };
        assert_eq!(e.kind(), ErrorKind::Timeout);
        assert!(ErrorKind::Timeout.must_requery(), "the ambiguous path MUST re-query");
        assert!(!ErrorKind::Timeout.is_retryable(), "it must NOT be blind-retried (double-submit)");
        // The other kinds are NOT the re-query path.
        for k in [
            ErrorKind::Network,
            ErrorKind::RateLimited,
            ErrorKind::ServerError,
            ErrorKind::Auth,
            ErrorKind::InvalidRequest,
            ErrorKind::NotFound,
            ErrorKind::Unknown,
        ] {
            assert!(!k.must_requery(), "{k} must not be treated as the ambiguous re-query path");
        }
    }

    /// `code == 0` (definite pre-send DNS/connect/TLS failure) is Network — distinct from the
    /// ambiguous timeout, retryable, and NOT the re-query path (nothing landed → safe to reject).
    #[test]
    fn pre_send_network_failure_is_network() {
        let e = VenueApiError { code: 0, msg: "network error: dns".into() };
        assert_eq!(e.kind(), ErrorKind::Network);
        assert!(ErrorKind::Network.is_retryable());
        assert!(!ErrorKind::Network.must_requery());
    }

    /// The HTTP-status-as-code path (transport stores `i64::from(status)` when the error body has no
    /// venue `{code}`) maps by HTTP semantics.
    #[test]
    fn http_statuses_map_to_expected_kinds() {
        let cases = [
            (429, ErrorKind::RateLimited),
            (418, ErrorKind::RateLimited), // Binance teapot = IP auto-ban
            (401, ErrorKind::Auth),
            (403, ErrorKind::Auth), // default; a venue override can remap to RateLimited
            (404, ErrorKind::NotFound),
            (400, ErrorKind::InvalidRequest),
            (422, ErrorKind::InvalidRequest),
            (500, ErrorKind::ServerError),
            (503, ErrorKind::ServerError),
        ];
        for (status, want) in cases {
            assert_eq!(ErrorKind::from_http_status(status), want, "HTTP {status}");
            // Same mapping via the merged {code,msg} classifier (status carried as the code).
            assert_eq!(
                VenueApiError { code: i64::from(status), msg: "http error".into() }.kind(),
                want,
                "code={status}"
            );
        }
    }

    /// Representative venue business codes (Binance negative; Bybit/OKX ≥ 10000) map to the seeded
    /// kinds — the consolidation of the previously ad-hoc per-venue code lists.
    #[test]
    fn venue_business_codes_map_to_expected_kinds() {
        let cases = [
            // rate limit
            (-1003, ErrorKind::RateLimited), // Binance too many requests
            (10006, ErrorKind::RateLimited), // Bybit too many visits
            (50011, ErrorKind::RateLimited), // OKX requests too frequent
            // auth
            (-2015, ErrorKind::Auth), // Binance invalid key/IP/perms
            (10003, ErrorKind::Auth), // Bybit invalid api key
            (50113, ErrorKind::Auth), // OKX invalid signature
            // not found
            (-2011, ErrorKind::NotFound), // Binance unknown order
            (51603, ErrorKind::NotFound), // OKX order does not exist
            // server
            (-1001, ErrorKind::ServerError), // Binance disconnected/internal
            (10016, ErrorKind::ServerError), // Bybit server error
            // bad request
            (-1013, ErrorKind::InvalidRequest), // Binance filter failure
            (-2010, ErrorKind::InvalidRequest), // Binance new-order rejected
            (10001, ErrorKind::InvalidRequest), // Bybit param error
        ];
        for (code, want) in cases {
            assert_eq!(
                VenueApiError { code, msg: String::new() }.kind(),
                want,
                "venue code {code}"
            );
        }
    }

    /// An unknown code is Unknown by default, but a clear rate-limit phrase in the message is the
    /// last-resort textual fallback (e.g. a CDN block that arrived with an odd code).
    #[test]
    fn unknown_code_uses_msg_text_as_last_resort() {
        assert_eq!(
            VenueApiError { code: 999_999, msg: "some novel error".into() }.kind(),
            ErrorKind::Unknown
        );
        assert_eq!(
            VenueApiError { code: 999_999, msg: "Too Many Requests from your IP".into() }.kind(),
            ErrorKind::RateLimited
        );
        assert_eq!(
            VenueApiError { code: 888_888, msg: "hit the venue RATE LIMIT".into() }.kind(),
            ErrorKind::RateLimited
        );
    }

    /// The override hook wins when it returns `Some`, and falls back to the central default on
    /// `None`. Concrete example: a venue whose CDN uses HTTP 403 as an IP-block remaps 403 →
    /// RateLimited, while every other error keeps the central classification.
    #[test]
    fn override_hook_wins_then_falls_back() {
        // A Bybit-shaped override: 403 (Cloudflare IP block) means rate-limited, not auth.
        let remap_403 = |e: &VenueApiError| (e.code == 403).then_some(ErrorKind::RateLimited);

        let forbidden = VenueApiError { code: 403, msg: "blocked".into() };
        assert_eq!(forbidden.kind(), ErrorKind::Auth, "central default for 403 is Auth");
        assert_eq!(forbidden.kind_with(remap_403), ErrorKind::RateLimited, "override wins");

        // A different error the override doesn't touch falls back to the central default.
        let unknown_order = VenueApiError { code: -2011, msg: "unknown order".into() };
        assert_eq!(
            unknown_order.kind_with(remap_403),
            ErrorKind::NotFound,
            "None → central default"
        );
    }

    /// The retry-vs-abort helpers are a single principled source: transient kinds are retryable,
    /// hard-failure kinds are not, and only the ambiguous timeout is the re-query path.
    #[test]
    fn retry_and_requery_semantics() {
        for k in [ErrorKind::RateLimited, ErrorKind::ServerError, ErrorKind::Network] {
            assert!(k.is_retryable(), "{k} should be retryable");
            assert!(!k.must_requery(), "{k} is not the re-query path");
        }
        for k in
            [ErrorKind::Auth, ErrorKind::InvalidRequest, ErrorKind::NotFound, ErrorKind::Unknown]
        {
            assert!(!k.is_retryable(), "{k} is a terminal failure");
            assert!(!k.must_requery(), "{k} is not the re-query path");
        }
        assert!(ErrorKind::Timeout.must_requery());
        assert!(!ErrorKind::Timeout.is_retryable());
    }

    /// Display strings are stable, snake_case log tokens (used as structured field values).
    #[test]
    fn display_tokens_are_stable() {
        assert_eq!(ErrorKind::RateLimited.to_string(), "rate_limited");
        assert_eq!(ErrorKind::Timeout.to_string(), "timeout");
        assert_eq!(ErrorKind::Network.to_string(), "network");
        assert_eq!(ErrorKind::Unknown.to_string(), "unknown");
    }

    /// The WS-handshake transient predicate the binance/bybit/okx `ws_auth` matchers delegate to
    /// (dedup F5). This is a BEHAVIOR-PRESERVATION pin, not just a unit test: the three matchers it
    /// replaced each decided reconnect-vs-fatal on a live private order stream, so the union here
    /// must answer EXACTLY as their three private 2-code lists did — no code gained, none lost.
    #[test]
    fn ws_ack_transient_reproduces_each_venues_original_list_exactly() {
        // The three lists as they were before the hoist (binance/bybit/okx `ws_auth`).
        let binance_was = |c: i64| matches!(c, -1021 | -1003);
        let bybit_was = |c: i64| matches!(c, 10002 | 10006);
        let okx_was = |c: i64| matches!(c, 50011 | 50102);

        // Every code either venue's REST table knows, plus the absent-code sentinel and an
        // unseeded one — each classified by the shared predicate exactly as its venue used to.
        let binance_codes =
            [-1021, -1003, -1015, -1022, -1000, -1001, -1013, -1016, -1100, -2010, -2011, -2015];
        let bybit_codes = [10002, 10006, 10018, 10001, 10003, 10004, 10005, 10016, 110001];
        let okx_codes = [50011, 50102, 50061, 50001, 50111, 50113, 51000, 51603];

        for c in binance_codes {
            assert_eq!(ws_ack_is_transient(c), binance_was(c), "binance ws ack {c} changed");
        }
        for c in bybit_codes {
            assert_eq!(ws_ack_is_transient(c), bybit_was(c), "bybit ws ack {c} changed");
        }
        for c in okx_codes {
            assert_eq!(ws_ack_is_transient(c), okx_was(c), "okx ws ack {c} changed");
        }
        // An absent code (every matcher's `unwrap_or(0)` on a malformed frame) and an unseeded one
        // must surface, never reconnect-loop.
        assert!(!ws_ack_is_transient(0), "an absent code must not reconnect-loop");
        assert!(!ws_ack_is_transient(999_999), "an unseeded code must not reconnect-loop");
    }

    /// The REST table is deliberately BROADER than the WS handshake set and the two must not be
    /// silently merged: these three REST-only throttle codes are `RateLimited` (retryable at REST)
    /// yet have never been handshake-transient. Folding `classify_venue_code`'s RateLimited bucket
    /// into [`ws_ack_is_transient`] would flip all three from a fatal handshake rejection to a
    /// silent reconnect loop on three LIVE order streams — this pins that it has not happened.
    #[test]
    fn rest_only_throttle_codes_are_not_ws_handshake_transient() {
        for code in [
            -1015, // binance: too many new orders
            10018, // bybit:   exceeded IP rate limit
            50061, // okx:     requests too frequent
        ] {
            assert_eq!(
                VenueApiError { code, msg: String::new() }.kind(),
                ErrorKind::RateLimited,
                "{code} is a REST rate-limit code"
            );
            assert!(
                !ws_ack_is_transient(code),
                "{code} must NOT be handshake-transient (that would be a live behavior change)"
            );
        }
    }
}
