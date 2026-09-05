//! Replays the FROZEN oracle export `fixtures/hl_cohort/loader.json` through the REAL
//! `data.vike.io` COLLECTOR and gates that the rows it would write into `kind=cohort` reproduce
//! the oracle's own two axes BIT FOR BIT.
//!
//! # What this replaced, and why the claim moved here
//!
//! It replaces `crates/vike-research/tests/parity_loader.rs`, which drove the same frozen bytes
//! through that crate's own HTTP loader (`CohortClient` over a fixture transport, then
//! `by_size`/`by_pnl`). That loader machinery has moved into the core as the data.vike.io VENDOR
//! collector — `crates/vike-backfill/src/vikedata/parse.rs` — under
//! `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`: **a study reads the STORE,
//! the collector owns the WIRE.** The oracle's evidence is evidence about the WIRE — label
//! normalisation, the two junk-label filters, the derived short side — so it belongs where the
//! wire is decoded, and it is CARRIED here rather than deleted with the crate that used to hold it.
//!
//! # What is replayed, and through which door
//!
//! The fixture's `raw.{size,pnl}` are the endpoint's own rows — raw lowercase labels,
//! `total_position_value` / `total_position_value_long`, floats as IEEE-754 hex. They are
//! re-serialised into the endpoint's own page envelope and pushed through the SHIPPED pure path:
//! `parse_page` -> `guard_label_basis` (pnl axis) -> `rows_from_metrics` (normalise, filter, snap,
//! seconds->milliseconds) -> `dedupe_first_wins`. Nothing here reimplements a step and compares
//! two copies of the same arithmetic.
//!
//! The one thing the old file could reach and this one cannot is the study's HTTP client, which is
//! deliberate: the collector's transport is `crates/vike-backfill/src/vikedata/client.rs` and it is
//! not on this path at all. What is gated here is the DECODE, which is where every rule the oracle
//! paid for actually lives.
//!
//! ⚠ The JSON hop is exact and is not a tolerance. `serde_json` is pinned with `float_roundtrip`
//! (correctly-rounded parse) in the root `Cargo.toml` and serialises through ryu (shortest
//! round-tripping form), so a value that goes out as `f64` comes back bit-identical — and
//! `the_collector_reproduces_the_oracles_two_axes_bit_for_bit` would fail on the first `long_usd`
//! if it did not, since the oracle's `long_usd` IS the raw `total_position_value_long`.
//!
//! # Gate against the committed bytes; NEVER regenerate and diff
//!
//! The pnl axis of `/cohort-metrics` is not bit-reproducible between two identical fetches (the
//! fixture's own `manifest.source_stability` measured 20,196 of 80,704 cells moving by up to 3 ULP
//! in one re-fetch). The fixture's `raw` and `expected` come from ONE fetch and the exporter
//! re-derived `expected` from the serialised `raw` before writing, so the FILE is internally
//! consistent and replaying it must be exact. A regenerated copy is not comparable to it, and no
//! exporter survives in this tree to regenerate one with:
//! `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is the verdict, and it
//! FENCES the one relaxation this file carries (see `as_this_port_spells_it` and `SNAP_GRID`).
//!
//! `fixtures/hl_cohort/README.md` is the authority on the export command, on the tokens and window
//! the capture covers, and on the `source_stability` measurement quoted above — restated nowhere
//! else, this file included. ⚠ Since `crates/vike-research/` dissolved this is the LAST in-repo
//! consumer of that directory's `loader.json` and of the README beside it. The directory's other
//! two exports — the feature-matrix one and the signal one, deliberately NOT named here, because
//! naming a file beside a `fixtures/<dir>` mention is how
//! `crates/vike-ops/tests/fixture_consumption_gate.rs` counts a CONSUMER — left with the study and
//! carry a written row in that gate's `ORPHANED_EXCEPTIONS` saying so.
//!
//! # The ONE deliberate difference: the `Unknown` drop is the READER's now, not the wire's
//!
//! The oracle dropped the unrankable bucket from the pnl axis inside its loader. This collector
//! does NOT: `crates/vike-backfill/src/vikedata/parse.rs`'s `PNL_UNRANKABLE` admits `Unknown` on
//! the pnl ladder, so the row is STORED — those wallets hold real positions, and dropping them at
//! collect time would make the size axis's open interest unrecoverable from the tape. The drop now
//! happens one layer up, at read time, in the study's own store reader
//! (`user_data/research/studies/rust/cohort/store.rs`'s `cohorts`, which drops it on the pnl axis
//! and only there). So the pnl comparison below filters `Unknown` out of the COLLECTOR's rows
//! before comparing, and asserts the count it removed against the manifest's own measurement — the
//! difference is stated and gated, never absorbed.
//!
//! # Three of the original file's premises were wrong, and the export measured them
//!
//! `manifest.adversarial_cases` is the AUTHORITY for what the window contains, and those
//! measurements are the reason the tests below are shaped the way they are. Each is re-asserted at
//! the test it rewrote, so a fixture that stopped carrying the case fails as a fixture problem
//! rather than passing vacuously:
//!
//! * lowercase `unknown` is on the **pnl** axis and has never been on the size axis
//!   (`the_lowercase_unknown_label_is_filtered_from_the_pnl_axis`);
//! * the empty-string label is REAL, six rows of it (`the_empty_string_label_is_filtered_out`);
//! * `3x smart` is never individually absent, and the only realisation of "missing" is an hour
//!   whose pnl axis has no rows at all
//!   (`an_hour_with_no_pnl_rows_at_all_reaches_the_store_as_no_rows`).

