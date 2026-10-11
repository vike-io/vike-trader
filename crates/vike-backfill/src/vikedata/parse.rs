//! Pure `data.vike.io` cohort-metrics decoding — no I/O, fixture-tested, the twin of
//! `crates/vike-backfill/src/tardis/parse.rs`.
//!
//! Everything a page has to survive before it may reach the store lives here: the JSON shape, the
//! two response guards, the per-axis label taxonomy, the notional snap, the seconds→milliseconds
//! conversion and the first-wins dedup. The rules are PORTED from
//! `crates/vike-research/src/sources/api.rs`, which paid for each of them one incident at a time
//! and is this module's DELETED predecessor — that crate dissolved and its fetch became this
//! directory, so every citation of that path here is provenance rather than a place to go and read
//! (`crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS` carries the row, and
//! `crates/vike-backfill/src/vikedata/mod.rs` states it once for the whole module). Where this
//! module deliberately differs from what it inherited, the difference is stated on the
//! item.
//!
//! # Two behaviours a Rust port gets wrong by default
//!
//! 1. **Python's `str.capitalize()` LOWERCASES the tail.** `"SHRIMP".capitalize()` is `"Shrimp"`. A
//!    port that upper-cases the first character and leaves the rest alone diverges the moment an
//!    unaliased mixed-case label appears — and then the taxonomy filter drops a real cohort.
//! 2. **`"".split()` in Python yields `[]`, so an empty label normalises to `""`.** The empty label
//!    is real (measured: 3 hours in 60 days) and must reach the taxonomy filter UNCHANGED so that
//!    filter is what drops it, visibly, by name.
//!
//! There is deliberately NO alias-resolution step: an unrecognised label case-folds, misses its
//! axis's taxonomy, and is dropped AND COUNTED under its folded name — never folded onto a rung.

use std::collections::BTreeMap;

use serde::Deserialize;
use vike_data::CohortRow;

use crate::error::CollectError;
use crate::vikedata::SECS_PER_HOUR;
use crate::vikedata::client::{Axis, CTX, Grading};

/// The 12 size rungs, already normalised. Order is the ladder's own (descending account value).
pub const SIZE_COHORTS: [&str; 12] = [
    "4xWhale",
    "3xWhale",
    "2xWhale",
    "Whale",
    "2xShark",
    "Shark",
    "2xDolphin",
    "Dolphin",
    "2xFish",
    "Fish",
    "2xShrimp",
    "Shrimp",
];

/// The 12 PnL rungs, already normalised (top percentile first).
pub const PNL_COHORTS: [&str; 12] = [
    "3xSmart", "2xSmart", "Smart", "3xWinner", "2xWinner", "Winner", "Loser", "2xLoser", "3xLoser",
    "Rekt", "2xRekt", "3xRekt",
];

/// The ladder's two spellings of the UNRANKABLE bucket, admitted on the pnl axis and only there.
/// `Neutral` is the old table's spelling; `Unknown` the current one (a wallet-PnL dictionary MISS
/// returns the default `unknown`). Omitting `Unknown` once dropped 6,607,011 rows — 7.08% of the
/// table — out of a load. It is NOT legacy junk.
pub const PNL_UNRANKABLE: [&str; 2] = ["Neutral", "Unknown"];

/// The 10 position-notional buckets, already normalised to their column-safe SLUGS (descending
/// notional). These are what [`normalize_cohort`] produces from the raw wire labels (`>$2.5m`,
/// `$1m-$2.5m`, … `<$1k`), which carry `$ < > - .` — characters no other cohort label has.
pub const TIER_COHORTS: [&str; 10] = [
    "above_2_5m",
    "1m_to_2_5m",
    "500k_to_1m",
    "250k_to_500k",
    "100k_to_250k",
    "50k_to_100k",
    "25k_to_50k",
    "10k_to_25k",
    "1k_to_10k",
    "below_1k",
];

