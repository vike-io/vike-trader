//! The `LATENCY-GATE` report LINE — the wire format between this crate's measurement harnesses and
//! the two things that read them, and the ONE place a new variant can silently break either.
//!
//! # What reads a report line, and what each one does with it
//!
//! `crates/vike-core/tests/runtime_latency.rs`'s `HopStats` writes exactly one `report` line per
//! measured variant, straight to the stderr HANDLE so libtest's capture cannot swallow it. Two
//! independent consumers parse that line, both living in `.github/workflows/ci.yml`'s latency step:
//!
//!   1. **`series_append`'s awk** turns every line into one JSON row in
//!      `/home/the CI user/.vike-ci-metrics/latency-series.jsonl` on the latency box — the durable series that exists so
//!      nobody re-scrapes job logs (it took 98 hand-harvested attempts to derive `JOURNAL_P99_NS`
//!      once, and a four-day follow-up that still had to conclude "underpowered — no pre-period
//!      exists"). It carries every `k=v` after the marker VERBATIM, which is the property that lets
//!      a NEW measurement variant land in the series with **no workflow change at all**.
//!   2. **the box-health canary** greps `LATENCY-GATE variant=baseline ` for that attempt's
//!      journal-free p99 — the one number no journal change can move, and therefore the field that
//!      separates "the box was disturbed" from "the code regressed".
//!
//! # Why this file exists rather than a comment saying "be careful"
//!
//! Both consumers fail SILENTLY and in opposite directions, and neither failure is visible from the
//! harness that caused it:
//!
//!   * **A label with a space in it** (`variant=book hop`) does not produce a mangled row — it
//!      produces `variant=book`, and then `hop` is a token with no `=` that the awk skips. The row
//!      lands, looks ordinary, and is filed under a variant that does not exist. Nothing anywhere
//!      says so.
//!   * **A line that stops carrying one of the three required keys** produces NO row. The step
//!      prints `parsed ZERO LATENCY-GATE rows`, which is a `::warning::` on an ADVISORY job — so
//!      the series quietly stops growing while every PR stays green. The workflow's own comment
//!      already names that class ("a telemetry sink that has been silently broken for a month is
//!      the same class of defect as a probe that can only ever pass"); this is the check that makes
//!      it loud in the FAST lane instead.
//!   * **Two harnesses emitting the SAME label** is the sneakiest of the three, because a series row
//!      records `variant` and does **not** record which test produced it. Two distributions merge
//!      into one trend line and every percentile read off it afterwards is a fiction. Every label
//!      was unique when this file landed — verified against the live series on the latency box, which then
//!      held exactly ten distinct `variant` values at 172 attempts each, and 15 once this file's
//!      own submit/snapshot variants started reporting — but nothing enforced it. ⚠ Those counts
//!      are DATED observations, not a roster: [`claim_variant_label`] is what actually holds the
//!      property, and `sudo -n jq -r .variant /home/the CI user/.vike-ci-metrics/latency-series.jsonl | sort -u` on the latency box
//!      is the live answer. Do not re-state a count here that the next variant makes wrong.
//!
//! So the rule is a gate at the one choke point every label passes through
//! ([`check_report_line`], called from `HopStats::report`), not a paragraph asking authors to
//! remember. Same reasoning as `docs/decisions/0007-gates-not-prose.md`.
//!
//! # This module is deliberately vike-FREE
//!
//! It names no type from `vike-core`, `vike-exec` or `vike-model` and uses nothing but `std`. That
//! is what makes it compilable and RUNNABLE on its own —
//! `rustc --test crates/vike-core/tests/common/latency_line.rs` builds a standalone harness — on a
//! dev box that cannot compile this workspace. A gate nobody can execute is a gate nobody can
//! mutation-test, and an un-mutation-tested gate is this repo's most-repeated defect
//! (`docs/decisions/0007-gates-not-prose.md`; the memory note "declaration-pinning tests don't
//! gate" records three separate cases of `caps.rs` drifting while GREEN).
//!
//! It lives under `tests/common/` because cargo auto-discovers integration-test binaries from
//! `tests/*.rs` and `tests/*/main.rs` only, so a file one directory down is a plain module and not
//! a second test binary. `crates/bridges/ctrader/tests/` is the in-tree precedent for that layout.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// The keys `ci.yml`'s `series_append` awk requires before it will emit a row at all — its
/// `if (!("variant" in kv) || !("p50_ns" in kv) || !("p99_ns" in kv)) next` guard.
///
/// A hand copy of a rule that lives in another file is exactly what this repo keeps paying for, so
/// it is NOT left as one: [`the_required_key_set_still_matches_the_workflow`] reads the workflow
/// and fails if that guard is re-spelled. Change the awk and this array goes red the same day.
pub const REQUIRED_KEYS: [&str; 3] = ["variant", "p50_ns", "p99_ns"];