#![cfg(feature = "vikedata")]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use vike_backfill::vikedata::client::{Axis, Grading};
use vike_backfill::vikedata::parse::{
    LABEL_BASIS_UNSET, PNL_COHORTS, PNL_UNRANKABLE, POINT_IN_TIME, SIZE_COHORTS,
    canonical_notional, dedupe_first_wins, guard_label_basis, is_known, normalize_cohort,
    parse_page, rows_from_metrics,
};
use vike_data::CohortRow;

/// The URL every decode error in this file would name. The replay opens no socket — this is the
/// `url` parameter `parse_page` / `guard_label_basis` put in their messages, and it is spelled as
/// the real endpoint so a failure here reads like a failure in production.
const URL: &str = "https://data.vike.io/v1/hyperliquid/coins/REPLAY/cohort-metrics";

/// The unrankable bucket's CURRENT spelling — what `normalize_cohort(Axis::Pnl, "unknown")`
/// produces, and one of the two `crates/vike-backfill/src/vikedata/parse.rs`'s `PNL_UNRANKABLE`
/// admits. (`Neutral` is the old table's spelling and does not occur in this window.)
const UNRANKABLE: &str = "Unknown";

/// The largest relative move the snap can make: half a grid step at
/// `crates/vike-backfill/src/vikedata/parse.rs`'s `NOTIONAL_SIG_DIGITS`, rounded up to the step
/// itself so the bound needs no arithmetic to read. Anything the collector does WRONG moves a
/// notional by orders of magnitude more.
///
/// ⚠ Spelled here rather than derived from the constant, deliberately: this file is the OUTSIDE
/// view of the collector, and a bound computed from the very constant it is checking would follow a
/// wrong value silently. Widening it needs the same evidence the constant's own doc carries.
const SNAP_GRID: f64 = 1e-9;

// ---------------------------------------------------------------------------------------------
// fixture access
// ---------------------------------------------------------------------------------------------

/// `fixtures/hl_cohort/loader.json`, resolved from the crate manifest rather than the working
/// directory — `crates/vike-backfill` is two levels below the repo root.
///
/// `env!` is a COMPILE-TIME macro, not `env::var`, so this does not trip
/// `crates/vike-ops/tests/settings_registry.rs`.
fn fixture() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/hl_cohort/loader.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()))
}

/// One f64 out of the fixture's IEEE-754 hex bit pattern. `null` is a missing value, i.e. NaN —
/// never `0.0`, which is a number the source did not report.
fn bits(v: &Value) -> f64 {
    match v {
        Value::Null => f64::NAN,
        Value::String(s) => f64::from_bits(
            u64::from_str_radix(s, 16).unwrap_or_else(|e| panic!("bad float hex {s:?}: {e}")),
        ),
        other => panic!("expected a hex float string or null, got {other}"),
    }
}

fn hex(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}

fn as_i64(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("expected an integer, got {v}"))
}

fn as_str(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("expected a string, got {v}"))
}

/// One whole unix SECOND on the hour, rendered as the endpoint's own `ts` spelling
/// (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// ⚠ Spelled from `crates/vike-model/src/time.rs`'s `civil_from_days` rather than from a datetime
/// crate: `crates/vike-backfill/src/vikedata/client.rs`'s `fmt_start` records that consolidating
/// calendar math there is what dropped vike-backfill's own datetime dependency, and a test is not a
/// reason to re-add one. It is checked against an INDEPENDENT oracle — the manifest's own
/// `window_start_iso` / `window_end_iso`, inside the frozen bytes — by
/// `the_replays_timestamp_spelling_matches_the_manifests_own_window_labels`.
fn rfc3339_hour(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = vike_model::time::civil_from_days(days);
    let (hh, mm, ss) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// The assets the fixture covers, in the manifest's order — which is also the order its `expected`
/// arrays are laid out in, since each asset was a separate fetch.
fn tokens(fx: &Value) -> Vec<String> {
    fx["manifest"]["tokens"]
        .as_array()
        .expect("manifest.tokens")
        .iter()
        .map(|t| as_str(t).to_string())
        .collect()
}

/// The fixture key for one axis. The parity export predates the tier axis (the oracle has none), so
/// no fixture carries a `raw.tier` array — reaching that arm means a parity test asked the frozen
/// oracle export for an axis the oracle never had.
fn axis_key(axis: Axis) -> &'static str {
    match axis {
        Axis::Size => "size",
        Axis::Pnl => "pnl",
        Axis::Tier => unreachable!("the frozen oracle export has no tier axis"),
    }
}

