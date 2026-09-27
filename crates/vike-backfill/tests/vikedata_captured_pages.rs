//! Six captured `/cohort-metrics` bodies, decoded through the shipped collector.
//!
//! # What these are and where they came from
//!
//! `crates/vike-backfill/tests/fixtures/vikedata/` holds six response bodies captured from the live
//! `data.vike.io` endpoint (plus one hand-edited from a capture, and one whose metrics come
//! verbatim from an upstream prod capture) — `PROVENANCE.md` beside them carries every command, the
//! reason each page was captured the way it was, and one ⚠ CORRECTION where a claim it made turned
//! out to be a window artefact. They moved here byte-identical from
//! `crates/vike-research/tests/fixtures/api/`, where they were consumed only by that crate's own
//! `#[cfg(test)]` module. They are REAL-WIRE EVIDENCE, and this file is what keeps them alive:
//! `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` moved the wire to the
//! collector, so the bodies belong beside the decoder that has to survive them.
//!
//! # What a captured body proves that a synthetic one cannot
//!
//! `crates/vike-backfill/src/vikedata/parse.rs`'s own unit tests build pages by hand, so they prove
//! the RULES. They cannot prove the SHAPE: a real envelope carries `bias`, `n_wallets`,
//! `position_count*`, `total_position_size*` and `total_unrealized_pnl` alongside the five fields
//! `MetricRow` consumes, the pnl pages carry a real opaque `nextCursor` while the standalone pages
//! carry `null`, and the tier labels arrive with `$ < > - .` in them. Every test below drives a
//! whole captured body through `parse_page` -> the guards -> `rows_from_metrics` ->
//! `dedupe_first_wins` and asserts what THAT body actually demonstrates.
//!
//! # What is NOT here, because it moved rather than died
//!
//! The replaced module also used these bodies for WIRE-SHAPE tests — that `start` goes out floored
//! to the hour, that `hours` and an empty first `cursor` are always sent, that `labelBasis` rides
//! the pnl axis and not the size axis. Those are questions about the REQUEST, and this collector
//! answers them in `crates/vike-backfill/src/vikedata/client.rs`'s `page_url`, whose own unit tests
//! pin the exact query string. A captured RESPONSE is not evidence about a request, and re-driving
//! one through a fixture transport to read back a URL would be testing the transport, not the page.

#![cfg(feature = "vikedata")]

use std::collections::{BTreeMap, BTreeSet};

use vike_backfill::vikedata::client::{Axis, Grading};
use vike_backfill::vikedata::parse::{
    CohortPage, LABEL_BASIS_UNSET, PNL_COHORTS, POINT_IN_TIME, SIZE_COHORTS, TIER_COHORTS,
    dedupe_first_wins, guard_grading, guard_label_basis, is_known, normalize_cohort, parse_page,
    rows_from_metrics,
};
use vike_data::CohortRow;

