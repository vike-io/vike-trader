//! The pins every venue error-code table carries (`test-support` only): the modify wording, the codes the
//! venue-neutral layer once held for the venue, and a sample behind every row.
//!
//! The venue keeps its own code range and its own table; each walk is written here once.

use std::path::Path;

use vike_model::events::Event;

use crate::error_kind::{VenueTaxonomy, classify_venue, same_disposition};
use crate::transport::{ErrorKind, VenueApiError};

/// A failed modify through the venue's `modify_failed`: its status-unknown code `unknown` and a code its table
/// does not map are worded as the timeout sentinel is, keeping the venue's message; `definite`, a refusal row,
/// keeps the venue's words verbatim.
pub fn assert_modify_failed_words_unknown_as_the_sentinel(
    modify_failed: impl Fn(&str, &VenueApiError) -> Event,
    unknown: i64,
    definite: i64,
) {
    let reason = |code| match modify_failed("c1", &VenueApiError { code, msg: "m".into() }) {
        Event::OrderModifyRejected(r) => r.reason.to_string(),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    };
    for code in [unknown, 424_242] {
        assert_eq!(
            reason(code),
            "modify outcome unknown (timed out after send; the venue may have applied it): m",
            "{code}"
        );
    }
    assert_eq!(reason(definite), "m");
}

/// The codes the venue-neutral layer held for this venue until 2026-11-23 (`moved`, each with the kind it gave
/// then) keep their disposition under the venue's table, which alone reads them now: moving a code may sharpen
/// its kind (`-1016` server error → maintenance), never change what a caller does with it.
pub fn assert_keeps_the_moved_seed_dispositions(
    taxonomy: &VenueTaxonomy,
    moved: &[(i64, ErrorKind)],
) {
    for &(code, was) in moved {
        let now = classify_venue(Some(taxonomy), &VenueApiError { code, msg: String::new() });
        assert!(
            same_disposition(now, was),
            "{} {code}: {now} changes the disposition the shared seed gave it ({was})",
            taxonomy.venue
        );
    }
}

/// One venue answer a table row is proved on: its code and words as the venue sent them, and the file of this
/// repository (repository-root relative) that keeps the capture, a recorded fixture or a test pinning a reply
/// measured on the venue.
#[derive(Debug, Clone, Copy)]
pub struct RowSample {
    pub code: i64,
    pub words: &'static str,
    pub recorded_in: &'static str,
}

/// Every row of `taxonomy`'s `by_code` among `codes` is proved by one of `samples` or named in
/// `without_sample`, never both, and `without_sample` names rows only; each sample's file holds its code and its
/// words (compared on letters and digits, so a line-wrapped quote still counts) and the sample classifies to its
/// row's own kind. `root` is the repository root the samples' paths are read under (the venue's test passes it).
pub fn assert_every_row_has_a_sample(
    taxonomy: &VenueTaxonomy,
    root: &Path,
    codes: impl IntoIterator<Item = i64>,
    samples: &[RowSample],
    without_sample: &[i64],
) {
    let venue = taxonomy.venue;
    let letters = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    for s in samples {
        let answer = VenueApiError { code: s.code, msg: s.words.to_string() };
        assert_eq!(
            (taxonomy.by_code)(s.code),
            Some(classify_venue(Some(taxonomy), &answer)),
            "{venue} {}: a sample lands on its row's own kind (and a sampled code has a row)",
            s.code
        );
        let file = std::fs::read_to_string(root.join(s.recorded_in));
        assert!(file.is_ok(), "{venue} {}: {}: {file:?}", s.code, s.recorded_in);
        let file = file.unwrap_or_default();
        assert!(file.contains(&s.code.to_string()), "{venue} {}: not in {}", s.code, s.recorded_in);
        assert!(
            letters(&file).contains(&letters(s.words)),
            "{venue} {}: {:?} not in {}",
            s.code,
            s.words,
            s.recorded_in
        );
    }
    let mut rows = 0;
    for code in codes {
        let listed = without_sample.contains(&code);
        if (taxonomy.by_code)(code).is_none() {
            assert!(!listed, "{venue} {code}: listed but has no row");
            continue;
        }
        rows += 1;
        let sampled = samples.iter().any(|s| s.code == code);
        assert!(!(sampled && listed), "{venue} {code}: sampled, so off the no-sample list");
        assert!(
            sampled || listed,
            "{venue} {code}: a row with no sample and not on the no-sample list"
        );
    }
    assert_eq!(
        rows,
        samples.len() + without_sample.len(),
        "{venue}: a code outside the walked range"
    );
}
