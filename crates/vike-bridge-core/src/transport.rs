//! Signed blocking REST transport (e.g. Binance's shape: signed query in the URL for every
//! method, no body): a `{"code":-XXXX,"msg":"..."}` non-2xx body becomes a typed
//! [`VenueApiError`]; non-JSON error bodies (WAF/HTML) become a generic (http-status, reason)
//! error; a send failure provably before the write maps to code 0, every other one to `E_TIMEOUT_AMBIGUOUS`, as do a 5xx and a 2xx with an unreadable body on a write. Also the venue-neutral error taxonomy ([`ErrorKind`]).
//!
//! Those rules are [`reply`]'s, the ONE reply engine (`classify_rest_reply`): [`UreqTransport`] is one of its users, and a
//! venue with another envelope or other write rules states them as a `RestReplySpec` instead of writing its own `run`.
//!
//! The pure facts under them (did a send failure precede the write, which statuses leave a write unknown) are the
//! generic network layer's, `vike_transport::send_outcome`; this module words them as the venue error.

use crate::ratelimit::RateGate;
use crate::signer::{Signer, urlencode};
use vike_transport::send_outcome::send_error_is_definite;

pub mod auth;
pub mod http_error;
pub mod json;
pub mod reply;

/// The global timeout of the order-status RE-QUERY's agent (`signed_requery`), instead of the shared 30 s, so a double
/// timeout (the submit, then its re-query) cannot stall the exec thread toward a minute.
pub const REQUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A venue order/account error, normalized to (code, msg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueApiError {
    pub code: i64,
    pub msg: String,
}

/// Ambiguous transport failure: a timeout or other post-write failure, AFTER the request may have reached the venue,
/// so the venue MAY have accepted the order. Distinct from `code: 0` (a definite pre-send failure,
/// DNS/connect/TLS — where the order never reached the venue). A submit that sees this MUST re-query
/// order status, never emit a terminal `OrderRejected` (which would leave a phantom position).
pub const E_TIMEOUT_AMBIGUOUS: i64 = i64::MIN;

/// The codes a bridge mints for a refusal of its OWN, one no venue answered (binance's `E_NO_ALGO_SERVICE`,
/// `E_NO_QUANTO_BOOK` and `E_NO_COIN_M_ROW`, bybit's and okx's `E_NO_INSTRUMENT_ROW`, okx's `E_UNCLASSED_INSTRUMENT`,
/// polymarket's `E_CLOB_AUTH_UNSIGNED`): [`ErrorKind::Unknown`], a refusal, never [`ErrorKind::UnmappedCode`]. No
/// code a venue table maps lies in this range (binance's are `-1000` to `-5999`, bybit's and okx's positive,
/// deribit's positive or JSON-RPC `-32xxx`).
pub const BRIDGE_LOCAL_CODES: std::ops::RangeInclusive<i64> = -9_999..=-9_000;

/// Is `code` a venue's own business code: not a transport sentinel (`0`, [`E_TIMEOUT_AMBIGUOUS`]), not an HTTP
/// status carried as the code (`100..600`) and not one of [`BRIDGE_LOCAL_CODES`]? Only such a code can be
/// [`ErrorKind::UnmappedCode`].
pub(crate) fn is_venue_code(code: i64) -> bool {
    code != 0
        && code != E_TIMEOUT_AMBIGUOUS
        && !(100..600).contains(&code)
        && !BRIDGE_LOCAL_CODES.contains(&code)
}

impl std::fmt::Display for VenueApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "venue error {}: {}", self.code, self.msg)
    }
}

impl std::error::Error for VenueApiError {}

impl VenueApiError {
    /// The catalog id an order event built from this error carries, by its CODE: a definite pre-send failure
    /// (`code` 0) is NOT SENT, an ambiguous one ([`E_TIMEOUT_AMBIGUOUS`]) OUTCOME UNKNOWN, and any other code is the
    /// venue's own refusal (its codes are mapped per venue later). Show-only.
    #[must_use]
    pub fn refusal(&self) -> vike_model::OrderRefusalId {
        match self.code {
            0 => vike_model::OrderRefusalId::NotSent,
            E_TIMEOUT_AMBIGUOUS => vike_model::OrderRefusalId::OutcomeUnknown,
            _ => vike_model::OrderRefusalId::VenueRefused,
        }
    }