/// The marker every report line carries. ci.yml finds it with awk's `index` (a SUBSTRING search),
/// not `==`, because libtest writes `test NAME ... ` to stdout while the report goes to the stderr
/// handle and the step's `2>&1` interleaves the two onto one line.
pub const MARKER: &str = "LATENCY-GATE";

/// `.github/workflows/ci.yml`, read at RUN time so the pin below reads the real workflow rather
/// than a description of it.
///
/// ⚠ It was an `include_str!` — a COMPILE-time dependency — and that is the one thing it could not
/// be. `scripts/publish_mirror.sh` builds the public source mirror WITHOUT `.github/`, because
/// every workflow here runs on self-hosted runners and publishing them invites a fork PR onto the
/// box that signs orders. A compile-time include made the whole mirror fail to build over a file
/// deliberately withheld from it; a run-time read lets the mirror compile and lets the pin below
/// skip, loudly, where there is no workflow to compare against. In THIS repository the file is
/// always present, so the gate is unchanged.
///
/// `CARGO_MANIFEST_DIR`-relative rather than source-relative for the same reason `include_str!`
/// was source-relative: it must resolve from wherever the test binary is run.
fn ci_yml() -> Option<String> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    std::fs::read_to_string(path).ok()
}

/// PANICKING choke point — call it from the one place that builds a report line.
///
/// Panics rather than returns, deliberately: it runs inside `HopStats::report`, which every
/// harness in `crates/vike-core/tests/runtime_latency.rs` calls, and the only way to reach a
/// failure here is to have just written a variant that corrupts the series. A red gate at that
/// moment is the cheapest possible feedback; a `Result` nobody threads through is not.
pub fn check_report_line(line: &str, label: &str) {
    if let Err(why) = validate_report_line(line, label) {
        panic!("LATENCY-GATE report line is not series-safe: {why}\nline: {line:?}");
    }
    if let Err(why) = claim_variant_label(label) {
        panic!("LATENCY-GATE variant label is not unique: {why}");
    }
}

/// The PURE half: is this line, carrying this label, something both consumers can read?
///
/// Three things, in the order they can go wrong:
///   1. the label survives whitespace tokenisation at all ([`is_awk_safe_label`]);
///   2. the line parses to a row the awk would actually emit ([`parse_report_line`]);
///   3. the row's `variant` is the label the harness thinks it reported — which is what catches a
///      format string whose `variant=` field drifted away from its own argument.
pub fn validate_report_line(line: &str, label: &str) -> Result<(), String> {
    if !is_awk_safe_label(label) {
        return Err(format!(
            "variant label {label:?} must be a non-empty run of [A-Za-z0-9._-]. A label carrying \
             whitespace does NOT produce a mangled row — it produces a plausible row filed under a \
             truncated variant name, silently"
        ));
    }
    let Some(kv) = parse_report_line(line) else {
        return Err(format!(
            "the line carries no {MARKER} marker, or is missing one of the required keys \
             {REQUIRED_KEYS:?} — ci.yml's series_append would emit NO row for it and warn instead, \
             on an ADVISORY job nobody reads"
        ));
    };
    match kv.get("variant") {
        Some(v) if *v == label => Ok(()),
        Some(v) => Err(format!(
            "the line reports variant={v:?} but the harness passed label={label:?} — the series \
             would be keyed on the wrong name"
        )),
        // Unreachable while REQUIRED_KEYS names "variant"; kept as a real arm so that editing
        // REQUIRED_KEYS produces a diagnosable failure rather than a panic in an `unreachable!`.
        None => {
            Err(format!("no variant key in the parsed row (REQUIRED_KEYS = {REQUIRED_KEYS:?})"))
        }
    }
}