// ---------------------------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------------------------

/// The raw rows for one asset on one axis, re-serialised as the endpoint's own page shape.
///
/// `label_basis` comes from the fixture rather than being spelled here: an export that recorded a
/// different basis must fail the pnl axis's guard loudly instead of being quietly overwritten.
fn page_body(fx: &Value, axis: Axis, asset: &str) -> String {
    let key = axis_key(axis);
    let metrics: Vec<Value> = fx["raw"][key]
        .as_array()
        .unwrap_or_else(|| panic!("raw.{key}"))
        .iter()
        .filter(|r| as_str(&r["asset"]) == asset)
        .map(|r| {
            let total = bits(&r["total_position_value"]);
            let long = bits(&r["total_position_value_long"]);
            assert!(
                total.is_finite() && long.is_finite(),
                "a non-finite notional cannot survive the JSON hop (serde_json writes NaN as null)"
            );
            json!({
                "ts": rfc3339_hour(as_i64(&r["ts"])),
                "cohort": r["cohort"],
                "total_position_value": total,
                "total_position_value_long": long,
            })
        })
        .collect();
    json!({
        "axis": key,
        "label_basis": fx["raw"]["label_basis"],
        "nextCursor": Value::Null,
        "metrics": metrics,
    })
    .to_string()
}

/// What one asset's replay produced: the store rows, and the taxonomy filter's drop counts.
struct Replayed {
    rows: Vec<CohortRow>,
    dropped: BTreeMap<String, usize>,
}

/// One asset's raw rows through the SHIPPED pure path.
fn replay(fx: &Value, axis: Axis, asset: &str) -> Replayed {
    let body = page_body(fx, axis, asset);
    let page =
        parse_page(&body, URL).unwrap_or_else(|e| panic!("{asset} {} axis: {e}", axis.as_str()));
    if axis == Axis::Pnl {
        // The pnl ladder must be read point-in-time, and an ABSENT basis is the dangerous case.
        // The fixture records `point_in_time`, so this passes — and an export that recorded
        // anything else fails HERE rather than being silently replayed as lookahead.
        guard_label_basis(&page, URL).unwrap_or_else(|e| panic!("{asset} pnl axis: {e}"));
    }
    let basis = page.label_basis.as_deref().unwrap_or(LABEL_BASIS_UNSET).to_string();
    let metrics = page.metrics.as_deref().unwrap_or(&[]);
    let (rows, dropped) = rows_from_metrics(asset, axis, Grading::Realized, &basis, metrics)
        .unwrap_or_else(|e| panic!("{asset} {} axis: {e}", axis.as_str()));
    Replayed { rows: dedupe_first_wins(rows), dropped }
}

/// Every asset's replayed rows, concatenated in manifest-token order — the layout the oracle's
/// `expected` arrays have, since each asset was a separate fetch.
fn collected(fx: &Value, axis: Axis) -> Vec<CohortRow> {
    let mut out = Vec::new();
    for asset in tokens(fx) {
        out.extend(replay(fx, axis, &asset).rows);
    }
    out
}

/// One expected row: `(asset, ts SECONDS, cohort, long_usd bits, short_usd bits)`.
fn expected(fx: &Value, key: &str) -> Vec<(String, i64, String, u64, u64)> {
    fx["expected"][key]
        .as_array()
        .unwrap_or_else(|| panic!("expected.{key}"))
        .iter()
        .map(|r| {
            (
                as_str(&r["asset"]).to_string(),
                as_i64(&r["ts"]),
                as_str(&r["cohort"]).to_string(),
                bits(&r["long_usd"]).to_bits(),
                bits(&r["short_usd"]).to_bits(),
            )
        })
        .collect()
}

/// The `(asset, ts SECONDS, normalised cohort)` -> `(total, long)` map the derivation is checked
/// against. Takes the AXIS rather than the fixture key: normalisation is axis-scoped, so the key it
/// derives and the normaliser it applies cannot disagree.
fn raw_index(fx: &Value, axis: Axis) -> HashMap<(String, i64, String), (f64, f64)> {
    let key = axis_key(axis);
    fx["raw"][key]
        .as_array()
        .unwrap_or_else(|| panic!("raw.{key}"))
        .iter()
        .map(|r| {
            (
                (
                    as_str(&r["asset"]).to_string(),
                    as_i64(&r["ts"]),
                    normalize_cohort(axis, as_str(&r["cohort"])),
                ),
                (bits(&r["total_position_value"]), bits(&r["total_position_value_long"])),
            )
        })
        .collect()
}