/// What a stored row's `label_basis` says when the server echoed NO basis at all.
///
/// ⚠ Reachable on the size and tier axes ONLY: the pnl axis hard-fails an absent basis
/// ([`guard_label_basis`]), which is the whole point of that guard. An empty string is not used
/// instead, because `label_basis` is a commit-key SEGMENT
/// (`crates/vike-data/src/rec/cohort_rec.rs`'s `CohortFetch`) and an empty segment reads as a
/// truncated key rather than as a recorded fact.
///
/// ⚠ Known consequence, stated rather than discovered later: if the server later STARTS echoing a
/// basis on those axes, the key for an already-ingested window changes and that window re-ingests
/// under the new basis. The rows are then distinguishable by the column that changed, which is the
/// behaviour this kind's columns exist to give — but it is a re-ingest, not a no-op.
pub const LABEL_BASIS_UNSET: &str = "unset";

/// The only `labelBasis` this client ever asks for, and the only one it accepts on the pnl axis.
pub const POINT_IN_TIME: &str = "point_in_time";

/// One page of `/cohort-metrics`, verbatim wire shape.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct CohortPage {
    /// The basis the server RESOLVED, echoed back. See [`guard_label_basis`] — absent is a FAILURE
    /// on the pnl axis, not a pass.
    #[serde(default)]
    pub label_basis: Option<String>,
    /// The grading the server RESOLVED, echoed back. Absent from a server older than the parameter,
    /// which is why [`guard_grading`] treats absence as a failure only when a NON-default grading
    /// was asked for.
    #[serde(default)]
    pub grading: Option<String>,
    #[serde(default)]
    pub metrics: Option<Vec<MetricRow>>,
    #[serde(rename = "nextCursor", default)]
    pub next_cursor: Option<String>,
}

/// One hourly cohort marginal, verbatim wire shape.
///
/// The frozen (point-in-time) path returns `null` for fields it cannot serve, so every consumed
/// number is an `Option` — a missing `total_position_value` is a row to REFUSE, not a zero to
/// invent.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricRow {
    pub ts: String,
    pub cohort: String,
    pub total_position_value: Option<f64>,
    pub total_position_value_long: Option<f64>,
    #[serde(default)]
    pub label_basis: Option<String>,
}

/// Decode one page body. `url` rides the error because a decode failure is almost always a shape
/// change on ONE endpoint and the message has to say which.
pub fn parse_page(body: &str, url: &str) -> Result<CohortPage, CollectError> {
    serde_json::from_str(body).map_err(|e| CollectError::Fetch(format!("{CTX} decode {url}: {e}")))
}

/// **`labelBasis=point_in_time` is mandatory on the pnl axis, and a response that does not echo it
/// is a HARD FAILURE — absence included.**
///
/// The endpoint's default resolves a wallet's cohort from its realized-PnL dictionary AT QUERY
/// TIME, so an April row comes back carrying today's grading. That is lookahead, and it already
/// shipped once. The reason ABSENCE has to fail rather than pass: the server ignores unknown query
/// parameters, so a build predating the flag answers `200` with query-time labels and NO field at
/// all — and a guard written as "if the field is present and wrong" calls exactly that case fine.
///
/// Both halves are checked, envelope and rows: the API sets them in two separate statements, and
/// the ROWS are what gets stored.
pub fn guard_label_basis(page: &CohortPage, url: &str) -> Result<(), CollectError> {
    if page.label_basis.as_deref() != Some(POINT_IN_TIME) {
        return Err(CollectError::Fetch(format!(
            "{CTX} {url}: label_basis is {} — the pnl ladder must be read point-in-time, and an \
             ABSENT field is the dangerous case, not the safe one: the server ignores unknown \
             query parameters, so a build predating `labelBasis` answers 200 with QUERY-TIME \
             labels and no field. Those rows are lookahead.",
            match page.label_basis.as_deref() {
                Some(v) => format!("{v:?}"),
                None => "<field absent>".to_string(),
            }
        )));
    }
    for (i, r) in page.metrics.iter().flatten().enumerate() {
        if let Some(b) = r.label_basis.as_deref()
            && b != POINT_IN_TIME
        {
            return Err(CollectError::Fetch(format!(
                "{CTX} {url}: row {i} says label_basis={b:?} while the envelope says \
                     {POINT_IN_TIME:?} — the rows are what gets stored, so the envelope alone is \
                     not enough to trust"
            )));
        }
    }
    Ok(())
}

