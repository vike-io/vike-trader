//! **The ONE HTTP API error** of the venues that answer a bearer-token call with a status and a body of their own words
//! (alpaca, oanda, ig): [`VenueApiHttpError`] `{status, message}`, beside the signed venues'
//! [`VenueApiError`](super::VenueApiError) `{code, msg}`. A `status` below 100 is no HTTP status but a verdict on the request:
//! - `0`: it provably never left this host ([`send_error_is_definite`]), so a submit may reject;
//! - `1` ([`VenueApiHttpError::STATUS_OUTCOME_UNKNOWN`]): the venue may have acted (a failure after the write, a reply
//!   whose body never came), so a submit re-queries and NEVER emits a terminal reject;
//! - `2` and up: the venue's own, held in its bridge (ig's `2`: the login has no account to trade).
//!
//! Each venue's facts are a [`VenueApiHttpSpec`] in its bridge: its message prefix (the `Display` words stay the
//! venue's, byte for byte), the extra statuses it knows leave a write unknown (oanda's Cloudflare `520`), how it words
//! a network failure, and the write rules each owner ruling switches on. The helpers are the ones the three bridges each
//! wrote: [`VenueApiHttpSpec::network`], [`VenueApiHttpSpec::write_send_error`],
//! [`VenueApiHttpSpec::write_read_error`] and [`VenueApiHttpSpec::finish`] (a status and a body, read).

use vike_transport::send_outcome::{send_error_is_definite, status_is_gateway_ambiguous};

/// One venue's HTTP error facts. A `static` in the venue's bridge.
#[derive(Debug, PartialEq, Eq)]
pub struct VenueApiHttpSpec {
    /// The words before `{status}: {message}` in the error's `Display`: `"alpaca HTTP"`, `"IG error"`, `"OANDA error"`.
    pub prefix: &'static str,
    /// What a never-sent failure's message starts with ([`Self::network`]): `"network error: "` (oanda, ig), `""`
    /// (alpaca, which carries the cause's words alone).
    pub network_words: &'static str,
    /// Statuses beyond `1` and the gateway set ([`status_is_gateway_ambiguous`]) that leave the venue's act unknown
    /// (oanda: `520`, Cloudflare's "the origin answered something unexpected", measured 2026-10-18).
    pub also_unknown: &'static [u16],
    /// A 2xx with an empty body is JSON `null`, not "bad json" (alpaca's `204`-style replies).
    pub empty_2xx_is_null: bool,
    /// **Owner ruling B-2 (2026-11-11): a 2xx whose body does not read is "outcome unknown" on a write** (the venue
    /// answered yes to something; oanda's order replies). `false` keeps the 2xx status (alpaca and ig before B-2; both
    /// set it since), which a submit reads as a refusal over an order the venue took.
    pub unreadable_2xx_on_write_unknown: bool,
    /// **Owner ruling B-1 (2026-11-11): a 5xx on a write is "outcome unknown"**: nothing says the venue did not act
    /// before it failed. `false` keeps the status (alpaca, oanda and ig before B-1; all three set it since), so a bare
    /// 500 is the venue's refusal and only the gateway set is unknown.
    pub server_error_on_write_unknown: bool,
    /// **Owner ruling B-3 (2026-11-11): a 3xx on a write is "outcome unknown"**: no venue agent follows a redirect
    /// (`vike_transport::http`), so the 3xx is the answer and no verdict on the order. `false` keeps the status (a refusal).
    pub redirect_on_write_unknown: bool,
}

/// A bearer-token venue's REST error, normalized to `{status, message}` (the module doc has the statuses below 100).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueApiHttpError {
    pub status: u16,
    pub message: String,
    /// The venue this error speaks for.
    pub spec: &'static VenueApiHttpSpec,
}

impl std::fmt::Display for VenueApiHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.spec.prefix, self.status, self.message)
    }
}

impl std::error::Error for VenueApiHttpError {}

impl VenueApiHttpError {
    /// The `status` of a failure AFTER the request may have reached the venue: it MAY have acted. Not an HTTP status
    /// (those start at 100), so it cannot collide; `0` stays "provably never sent".
    pub const STATUS_OUTCOME_UNKNOWN: u16 = 1;

    /// Did the venue possibly act on the request? A submit that sees this re-queries and NEVER emits a terminal reject:
    /// [`Self::STATUS_OUTCOME_UNKNOWN`], a gateway status (a proxy in front of the venue says nothing about what the
    /// venue did) or one of the venue's [`VenueApiHttpSpec::also_unknown`].
    pub fn outcome_unknown(&self) -> bool {
        self.spec.status_unknown(self.status)
    }

    /// This error's [`ErrorKind`](super::ErrorKind), the one classification: status `0` (never sent) is
    /// `Network`, an unknown outcome ([`Self::outcome_unknown`]) `Timeout`, any other its HTTP status's
    /// ([`ErrorKind::from_http_status`](super::ErrorKind::from_http_status); a venue's own status below 100 is
    /// `Unknown`).
    pub fn kind(&self) -> super::ErrorKind {
        match self.status {
            0 => super::ErrorKind::Network,
            _ if self.outcome_unknown() => super::ErrorKind::Timeout,
            status => super::ErrorKind::from_http_status(status),
        }
    }