    /// [`Self::refusal`]'s word, the form the event's field takes (`vike_model::events::refusal_id_of`).
    #[must_use]
    pub fn refusal_id_word(&self) -> &'static str {
        self.refusal().word()
    }

    /// This WRITE's error with its outcome named by the venue's own rule: a code `outcome_unknown` answers `true` for
    /// (the bridge's `error_codes::TAXONOMY`'s [`crate::error_kind::VenueTaxonomy::submit_outcome_unknown`]) becomes
    /// [`E_TIMEOUT_AMBIGUOUS`], its words kept, so every answer built from it says OUTCOME UNKNOWN
    /// (`crate::rest::cancel_rejected`, `crate::rest::modify_rejected`); any other error is itself.
    #[must_use]
    pub fn outcome_unknown_when(self, outcome_unknown: impl FnOnce(i64) -> bool) -> Self {
        if outcome_unknown(self.code) {
            VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: self.msg }
        } else {
            self
        }
    }

    /// This error's venue-NEUTRAL [`ErrorKind`] ([`ErrorKind::classify`]): the sentinels, the HTTP status and the
    /// rate-limit words. It knows no venue's codes; a venue's error is classified under its own table,
    /// [`crate::error_kind::classify_venue`].
    pub fn kind(&self) -> ErrorKind {
        ErrorKind::classify(self.code, &self.msg)
    }

    /// Per-venue override hook: `f` is consulted FIRST and, when it returns `Some(kind)`, wins;
    /// `None` falls back to the central [`kind`](Self::kind) default. This is how a venue keeps its
    /// own nuance (e.g. remapping a shared HTTP 403 to [`ErrorKind::RateLimited`] for a CDN IP-block,
    /// as Bybit fronts REST with Cloudflare) WITHOUT forking the baseline classification, which stays
    /// central.
    pub fn kind_with(&self, f: impl FnOnce(&VenueApiError) -> Option<ErrorKind>) -> ErrorKind {
        f(self).unwrap_or_else(|| self.kind())
    }
}

/// A coarse, venue-neutral classification of a transport-boundary failure. Collapses the raw venue
/// `{code, msg}` / HTTP status behind [`VenueApiError`] into a small set so retry-vs-abort
/// decisions have ONE source. The venue-neutral mapping ([`ErrorKind::classify`]) lives here; a venue's
/// own codes are its table's ([`crate::error_kind::VenueTaxonomy`]).
///
/// `code == 0` → [`Network`](Self::Network) (a definite pre-send failure), [`E_TIMEOUT_AMBIGUOUS`]
/// → [`Timeout`](Self::Timeout) (the "must re-query, never reject" path, its OWN kind so it can't
/// regress into a blind retry/reject), an HTTP status carried as the code → its HTTP meaning; a venue's code
/// is read by its venue's table, and one that table does not map is [`UnmappedCode`](Self::UnmappedCode).
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
    /// (a phantom position). A distinct kind so this path can't be collapsed into a blind retry.
    Timeout,
    /// The order was refused for lack of margin / wallet balance (a venue "insufficient balance"
    /// code). Terminal for THIS request and semantically distinct from [`InvalidRequest`](Self::InvalidRequest):
    /// the request was well-formed, the ACCOUNT could not back it — a sizing/risk signal, not a bug.
    ///
    /// Produced only by the per-venue tables ([`crate::error_kind`]); [`ErrorKind::classify`] never returns it.
    InsufficientFunds,
    /// The venue is in a scheduled maintenance / upgrade window (OKX 50001/50004, Bybit 10016
    /// "service is restarting", Binance -1016). Retryable, but on a LONG backoff — unlike
    /// [`ServerError`](Self::ServerError) this is expected to last minutes, not milliseconds.
    ///
    /// Produced only by the per-venue tables, as [`InsufficientFunds`](Self::InsufficientFunds) is.
    VenueMaintenance,
    /// A VENUE's own error code that its table does not map (`crate::error_kind::classify_venue`; never
    /// [`ErrorKind::classify`], which reads no venue's codes). The error keeps the venue's code and words. What
    /// the venue said about the request is not known, so it is never final: OUTCOME UNKNOWN (never resent,
    /// never called refused, resolved by a read), and the first sighting of each code is logged at WARN so an
    /// operator sees it and adds the row. Not a read failure held only as TEXT
    /// (`vike_execution::recon::ReconcileFetchError::unclassified`), which carries no code and stays refused.
    UnmappedCode,
    /// No rule matched and no venue's code is involved: a non-error HTTP status, a code a bridge minted for a
    /// refusal of its own ([`BRIDGE_LOCAL_CODES`]), a bridge's own failure it chose to park on. Terminal: the
    /// "default Fatal" posture for a failure this crate DID classify, never retried
    /// (see [`is_terminal`](Self::is_terminal)). A venue's unmapped code is [`UnmappedCode`](Self::UnmappedCode).
    Unknown,
}