/// ci.yml's `series_append` awk, reimplemented over one line: find the marker by SUBSTRING, then
/// read every `k=v` token after it, skipping the ones the awk skips and keeping FIRST occurrences.
/// Returns `None` for a line the awk would not turn into a row.
///
/// ⚠ This is a MIRROR, and a mirror is a copy. What keeps it honest is that its one rule with real
/// content — the required-key set — is pinned against the workflow text itself
/// ([`the_required_key_set_still_matches_the_workflow`]); the tokenising rules below are awk's own
/// `split`/`index`/`substr` semantics, which do not drift. What it buys is that the row SHAPE is
/// checked at all: before this file, nothing in the tree compared the emitted line to the parser.
pub fn parse_report_line(line: &str) -> Option<BTreeMap<&str, &str>> {
    let mut fields = line.split_ascii_whitespace();
    // awk: `for (i=1;i<=NF;i++) if (index($i, "LATENCY-GATE")) { start = i + 1; break }`
    fields.by_ref().find(|tok| tok.contains(MARKER))?;
    let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
    for tok in fields {
        // awk: `p = index($i, "="); if (p < 2) continue` — a 1-based index, so `p < 2` rejects both
        // "no `=` at all" (p == 0) and "an EMPTY key" (p == 1).
        let Some(p) = tok.find('=') else { continue };
        if p == 0 {
            continue;
        }
        let (k, v) = (&tok[..p], &tok[p + 1..]);
        if !is_awk_key(k) {
            continue;
        }
        // awk: `if (k in kv) continue` — first occurrence wins.
        kv.entry(k).or_insert(v);
    }
    REQUIRED_KEYS.iter().all(|k| kv.contains_key(k)).then_some(kv)
}

