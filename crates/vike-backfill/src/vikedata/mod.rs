//! `data.vike.io` cohort-metrics backfill — the graded positioning panel (`kind=cohort`) served by
//! `GET {base}/{exchange}/coins/{asset}/cohort-metrics`, paged by cursor, into the `vike-data` hist
//! store. A sibling of [`crate::tardis`] / [`crate::databento`] / [`crate::eod`] / [`crate::pmxt`].
//!
//! # Why this directory exists at all
//!
//! **Every other historical source in this workspace is a DIRECTORY, and `data.vike.io` was the one
//! that was not.** `crates/vike-backfill/src/tardis/` and `crates/vike-backfill/src/databento/` are
//! each `mod.rs` + `client.rs` + `parse.rs` + `ingest.rs` behind a `<vendor>` feature with a
//! `<vendor>_backfill` bin declaring `required-features`; `crates/vike-backfill/src/eod/` (Yahoo)
//! and `crates/vike-backfill/src/pmxt/` carry the same shape. `data.vike.io` was three FLAT files
//! instead — `crates/vike-backfill/src/vike_archive.rs`, `crates/vike-backfill/src/archive_store.rs`
//! and `crates/vike-backfill/src/events_api.rs` — sharing one `vike-archive` feature between two
//! bins and mixing transport, wire schema and store writes inside single 900-to-3500-line files.
//!
//! The shape is not decoration. It is what makes a vendor's rules READABLE one concern at a time:
//! [`client`] owns the wire (URL, auth header, one request), [`parse`] is pure and fixture-testable
//! (the taxonomy fold, the guards, the snap), [`ingest`] owns the cursor WALK and the store write.
//! A guard that lives in a pure module gets a test that needs no network; a guard buried in a
//! transport function gets a comment. This module makes `data.vike.io` a peer of the other vendors
//! rather than an exception to them, and it is deliberately the FIRST of that source's lanes to be
//! built this way — the two archive files keep their own feature and are untouched here.
//!
//! # What it fetches
//!
//! One `(exchange, asset, axis, grading)` ladder over one hourly window: for each hour and each
//! cohort LABEL, the long-side notional and the total notional the grading service reported. Those
//! land as `vike_data::CohortRow`s through `vike_data::CohortRecorder`, the write-side entry point
//! `kind=cohort` already has — this module builds no commit key of its own
//! (`crates/vike-data/src/cohort_rec.rs`'s `CohortFetch` is the one place that key is spelled).
//!
//! # The guards are the value, and they are PORTED rather than re-derived
//!
//! `crates/vike-research/src/sources/api.rs` is where these rules were paid for, one incident at a
//! time. ⚠ **That file no longer exists** — the research crate was dissolved and its HTTP fetch
//! became THIS directory, so every citation of it in this module (here, in [`client`], [`parse`],
//! [`ingest`] and [`crate::vikedata::schedule`]) names the PREDECESSOR of the file it is written
//! in: provenance for a rule, never a place to go and read. `crates/vike-ops/tests/citation_gate.rs`'s
//! `DEAD_PATH_EXCEPTIONS` carries the one row that covers them all. The rules themselves were
//! ported rather than re-derived, and each is now enforced here:
//!
//! 1. **`label_basis` must AGREE across every page, and an absent one is a HARD failure on the pnl
//!    axis** ([`parse::guard_label_basis`], [`ingest::guard_basis_agrees`]). The endpoint's default
//!    resolves a wallet's cohort at QUERY time, so an April row comes back with today's grading —
//!    that is LOOKAHEAD, and it shipped once already. FastAPI ignores unknown query parameters, so
//!    a server predating the flag answers `200` with query-time labels and NO field at all: a guard
//!    written as "if the field is present and wrong" calls that fine.
//! 2. **The grading echo must match what was asked for** ([`parse::guard_grading`]). The three
//!    gradings return SHAPE-IDENTICAL rows over the same hours, so a server that answered a
//!    `realized-pit` request with realized rows produces a store nothing downstream can tell apart.
//! 3. **A `(ts, cohort)` seen twice across a page overlap is FIRST-WINS**
//!    ([`parse::dedupe_first_wins`]), never summed — pages arrive newest-first and overlap.
//! 4. **The cursor walk has THREE stops, not two** ([`ingest::WalkStop`]): an absent cursor, an
//!    empty page, AND the window break — `anchor − oldest >= days × 86400`. The third caps an
//!    ANCHORED read: this endpoint keeps serving buckets up to ITS OWN now regardless of the
//!    `start` sent, so without it a gap-fill of last April pages forward through today and issues
//!    MORE requests than a trailing read against a metered endpoint.
//! 5. **Both notionals are SNAPPED before anything is derived from or written with them**
//!    ([`parse::canonical_notional`]) — the endpoint does not answer one request the same way
//!    twice (measured: 430 of 8,190 rows differing at ~1.4 ULP between two GETs of a
//!    byte-identical URL). Here that matters for a reason the study does not have: this store's
//!    idempotency is BATCH-level on the commit key, so the FIRST write of a window is authoritative
//!    forever and unsnapped reduction noise is not repairable afterwards.
//! 6. **The bucket label is on the hour, and the store partitions in MILLISECONDS**
//!    ([`parse::hour_ms`]). The wire serves whole unix SECONDS; a row that reaches
//!    `vike_data::CohortRow`'s `ts` unmultiplied lands in the 1970 partition — silently, once, and
//!    for good.
//!
//! # Measured against the LIVE endpoint, 2026-08-25 — the first time this module reached it
//!
//! Everything above was built and gated against SCRIPTED bodies, which proves this client handles
//! what we believed the server sends. Two beliefs were named as unverified and are now measured, by
//! `curl` against `https://data.vike.io/v1` and by a real `--axis size --days 1` run that stored
//! 300 rows and exited 0:
//!
//! * **The pnl `label_basis` echo is REAL.** The envelope and every row carry
//!   `label_basis: "point_in_time"`, so [`parse::guard_label_basis`] passes rather than refusing
//!   every pnl fetch. That guard is the lookahead defence in point 1 above, and until this run
//!   nobody had seen the field it keys on.
//! * **The auth header is case-insensitive, as HTTP requires.** `X-API-KEY` and `X-API-Key` both
//!   answer `200`; the control — the same request with NO key — answers `401`, which is what makes
//!   the two `200`s evidence of anything. [`client::API_KEY_HEADER`]'s spelling is therefore a
//!   convention, not a requirement, and the sibling `events_api.rs` casing is equally fine.
//!
//! And one belief that was WRONG, corrected in [`ingest::walk_pages`] where it was written: the
//! size and tier axes DO return `label_basis`, unasked, on the envelope and on every row. It
//! reaches the commit key, so those ladders key on `point_in_time` rather than on the unset
//! sentinel. No guard or store behaviour changes; the comment claiming otherwise did.
//!
//! ⚠ **Still unmeasured**: only the `size` axis has made a full ingest run. The pnl and tier axes
//! are proven at the WIRE (their responses were read) but not end to end through the recorder, and
//! no run has yet exercised the cursor walk past ONE page — the 300-row window stopped at
//! `CursorExhausted` on page 1. The multi-page path, the window break and the dedupe remain
//! scripted-only. Say so rather than reading this section as "the vendor is proven".
//!
//! # Three run shapes, and only one of them may go on a TIMER
//!
//! [`ingest::CohortWindow::trailing`] is the periodic one (`--days N` back from now) and
//! [`ingest::CohortWindow::range`] the AD-HOC one (`--start`/`--end`), so an operator fills a
//! measured gap with one command instead of widening a trailing window until it happens to cover
//! it. They differ in more than arithmetic: the range shape arms an exact `[start, end]` retain,
//! because the endpoint serves past the window's end whatever it was asked for.
//!
//! ⚠ **Both of those are MANUAL shapes, and putting the trailing one on a timer is the defect this
//! module's guards cannot catch.** Idempotency here is BATCH-level on the commit key, so a window
//! that SLIDES with the clock is a new key every firing and the hours two firings share land TWICE
//! (`crates/vike-backfill/src/vikedata/ingest.rs`'s
//! `two_overlapping_windows_are_two_batches_and_the_shared_hours_land_twice`). [`schedule`] is the
//! third shape: whole COMPLETE UTC days off a fixed grid, over a declared ladder set, with no
//! boundary for a caller to pass and therefore no overlapping boundary a caller can pass.