impl ErrorKind {
    /// The venue-NEUTRAL classification of a `{code, msg}` pair. `code` carries the [`VenueApiError`]
    /// overload, decoded in order: the two sentinels ([`E_TIMEOUT_AMBIGUOUS`] and `0`) FIRST, then the
    /// HTTP-status-as-code range (`100..600`, produced by the transport when an error body has no venue
    /// `{code}`); `msg` is a last-resort textual signal for a rate limit. Every other code is
    /// [`Unknown`](Self::Unknown) here: a venue's codes are its own table's
    /// (`crate::error_kind::classify_venue`), since one flat table across venues reads one venue's code as
    /// another's (deribit's `10004` `order_not_found` was Bybit's "auth").
    pub fn classify(code: i64, msg: &str) -> ErrorKind {
        // FIRST: the ambiguous-timeout sentinel is its OWN kind and nothing may shadow it.
        if code == E_TIMEOUT_AMBIGUOUS {
            return ErrorKind::Timeout;
        }
        // Definite pre-send failure (DNS/connect/TLS): the request never reached the venue.
        if code == 0 {
            return ErrorKind::Network;
        }
        // HTTP-status-as-code: the transport stores `i64::from(status)` when an error body carries no
        // venue `{code}`. Venue business codes never fall in this range (Binance is negative;
        // Bybit/OKX/Deribit are ≥ 10000), so for transport-produced errors the split is unambiguous.
        if (100..600).contains(&code) {
            return ErrorKind::from_http_status(code as u16);
        }
        let m = msg.to_ascii_lowercase();
        if m.contains("too many request") || m.contains("rate limit") {
            ErrorKind::RateLimited
        } else {
            ErrorKind::Unknown
        }
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

    /// This kind in the four words a caller decides on: the ambiguous [`Timeout`](Self::Timeout) and an
    /// [`UnmappedCode`](Self::UnmappedCode) venue code are `OutcomeUnknown`, a definite pre-send
    /// [`Network`](Self::Network) failure `NotSent`, the other retryable kinds `NotNow`, every terminal kind
    /// `Refused` (exhaustive, so a new kind must choose). The trio below reads it.
    pub fn failure_outcome(self) -> vike_utility::errors::FailureOutcome {
        use vike_utility::errors::FailureOutcome;
        match self {
            ErrorKind::Timeout | ErrorKind::UnmappedCode => FailureOutcome::OutcomeUnknown,
            ErrorKind::Network => FailureOutcome::NotSent,
            ErrorKind::RateLimited | ErrorKind::ServerError | ErrorKind::VenueMaintenance => {
                FailureOutcome::NotNow
            }
            ErrorKind::Auth
            | ErrorKind::InvalidRequest
            | ErrorKind::NotFound
            | ErrorKind::InsufficientFunds
            | ErrorKind::Unknown => FailureOutcome::Refused,
        }
    }

    /// The must-re-query signal ([`failure_outcome`](Self::failure_outcome) `OutcomeUnknown`): the venue MAY
    /// have accepted the request, so the caller MUST re-query status and NEVER synthesize a terminal reject.
    /// [`Timeout`](Self::Timeout) and [`UnmappedCode`](Self::UnmappedCode) — deliberately NOT
    /// [`Network`](Self::Network), which is a definite pre-send failure (nothing landed) and so is safe to reject.
    pub fn must_requery(self) -> bool {
        self.failure_outcome() == vike_utility::errors::FailureOutcome::OutcomeUnknown
    }

    /// Whether the REQUEST itself is safe to retry as-is ([`failure_outcome`](Self::failure_outcome) `NotSent`
    /// or `NotNow`): rate-limit, venue-server error, maintenance and definite pre-send network failures. An
    /// ambiguous [`Timeout`](Self::Timeout) is NOT blind-retryable (it must [`must_requery`](Self::must_requery)
    /// instead, to avoid a double-submit).
    pub fn is_retryable(self) -> bool {
        use vike_utility::errors::FailureOutcome;
        matches!(self.failure_outcome(), FailureOutcome::NotSent | FailureOutcome::NotNow)
    }

    /// `true` when this kind means "stop — re-issuing as-is cannot help" ([`failure_outcome`](Self::failure_outcome)
    /// `Refused`). Terminal kinds are the ones an order submit may answer with a synthesized `OrderRejected`; the
    /// rest either retry ([`is_retryable`](Self::is_retryable)) or re-query ([`must_requery`](Self::must_requery)).
    ///
    /// [`Unknown`](Self::Unknown) is terminal BY DESIGN (a failure this crate classified and parks on);
    /// [`UnmappedCode`](Self::UnmappedCode), a venue code no table maps, is not.
    pub fn is_terminal(self) -> bool {
        self.failure_outcome() == vike_utility::errors::FailureOutcome::Refused
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
            ErrorKind::UnmappedCode => "unmapped_code",
            ErrorKind::Unknown => "unknown",
        })
    }
}