/// One page of the size ladder, `nextCursor: null` BY CONSTRUCTION — `start` was chosen close
/// enough to capture time that the whole requested window came back in one page, because this body
/// is used standalone. See `PROVENANCE.md`.
const SIZE_P1: &str = include_str!("fixtures/vikedata/cohort_size_page1.json");
/// The pnl ladder's first page — ONE hour, carrying a real opaque `nextCursor`.
const PNL_P1: &str = include_str!("fixtures/vikedata/cohort_pnl_page1.json");
/// The pnl ladder's second page — FOUR hours, `nextCursor: null`, i.e. the walk's terminator. Its
/// hours do not overlap page 1's, so a concatenation of the two is a clean five-hour window.
const PNL_P2: &str = include_str!("fixtures/vikedata/cohort_pnl_page2.json");
/// The ONE edited-from-a-capture body: page 1 with the ENVELOPE's `label_basis` deleted by hand
/// (`jq 'del(.label_basis)'`). The exact shape a server predating the flag returns — 200, per-row
/// `label_basis` still present, envelope field gone. No live endpoint produces it any more.
const PNL_NO_BASIS: &str = include_str!("fixtures/vikedata/cohort_pnl_no_label_basis.json");
/// One real captured pnl hour carrying all 13 labels the ladder plus the unrankable bucket produce,
/// with ONE hand-added `cohort: ""` row modelled on a real row's field shape.
///
/// ⚠ That row is SYNTHETIC and `PROVENANCE.md` says so, along with the correction that explains
/// why: the empty label IS real, it is a SIZE-axis value, and the search that concluded otherwise
/// began after the 2026-04-03 cutover. `fixtures/hl_cohort/loader.json` is the real capture of it,
/// replayed by `crates/vike-backfill/tests/vikedata_loader_parity.rs`.
const JUNK: &str = include_str!("fixtures/vikedata/cohort_pnl_junk_labels.json");
/// ⚠ Metrics VERBATIM from the upstream prod capture of the regenerated tier query layer (30 rows,
/// 3 hours × all ten buckets) — but NOT a live-endpoint HTTP capture from this workspace. See
/// `PROVENANCE.md`, which also records that the LIVE tier fetch is unverified until it is
/// re-captured.
const TIER_P1: &str = include_str!("fixtures/vikedata/cohort_tier_page1.json");

/// Every captured body, for the checks that are about all of them at once.
const ALL: [(&str, &str); 6] = [
    ("cohort_size_page1", SIZE_P1),
    ("cohort_pnl_page1", PNL_P1),
    ("cohort_pnl_page2", PNL_P2),
    ("cohort_pnl_no_label_basis", PNL_NO_BASIS),
    ("cohort_pnl_junk_labels", JUNK),
    ("cohort_tier_page1", TIER_P1),
];

/// Every capture is BTC — the endpoint is `/coins/BTC/cohort-metrics`.
const ASSET: &str = "BTC";

/// The URL a decode error would name. No socket is opened; this is the `url` parameter the parser
/// and the guards put in their messages.
const URL: &str = "https://data.vike.io/v1/hyperliquid/coins/BTC/cohort-metrics";

/// The basis a QUERY-TIME grading resolves under — what the guard must refuse. It is the value the
/// two probes below splice into a captured body; no capture carries it.
const CURRENT: &str = "current";

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn decode(body: &str) -> CohortPage {
    parse_page(body, URL).unwrap_or_else(|e| panic!("a real captured body must decode: {e}"))
}

/// One decoded page's rows, through the same call `crates/vike-backfill/src/vikedata/ingest.rs`
/// makes: the basis is stamped from the FETCH, so a row's own echo is a thing the guard checked
/// rather than a thing the store carries per row.
fn rows_of(page: &CohortPage, axis: Axis) -> (Vec<CohortRow>, BTreeMap<String, usize>) {
    let basis = page.label_basis.as_deref().unwrap_or(LABEL_BASIS_UNSET).to_string();
    let metrics = page.metrics.as_deref().unwrap_or(&[]);
    rows_from_metrics(ASSET, axis, Grading::Realized, &basis, metrics)
        .unwrap_or_else(|e| panic!("{} axis: {e}", axis.as_str()))
}

/// How many rows a decoded body served, before any filter.
fn served(page: &CohortPage) -> usize {
    page.metrics.as_deref().unwrap_or(&[]).len()
}

/// A cursor WALK over several captured bodies, in the order the endpoint served them: decode, guard
/// the pnl basis, collect, then dedupe ONCE at the end — `ingest`'s own shape.
fn walk(bodies: &[&str], axis: Axis) -> Vec<CohortRow> {
    let mut rows = Vec::new();
    for body in bodies {
        let page = decode(body);
        if axis == Axis::Pnl {
            guard_label_basis(&page, URL).expect("the captured pnl pages echo point_in_time");
        }
        rows.extend(rows_of(&page, axis).0);
    }
    dedupe_first_wins(rows)
}

fn hours(rows: &[CohortRow]) -> BTreeSet<i64> {
    rows.iter().map(|r| r.ts).collect()
}