/// One raw `(total, long)` pair as THIS COLLECTOR now spells it: both notionals snapped at the
/// parse boundary, then the short side derived from the snapped pair — the order
/// `crates/vike-backfill/src/vikedata/parse.rs`'s `rows_from_metrics` uses.
///
/// # ⚠ The one place this port deliberately diverges from the oracle, and why it is here
///
/// The oracle passes `total_position_value_long` through untouched. This collector snaps it,
/// because **the endpoint does not answer one request the same way twice** —
/// `crates/vike-backfill/src/vikedata/parse.rs`'s `canonical_notional` carries the measurement, and
/// this fixture's own `manifest.source_stability` recorded the same instability (20,196 of 80,704
/// cells moving by up to 3 ULP in one re-fetch). It matters MORE here than in the study it was
/// written for: this store's idempotency is BATCH-level on the commit key, so the first write of a
/// window is authoritative forever and unsnapped noise is not repairable by re-running anything.
///
/// The gate stays EXACT. It compares bits against a STATED FUNCTION of the oracle's own values
/// rather than against the oracle's values raw; nothing is compared with a tolerance, the committed
/// fixture bytes are untouched, and deleting this composition is how you get the old assertion
/// back. `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is where that fence
/// is argued, and all four of its conditions are carried below: exact comparison, the oracle's raw
/// value still bounded by [`SNAP_GRID`], a control that fails if the snap is a no-op on this
/// fixture, and untouched bytes.
///
/// ⚠ **No `max(0.0)` clamp, unlike the study's spelling of the same derivation.** This producer
/// stores the PAIR and deliberately cannot clamp — clamping would mean writing a `long_usd` or a
/// `total_usd` the wire never served (`crates/vike-backfill/src/vikedata/parse.rs`'s
/// `rows_from_metrics` says so at the check it does make). On these bytes the two spellings cannot
/// disagree, and that is asserted rather than assumed:
/// `short_usd_is_derived_from_the_total_and_the_long_side` requires every derived short to be
/// strictly positive.
fn as_this_port_spells_it(total: f64, long: f64) -> (f64, f64) {
    let (t, l) = (canonical_notional(total), canonical_notional(long));
    (l, t - l)
}

// ---------------------------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------------------------

/// The replay's `ts` spelling is checked against the fixture's OWN ISO labels before anything is
/// built on it. Without this the whole replay could be an hour out and every comparison below would
/// still be internally consistent, because both sides would come from the same helper.
#[test]
fn the_replays_timestamp_spelling_matches_the_manifests_own_window_labels() {
    let fx = fixture();
    for (secs_key, iso_key) in
        [("window_start", "window_start_iso"), ("window_end", "window_end_iso")]
    {
        let secs = as_i64(&fx["manifest"][secs_key]);
        assert_eq!(
            rfc3339_hour(secs),
            as_str(&fx["manifest"][iso_key]),
            "{secs_key}: the replay's ts spelling disagrees with the fixture's own label"
        );
    }
}