/// A `ureq` SEND failure → [`VenueApiError`]. **The ONE site for that rule** (shared by
/// [`UreqTransport::run`] and the venues' own transports; a wrong answer either strands a phantom
/// position or double-submits an order): only a failure PROVABLY before the request was written
/// ([`send_error_is_definite`]: DNS, connect refused/failed, proxy, bad URI, TLS policy; the rule is the network
/// layer's, this is its venue wording) carries
/// `code: 0` and lets a submit synthesize a terminal reject. EVERYTHING else (a timeout, an `Io`
/// reset or `UnexpectedEof` after the POST body went out, `Protocol`, `LargeResponseHeader`,
/// `RedirectFailed`, any future variant) carries the ambiguous [`E_TIMEOUT_AMBIGUOUS`] sentinel
/// and the caller MUST re-query, never reject.
///
/// The CALLER keeps its own `tracing::warn!` — each venue logs under its own target/venue field,
/// and those differ deliberately (`RUST_LOG` directives filter on them).
pub fn classify_send_error(e: &ureq::Error) -> VenueApiError {
    VenueApiError {
        code: if send_error_is_definite(e) { 0 } else { E_TIMEOUT_AMBIGUOUS },
        msg: format!("network error: {}", send_error_text(e)),
    }
}

/// ureq's own words for a send failure, except the two that quote the request's whole URL (`BadUri`,
/// `RequireHttpsOnly`): a query string can carry a key, a signature or an OAuth client secret, so those name only
/// what is wrong with it.
pub fn send_error_text(e: &ureq::Error) -> String {
    match e {
        ureq::Error::BadUri(_) => "bad uri (not shown)".to_string(),
        ureq::Error::RequireHttpsOnly(_) => "configured for https only (uri not shown)".to_string(),
        other => other.to_string(),
    }
}