pub mod client;
pub mod ingest;
pub mod panel;
pub mod parse;
pub mod schedule;

pub use client::{Axis, Grading, VIKEDATA_BASE, cohort_metrics_url, fetch_page, page_url};
pub use ingest::{CohortFetchResult, CohortWindow, WalkStop, backfill_cohort, fetch_cohort};
pub use panel::{
    PanelRows, fetch_panel, panel_bars_commit_key, panel_commit_key, panel_funding_commit_key,
};
pub use parse::{canonical_notional, is_known, normalize_cohort};
pub use schedule::{DEFAULT_CATCH_UP, DEFERRED, Ladder, MAX_CATCH_UP, POLLED, scheduled_windows};

/// Seconds in an hour — the bucket width this endpoint serves.
pub const SECS_PER_HOUR: i64 = 3_600;
/// Seconds in a day — the unit `--days` and the walk's window break are expressed in.
pub const SECS_PER_DAY: i64 = 86_400;

/// Floor a unix instant to the hour.
///
/// ⚠ **Load-bearing on the way OUT, not just on the way in.** Measured 2026-08-09 (BTC, size axis,
/// 72 hours): a `start` of `…T13:02:17Z` made the response disagree with the ClickHouse ground
/// truth on 12 of 1,728 values by up to 5.0%, EVERY disagreement in the single oldest bucket; the
/// same window sent as `…T13:00:00Z` matched 864/864 cells exactly. The endpoint returns hourly
/// buckets but honours a sub-hour `start`, truncating that bucket's aggregate while still labelling
/// it on the hour. `crates/vike-research/src/sources/api.rs`'s `floor_to_hour` was where that was
/// measured; this function is that one, moved (see this module's doc on the dead citations).
///
/// `rem_euclid` so a pre-epoch instant floors DOWN too.
pub fn floor_to_hour(secs: i64) -> i64 {
    secs - secs.rem_euclid(SECS_PER_HOUR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flooring_moves_a_sub_hour_start_back_to_the_bucket_label() {
        // The measured incident: 13:02:17 is inside the 13:00 bucket, and sending it truncates
        // that bucket's aggregate server-side.
        assert_eq!(floor_to_hour(1_754_744_537), 1_754_744_400);
        assert_eq!(floor_to_hour(1_754_744_400), 1_754_744_400, "already on the hour is a no-op");
    }

    #[test]
    fn flooring_a_pre_epoch_instant_rounds_down_not_toward_zero() {
        // `secs % 3600` would give a NEGATIVE remainder here and floor UP, into the future.
        assert_eq!(floor_to_hour(-1), -SECS_PER_HOUR);
        assert_eq!(floor_to_hour(-SECS_PER_HOUR), -SECS_PER_HOUR);
    }
}