/// THE gate: every collected row, bit for bit, against the oracle's own two axes — composed with
/// the ONE declared divergence, the parse-boundary snap [`as_this_port_spells_it`] documents, and
/// with the ONE deliberate behavioural difference (the `Unknown` drop, now the reader's) stated as
/// a counted filter rather than absorbed.
///
/// The row counts are checked against `manifest.counts` first, so a fixture that silently shrank
/// fails as a fixture problem rather than as 120 passing comparisons over 3 rows.
#[test]
fn the_collector_reproduces_the_oracles_two_axes_bit_for_bit() {
    let fx = fixture();
    let counts = &fx["manifest"]["counts"];
    assert_eq!(fx["raw"]["size"].as_array().unwrap().len() as i64, as_i64(&counts["raw_size"]));
    assert_eq!(fx["raw"]["pnl"].as_array().unwrap().len() as i64, as_i64(&counts["raw_pnl"]));
    let n_unknown =
        as_i64(&fx["manifest"]["adversarial_cases"]["lowercase_unknown"]["count"]) as usize;
    assert!(PNL_UNRANKABLE.contains(&UNRANKABLE), "the collector must still admit the bucket");

    for (axis, key) in [(Axis::Size, "by_size"), (Axis::Pnl, "by_pnl")] {
        let mut got = collected(&fx, axis);
        // ⚠ THE ONE DELIBERATE DIFFERENCE, applied here and nowhere else. `rows_from_metrics` keeps
        // `Unknown` on the pnl ladder (`crates/vike-backfill/src/vikedata/parse.rs`'s
        // `PNL_UNRANKABLE` admits it) because those wallets hold real positions and the size axis's
        // open interest is unrecoverable from a tape that dropped them. The oracle dropped them in
        // its loader; the study now drops them at READ time
        // (`user_data/research/studies/rust/cohort/store.rs`'s `cohorts`). So the wire keeps them,
        // the reader loses them, and the count removed here is checked against the manifest rather
        // than taken on trust.
        if axis == Axis::Pnl {
            let before = got.len();
            got.retain(|r| r.cohort != UNRANKABLE);
            assert_eq!(
                before - got.len(),
                n_unknown,
                "the pnl replay must carry exactly the manifest's unrankable rows into the store"
            );
        } else {
            assert!(
                got.iter().all(|r| r.cohort != UNRANKABLE),
                "measured: the unrankable bucket has never been on the size axis"
            );
        }

        let want = expected(&fx, key);
        let raw = raw_index(&fx, axis);
        assert_eq!(
            want.len() as i64,
            as_i64(&counts[key]),
            "{key}: the fixture disagrees with its own manifest"
        );
        assert_eq!(got.len(), want.len(), "{key}: row count");
        let mut snapped_rows = 0usize;
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            // ⚠ The store row's `ts` is epoch MILLISECONDS —
            // `crates/vike-backfill/src/vikedata/parse.rs`'s `hour_ms` is the multiplication, and
            // the defect it exists to prevent puts the whole series in 1970 — while the oracle's
            // `expected.ts` is whole unix SECONDS. The factor is spelled INTO the key assertion
            // rather than divided out of it, so a row that arrived unmultiplied fails here instead
            // of matching after a rounding.
            assert_eq!(g.ts % 1_000, 0, "{key} row {i}: a store ts is a whole millisecond");
            assert_eq!(
                (g.asset.as_str(), g.ts, g.cohort.as_str()),
                (w.0.as_str(), w.1 * 1_000, w.2.as_str()),
                "{key} row {i}: key",
            );
            // ...and the three per-FETCH columns the oracle had no schema for, stamped from the
            // fetch rather than from the row (`rows_from_metrics`'s own contract).
            assert_eq!(g.axis, axis.as_str(), "{key} row {i}: axis column");
            assert_eq!(g.grading, Grading::Realized.echoed(), "{key} row {i}: grading column");
            assert_eq!(g.label_basis, POINT_IN_TIME, "{key} row {i}: label_basis column");

            let (total, long) = raw[&(g.asset.clone(), g.ts / 1_000, g.cohort.clone())];
            let (want_long, want_short) = as_this_port_spells_it(total, long);
            assert_eq!(
                g.long_usd.to_bits(),
                want_long.to_bits(),
                "{key} row {i} ({} {} {}): long_usd {} != {}",
                g.asset,
                g.ts,
                g.cohort,
                hex(g.long_usd),
                hex(want_long),
            );
            assert_eq!(
                g.short_usd().to_bits(),
                want_short.to_bits(),
                "{key} row {i} ({} {} {}): short_usd {} != {}",
                g.asset,
                g.ts,
                g.cohort,
                hex(g.short_usd()),
                hex(want_short),
            );
            // ...and the ORACLE's own stored value is still gated, as the pre-image of the snap:
            // one grid step is the most the canonicalisation can move a notional, so every failure
            // this test was built for is still far outside the bound.
            //
            // ⚠ The reference magnitude is what the snap ACTED ON, not what it produced. `long` is
            // snapped once and carries its own grid step. `short` is a CANCELLATION of two snapped
            // inputs, so its error is inherited from `total`: a short side that is a few percent of
            // the total carries the TOTAL's grid step, which is a large multiple of its own.
            // Bounding it against `short` instead is how the first draft of this loop failed on a
            // row that was, in fact, exactly right.
            for (got_v, oracle_bits, reference, what) in [
                (g.long_usd, w.3, long.abs(), "long_usd"),
                (g.short_usd(), w.4, total.abs(), "short_usd"),
            ] {
                let oracle = f64::from_bits(oracle_bits);
                let moved = (got_v - oracle).abs();
                assert!(
                    moved <= SNAP_GRID * reference,
                    "{key} row {i} ({} {} {}): {what} moved {moved:e} from the oracle's {}, which \
                     is more than the snap of a {reference:e} input can account for",
                    g.asset,
                    g.ts,
                    g.cohort,
                    hex(oracle),
                );
            }
            if g.long_usd.to_bits() != w.3 || g.short_usd().to_bits() != w.4 {
                snapped_rows += 1;
            }
        }
        // The control this gate would otherwise lose: if the snap were a no-op on this fixture,
        // every assertion above would still pass and the composition would be proving nothing. The
        // endpoint's noise is REAL in these committed bytes, and this is where that shows.
        assert!(
            snapped_rows > 0,
            "{key}: the snap moved no row at all — this fixture can no longer tell the composed \
             gate from the raw one, so it is not evidence for either"
        );
    }
}