/// Replace the LAST occurrence of `pat` — the way to reach a captured body's final ROW without
/// touching its envelope, which carries the same text.
fn replace_last(body: &str, pat: &str, with: &str) -> String {
    let at =
        body.rfind(pat).unwrap_or_else(|| panic!("the probe is inert: no {pat:?} in the body"));
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..at]);
    out.push_str(with);
    out.push_str(&body[at + pat.len()..]);
    out
}

// ---------------------------------------------------------------------------------------------
// the envelope, the rows, the cursor
// ---------------------------------------------------------------------------------------------

/// A real size-ladder body decodes whole, and every one of its rows reaches the store.
///
/// The row count is asserted as `hours × ladder`, not as a literal: this capture is a full
/// partition of the book, so a row going missing is a hole in an hour rather than a number that
/// shrank, and stating it that way is what makes the assertion able to see the difference.
#[test]
fn a_real_captured_size_page_decodes_its_envelope_its_rows_and_its_absent_cursor() {
    let page = decode(SIZE_P1);
    assert_eq!(page.label_basis.as_deref(), Some(POINT_IN_TIME));
    assert_eq!(
        page.next_cursor, None,
        "PROVENANCE.md: this page terminates BY CONSTRUCTION so it can be used standalone"
    );

    let (rows, dropped) = rows_of(&page, Axis::Size);
    assert!(dropped.is_empty(), "a clean capture carries no off-taxonomy label: {dropped:?}");
    let rows = dedupe_first_wins(rows);
    let hs = hours(&rows);
    assert_eq!(rows.len(), hs.len() * SIZE_COHORTS.len(), "a size rung is missing an hour");
    assert!(hs.len() > 1, "one hour cannot demonstrate a multi-hour page");
    assert!(rows.iter().all(|r| SIZE_COHORTS.contains(&r.cohort.as_str())));
    assert!(rows.iter().all(|r| r.long_usd >= 0.0 && r.short_usd() >= 0.0));
    assert!(
        rows.windows(2).all(|w| (w[0].ts, &w[0].cohort) < (w[1].ts, &w[1].cohort)),
        "the store reads back ascending by (ts, cohort), so that is the order to write"
    );
    // The per-fetch columns, stamped from the fetch rather than from the row.
    assert!(rows.iter().all(|r| r.axis == "size" && r.label_basis == POINT_IN_TIME));
    assert!(rows.iter().all(|r| r.grading == Grading::Realized.echoed()));
}

/// The cursor half of the capture set, which is why there are two pnl pages at all: page 1 carries
/// a real opaque cursor (so a walk continues) and page 2 carries none (so it stops).
#[test]
fn the_two_real_pnl_captures_carry_page_ones_cursor_and_page_twos_terminator() {
    let p1 = decode(PNL_P1);
    let p2 = decode(PNL_P2);
    let cursor = p1.next_cursor.as_deref().expect("page 1 must carry a real cursor");
    assert!(
        !cursor.is_empty(),
        "an EMPTY cursor is what the FIRST request sends, not a page's echo"
    );
    assert_eq!(p2.next_cursor, None, "page 2 is the walk's terminator");

    // ...and the two pages concatenate into one clean window: their hours do not overlap
    // (PROVENANCE.md chose them that way), so the walk's output is the union.
    let rows = walk(&[PNL_P1, PNL_P2], Axis::Pnl);
    let h1 = hours(&dedupe_first_wins(rows_of(&p1, Axis::Pnl).0));
    let h2 = hours(&dedupe_first_wins(rows_of(&p2, Axis::Pnl).0));
    assert!(h1.is_disjoint(&h2), "the captures were chosen to be disjoint; they no longer are");
    assert_eq!(hours(&rows).len(), h1.len() + h2.len());
    assert!(
        rows.windows(2).all(|w| (w[0].ts, &w[0].cohort) < (w[1].ts, &w[1].cohort)),
        "pages arrive newest-first and the walk must still hand the store ascending rows"
    );
    // Every pnl row is a ladder rung or the admitted unrankable bucket, and nothing was dropped.
    assert!(rows.iter().all(|r| is_known(Axis::Pnl, &r.cohort)));
    assert_eq!(
        rows.iter().map(|r| r.cohort.as_str()).collect::<BTreeSet<_>>().len(),
        PNL_COHORTS.len() + 1,
        "the whole ladder plus `Unknown` — the capture carries a genuine unrankable row"
    );
}