/// The ambiguous error for a gateway-style status ([`vike_transport::send_outcome::status_is_gateway_ambiguous`]): the response
/// HEADERS arrived but the venue's verdict did not, so the caller re-queries and never rejects.
pub fn gateway_ambiguous(status: u16) -> VenueApiError {
    VenueApiError {
        code: E_TIMEOUT_AMBIGUOUS,
        msg: format!("network error: gateway http {status}, outcome unknown"),
    }
}

/// The longest venue sentence an error's text shows out of a body ([`body_summary`]).
const SHOWN_BODY_CHARS: usize = 160;

/// **What of a response body may enter an error's text: the ONE rule, first half** (the reply engine
/// `reply::classify_rest_reply`, `http_error::field_or_text` and every venue word built on them). A person must read
/// the venue's own refusal, and a proxy or CDN page in front of the venue can echo the request, credential headers
/// included (polymarket's `POLY_API_KEY` and `POLY_PASSPHRASE`, alpaca's bearer), while an error's text reaches the
/// log, the journal and an `OrderRejected` reason. What may enter:
/// - a JSON body: its string `error` member, trimmed, at most 160 characters;
/// - a body that is not JSON, from a venue whose spec DECLARES that its own errors are plain text (`plain_text`;
///   hyperliquid's `422 Failed to deserialize …`, measured): the body itself when it is one line of at most 160
///   characters with no `<` (no page);
/// - anything else (HTML, several lines, longer, another venue's plain text): `"<n>-byte body not shown"`.
///
/// The second half is [`redact_sent_values`]: whatever is shown never hands back what the request carried. A
/// venue's own envelope field (`reply::RestReplyCode::msg`, `field_or_text`'s field) is read by its caller first.
pub fn body_summary(text: &str, plain_text: bool) -> String {
    let sentence = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(body) => body
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(|sentence| sentence.trim().chars().take(SHOWN_BODY_CHARS).collect()),
        Err(_) if plain_text => {
            let line = text.trim();
            (line.chars().count() <= SHOWN_BODY_CHARS && !line.contains(['\n', '\r', '<']))
                .then(|| line.to_string())
        }
        Err(_) => None,
    };
    sentence
        .filter(|sentence: &String| !sentence.is_empty())
        .unwrap_or_else(|| format!("{}-byte body not shown", text.len()))
}

/// A value the request carried at least this long is redacted wherever it appears; a shorter one (an okx passphrase,
/// an IG login, bybit's receive window) only where it stands as a whole word ([`redact_sent_values`]).
const REDACTED_ANYWHERE_MIN_CHARS: usize = 8;

/// **The ONE rule, second half**: `text` with every value the request itself carried and the venue could echo (its
/// API key, passphrase, signature, bearer or session token, login: `credentials`, which the transport that signed the
/// request hands over) replaced by `[redacted]`. The reply engine and `http_error::VenueApiHttpSpec::finish` apply it
/// to every error text they return, so even a declared plain-text body ([`body_summary`]) or a venue's own message
/// field that echoes the request cannot hand a credential back.
///
/// A value of [`REDACTED_ANYWHERE_MIN_CHARS`] or more is replaced wherever it appears. A shorter one is replaced only
/// where no letter or digit touches it on either side: a short passphrase `pass` is gone from `passphrase: pass` and
/// `pass=pass&`, while `passed` and `bypass` keep their letters. Longer values go first, so a short value that is a
/// word of a longer one cannot cut the longer one open. An empty value redacts nothing.
pub fn redact_sent_values(text: &str, credentials: &[&str]) -> String {
    let mut values: Vec<&str> = credentials.iter().copied().filter(|v| !v.is_empty()).collect();
    values.sort_by_key(|value| std::cmp::Reverse(value.chars().count()));
    values.into_iter().fold(text.to_string(), |text, value| {
        if value.chars().count() >= REDACTED_ANYWHERE_MIN_CHARS {
            text.replace(value, "[redacted]")
        } else {
            redact_whole_words(&text, value)
        }
    })
}