/// ⚠ REWRITTEN AGAINST MEASURED DATA in the file this replaces, and the correction is carried with
/// it. The claim was once "filtered from the SIZE axis, 11 hours in 60 days" and could not be
/// written.
///
/// `manifest.adversarial_cases.lowercase_unknown` measured the opposite and carries the forensics:
/// lowercase `unknown` lives on the **pnl** axis (`on_pnl_axis: 10`, one per pnl hour in this
/// window and present in every hour of the endpoint's full retained history) and has NEVER appeared
/// on the size axis (`on_size_axis: 0`).
///
/// The point that survives the correction and is worth gating: after normalisation the raw
/// dictionary-MISS value `unknown` and the documented `Unknown` unrankable bucket are the SAME
/// STRING, so only the AXIS distinguishes them — and the axis taxonomies disagree about it.
#[test]
fn the_lowercase_unknown_label_is_filtered_from_the_pnl_axis() {
    let fx = fixture();
    let case = &fx["manifest"]["adversarial_cases"]["lowercase_unknown"];
    assert!(case["present"].as_bool() == Some(true), "the fixture must contain the case at all");
    assert_eq!(as_i64(&case["on_size_axis"]), 0, "the manifest is the authority: never on size");
    let on_pnl = as_i64(&case["on_pnl_axis"]);
    assert!(on_pnl > 0);

    // The raw window agrees with its own manifest — a count, not a claim.
    let count = |key: &str| {
        fx["raw"][key]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| as_str(&r["cohort"]) == "unknown")
            .count() as i64
    };
    assert_eq!(count("pnl"), on_pnl);
    assert_eq!(count("size"), 0);

    // One string, two axes, two answers. (Both axes run the same bare case fold — there is no alias
    // step — so the normalised form is the same on both and only the taxonomies disagree about it.)
    assert_eq!(normalize_cohort(Axis::Pnl, "unknown"), UNRANKABLE);
    assert_eq!(normalize_cohort(Axis::Size, "unknown"), UNRANKABLE);
    assert_eq!(normalize_cohort(Axis::Pnl, "Unknown"), UNRANKABLE);
    assert!(is_known(Axis::Pnl, UNRANKABLE), "the pnl taxonomy admits it");
    assert!(!is_known(Axis::Size, UNRANKABLE), "the size taxonomy has no such rung");

    // ...and the collector acts on that difference: the pnl replay KEEPS it (it is not in
    // `dropped`), so the row reaches the store.
    let replayed = replay(&fx, Axis::Pnl, &tokens(&fx)[0]);
    assert!(
        !replayed.dropped.contains_key(UNRANKABLE),
        "the taxonomy filter must NOT be what removes it: {:?}",
        replayed.dropped
    );
    assert!(
        replayed.rows.iter().any(|r| r.cohort == UNRANKABLE),
        "it survived the decode and would be written"
    );
    // ⚠ And the ORACLE does not carry it, which is the whole of the deliberate difference: the
    // oracle's loader dropped it, this collector stores it, and the drop now belongs to the study's
    // store reader (`user_data/research/studies/rust/cohort/store.rs`'s `cohorts`).
    assert!(expected(&fx, "by_pnl").iter().all(|r| r.2 != UNRANKABLE));
}

/// The empty-string label is REAL — six rows, measured.
///
/// ⚠ The older claim in `crates/vike-backfill/tests/fixtures/vikedata/PROVENANCE.md`, that an
/// exhaustive search found none, was a WINDOW ARTEFACT: that search started 2026-06-10, after the
/// 2026-04-03 cutover that replaced `''` with the ladder names. This fixture's window straddles the
/// cutover, which is why it has them. That file carries its own correction.
///
/// An empty label would pivot into a study column literally named `bias_` and correlate with
/// nothing, and here it would become a stored `cohort` column nothing downstream can interpret.
#[test]
fn the_empty_string_label_is_filtered_out() {
    let fx = fixture();
    let case = &fx["manifest"]["adversarial_cases"]["empty_string_label"];
    let n = as_i64(&case["count"]);
    assert!(n > 0, "the fixture must contain the case or this test proves nothing");
    assert_eq!(
        fx["raw"]["size"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| as_str(&r["cohort"]).is_empty())
            .count() as i64,
        n
    );
    assert_eq!(normalize_cohort(Axis::Size, ""), "", "it must reach the taxonomy filter unchanged");
    assert!(!is_known(Axis::Size, ""), "and the filter is what refuses it");

    // The collector drops it BY NAME and counts it, per asset.
    let mut dropped = 0i64;
    for asset in tokens(&fx) {
        let replayed = replay(&fx, Axis::Size, &asset);
        dropped += *replayed.dropped.get("").unwrap_or(&0) as i64;
        assert!(replayed.rows.iter().all(|r| !r.cohort.is_empty()));
    }
    assert_eq!(dropped, n, "every empty-label row must be accounted for by name");
    // ...and the oracle agrees: no such row reached its output either.
    assert!(expected(&fx, "by_size").iter().all(|r| !r.2.is_empty()));
}