/// A page served TWICE across a boundary must contribute nothing the second time.
///
/// ⚠ The replaced test also carried a `sum(duplicated) < 2 * sum(one page)` bound and recorded, in
/// its own words, that the bound CANNOT FAIL on these fixtures: with page1 ≈ $1.12B and page2 ≈
/// $4.50B, a double-counting walk sums to $6.75B, still under the $9.00B bound. It is not carried,
/// because a comparison that cannot fail is not evidence. What IS carried is the statement that
/// discriminates: the duplicated walk must equal the clean walk EXACTLY, row for row and bit for
/// bit. (First-wins over two pages that DISAGREE about one cell is a different claim, and
/// `crates/vike-backfill/src/vikedata/parse.rs`'s
/// `a_repeated_ts_cohort_across_a_page_overlap_keeps_the_first_and_never_sums` is where it is
/// pinned — a byte-identical duplicate cannot tell first-wins from last-wins.)
#[test]
fn a_repeated_page_across_the_captured_boundary_is_deduped_not_doubled() {
    let clean = walk(&[PNL_P1, PNL_P2], Axis::Pnl);
    let repeated = walk(&[PNL_P1, PNL_P1, PNL_P2], Axis::Pnl);
    assert_eq!(repeated.len(), clean.len(), "the duplicated page added rows");
    assert_eq!(repeated, clean, "the duplicated page changed a row");
    let sum = |rs: &[CohortRow]| rs.iter().map(|r| r.long_usd).sum::<f64>();
    assert_eq!(
        sum(&repeated).to_bits(),
        sum(&clean).to_bits(),
        "the duplicated page changed the notional; dedup is not first-wins"
    );
}

// ---------------------------------------------------------------------------------------------
// the two guards
// ---------------------------------------------------------------------------------------------

/// **THE lookahead case, from a real body.** The endpoint's default resolves a wallet's cohort at
/// QUERY time, so an April row comes back carrying today's grading — and because the server ignores
/// unknown query parameters, a build predating `labelBasis` answers 200 with query-time labels and
/// NO envelope field at all. A guard written as "if the field is present and wrong" calls exactly
/// that case fine.
///
/// The contrast is what makes this a guard test rather than a parser test: the same body's ROWS
/// decode and would produce a full hour of store rows. Nothing between the wire and the store
/// refuses them except this guard.
#[test]
fn a_captured_page_that_echoes_no_label_basis_is_a_hard_failure_not_a_pass() {
    let page = decode(PNL_NO_BASIS);
    assert_eq!(page.label_basis, None, "the fixture must still be missing the envelope field");
    let wire_rows = page.metrics.as_deref().unwrap_or(&[]);
    assert!(
        wire_rows.iter().all(|r| r.label_basis.as_deref() == Some(POINT_IN_TIME)),
        "the dangerous shape is per-ROW basis PRESENT and envelope basis absent"
    );
    assert!(
        page.next_cursor.is_some(),
        "it was edited from page 1, so it keeps page 1's cursor — a walk would carry on"
    );

    let err = guard_label_basis(&page, URL).unwrap_err().to_string();
    assert!(err.contains("<field absent>"), "{err}");
    assert!(err.contains("lookahead"), "the message has to say WHY: {err}");

    // ...and the rows themselves are fine, which is the point: the guard is the ONLY refusal.
    let (rows, dropped) = rows_of(&page, Axis::Pnl);
    assert!(dropped.is_empty());
    assert_eq!(rows.len(), served(&page));
}