/// **The response must NAME the grading it served, and it must be the one that was asked for.**
///
/// The three gradings answer three different questions over the same asset and the same hours, and
/// their rows are SHAPE-IDENTICAL — same ladder, same two notionals. A server that answered a
/// `realized-pit` request with realized rows would fill a store nothing downstream can tell apart:
/// there is no arithmetic property that separates them.
///
/// Absence is a failure only for a NON-default grading, and the asymmetry is deliberate. A server
/// predating the parameter echoes no `grading` at all; for [`Grading::Realized`] that is the old
/// contract answering correctly, and refusing it would break every read against an un-upgraded
/// server for no gain. For the other two it is the exact dangerous case — the parameter was
/// ignored, realized rows came back, and silence is what the mistake looks like.
pub fn guard_grading(page: &CohortPage, url: &str, want: Grading) -> Result<(), CollectError> {
    match page.grading.as_deref() {
        Some(got) if got == want.echoed() => Ok(()),
        None if want == Grading::Realized => Ok(()),
        other => Err(CollectError::Fetch(format!(
            "{CTX} {url}: asked for grading={:?}, got {} — the three gradings return \
             shape-identical rows over the same hours, so a mismatch stores the wrong question's \
             answer under the right question's commit key",
            want.echoed(),
            match other {
                Some(v) => format!("{v:?}"),
                None => "<field absent — a server older than the grading parameter>".to_string(),
            }
        ))),
    }
}