/// `text` with every occurrence of `value` that no letter or digit touches replaced by `[redacted]`.
fn redact_whole_words(text: &str, value: &str) -> String {
    let in_a_word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(value) {
        out.push_str(&rest[..at]);
        let after = &rest[at + value.len()..];
        if in_a_word(out.chars().next_back()) || in_a_word(after.chars().next()) {
            // Part of a longer word: keep its first character and look again after it.
            let first = rest[at..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[at..at + first]);
            rest = &rest[at + first..];
        } else {
            out.push_str("[redacted]");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Read a response body to text; a read failure is ALWAYS the ambiguous [`E_TIMEOUT_AMBIGUOUS`]
/// — the response HEADERS already arrived, so the venue demonstrably RECEIVED the request and may
/// have acted on it. Never `code: 0` here: that would license a terminal reject for an order that
/// might be live. The read-half twin of [`classify_send_error`].
pub fn read_body_ambiguous(
    response: &mut ureq::http::Response<ureq::Body>,
) -> Result<String, VenueApiError> {
    vike_transport::http::read_body_text(response.body_mut()).map_err(|e| VenueApiError {
        code: E_TIMEOUT_AMBIGUOUS,
        msg: format!("network error: {e}"),
    })
}

/// Is `code` a venue business code that a private-WS handshake ack should treat as **TRANSIENT**
/// (close + reconnect with backoff) rather than a genuine auth failure (surface and stop)?
///
/// ONE list, delegated to by the binance/bybit/okx `websocket_auth` ack matchers. Per-venue codes are
/// disjoint (Binance is negative; Bybit/OKX are ≥ 10000), so one flat union is EXACT per venue —
/// each only ever sees its own:
/// - Binance `-1021` (timestamp outside recvWindow), `-1003` (too many requests)
/// - Bybit `10002` (request timestamp outside the recv window), `10006` (too many visits)
/// - OKX `50011` (rate limit), `50102` (timestamp expired)
///
/// Deliberately NOT derived from the venue tables' [`RateLimited`](ErrorKind::RateLimited) rows
/// (`crate::error_kind::VenueTaxonomy`), though it overlaps them: the WS handshake has its own code vocabulary, and
/// those rows also carry REST-only throttle codes (Binance `-1015` too-many-orders, Bybit `10018`, OKX
/// `50061`) which are not handshake-transient. Folding those in would turn a fatal handshake
/// rejection into a silent reconnect loop on three LIVE order streams.
///
/// An absent (`0` — every matcher's `unwrap_or(0)` on a malformed frame) or unknown code is NOT
/// transient, so a genuinely broken session still surfaces instead of reconnect-looping forever.
pub fn websocket_ack_is_transient(code: i64) -> bool {
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

    /// Signed request for the order-status **re-query** on an ambiguous submit timeout.
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
    /// Short-timeout agent for the order-status re-query only, so a double-timeout can't stall the
    /// core toward ~60s. Built once; the re-query is off the hot path.
    requery_agent: ureq::Agent,
    /// The venue this transport instance is wired to (e.g. "binance"), stamped onto `run`'s
    /// diagnostic logs: more than one venue constructs this generic-shaped transport, and
    /// `tracing`'s `target:` must be a compile-time constant per callsite, so the venue rides a
    /// structured `venue` field instead.
    venue: &'static str,
    /// Optional REST rate gate. Consulted on the NORMAL request paths (`signed`/`public`) only —
    /// `signed_requery` is deliberately exempt so a throttle can never reintroduce the ~60s stall
    /// its short-timeout agent exists to bound.
    rate_gate: Option<RateGate>,
}

impl UreqTransport {
    pub fn new(venue: &'static str) -> Self {
        UreqTransport {
            agent: vike_transport::http::blocking_agent(),
            requery_agent: vike_transport::http::blocking_agent_with_timeout(REQUERY_TIMEOUT),
            venue,
            rate_gate: None,
        }
    }

    /// Like [`new`](Self::new), but wired to a CALLER-BUILT `ureq::Agent` for BOTH the normal and
    /// the re-query lanes. [`new`](Self::new) builds a PLAIN agent with no proxy, which is wrong for
    /// a geo-blocked venue (Polymarket) whose every other lane routes through a SOCKS tunnel:
    /// passing that proxied agent here lets a REST warmup (e.g. the market feed's tick-size lookup)
    /// share the egress instead of dialing direct into the geo-block. One agent serves both fields
    /// because the two-agent split only bounds the signed re-query's timeout, a path a proxied
    /// public-GET-only caller never walks.
    pub fn with_agent(venue: &'static str, agent: ureq::Agent) -> Self {
        UreqTransport { agent: agent.clone(), requery_agent: agent, venue, rate_gate: None }
    }

    /// Attach a REST rate gate. Every `signed`/`public` call then rides it (blocking on throttle,
    /// with a single warn) with zero call-site changes; the re-query stays exempt. `Clone` the
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

    /// `path` is only for the diagnostics: `url` carries the query, which holds the signature.
    fn run(
        &self,
        agent: &ureq::Agent,
        url: &str,
        path: &str,
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
        let write = matches!(method, "POST" | "PUT" | "DELETE");
        // What the request carried: the signer's headers (the API key) and the query's `signature`.
        let signature = url.split_once("signature=").and_then(|(_, s)| s.split('&').next());
        let credentials: Vec<&str> =
            headers.iter().map(|(_, v)| v.as_str()).chain(signature).collect();
        reply::classify_rest_reply(sent, write, &UREQ_REPLY, &credentials, |note| match note {
            reply::RestReplyNote::SendFailed(e) => {
                tracing::warn!(venue = self.venue, method, path, error = %e, "REST request failed");
            }
            // The line names the call and the venue's own verdict (a caller may swallow a 400 as an
            // answer, e.g. Binance -2013 on an unknown order).
            reply::RestReplyNote::Refused { status, body } => tracing::warn!(
                venue = self.venue, status, method, path,
                code = body.and_then(|b| b.get("code")?.as_i64()),
                msg = %redact_sent_values(
                    &body.and_then(|b| b.get("msg")?.as_str()).unwrap_or("").chars().take(200).collect::<String>(),
                    &credentials,
                ),
                "REST request failed"
            ),
        })
    }
}

/// [`UreqTransport`]'s reply rules: the venue's `{code, msg}` body (a JSON integer code). ANY 5xx on a
/// MUTATING call says nothing about the venue's verdict (Binance: "execution status UNKNOWN and could
/// have been a success"; the gateway statuses are the proxy's words), unless the body carries the
/// venue's own non-zero code (e.g. binance -1007 on a 503). On a 5xx Binance's `-1000` UNKNOWN and
/// `-1001` DISCONNECTED say no more than the status does, so they are no venue code there (a 4xx
/// keeps them). A 3xx on a mutating call is unknown too (owner ruling B-3): the agent follows no
/// redirect, so the 3xx is the answer and no verdict. A read (GET / public / re-query) has no
/// outcome to be unsure of: it keeps the status-as-code.
const UREQ_REPLY: reply::RestReplySpec = reply::RestReplySpec {
    code: Some(reply::RestReplyCode { field: "code", msg: "msg", string_ok: false }),
    no_verdict_on_5xx: &[-1000, -1001],
    gateway_first: reply::RestReplyGatewayFirst::Never,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
    words: None,
    unknown_body_head: 0,
    plain_text_errors: false,
    verdict_on_5xx: &[],
};

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
        self.run(&self.agent, &url, path, method, &prepared.headers)
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
        self.run(&self.agent, &url, path, "GET", &[])
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        // Same request as `signed`, but on the short-timeout agent (the stall bound).
        // DELIBERATELY NOT rate-gated: this rare re-query exists to BOUND latency after a
        // timeout, so a blocking throttle here would defeat its purpose. The normal paths carry
        // the venue budget; exempting the re-query costs ~one extra call in a throttled window.
        let prepared = signer.prepare(params, method, path);
        let url = format!("{base_url}{path}?{}", prepared.query);
        self.run(&self.requery_agent, &url, path, method, &prepared.headers)
    }
}

#[cfg(test)]
mod rate_gate_wiring;

#[cfg(test)]
mod transport_error_kind_tests;

#[cfg(test)]
mod redaction_tests;