/// The other half of the same guard, on a real body: a basis that NAMES another question is refused
/// in the envelope and in a ROW, and the row arm names the offending index.
///
/// The rows are what gets stored, so an envelope-only check is not enough — the API sets the two in
/// separate statements.
#[test]
fn a_captured_page_whose_basis_names_another_question_is_a_hard_failure_too() {
    let pat = "\"label_basis\":\"point_in_time\"";
    let with = "\"label_basis\":\"current\"";

    // The FIRST occurrence in a compact body is the envelope's.
    let envelope = PNL_P1.replacen(pat, with, 1);
    assert_ne!(envelope, PNL_P1, "the replace found nothing to mutate — the probe is inert");
    let err = guard_label_basis(&decode(&envelope), URL).unwrap_err().to_string();
    assert!(err.contains(CURRENT), "{err}");

    // ...and the LAST is the final row's, with the envelope left saying `point_in_time`.
    let row = replace_last(PNL_P2, pat, with);
    assert_ne!(row, PNL_P2, "the probe is inert");
    let page = decode(&row);
    assert_eq!(page.label_basis.as_deref(), Some(POINT_IN_TIME), "the envelope must stay honest");
    let last = served(&page) - 1;
    let err = guard_label_basis(&page, URL).unwrap_err().to_string();
    assert!(err.contains(&format!("row {last}")), "the message names the offending index: {err}");
    assert!(err.contains(CURRENT), "{err}");
}

