//! The one baseline-disposition pin every venue error-code table carries (`test-support` only).
//!
//! A venue's `error_codes::by_code` may ADD resolution where the shared baseline
//! (`VenueApiError::kind`) says `Unknown`, but it never changes the retry / re-query / terminal
//! disposition of a code the baseline already classifies. The venue keeps its own code range and its
//! own table; the walk is written here once.

use vike_model::events::Event;

use crate::transport::{ErrorKind, VenueApiError};

/// A failed modify through the venue's `modify_failed`: its status-unknown code `unknown` is worded as
/// the timeout sentinel is, keeping the venue's message; a definite code keeps the venue's words
/// verbatim.
pub fn assert_modify_failed_words_unknown_as_the_sentinel(
    modify_failed: impl Fn(&str, &VenueApiError) -> Event,
    unknown: i64,
) {
    let reason = |code| match modify_failed("c1", &VenueApiError { code, msg: "m".into() }) {
        Event::OrderModifyRejected(r) => r.reason.to_string(),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    };
    assert_eq!(
        reason(unknown),
        "modify outcome unknown (timed out after send; the venue may have applied it): m"
    );
    assert_eq!(reason(424_242), "m");
}

/// For every code in `codes` the baseline classifies, `by_code`'s answer keeps the baseline's
/// disposition; `venue` names the table in the failure.
pub fn assert_keeps_baseline_dispositions(
    venue: &str,
    codes: impl IntoIterator<Item = i64>,
    by_code: impl Fn(i64) -> Option<ErrorKind>,
) {
    for code in codes {
        let e = VenueApiError { code, msg: String::new() };
        let baseline = e.kind();
        if baseline == ErrorKind::Unknown {
            continue; // the table is free to ADD resolution here
        }
        let Some(ours) = by_code(code) else { continue };
        assert_eq!(
            (ours.is_retryable(), ours.must_requery(), ours.is_terminal()),
            (baseline.is_retryable(), baseline.must_requery(), baseline.is_terminal()),
            "{venue} {code}: {ours} changes the disposition of baseline {baseline}"
        );
    }
}
