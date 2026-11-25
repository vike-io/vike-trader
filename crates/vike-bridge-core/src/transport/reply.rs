//! **The ONE REST reply engine**: a sent request's answer becomes the venue's JSON or a [`VenueApiError`], one way for
//! every venue that answers a status over HTTP (`classify_rest_reply`). Send, then status, then body, then error: the
//! steps each venue transport used to write out by hand. What differs between venues is DATA, a [`RestReplySpec`] each
//! bridge declares once: where its error body carries a code, which codes say nothing, and the write rules below.
//!
//! The steps, in order (the order is part of the answer: a step that answers first decides the words):
//! 1. a send failure is [`classify_send_error`]'s (code `0` only when it provably preceded the write);
//! 2. a 3xx on a write is unknown where the spec says so ([`RestReplySpec::redirect_on_write_unknown`], owner ruling
//!    B-3);
//! 3. a gateway status ([`status_is_gateway_ambiguous`]) is unknown BEFORE the body is read on the calls
//!    [`RestReplyGatewayFirst`] names;
//! 4. a venue whose error body carries no code, and that declares no typed 5xx verdict
//!    ([`RestReplySpec::verdict_on_5xx`]), decides a write's 5xx before reading it (nothing in it could change the
//!    verdict);
//! 5. the body is read (a failure after the headers is unknown, as [`super::read_body_ambiguous`] answers);
//! 6. a 2xx is the JSON, or "bad json": unknown on a write (the venue answered and we cannot say what), `0` on a read;
//! 7. anything else is the venue's refusal, by its envelope, unless a write's 5xx leaves it unknown
//!    ([`RestReplySpec::server_error_on_write_unknown`], owner ruling B-1) and its body holds none of the typed
//!    verdicts the venue declares ([`RestReplySpec::verdict_on_5xx`]).
//!
//! **No line is logged here.** A venue logs under its own compile-time `tracing` target, which `RUST_LOG` directives
//! filter on, so the engine hands the caller a [`RestReplyNote`] at the two points a transport has always warned at
//! (a send failure, and a non-2xx answer whose body arrived) and the caller writes its own line.
//!
//! **The words are the venue's where they always were.** Each verdict the engine writes itself is a [`RestReplyWord`];
//! [`RestReplyWord::shared`] words it as every venue on the engine always read (`network error: …`, `bad json: …`), and
//! a venue whose errors have always read otherwise states its own ([`RestReplySpec::words`]: polymarket's
//! `polymarket {status}: `, vike-ibkr's `cpapi ` prefix). Only the verdict, never the code, is the engine's.
//!
//! **No body reaches the words, and no error hands back what the request carried.** A word is handed the body only as
//! [`super::body_summary`] (the venue's `{"error": …}` sentence, a declared plain-text venue's one short line, else the
//! body's size), an envelope's message is its one named field, and every error text the engine returns has the
//! request's own credential values redacted ([`super::redact_sent_values`], from the `credentials` its caller hands
//! over), so no venue's words can quote a proxy page that echoes the request's credential headers.

use std::io::Read;

use super::{E_TIMEOUT_AMBIGUOUS, VenueApiError, classify_send_error, gateway_ambiguous};
use vike_transport::send_outcome::{
    server_error_leaves_write_unknown, status_is_gateway_ambiguous,
};

/// Where a venue's error body carries its own code and message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestReplyCode {
    /// The code's field: `"code"` (binance, aster, okx), `"retCode"` (bybit).
    pub field: &'static str,
    /// The message's field, read when the code is there: `"msg"`, `"retMsg"`.
    pub msg: &'static str,
    /// `false`: only a JSON integer is a code, and a body whose field holds anything else is no envelope (the body's
    /// [`super::body_summary`] is the message). `true` (okx, which sends its code as a string): a field that is PRESENT
    /// is the envelope, its value an integer or a decimal string, and a value that does not read, or reads as the HTTP
    /// status itself, is the status (the venue named no code of its own).
    pub string_ok: bool,
}

/// Which calls answer a gateway status ([`status_is_gateway_ambiguous`]) as unknown BEFORE the body is read, whatever
/// the body would say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestReplyGatewayFirst {
    /// None: the body is read, and a write's gateway status goes through the 5xx rule like any other 5xx (binance,
    /// aster: a venue code in a 503 body, `-1007`, is the venue's word).
    Never,
    /// Writes only; a read keeps its status-as-code (vike-ibkr's cpapi). **Owner ruling B-3 (2026-11-11): a read
    /// keeps its status on a gateway answer**, so this is what a venue on `OnEveryCall` moves to (the read is then a
    /// retryable `ServerError`, not the re-query sentinel).
    OnWrites,
    /// Every call, reads included (bybit, okx before they apply B-3).
    OnEveryCall,
}