/// `Unknown` reaches the store from the pnl axis and from nowhere else.
///
/// ⚠ CORRECTED, and the correction is carried from the file this replaces. The original plan asked
/// for `assert!(size_cohorts.contains("Unknown"))`, and that is unwritable twice over:
/// `manifest.adversarial_cases.unknown_on_both_axes` proves no `unknown` row has ever existed on
/// the size axis (13 `size_cohort` groups over the whole table — the 12 ladder names and `''`), and
/// this port's own size taxonomy refuses the label besides.
///
/// The rule the oracle's 7.92%-of-open-interest measurement is really about is the one asserted
/// here. In the OLD shape it was "the drop belongs to `by_pnl` alone", enforced by three functions
/// that each refused the wrong axis. In THIS shape the axis is a per-ROW column and the taxonomy is
/// the only gate, so the equivalent structural statement is the pair of taxonomy answers: the size
/// ladder cannot admit the bucket, the pnl ladder must, and therefore the store carries it on
/// exactly one axis and a reader is the only thing that can remove it.
#[test]
fn unknown_is_dropped_from_the_pnl_axis_and_only_there() {
    let fx = fixture();
    let case = &fx["manifest"]["adversarial_cases"]["lowercase_unknown"];
    let n_unknown = as_i64(&case["count"]);
    assert_eq!(
        fx["manifest"]["adversarial_cases"]["unknown_on_both_axes"]["present"].as_bool(),
        Some(false),
        "the manifest's own forensics: no hour carries the label on both axes"
    );

    let mut kept_by_collect = 0usize;
    for asset in tokens(&fx) {
        let replayed = replay(&fx, Axis::Pnl, &asset);
        kept_by_collect += replayed.rows.iter().filter(|r| r.cohort == UNRANKABLE).count();
    }
    assert_eq!(kept_by_collect as i64, n_unknown, "the collector must keep every one of them");

    // The oracle's rows are the collector's MINUS exactly those, and nothing else.
    let pnl_rows = collected(&fx, Axis::Pnl);
    assert_eq!(
        expected(&fx, "by_pnl").len() + kept_by_collect,
        pnl_rows.len(),
        "the reader's drop must account for the whole difference between the two"
    );

    let stored_pnl: BTreeSet<&str> = pnl_rows.iter().map(|r| r.cohort.as_str()).collect();
    let size_rows = collected(&fx, Axis::Size);
    let stored_size: BTreeSet<&str> = size_rows.iter().map(|r| r.cohort.as_str()).collect();
    assert!(stored_pnl.contains(UNRANKABLE), "the wire's answer survives into the store");
    assert!(!stored_size.contains(UNRANKABLE), "measured: it has never been on the size axis");
    assert_eq!(
        stored_pnl.len(),
        PNL_COHORTS.len() + 1,
        "the pnl axis kept its whole ladder plus the unrankable bucket"
    );
    assert_eq!(stored_size.len(), SIZE_COHORTS.len(), "and the size axis kept its whole ladder");

    // The oracle's own two axes, as the pair of ladders with no unrankable rung anywhere.
    let oracle_pnl: BTreeSet<String> = expected(&fx, "by_pnl").into_iter().map(|r| r.2).collect();
    let oracle_size: BTreeSet<String> = expected(&fx, "by_size").into_iter().map(|r| r.2).collect();
    assert_eq!(oracle_pnl.len(), PNL_COHORTS.len());
    assert_eq!(oracle_size.len(), SIZE_COHORTS.len());
    assert!(!oracle_pnl.contains(UNRANKABLE) && !oracle_size.contains(UNRANKABLE));
}

/// ⚠ REWRITTEN AGAINST MEASURED DATA in the file this replaces, and the measurement is what is
/// carried. The original asserted that an hour missing `3x smart` still yields a FINITE
/// `smart_minus_rekt`. Two things were wrong with it:
///
/// * `3x smart` is never INDIVIDUALLY absent for BTC/ETH —
///   `individually_absent_pnl_hours_over_fetch_span: 0` of `fetch_span_pnl_hours: 6208`.
/// * The only realisation of "missing" is an hour whose pnl axis has NO rows at all.
///
/// What the COLLECTOR can gate about that is one step short of what the old test gated: those hours
/// reach the store as no rows on the pnl axis, so the absence is a genuine gap rather than a
/// fabricated zero. What a built feature matrix then does with such a gap is a READER's question
/// now — see this file's `COULD NOT CARRY` note at the bottom.
#[test]
fn an_hour_with_no_pnl_rows_at_all_reaches_the_store_as_no_rows() {
    let fx = fixture();
    let case = &fx["manifest"]["adversarial_cases"]["three_x_smart_absent"];
    assert_eq!(
        as_i64(&case["individually_absent_pnl_hours_over_fetch_span"]),
        0,
        "the premise this test replaces: `3x smart` alone is never the missing thing"
    );
    let empty_pnl_hours: Vec<(String, i64)> = case["hours"]
        .as_array()
        .expect("adversarial_cases.three_x_smart_absent.hours")
        .iter()
        .map(|h| (as_str(&h[0]).to_string(), as_i64(&h[1])))
        .collect();
    assert!(
        !empty_pnl_hours.is_empty(),
        "the fixture must contain such an hour or it proves nothing"
    );

    let pnl = collected(&fx, Axis::Pnl);
    let size = collected(&fx, Axis::Size);
    for key in &empty_pnl_hours {
        assert!(
            !pnl.iter().any(|r| r.asset == key.0 && r.ts == key.1 * 1_000),
            "{key:?} is on the pnl axis after all; the manifest and the data disagree"
        );
        // ...and the size axis does not cover them either: their raw size rows were ALL the empty
        // label, so the taxonomy filter removed the hour entirely rather than half of it.
        assert!(
            !size.iter().any(|r| r.asset == key.0 && r.ts == key.1 * 1_000),
            "{key:?} kept a size row; the empty-label filter did not remove the whole hour"
        );
    }
    // The contrast, without which every assertion above would pass on an empty replay.
    assert!(!pnl.is_empty() && !size.is_empty());
}

