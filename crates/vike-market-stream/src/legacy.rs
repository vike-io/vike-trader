//! The WORDS a subscription refusal was classed by before its class crossed the wire, and the only place they are
//! read or written (decision 0301: a decision is made on a type, never on text).
//!
//! shortcut: a hub or a client that predates `vike_hub_protocol::features::FEATURE_REFUSAL_FRAME` still classes a
//! refusal by these words: a not-now carried "retried on the caller's next pass" and a cap "a cap, not a link". So a
//! refusal sent to a subscriber that did not say it reads the classes is worded so ([`as_older_kind`],
//! [`older_cap_text`]), and a refusal from a hub that does not send them is classed by them ([`classed`],
//! [`older_open_was_capped`]). Delete this module once every deployed hub and client speaks `refusal_frame`.

use vike_data::DataClientError;

use crate::wire::StreamSubscriptionErrorKind;

/// What an older reader reads as "transient, ask again".
const NOT_NOW_WORDS: &str = "retried on the caller's next pass";

/// What an older reader reads as "a cap: an operator must act", whatever else the text says.
const CAP_WORDS: &str = "a cap, not a link";

/// A `Subscribe` refusal from a daemon that predates the classes, as the class its words say.
pub(crate) fn classed(text: String) -> DataClientError {
    if text.contains(CAP_WORDS) {
        DataClientError::Cap(text)
    } else if text.contains(NOT_NOW_WORDS) {
        DataClientError::NotNow(text)
    } else {
        DataClientError::Subscribe(text)
    }
}

/// Whether an open a daemon that predates the refusal frame refused (its `Response::Error` text) was refused by a
/// CAP.
pub(crate) fn older_open_was_capped(text: &str) -> bool {
    text.contains(CAP_WORDS)
}

/// `kind` as a subscriber that did not say it reads the classes reads it: a not-now or a cap is the `Subscribe` its
/// words class; every other kind is unchanged.
pub(crate) fn as_older_kind(kind: StreamSubscriptionErrorKind) -> StreamSubscriptionErrorKind {
    match kind {
        StreamSubscriptionErrorKind::NotNow(text) => {
            StreamSubscriptionErrorKind::Subscribe(worded(text, NOT_NOW_WORDS))
        }
        StreamSubscriptionErrorKind::Cap(text) => {
            StreamSubscriptionErrorKind::Subscribe(worded(text, CAP_WORDS))
        }
        other => other,
    }
}

/// The text of a cap's refusal FRAME as a client that does not read the frame reads it (the hub answers it
/// `Response::Error` with this text): the words an older engine classes a cap by.
pub fn older_cap_text(text: &str) -> String {
    worded(text.to_string(), CAP_WORDS)
}

/// `text` carrying `words` once.
fn worded(text: String, words: &str) -> String {
    if text.contains(words) { text } else { format!("{text}; {words}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Both directions agree**: what is written for an older reader is classed back as the class it was, and a
    /// refusal carrying neither phrase is the venue's own answer.
    #[test]
    fn an_older_wording_round_trips_to_its_class() {
        let cap = as_older_kind(StreamSubscriptionErrorKind::Cap("the pump cap is full".into()));
        let StreamSubscriptionErrorKind::Subscribe(cap_text) = cap else { panic!("{cap:?}") };
        assert!(matches!(classed(cap_text.clone()), DataClientError::Cap(_)), "{cap_text}");
        assert!(older_open_was_capped(&older_cap_text("already serving 3 sessions")));

        let not_now = as_older_kind(StreamSubscriptionErrorKind::NotNow("the link is down".into()));
        let StreamSubscriptionErrorKind::Subscribe(text) = not_now else { panic!("{not_now:?}") };
        assert!(matches!(classed(text.clone()), DataClientError::NotNow(_)), "{text}");

        // A cap that also says "transient" stays a cap; the venue's own answer stays its own.
        let both = format!("{}; {NOT_NOW_WORDS}", older_cap_text("x"));
        assert!(matches!(classed(both), DataClientError::Cap(_)));
        assert!(matches!(classed("no such symbol".into()), DataClientError::Subscribe(_)));
        assert_eq!(older_cap_text(&older_cap_text("x")), older_cap_text("x"), "worded once");
    }
}