/// awk's `/^[A-Za-z_][A-Za-z0-9_]*$/` key filter.
fn is_awk_key(k: &str) -> bool {
    let mut chars = k.chars();
    let Some(first) = chars.next() else { return false };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A label that survives whitespace tokenisation, JSON string escaping and a `grep` alike.
///
/// Stricter than strictly necessary on purpose. `=` would in fact survive (the awk splits on the
/// FIRST `=`, so `variant=a=b` still reads `a=b`), and so would most punctuation — but every label
/// this file has ever emitted is `[a-z0-9-]+`, and a rule that admits exactly the shapes already in
/// use cannot be the thing that lets a surprise through.
fn is_awk_safe_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Every variant label reported by THIS PROCESS. `--test-threads=1` puts every harness in one
/// process, so this set sees the whole run.
static CLAIMED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Claim a variant label for this process; `Err` if some other harness already reported it.
///
/// The hazard is specific and silent: a series row carries `variant` and NOT the test that emitted
/// it, so two harnesses sharing a label merge two distributions into one trend line with nothing to
/// separate them afterwards.
///
/// Poison-tolerant (`into_inner` on a poisoned lock). A harness that panicked mid-run has already
/// failed its own test; turning every LATER report into a second, unrelated panic would bury the
/// first one under noise.
///
/// ⚠ **No test in this module may call [`check_report_line`] with a production label.** The claim
/// set is process-global, `cargo test -- --include-ignored` puts these tests and the real harnesses
/// in ONE process, and a test that claimed `"baseline"` first would make the actual gate panic. The
/// tests below therefore exercise [`validate_report_line`] (which claims nothing) and reserve
/// `unit-test-*` labels for the uniqueness case.
pub fn claim_variant_label(label: &str) -> Result<(), String> {
    let mut claimed = CLAIMED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if claimed.insert(label.to_string()) {
        return Ok(());
    }
    Err(format!(
        "{label:?} was already reported by another harness in this process. A series row records \
         `variant` but NOT which test produced it, so two harnesses sharing a label merge two \
         distributions into one trend line"
    ))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The gate's own tests. NOT `#[ignore]`d: they are pure and instant, so they ride the FAST CI lane
// (`cargo nextest run -p vike-core`) — while the the latency box latency job invokes the binary with
// `--ignored`, which runs ONLY ignored tests and therefore skips every one of these. That split is
// exactly right: this is a format contract, not a measurement, and it must not cost a microsecond
// on the quiet box.
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A line shaped exactly like `HopStats::report`'s, for the tests below.
#[cfg(test)]
fn sample_line(variant: &str) -> String {
    format!(
        "{MARKER} variant={variant} n=100000 p50_ns=271 p99_ns=551 p999_ns=2104 max_ns=25709 \
         max_at_hop=58518 hops_over_100us=0\n"
    )
}

/// Read the key names ci.yml's row-admission guard ACTUALLY requires, out of the workflow text:
/// every `"NAME" in kv` on the one `awk` line that ends in `next`.
///
/// ⚠ It must EXTRACT the names, not merely confirm a sentence is present. The first version of the
/// pin below asserted `CI_YML.contains("<the whole guard, spelled out>")` — which reads like a
/// derivation and is not one, because the assertion never looks at [`REQUIRED_KEYS`] at all.
/// MEASURED, by mutation: deleting `"p99_ns"` from that array left all ten tests GREEN. A pin that
/// survives the deletion of the thing it pins is decoration.
#[cfg(test)]
fn workflow_required_keys(ci_yml: &str) -> Option<Vec<String>> {
    const NEEDLE: &str = r#"" in kv)"#;
    let line =
        ci_yml.lines().find(|l| l.contains(NEEDLE) && l.trim_end().ends_with("next"))?.trim_end();
    let mut keys = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find(NEEDLE) {
        let open = rest[..at].rfind('"')?;
        keys.push(rest[open + 1..at].to_string());
        rest = &rest[at + NEEDLE.len()..];
    }
    keys.sort();
    Some(keys)
}

/// The pin that keeps [`REQUIRED_KEYS`] from being a hand copy: the array must equal what the
/// workflow's own guard names.
///
/// If this fails, ci.yml's row-admission rule changed. Re-read `series_append`'s awk and update
/// [`REQUIRED_KEYS`] to match it — do NOT relax this test, which would put the mirror back to being
/// a description of a rule rather than a copy checked against it.
///
/// Compared as SORTED sets: the order the guard lists its keys in carries no meaning (they are
/// `||`-ed), so a reordering must not be a red.
#[test]
fn the_required_key_set_still_matches_the_workflow() {
    // ⚠ SKIP, loudly, when there is no workflow to read. The only tree where that happens is the
    // public source mirror, which withholds `.github/` on purpose; in this repository the file is
    // always there and the assertions below run exactly as before. A silent skip would be the
    // "green means nothing ran" defect, so it says so on stderr.
    let Some(ci_yml) = ci_yml() else {
        eprintln!(
            "SKIPPED: .github/workflows/ci.yml is absent — this is the source mirror, where the \
             workflow is withheld deliberately. In the private tree this test never skips."
        );
        return;
    };
    let from_workflow = workflow_required_keys(&ci_yml).expect(
        "ci.yml no longer carries a `!(\"NAME\" in kv) … next` row-admission guard on one line — \
         re-read series_append's awk in .github/workflows/ci.yml",
    );
    let mut declared: Vec<String> = REQUIRED_KEYS.iter().map(|k| (*k).to_string()).collect();
    declared.sort();
    assert_eq!(
        from_workflow, declared,
        "REQUIRED_KEYS disagrees with the guard in .github/workflows/ci.yml's series_append. \
         The WORKFLOW is the authority — a key it requires and this array omits means a line this \
         module calls series-safe would be silently dropped by CI."
    );
    assert!(
        ci_yml.contains(MARKER),
        "ci.yml no longer mentions the {MARKER} marker at all — the series sink is gone or renamed"
    );
}

#[test]
fn a_well_formed_line_parses_to_its_fields() {
    let line = sample_line("baseline");
    let kv = parse_report_line(&line).expect("a real report line is a row");
    assert_eq!(kv["variant"], "baseline");
    assert_eq!(kv["p50_ns"], "271");
    assert_eq!(kv["hops_over_100us"], "0");
    assert_eq!(validate_report_line(&line, "baseline"), Ok(()));
}

/// The trailing `extra` a harness appends must land as ordinary fields — that is the property that
/// lets a new variant carry a new axis into the series with no workflow change.
#[test]
fn trailing_extra_fields_ride_through_verbatim() {
    let line = format!("{} resting=32 registry=33\n", sample_line("submit-hop").trim_end());
    let kv = parse_report_line(&line).expect("row");
    assert_eq!(kv["resting"], "32");
    assert_eq!(kv["registry"], "33");
}

/// The `journal` variants' REAL trailing fields, including the `u64::MAX` "never snapshot"
/// sentinel that ci.yml's own `jauto` has to string-quote so a consumer cannot mangle it.
///
/// This is the "does the new gate break the old gates" check, made concrete: wiring
/// [`check_report_line`] into `HopStats::report` puts it on the path of the two CALIBRATED journal
/// gates, and a mirror that rejected their real line would have turned them red for a telemetry
/// reason — the worst possible outcome for a change whose entire premise is "measure, do not
/// enforce".
#[test]
fn the_journal_variants_real_extra_fields_parse() {
    let line = format!(
        "{} wal_bytes_min=28700000 snap_every=18446744073709551615 snaps_expected=0\n",
        sample_line("journal").trim_end()
    );
    assert_eq!(validate_report_line(&line, "journal"), Ok(()));
    assert_eq!(parse_report_line(&line).expect("row")["snap_every"], "18446744073709551615");
}

/// The silent-corruption case this gate exists for.
#[test]
fn a_label_with_a_space_is_refused() {
    let line = sample_line("book hop");
    // The awk would happily emit a row here — that is the whole problem.
    assert!(parse_report_line(&line).is_some(), "the awk itself does NOT reject this");
    assert_eq!(parse_report_line(&line).unwrap()["variant"], "book", "…it files it under `book`");
    let err = validate_report_line(&line, "book hop").unwrap_err();
    assert!(err.contains("whitespace"), "the message must name the failure mode: {err}");
}

#[test]
fn a_line_missing_a_required_key_is_dropped_exactly_as_the_awk_drops_it() {
    let line = format!("{MARKER} variant=baseline n=100000 p999_ns=2104\n");
    assert!(parse_report_line(&line).is_none(), "no p50_ns/p99_ns ⇒ ci.yml emits NO row");
    let err = validate_report_line(&line, "baseline").unwrap_err();
    assert!(err.contains("required keys"), "{err}");
}

/// ci.yml's own reason for the required-key guard: the step echoes its script into the job log, so
/// the marker string appears in PROSE and must never become a fabricated data point.
#[test]
fn a_prose_mention_of_the_marker_is_not_a_row() {
    assert!(
        parse_report_line("# grep LATENCY-GATE in any job log; the shape is stable\n").is_none()
    );
}

/// libtest writes `test NAME ... ` to stdout while the report goes to the stderr HANDLE, and the
/// step's `2>&1` interleaves them. The marker is therefore reached MID-LINE.
///
/// ⚠ This test pins mid-LINE and nothing more — its fixture leaves the marker as its own
/// whitespace-delimited token, so it holds under `==` just as well as under a substring search.
/// [`a_marker_glued_to_preceding_output_is_still_found`] is the one that pins the actual rule.
#[test]
fn libtest_chatter_before_the_marker_is_ignored() {
    let line = format!("test p99_core_hop_under_10us ... {}", sample_line("baseline"));
    let kv = parse_report_line(&line).expect("the marker is found by substring, not equality");
    assert_eq!(kv["variant"], "baseline");
    assert!(!kv.contains_key("test"), "tokens BEFORE the marker are not fields");
}

/// …and MID-TOKEN, which is a DIFFERENT rule, and the one that was going unpinned.
///
/// ⚠ MEASURED, by mutation, which is the only reason this test exists: with only the test above,
/// replacing [`parse_report_line`]'s `find(|tok| tok.contains(MARKER))` with
/// `find(|tok| *tok == MARKER)` left all eleven tests GREEN. A mirror can therefore drift away from
/// the awk on exactly the rule the test above appears to cover — the "declaration-pinning test that
/// does not gate" shape this repo has been bitten by three times (`caps.rs`, per the memory note),
/// inside the file whose whole premise is that an un-mutation-tested gate is the repeated defect.
///
/// The rule is awk's, not a preference: `series_append` finds the marker with
/// `index($i, "LATENCY-GATE")`, a SUBSTRING search over each token. It has to be, because `2>&1`
/// joins two file descriptors onto ONE pipe and the streams interleave at BYTE granularity, not at
/// line granularity — a partial stdout write that ends without whitespace puts the marker inside a
/// token. Equality would then match nothing, the awk would emit no row, and the step's
/// `parsed ZERO LATENCY-GATE rows` is a `::warning::` on an ADVISORY job: the series stops growing
/// while every PR stays green. That is the silent failure this module exists to make loud.
#[test]
fn a_marker_glued_to_preceding_output_is_still_found() {
    let line = format!("test p99_core_hop_under_10us ...{}", sample_line("baseline"));
    // The fixture must genuinely exercise the substring rule. Without this guard the test could
    // decay into a copy of the one above the next time somebody edits the chatter prefix, and the
    // mutation it was written to kill would silently come back to life.
    assert!(
        !line.split_ascii_whitespace().any(|tok| tok == MARKER),
        "this fixture must NOT carry {MARKER} as a standalone token, or it pins nothing that \
         `find(|tok| *tok == MARKER)` would not also pass"
    );
    let kv = parse_report_line(&line).expect("awk finds the marker with index(), not with ==");
    assert_eq!(kv["variant"], "baseline");
    assert!(!kv.contains_key("test"), "tokens BEFORE the marker are not fields");
}

/// awk's `if (k in kv) continue`.
#[test]
fn the_first_occurrence_of_a_key_wins() {
    let line = format!("{} variant=impostor p50_ns=999999\n", sample_line("baseline").trim_end());
    let kv = parse_report_line(&line).expect("row");
    assert_eq!(kv["variant"], "baseline");
    assert_eq!(kv["p50_ns"], "271");
}

/// Tokens the awk skips: no `=`, an empty key, or a key outside `[A-Za-z_][A-Za-z0-9_]*`.
#[test]
fn tokens_the_awk_skips_are_skipped_here_too() {
    let line = format!("{} bare =empty 9bad=x ok_1=y\n", sample_line("baseline").trim_end());
    let kv = parse_report_line(&line).expect("row");
    assert!(!kv.contains_key("bare"));
    assert!(!kv.contains_key(""));
    assert!(!kv.contains_key("9bad"));
    assert_eq!(kv["ok_1"], "y");
}

#[test]
fn a_variant_label_may_be_claimed_only_once() {
    // Labels unique to THIS test — the claim set is process-global by design.
    assert_eq!(claim_variant_label("unit-test-claim-a"), Ok(()));
    assert_eq!(claim_variant_label("unit-test-claim-b"), Ok(()));
    let err = claim_variant_label("unit-test-claim-a").unwrap_err();
    assert!(err.contains("already reported"), "{err}");
}