/// `short_usd = total_position_value - total_position_value_long`. Exact — one IEEE subtraction
/// over the SNAPPED pair ([`as_this_port_spells_it`]), and the endpoint serves neither the short
/// side nor the difference. This collector does not even STORE it:
/// `crates/vike-data/src/cohort_log.rs`'s `short_usd` derives it from the stored pair at read time,
/// which is the accessor this test drives.
///
/// ⚠ The snap is applied to BOTH sides before the subtraction, not to the difference afterwards.
/// `total` and `long` are the same order of magnitude, so their difference is a cancellation:
/// snapping it after the fact would leave the noise in the very quantity that has the least
/// precision left to spare.
#[test]
fn short_usd_is_derived_from_the_total_and_the_long_side() {
    let fx = fixture();
    for (axis, key) in [(Axis::Size, "by_size"), (Axis::Pnl, "by_pnl")] {
        let raw = raw_index(&fx, axis);
        let rows = collected(&fx, axis);
        assert!(!rows.is_empty());
        for r in &rows {
            let (total, long) = raw[&(r.asset.clone(), r.ts / 1_000, r.cohort.clone())];
            let (want_long, want_short) = as_this_port_spells_it(total, long);
            assert_eq!(
                r.long_usd.to_bits(),
                want_long.to_bits(),
                "{key}: long_usd is served (snapped), not derived"
            );
            assert_eq!(
                r.short_usd().to_bits(),
                want_short.to_bits(),
                "{key} {} {} {}: short {} != total {} - long {}",
                r.asset,
                r.ts,
                r.cohort,
                hex(r.short_usd()),
                hex(total),
                hex(long)
            );
            // Non-vacuity: the subtraction has to be doing work. Serving `total` or echoing `long`
            // would satisfy a test that only compared shapes.
            assert!(
                r.short_usd().to_bits() != total.to_bits()
                    && r.short_usd().to_bits() != long.to_bits(),
                "{key} {} {} {}: the derived short coincides with an input",
                r.asset,
                r.ts,
                r.cohort
            );
            // ⚠ The clamp the STUDY's spelling of this derivation applies and this producer
            // deliberately does not: on these bytes it is inert, so the two spellings cannot
            // disagree here and [`as_this_port_spells_it`] is entitled to omit it. If a future
            // fixture ever carries a sub-threshold negative, THIS is the line that says so.
            assert!(
                r.short_usd() > 0.0,
                "{key} {} {} {}: a non-positive short would make the study's clamp observable, and \
                 this file would then be comparing two different derivations",
                r.asset,
                r.ts,
                r.cohort
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// COULD NOT CARRY
// ---------------------------------------------------------------------------------------------
//
// ⚠ `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` names THIS note as the
// authority on the split, so it states where each half went rather than only what is absent.
//
// Two of the replaced file's assertions are NOT here, and neither was weakened to fit. Both are
// about a BUILT FEATURE MATRIX, which is a reader's artefact a collector cannot reach —
// `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` is the split that put it out
// of reach, and re-creating a matrix builder inside a collector test would be inventing the very
// coupling that record removes. They went the OTHER way instead, into the cohort study's own tree
// at `user_data/research/studies/rust/cohort/parity_loader.rs`, over the same frozen bytes.
//
// COULD NOT CARRY: the COLUMN half of `the_empty_string_label_is_filtered_out` — the old file also
// built features over the loaded rows and asserted that no column named `bias_` (or ending in `_`)
// reached the matrix, with `bias_4xWhale` present as the contrast. The ROW half is carried above in
// full (the label is dropped by name, counted per asset, and absent from the oracle's output); the
// column-name half is the study's, as `no_column_named_after_the_empty_label_reaches_the_matrix`,
// because column names are minted by the study's pivot.
//
// COULD NOT CARRY: `an_hour_with_no_pnl_rows_at_all_blanks_the_divergence_family_and_that_is_
// correct` — it asserted that `smart_minus_rekt`, `winner_minus_loser`, `z72_smart_minus_rekt` and
// `d24_smart_minus_rekt` are NaN and not 0.0 at an hour the pnl axis does not cover, with a finite
// cell at a covered hour as the contrast. Those are pivoted study columns, and the test lives under
// its own name in the study tree. What survives HERE is its PREMISE and its input-side fact, both
// asserted in `an_hour_with_no_pnl_rows_at_all_reaches_the_store_as_no_rows`: `3x smart` is never
// individually absent, and the named hours reach the store as no rows at all on either axis.
//
// One further shape did not port and did NOT need to. The old file asserted that each axis filter
// refuses the other axis outright (`by_pnl` called on a size fetch is an error), which made "and
// only there" structural. That hazard does not exist here: the axis is a per-ROW column and
// `crates/vike-backfill/src/vikedata/parse.rs`'s `is_known` is axis-scoped, so calling the pnl
// taxonomy on size rows is not a mistake that can be MADE rather than one that is caught. The
// equivalent structural statement — the two taxonomies' opposite answers about `Unknown` — is
// asserted in `unknown_is_dropped_from_the_pnl_axis_and_only_there`.