/// One venue's reply rules. A `const` in the venue's bridge: the venue's facts are these fields, never a branch here.
#[derive(Debug, Clone, Copy)]
pub struct RestReplySpec {
    /// The error body's envelope; `None` for a venue whose error body carries no code (hyperliquid, vike-ibkr's cpapi):
    /// the status is the code and the body's [`super::body_summary`] the message.
    pub code: Option<RestReplyCode>,
    /// Venue codes that on a 5xx say no more than the status does, so a write that carries one is still unknown
    /// (binance `-1000` UNKNOWN and `-1001` DISCONNECTED). A 4xx and a read keep them.
    pub no_verdict_on_5xx: &'static [i64],
    /// See [`RestReplyGatewayFirst`].
    pub gateway_first: RestReplyGatewayFirst,
    /// **Owner ruling B-1 (2026-11-11): a 5xx on a write is "outcome unknown".** `true`: every 5xx whose body carries
    /// no venue code of its own ([`server_error_leaves_write_unknown`]: binance, aster, bybit, okx, hyperliquid).
    /// `false`: only the gateway set, so a bare 500 is read as the venue's refusal although nothing says the venue did
    /// not act (what every venue outside that list did before B-1; none does since).
    pub server_error_on_write_unknown: bool,
    /// **Owner ruling B-3 (2026-11-11): a 3xx on a write is "outcome unknown".** No venue agent follows a redirect
    /// (`vike_transport::http`), so the 3xx is the answer, and it is no verdict on the order. `false` reads it as a refusal
    /// (status-as-code), what every venue but vike-ibkr's cpapi did before B-3.
    pub redirect_on_write_unknown: bool,
    /// The venue's own words for each verdict the engine writes ([`RestReplyWord`]); `None` is
    /// [`RestReplyWord::shared`].
    pub words: Option<fn(&RestReplyWord<'_>) -> String>,
    /// How many bytes of the body a verdict made BEFORE the body is read (a write's 3xx, a gateway status, a status-only
    /// venue's write 5xx) reads for its words' [`super::body_summary`]: `0` none, `u64::MAX` the whole body
    /// (polymarket), vike-ibkr's cpapi 512 (the gateway's own sentence for the warning). The verdict stays unknown
    /// whatever they say.
    pub unknown_body_head: u64,
    /// The venue DECLARES that its own error bodies are plain text, as measured or documented (hyperliquid's
    /// `422 Failed to deserialize …`): a short single-line body that is not JSON is then shown as the venue's words
    /// ([`super::body_summary`]). `false` for every venue whose errors are JSON.
    pub plain_text_errors: bool,
    /// **The typed 5xx bodies that ARE the venue's verdict on a write**: `(field, value)` pairs the venue documents, a
    /// write's 5xx whose JSON body holds the string `value` at `field` being its refusal as step 7 reads it (with no
    /// code of its own the status is the code: `ErrorKind::ServerError`, NOT NOW) instead of unknown. Owner ruling B-1
    /// keeps every other 5xx unknown, and decision 0301 decides on a type, never on words, so a pair names a typed
    /// field, never a sentence: polymarket's `("code", "post_only_mode")`, while its 500 "order timed out" carries no
    /// code and stays unknown. A gateway status [`RestReplyGatewayFirst`] answers first stays unknown. `&[]` for every
    /// other venue.
    pub verdict_on_5xx: &'static [(&'static str, &'static str)],
}

/// A verdict the engine writes itself, before it is words ([`RestReplySpec::words`]). Every `summary` is
/// [`super::body_summary`]'s: the body itself is never handed over.
#[derive(Debug)]
pub enum RestReplyWord<'a> {
    /// Step 1: the request failed to send, or its reply never came (the code is [`classify_send_error`]'s).
    SendFailed(&'a ureq::Error),
    /// Step 2: a write answered a 3xx; `summary` is of the body's first [`RestReplySpec::unknown_body_head`] bytes.
    Redirect { status: u16, summary: &'a str },
    /// Steps 3, 4 and 7: a gateway status, or a write's 5xx the spec leaves unknown. `summary` is of the head (steps 3
    /// and 4, before the body is read) or of the whole body (step 7).
    Gateway { status: u16, summary: &'a str },
    /// Step 5: the body failed to read after the headers arrived.
    BodyUnread(&'a ureq::Error),
    /// Step 6: a 2xx whose body is not JSON.
    BadJson { status: u16, error: &'a serde_json::Error, summary: &'a str },
    /// Step 7 for a venue whose error body carries no code ([`RestReplySpec::code`] `None`): the body's summary, and
    /// why it is not JSON (`None` when it is).
    Refused { status: u16, summary: &'a str, not_json: Option<&'a serde_json::Error> },
}

impl RestReplyWord<'_> {
    /// The words every venue on the engine has always read.
    pub fn shared(&self) -> String {
        match self {
            Self::SendFailed(e) | Self::BodyUnread(e) => {
                format!("network error: {}", super::send_error_text(e))
            }
            Self::Redirect { status, .. } => {
                format!("network error: redirect http {status}, outcome unknown")
            }
            Self::Gateway { status, .. } => gateway_ambiguous(*status).msg,
            Self::BadJson { error, .. } => format!("bad json: {error}"),
            Self::Refused { summary, .. } => (*summary).to_string(),
        }
    }
}

/// What a transport has always logged, handed to the caller so the line keeps the caller's own target.
#[derive(Debug)]
pub enum RestReplyNote<'a> {
    /// The request failed to send (or its reply never came): step 1.
    SendFailed(&'a ureq::Error),
    /// A non-2xx answer whose body arrived: step 7, before the 5xx rule. `body` is the parsed JSON, `None` when it is
    /// none.
    Refused { status: u16, body: Option<&'a serde_json::Value> },
}

/// Classify one sent request's reply by `spec` (the module doc lists the steps). `write`: the call may change the
/// account (an order, a cancel, a leverage set), so a reply that does not say what happened is "outcome unknown"
/// ([`E_TIMEOUT_AMBIGUOUS`]) rather than a refusal; a read has no outcome to be unsure of. `credentials`: the values the
/// request carried that a page could echo (its API key, passphrase, signature, bearer), redacted from every error text
/// ([`super::redact_sent_values`]); `&[]` for a request that carries none. `note` is called at most once.
///
/// # Errors
/// [`classify_send_error`]'s on a send failure; [`E_TIMEOUT_AMBIGUOUS`] where the venue may have acted; else the
/// venue's refusal: its own `{code, msg}`, or the status-as-code.
pub fn classify_rest_reply(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    write: bool,
    spec: &RestReplySpec,
    credentials: &[&str],
    note: impl FnOnce(RestReplyNote<'_>),
) -> Result<serde_json::Value, VenueApiError> {
    classify(sent, write, spec, note)
        .map_err(|e| VenueApiError { msg: super::redact_sent_values(&e.msg, credentials), ..e })
}

/// [`classify_rest_reply`] before its redaction.
fn classify(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    write: bool,
    spec: &RestReplySpec,
    note: impl FnOnce(RestReplyNote<'_>),
) -> Result<serde_json::Value, VenueApiError> {
    let unknown = |msg: String| VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg };
    let mut response = match sent {
        Ok(response) => response,
        Err(e) => {
            note(RestReplyNote::SendFailed(&e));
            let code = classify_send_error(&e).code;
            return Err(VenueApiError { code, msg: spec.say(&RestReplyWord::SendFailed(&e)) });
        }
    };
    let status = response.status().as_u16();
    if write && spec.redirect_on_write_unknown && (300..400).contains(&status) {
        let summary = spec.head_summary(&mut response);
        return Err(unknown(spec.say(&RestReplyWord::Redirect { status, summary: &summary })));
    }
    let gateway_first = match spec.gateway_first {
        RestReplyGatewayFirst::Never => false,
        RestReplyGatewayFirst::OnWrites => write,
        RestReplyGatewayFirst::OnEveryCall => true,
    };
    if (gateway_first && status_is_gateway_ambiguous(status))
        || (write
            && spec.code.is_none()
            && spec.verdict_on_5xx.is_empty()
            && spec.write_unknown(status, None))
    {
        let summary = spec.head_summary(&mut response);
        return Err(unknown(spec.say(&RestReplyWord::Gateway { status, summary: &summary })));
    }
    // The headers arrived, so the venue received the request: a body that then fails to read is unknown, never a
    // definite "not sent".
    let text = vike_transport::http::read_body_text(response.body_mut())
        .map_err(|e| unknown(spec.say(&RestReplyWord::BodyUnread(&e))))?;
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&text);
    if (200..300).contains(&status) {
        // A 2xx on a write means the venue took the request: an unreadable body leaves the outcome unknown (`code: 0`
        // would license a terminal reject of an order that may be live).
        return parsed.map_err(|error| VenueApiError {
            code: if write { E_TIMEOUT_AMBIGUOUS } else { 0 },
            msg: spec.say(&RestReplyWord::BadJson {
                status,
                error: &error,
                summary: &super::body_summary(&text, spec.plain_text_errors),
            }),
        });
    }
    note(RestReplyNote::Refused { status, body: parsed.as_ref().ok() });
    let typed = parsed.as_ref().is_ok_and(|body| spec.is_typed_verdict(body));
    let summary = super::body_summary(&text, spec.plain_text_errors);
    let (refused, venue_code) = spec.refusal(status, &summary, parsed);
    if write && !typed && spec.write_unknown(status, venue_code) {
        return Err(unknown(spec.say(&RestReplyWord::Gateway { status, summary: &summary })));
    }
    Err(refused)
}

impl RestReplySpec {
    /// `word` in this venue's words.
    fn say(&self, word: &RestReplyWord<'_>) -> String {
        self.words.map_or_else(|| word.shared(), |words| words(word))
    }

    /// [`super::body_summary`] of the body's first [`Self::unknown_body_head`] bytes, for a verdict made before the body
    /// is read; a read that fails keeps what came (the words are for a warning only: the verdict is already unknown).
    fn head_summary(&self, response: &mut ureq::http::Response<ureq::Body>) -> String {
        if self.unknown_body_head == 0 {
            return String::new();
        }
        let mut head = Vec::new();
        let _ = response.body_mut().as_reader().take(self.unknown_body_head).read_to_end(&mut head);
        super::body_summary(&String::from_utf8_lossy(&head), self.plain_text_errors)
    }

    /// Does `body` hold one of the typed verdicts this venue declares ([`Self::verdict_on_5xx`])?
    fn is_typed_verdict(&self, body: &serde_json::Value) -> bool {
        self.verdict_on_5xx.iter().any(|(field, value)| {
            body.get(field).and_then(serde_json::Value::as_str) == Some(*value)
        })
    }

    /// Does this 5xx (or gateway) answer leave a write unknown? `venue_code` is the body's own non-zero code, `None`
    /// when it carries none.
    fn write_unknown(&self, status: u16, venue_code: Option<i64>) -> bool {
        if self.server_error_on_write_unknown {
            server_error_leaves_write_unknown(status, venue_code)
        } else {
            status_is_gateway_ambiguous(status) && venue_code.is_none()
        }
    }

    /// A non-2xx answer as the venue's refusal, and the venue's own code in it for the 5xx rule (`None`: none, `0`, a
    /// [`RestReplySpec::no_verdict_on_5xx`] code on a 5xx, or a `string_ok` value that is the status itself). A body
    /// code `0` is no code: the refusal's code is the status, never the never-sent `0`. `summary` is the body's
    /// [`super::body_summary`], the message of a body that carries no envelope.
    fn refusal(
        &self,
        status: u16,
        summary: &str,
        parsed: Result<serde_json::Value, serde_json::Error>,
    ) -> (VenueApiError, Option<i64>) {
        let fallback = i64::from(status);
        let Some(envelope) = self.code else {
            let word = RestReplyWord::Refused { status, summary, not_json: parsed.as_ref().err() };
            return (VenueApiError { code: fallback, msg: self.say(&word) }, None);
        };
        let Ok(body) = parsed else {
            return (VenueApiError { code: fallback, msg: "http error".to_string() }, None);
        };
        let raw = body.get(envelope.field);
        let read = match raw {
            Some(serde_json::Value::Number(n)) => n.as_i64(),
            Some(serde_json::Value::String(s)) if envelope.string_ok => s.parse::<i64>().ok(),
            _ => None,
        };
        let present = if envelope.string_ok { raw.is_some() } else { read.is_some() };
        if !present {
            return (VenueApiError { code: fallback, msg: summary.to_string() }, None);
        }
        let msg = body.get(envelope.msg).and_then(|m| m.as_str()).unwrap_or("").to_string();
        let server_error = (500..600).contains(&status);
        // A body code `0` is no code (owner ruling T-D7): `0` is the workspace's never-sent sentinel, and a venue
        // answered here, so the status is the code.
        let read = read.filter(|c| *c != 0);
        let venue_code = read
            .filter(|c| !(envelope.string_ok && *c == fallback))
            .filter(|c| !(server_error && self.no_verdict_on_5xx.contains(c)));
        (VenueApiError { code: read.unwrap_or(fallback), msg }, venue_code)
    }
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod reply_tests;