    /// This error as the signed venues' [`VenueApiError`](super::VenueApiError), its outcome kept, so a bearer-token
    /// venue answers a cancel or an amend through the same builders (`crate::rest::cancel_rejected`,
    /// `crate::rest::modify_rejected`): status `0` (never sent) is code `0`, an unknown outcome
    /// ([`Self::outcome_unknown`]) [`E_TIMEOUT_AMBIGUOUS`](super::E_TIMEOUT_AMBIGUOUS), any other status that code. The
    /// words are its `Display`.
    pub fn to_venue_error(&self) -> super::VenueApiError {
        let code = match self.status {
            0 => 0,
            _ if self.outcome_unknown() => super::E_TIMEOUT_AMBIGUOUS,
            status => i64::from(status),
        };
        super::VenueApiError { code, msg: self.to_string() }
    }
}

/// The non-2xx words of a venue whose error body names them in one string field (`"errorCode"` ig,
/// `"errorMessage"` oanda): that field, else the body's [`super::body_summary`] (never the body itself; none of these
/// venues documents a plain-text error, so none is shown as one).
pub fn field_or_text(text: &str, field: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|b| b.get(field).and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| super::body_summary(text, false))
}

impl VenueApiHttpSpec {
    /// This venue's error.
    pub fn error(&'static self, status: u16, message: impl Into<String>) -> VenueApiHttpError {
        VenueApiHttpError { status, message: message.into(), spec: self }
    }

    /// A failure that provably never reached the venue (or a read's, which has no outcome to be unsure of): status `0`.
    pub fn network(&'static self, e: impl std::fmt::Display) -> VenueApiHttpError {
        self.error(0, format!("{}{e}", self.network_words))
    }

    /// A send failure of a WRITE: [`Self::network`]'s status `0` only when it provably preceded the write
    /// ([`send_error_is_definite`], the one rule), else [`VenueApiHttpError::STATUS_OUTCOME_UNKNOWN`].
    pub fn write_send_error(&'static self, e: &ureq::Error) -> VenueApiHttpError {
        let words = super::send_error_text(e);
        if send_error_is_definite(e) {
            self.network(words)
        } else {
            self.error(VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, format!("network error: {words}"))
        }
    }

    /// A body that failed to read after the headers of a WRITE arrived: the venue received the request.
    pub fn write_read_error(&'static self, e: impl std::fmt::Display) -> VenueApiHttpError {
        self.error(
            VenueApiHttpError::STATUS_OUTCOME_UNKNOWN,
            format!("network error reading the reply: {e}"),
        )
    }

    /// A status and its body, read: the parsed JSON on a 2xx, else the venue's refusal in `words(body)` (its own
    /// grammar; [`field_or_text`] for a one-field body; a body that is not the venue's error goes into its words only
    /// as [`super::body_summary`]). `write`: the call may have changed the account, so the write rules this spec
    /// switches on apply. `credentials`: the values the request carried that a page could echo (the API key, the
    /// bearer or session token, a login's password), redacted from the error's words ([`super::redact_sent_values`]).
    ///
    /// # Errors
    /// A non-2xx answer, and a 2xx whose body is not JSON ("bad json: …", at the 2xx status, or unknown on a write
    /// under [`Self::unreadable_2xx_on_write_unknown`]). A write's answer the spec's rules leave unknown although its
    /// status does not say so is [`VenueApiHttpError::STATUS_OUTCOME_UNKNOWN`], its words led by the status
    /// (`http 500, outcome unknown: …`).
    pub fn finish(
        &'static self,
        status: u16,
        text: &str,
        write: bool,
        credentials: &[&str],
        words: impl FnOnce(&str) -> String,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        if (200..300).contains(&status) {
            if self.empty_2xx_is_null && text.trim().is_empty() {
                return Ok(serde_json::Value::Null);
            }
            let unknown = write && self.unreadable_2xx_on_write_unknown;
            return serde_json::from_str(text).map_err(|e| {
                self.error(
                    if unknown { VenueApiHttpError::STATUS_OUTCOME_UNKNOWN } else { status },
                    format!("bad json: {e}"),
                )
            });
        }
        let message = super::redact_sent_values(&words(text), credentials);
        if write && !self.status_unknown(status) && self.write_unknown(status) {
            return Err(self.error(
                VenueApiHttpError::STATUS_OUTCOME_UNKNOWN,
                format!("http {status}, outcome unknown: {message}"),
            ));
        }
        Err(self.error(status, message))
    }

    /// Does `status` by itself leave the venue's act unknown ([`VenueApiHttpError::outcome_unknown`])?
    fn status_unknown(&self, status: u16) -> bool {
        status == VenueApiHttpError::STATUS_OUTCOME_UNKNOWN
            || status_is_gateway_ambiguous(status)
            || self.also_unknown.contains(&status)
    }

    /// Does this non-2xx answer to a WRITE leave the venue's act unknown under the spec's rules?
    fn write_unknown(&self, status: u16) -> bool {
        (self.server_error_on_write_unknown && (500..600).contains(&status))
            || (self.redirect_on_write_unknown && (300..400).contains(&status))
    }
}

#[cfg(test)]
#[path = "http_error_tests.rs"]
mod http_error_tests;