/// Python's `str.capitalize()`: first character upper, EVERY OTHER character lower.
fn py_capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// The size/pnl case fold — the WHOLE normaliser on those axes. Splits on whitespace (Python's bare
/// `.split()`: any run, no empty parts), keeps a multiplier prefix (`2x`/`3x`/`4x`) lowercase,
/// `capitalize`s every other part, joins with no separator.
fn fold_label(label: &str) -> String {
    label
        .split_whitespace()
        .map(|p| {
            let digits = &p[..p.len().saturating_sub(1)];
            let is_mult = p.len() <= 3
                && p.ends_with('x')
                && !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit());
            if is_mult { p.to_string() } else { py_capitalize(p) }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The tier axis's normaliser: wire label → column-safe slug. A pure character fold, total over any
/// input: letters/digits lowercase, `<` → `below_`, `>` → `above_`, `-` → `_to_`, `.` → `_`, `$`
/// dropped, whitespace → `_`. The empty label folds to itself, so the taxonomy filter is what drops
/// it — exactly as on the other two axes.
fn tier_slug(label: &str) -> String {
    let mut out = String::with_capacity(label.len() + 8);
    for c in label.chars() {
        match c {
            '<' => out.push_str("below_"),
            '>' => out.push_str("above_"),
            '-' => out.push_str("_to_"),
            '.' => out.push('_'),
            '$' => {}
            c if c.is_whitespace() => out.push('_'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Wire label → the normalised label a row is stored under, axis-scoped and ALIAS-FREE. A label
/// matching nothing passes through as its fold so [`is_known`] can reject it BY NAME.
pub fn normalize_cohort(axis: Axis, label: &str) -> String {
    match axis {
        Axis::Size | Axis::Pnl => fold_label(label),
        Axis::Tier => tier_slug(label),
    }
}

/// The taxonomy filter, per axis. An unrecognised label would become a rung with a tiny population
/// that every later reader has to guess about; this is what rejects it. The pnl side ADMITS the
/// unrankable bucket ([`PNL_UNRANKABLE`]) — those wallets hold real positions.
pub fn is_known(axis: Axis, normalized: &str) -> bool {
    match axis {
        Axis::Size => SIZE_COHORTS.contains(&normalized),
        Axis::Pnl => PNL_COHORTS.contains(&normalized) || PNL_UNRANKABLE.contains(&normalized),
        Axis::Tier => TIER_COHORTS.contains(&normalized),
    }
}

/// How many significant decimal digits of a notional this collector keeps. Ten leaves cent
/// resolution on an eight-figure notional, and the noise this exists to remove lives six digits
/// below that. Safe only WITH [`MIDPOINT_NUDGE`] — see there.
pub const NOTIONAL_SIG_DIGITS: usize = 10;

/// The relative amount a notional is pulled TOWARD ZERO before rounding, so a value sitting exactly
/// on a grid midpoint has a deterministic side to fall on.
///
/// ⚠ **Rounding alone does not work, and the reason is specific to money.** The tempting argument —
/// the noise is ~4e-16 relative and the grid is 1e-9, so six orders of magnitude of headroom make a
/// straddle vanishingly unlikely — assumes a value sits UNIFORMLY inside its grid cell. These are
/// money: their decimal expansions TERMINATE, and they terminate disproportionately ON the grid's
/// own midpoints, where the smallest possible noise flips the rounding. Measured over 18,194
/// distinct notionals, the share within 1e-9 of a grid midpoint was 2.3% at ten digits — a spike,
/// not a tail. The witness, verbatim from two executions of one URL: `4960365.979499999` against
/// `4960365.9795`. The nudge moves the DECISION BOUNDARY off the midpoint, where the values pile
/// up, to `midpoint + 1e-13`, where nothing does: 250x above the noise so both spellings clear the
/// boundary together, 1,000x below the grid so the only rounding it biases is the knife edge.
pub const MIDPOINT_NUDGE: f64 = 1e-13;

/// One notional, snapped to [`NOTIONAL_SIG_DIGITS`] significant digits with the [`MIDPOINT_NUDGE`]
/// tie-break — applied once, at the parse boundary, before anything is derived from or stored with
/// the value.
///
/// **The endpoint does not answer one request the same way twice.** Measured on two back-to-back
/// GETs of a byte-identical URL: the `(ts, cohort)` key set was identical, 430 of 8,190 rows
/// carried a DIFFERENT value, and the largest relative deviation was 3.1e-16 — about 1.4 ULP. One
/// observed pair in full: `"total_position_value":95658388.82664996` against `95658388.82665`. The
/// aggregate is a parallel `sum()` server-side and its reduction order is not pinned, so the last
/// digits of a nine-figure notional are reduction noise rather than data.
///
/// ⚠ **Why this matters MORE here than in the study it was written for.** That client re-fetches
/// and re-derives; this one WRITES, and this store's idempotency is BATCH-level on the commit key
/// (`crates/vike-data/src/rec/cohort_rec.rs`). So the first write of a window is authoritative forever:
/// a second run of the same window is a silent no-op, and unsnapped noise is not repairable
/// afterwards by re-running anything.
///
/// SIGNIFICANT digits rather than decimal places, deliberately: cohort notionals span sub-dollar to
/// ten-figure, and a fixed number of decimals would either collapse the small end to zero or leave
/// the large end inside the noise. The render-and-reparse is not a trick — `{:.N$e}` emits the
/// mantissa with exactly `N` fractional digits, correctly rounded, and `str::parse` is correctly
/// rounded back. A non-finite value is returned untouched; a signed zero keeps its sign.
pub fn canonical_notional(v: f64) -> f64 {
    snap_with(v, NOTIONAL_SIG_DIGITS, MIDPOINT_NUDGE)
}

/// [`canonical_notional`] at a caller-chosen precision and nudge — the shape that lets the tests
/// demonstrate WHY each constant is what it is, by snapping a measured midpoint pair the way that
/// FAILS (`nudge = 0.0`) as well as the way that holds.
fn snap_with(v: f64, digits: usize, nudge: f64) -> f64 {
    if !v.is_finite() {
        return v;
    }
    // `unwrap_or(v)` is unreachable — `{:e}` of a finite f64 always parses — and is spelled rather
    // than `expect`ed because a canonicalisation must not be able to panic a fetch.
    format!("{:.prec$e}", v * (1.0 - nudge), prec = digits - 1).parse().unwrap_or(v)
}

/// One bucket label (`2026-08-09T13:00:00Z`) → whole unix SECONDS.
///
/// A deliberately NARROW parser rather than a datetime dependency: `crates/vike-model/src/time/mod.rs`
/// is this workspace's one home for calendar math, and `crates/vike-model/src/time/mod.rs`'s
/// `parse_date_label` records that consolidating there is what dropped vike-backfill's own datetime
/// crate. What is accepted is what this endpoint serves — `YYYY-MM-DDTHH:MM:SS`, an optional
/// all-zero fraction, and a UTC zone (`Z`, `z`, `+00:00`, `-00:00`, `+0000`). A NON-ZERO offset is
/// refused rather than converted: this endpoint serves UTC bucket labels, so an offset appearing
/// means the response shape changed, and silently shifting the hour would repartition the series.
///
/// Off-the-hour is refused for the same reason: the endpoint returns `toStartOfHour` buckets.
pub fn parse_hour(iso: &str) -> Result<i64, CollectError> {
    let bad = |why: &str| {
        CollectError::Fetch(format!(
            "{CTX}: ts {iso:?} — {why}. The endpoint returns UTC toStartOfHour bucket labels, so \
             this means the response shape changed"
        ))
    };
    // `YYYY-MM-DDTHH` is exactly 13 characters for a 4-digit year, which is every year this vendor
    // can serve; a wider year is a shape change and is refused with everything else.
    if iso.len() < 19 || !iso.is_char_boundary(13) {
        return Err(bad("too short to be a bucket label"));
    }
    let (head, tail) = iso.split_at(13);
    let Some((y, m, d, h)) = vike_model::time::parse_hour_label(head) else {
        return Err(bad("not a YYYY-MM-DDTHH label"));
    };
    let Some(rest) = tail.strip_prefix(":") else { return Err(bad("no :MM:SS after the hour")) };
    let (mm, rest) = rest.split_at(2);
    let Some(rest) = rest.strip_prefix(":") else { return Err(bad("no :SS after the minutes")) };
    if rest.len() < 2 {
        return Err(bad("no seconds"));
    }
    let (ss, mut zone) = rest.split_at(2);
    if mm != "00" || ss != "00" {
        return Err(bad("bucket label is not on the hour"));
    }
    if let Some(frac) = zone.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return Err(bad("a '.' with no fractional digits"));
        }
        if digits.bytes().any(|b| b != b'0') {
            return Err(bad("a non-zero sub-second fraction on an hourly bucket"));
        }
        zone = &zone[1 + digits.len()..];
    }
    if !matches!(zone, "Z" | "z" | "+00:00" | "-00:00" | "+0000" | "-0000") {
        return Err(bad("zone is not UTC (a non-zero offset would repartition the series)"));
    }
    Ok(vike_model::time::days_from_civil(y, m, d) * 86_400 + i64::from(h) * SECS_PER_HOUR)
}

/// Whole unix SECONDS on the hour → the epoch-MILLISECONDS `vike_data::CohortRow`'s `ts` is.
///
/// ⚠ **This is the conversion that puts the whole series in 1970 when it is forgotten**, and the
/// guard is why it is a function rather than a `* 1_000` at the call site. Every series codec and
/// the `date=` partitioner read that column as epoch-ms
/// (`crates/vike-model/src/time/mod.rs`'s `epoch_ms_to_utc_date` decides which partition a row lands
/// in), while this wire serves whole seconds: a row built the wrong way lands `1970-01-01`,
/// silently, and the batch commit key it landed under is spent, so the correct write of that window
/// becomes a no-op.
///
/// The on-the-hour check is deliberately a SECOND one — [`parse_hour`] already refuses an
/// off-the-hour STRING. This one refuses an off-the-hour INTEGER, which is a different input: it
/// guards every arithmetic path into a row's `ts`, including a caller that computed a bucket rather
/// than parsing one.
pub fn hour_ms(secs: i64) -> Result<i64, CollectError> {
    if secs.rem_euclid(SECS_PER_HOUR) != 0 {
        return Err(CollectError::Fetch(format!(
            "{CTX}: {secs} is not on the hour — `kind=cohort` stores hourly buckets and this value \
             would be filed under a `date=` partition it does not belong to"
        )));
    }
    secs.checked_mul(1_000)
        .ok_or_else(|| CollectError::Fetch(format!("{CTX}: {secs}s overflows epoch-milliseconds")))
}

/// Normalise every label, drop anything outside the axis's taxonomy (COUNTING each drop by its
/// folded name), snap both notionals, and build the store rows.
///
/// `label_basis` is stamped from the FETCH rather than from each row: [`guard_label_basis`] has
/// already refused any row that disagreed, and `crates/vike-data/src/rec/cohort_rec.rs`'s
/// `CohortRecorder` refuses a batch whose rows disagree with the key — so stamping is what makes
/// that second check structurally unreachable rather than merely unlikely.
///
/// Drops are returned as a `BTreeMap` (name-ordered) rather than an insertion-ordered map: this is a
/// log line, not a frame column, and this crate carries no `indexmap` dependency to reach for.
pub fn rows_from_metrics(
    asset: &str,
    axis: Axis,
    grading: Grading,
    label_basis: &str,
    raw: &[MetricRow],
) -> Result<(Vec<CohortRow>, BTreeMap<String, usize>), CollectError> {
    let mut out = Vec::with_capacity(raw.len());
    let mut dropped: BTreeMap<String, usize> = BTreeMap::new();
    for r in raw {
        let cohort = normalize_cohort(axis, &r.cohort);
        if !is_known(axis, &cohort) {
            *dropped.entry(cohort).or_insert(0) += 1;
            continue;
        }
        let ts = hour_ms(parse_hour(&r.ts)?)?;
        let (Some(total), Some(long)) = (r.total_position_value, r.total_position_value_long)
        else {
            return Err(CollectError::Fetch(format!(
                "{CTX}: {} @ {} — total_position_value / _long absent; a missing notional is a row \
                 to REFUSE, not a zero to invent (the frozen path returns null for fields it \
                 cannot serve)",
                r.cohort, r.ts
            )));
        };
        // ⚠ SNAPPED BEFORE ANYTHING IS DERIVED FROM OR CHECKED AGAINST THEM, so the stored pair and
        // every reader's `short = total - long` are functions of the canonical spelling rather than
        // of one of two spellings of it. See `canonical_notional`.
        let (total, long) = (canonical_notional(total), canonical_notional(long));
        // A total below the long side is a contradiction in the SOURCE, not a rounding artefact —
        // but float noise on a nine-figure notional is real, so only a MATERIAL negative stops the
        // run. The snap above is six orders of magnitude below this threshold and cannot move the
        // verdict.
        //
        // ⚠ A sub-threshold negative is kept AS IT CAME, and this producer deliberately cannot do
        // what its predecessor
        // `crates/vike-research/src/sources/api.rs`'s `normalize_filter_and_derive` did with
        // one (clamp the derived short to zero): that type stored the SHORT side, this kind stores
        // the PAIR, and clamping here would mean writing a `long_usd` or a `total_usd` the wire
        // never served. `crates/vike-data/src/cohort_log.rs`'s `short_usd` is written for exactly
        // this — it returns whatever the subtraction gives, "including a negative", and says the
        // producer is where a MATERIAL one is refused.
        if total - long < -1e-6 * total.abs() {
            return Err(CollectError::Fetch(format!(
                "{CTX}: {} @ {} — total_position_value {total} is LESS than \
                 total_position_value_long {long}, so the derived short side is negative",
                r.cohort, r.ts
            )));
        }
        out.push(CohortRow {
            ts,
            asset: asset.to_string(),
            axis: axis.as_str().to_string(),
            cohort,
            grading: grading.echoed().to_string(),
            label_basis: label_basis.to_string(),
            long_usd: long,
            total_usd: total,
        });
    }
    Ok((out, dropped))
}

/// Pages arrive newest-first and OVERLAP, so a `(ts, cohort)` seen twice across a page boundary
/// keeps the FIRST occurrence — never a sum, which would silently double one hour's notional.
/// Then sort ascending by `(ts, cohort)`, the order a store scan reads back.
pub fn dedupe_first_wins(rows: Vec<CohortRow>) -> Vec<CohortRow> {
    let mut seen: std::collections::HashSet<(i64, String)> = std::collections::HashSet::new();
    let mut kept: Vec<CohortRow> =
        rows.into_iter().filter(|r| seen.insert((r.ts, r.cohort.clone()))).collect();
    kept.sort_by(|a, b| (a.ts, &a.cohort).cmp(&(b.ts, &b.cohort)));
    kept
}

#[path = "parse_tests.rs"]
#[cfg(test)]
mod parse_tests;