/// **No capture echoes a `grading` at all** — every one of them predates that parameter — and that
/// is exactly the asymmetry `guard_grading` encodes: absence is the old contract answering
/// correctly for the DEFAULT grading, and the dangerous case for either of the other two, where it
/// means the parameter was ignored and realized rows came back under another question's name.
///
/// The three gradings return SHAPE-IDENTICAL rows over the same hours, and `grading` is a stored
/// column AND a commit-key segment, so a mismatch spends the key that names the honest fetch.
#[test]
fn no_capture_echoes_a_grading_and_only_the_default_grading_accepts_that() {
    for (name, body) in ALL {
        let page = decode(body);
        assert_eq!(page.grading, None, "{name} grew a grading echo; this test's premise is gone");
        assert!(guard_grading(&page, URL, Grading::Realized).is_ok(), "{name}");
        for g in [Grading::RealizedPit, Grading::Unrealized] {
            let err = guard_grading(&page, URL, g).unwrap_err().to_string();
            assert!(err.contains("field absent"), "{name}: {err}");
            assert!(err.contains(g.echoed()), "{name}: {err}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the taxonomy, over real labels
// ---------------------------------------------------------------------------------------------

/// The junk-label capture, on its OWN axis and on the wrong one.
///
/// On the pnl axis it loses exactly the empty-string row and keeps all thirteen real labels —
/// including the genuine `unknown`, which the pnl ladder ADMITS because those wallets hold real
/// positions (omitting it once dropped 7.08% of the table out of a load). On the size axis every
/// single row is off-taxonomy and each is dropped under its FOLDED name, which is the property that
/// replaced alias resolution: an unrecognised label is counted by name, never folded onto a rung.
#[test]
fn the_junk_label_capture_drops_and_counts_its_junk_by_its_folded_name() {
    let page = decode(JUNK);
    guard_label_basis(&page, URL).expect("the junk capture is a real point_in_time body");
    let n = served(&page);

    let (rows, dropped) = rows_of(&page, Axis::Pnl);
    assert_eq!(
        dropped.get(""),
        Some(&1),
        "the empty-string label must be dropped BY NAME: {dropped:?}"
    );
    assert_eq!(dropped.len(), 1, "nothing else on the pnl ladder is junk: {dropped:?}");
    assert_eq!(rows.len(), n - 1);
    assert!(rows.iter().all(|r| is_known(Axis::Pnl, &r.cohort)));
    assert!(
        rows.iter().any(|r| r.cohort == normalize_cohort(Axis::Pnl, "unknown")),
        "the capture's genuine `unknown` row must SURVIVE the collector — the drop is a reader's"
    );

    // ...and the same body read as the SIZE ladder: nothing survives, everything is counted.
    let (rows, dropped) = rows_of(&page, Axis::Size);
    assert!(rows.is_empty(), "a pnl body has no size rung in it");
    assert_eq!(dropped.values().sum::<usize>(), n);
    assert_eq!(dropped.len(), n, "each label is a distinct drop; none were folded together");
    for folded in ["3xSmart", "Winner", "Rekt", "Unknown", ""] {
        assert_eq!(dropped.get(folded), Some(&1), "counted by its FOLDED name: {dropped:?}");
    }
    assert!(dropped.keys().all(|k| !SIZE_COHORTS.contains(&k.as_str())));
}

/// Every tier wire label in the capture maps to its own admitted slug, end to end.
///
/// The tier labels are the only ones carrying `$ < > - .` on the wire, and they become COLUMN NAMES
/// downstream — so the slug is not cosmetic. The capture is a full partition of the book (three
/// hours × all ten buckets), which is what lets the row count be asserted as `hours × ladder`.
#[test]
fn every_tier_wire_label_in_the_capture_maps_to_its_own_admitted_slug() {
    let page = decode(TIER_P1);
    let wire_labels: BTreeSet<&str> =
        page.metrics.as_deref().unwrap_or(&[]).iter().map(|r| r.cohort.as_str()).collect();
    assert_eq!(wire_labels.len(), TIER_COHORTS.len(), "the capture must carry the whole ladder");
    assert!(
        wire_labels.iter().any(|l| l.contains('$')),
        "the raw wire spelling must still be raw, or the slug step is not being exercised"
    );

    let (rows, dropped) = rows_of(&page, Axis::Tier);
    assert!(dropped.is_empty(), "the contract fixture carries no off-taxonomy label: {dropped:?}");
    let rows = dedupe_first_wins(rows);
    let hs = hours(&rows);
    assert_eq!(rows.len(), hs.len() * TIER_COHORTS.len(), "a tier bucket is missing an hour");
    let slugs: BTreeSet<&str> = rows.iter().map(|r| r.cohort.as_str()).collect();
    assert_eq!(slugs.len(), TIER_COHORTS.len(), "two wire labels collided onto one slug");
    for s in &slugs {
        assert!(TIER_COHORTS.contains(s), "{s} is not an admitted tier rung");
        assert!(
            s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "{s} is not column-safe"
        );
    }
    assert!(rows.iter().all(|r| r.axis == "tier"));
    assert!(rows.iter().all(|r| r.long_usd >= 0.0 && r.short_usd() >= 0.0));
}

/// An ELEVENTH label in a tier response loses that row BY NAME rather than growing a ghost rung.
///
/// Two probes, each a one-row mutation of the real body: a plausible extension of the ladder
/// (`$5m-$10m`, which slugs cleanly and is still not a rung) and a spelling from the RETIRED
/// five-label taxonomy (`Micro`). Both must land in `dropped` under their slug, with every survivor
/// on the ladder — if any alias-resolution step ever comes back, one of them reaches a rung and
/// this test names it.
#[test]
fn an_eleventh_label_in_the_tier_capture_is_dropped_and_counted_not_admitted() {
    for (raw, replacement, slug) in [
        ("\"cohort\": \">$2.5m\"", "\"cohort\": \"$5m-$10m\"", "5m_to_10m"),
        ("\"cohort\": \"<$1k\"", "\"cohort\": \"Micro\"", "micro"),
    ] {
        let body = TIER_P1.replacen(raw, replacement, 1);
        assert_ne!(body, TIER_P1, "the replace found no row to mutate — the probe is inert");
        let (rows, dropped) = rows_of(&decode(&body), Axis::Tier);
        assert_eq!(dropped.get(slug), Some(&1), "dropped by SLUG: {dropped:?}");
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(!is_known(Axis::Tier, slug), "{slug} is admitted; the taxonomy grew a rung");
        assert!(rows.iter().all(|r| TIER_COHORTS.contains(&r.cohort.as_str())));
    }
}
