use super::*;

fn plan() -> RepairPlan {
    RepairPlan {
        id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string())),
        series_dir: "/s/kind=bar/venue=binance/symbol=BTCUSDT/interval=1m".to_string(),
        leaf_present: true,
        base_present: true,
        base_version: 7,
        current_parts: Some(3),
        current_rows: Some(30),
        current_error: None,
        delta_frames: Some(0),
        delta_error: None,
        orphan_commits: 0,
        report: RebuildReport { parts_recovered: 3, ..Default::default() },
        rebuilt_rows: 30,
    }
}

/// A clean rebuild says so and lists nothing — the baseline the lossy cases are read against.
#[test]
fn a_lossless_plan_has_no_losses_and_says_so() {
    let p = plan();
    assert!(p.is_lossless());
    assert!(p.losses().is_empty());
    assert!(p.notes().is_empty(), "{:?}", p.notes());
    let text = p.lines().join("\n");
    assert!(text.contains("verdict: LOSSLESS"), "{text}");
}

/// ⚠ **The headline requirement.** Each of the three lossy shapes flips the verdict on its own,
/// so no combination of them can hide behind another, and each loss line names the count AND
/// the next action — a count alone is not something an operator can act on.
#[test]
fn each_lossy_shape_flips_the_verdict_alone_and_names_a_next_step() {
    for (name, mutate) in [
        (
            "parts_without_keys",
            (|p: &mut RepairPlan| p.report.parts_without_keys = 2) as fn(&mut RepairPlan),
        ),
        ("parts_unreadable", |p: &mut RepairPlan| p.report.parts_unreadable = 1),
        ("orphan_commits", |p: &mut RepairPlan| p.orphan_commits = 13),
    ] {
        let mut p = plan();
        mutate(&mut p);
        assert!(!p.is_lossless(), "{name} must make a plan lossy");
        let losses = p.losses();
        assert_eq!(losses.len(), 1, "{name}: exactly its own line: {losses:?}");
        assert!(losses[0].starts_with("LOSSY:"), "{name}: {:?}", losses[0]);
        assert!(losses[0].contains("Next:"), "{name} must say what to do: {:?}", losses[0]);
        let text = p.lines().join("\n");
        assert!(text.contains("verdict: LOSSY"), "{name}: {text}");
    }
}

/// The two findings that are NOT losses are still REPORTED — the mechanism working is still a
/// fact about the store (a compaction crashed; a compaction may be live).
#[test]
fn the_mechanism_working_is_a_note_rather_than_a_loss() {
    let mut p = plan();
    p.report.parts_superseded = 4;
    p.report.parts_unpublished_merge = 1;
    assert!(p.is_lossless(), "neither is a LOSS");
    let notes = p.notes().join("\n");
    assert!(notes.contains("crashed before unlinking"), "{notes}");
    assert!(notes.contains("ANOTHER PROCESS"), "{notes}");
    let text = p.lines().join("\n");
    assert!(text.contains("verdict: LOSSLESS"), "{text}");
    assert!(text.contains("crashed before unlinking"), "a note must reach the rendering: {text}");
}

/// Recovering NOTHING while parts sit on disk is the one shape a clean-looking exit would be
/// worst about — it is what publishing an EMPTY index over a full series looks like from here.
#[test]
fn recovering_nothing_from_a_non_empty_leaf_is_called_out() {
    let mut p = plan();
    p.report = RebuildReport { parts_unreadable: 3, ..Default::default() };
    p.rebuilt_rows = 0;
    assert_eq!(p.parts_seen(), 3);
    let notes = p.notes().join("\n");
    assert!(notes.contains("NOTHING would be recovered"), "{notes}");
}

/// A rebuild that would name FEWER rows than the current index is a shortfall on disk, not a
/// tidy-up — the plan says so before the operator confirms.
#[test]
fn a_row_shortfall_against_the_current_index_is_called_out() {
    let mut p = plan();
    p.rebuilt_rows = 20;
    let notes = p.notes().join("\n");
    assert!(notes.contains("20 rows where the index before it named 30"), "{notes}");
}

/// ⚠ **The rehearsal and the record share ONE verdict implementation**, so a plan that read
/// LOSSLESS and an outcome that reads LOSSLESS cannot mean different things — and a LOSSY
/// outcome carries the same named losses the rehearsal showed, in the past tense.
#[test]
fn the_outcome_carries_the_same_verdict_the_plan_did() {
    let clean = plan();
    let text = clean.outcome_lines().join("\n");
    assert!(text.contains("verdict: LOSSLESS"), "{text}");
    assert!(text.starts_with("rebuilt "), "{text}");

    let mut lossy = plan();
    lossy.report.parts_without_keys = 2;
    let text = lossy.outcome_lines().join("\n");
    assert!(text.contains("verdict: LOSSY"), "{text}");
    assert!(text.contains("DUPLICATE rows"), "the consequence must survive the tense: {text}");
    assert!(text.contains("Next:"), "{text}");
}

/// ⚠ **The outcome carries NO row count**, and that is the point: the write returns a report
/// rather than the manifest it published, so the only rows in hand are the rehearsal's
/// forecast. A number that looks measured and is not is worse than no number.
#[test]
fn the_outcome_reports_parts_and_never_a_stale_row_count() {
    let mut p = plan();
    p.rebuilt_rows = 999_999;
    let text = p.outcome_lines().join("\n");
    assert!(text.contains("indexed 3 part(s)"), "{text}");
    assert!(
        !text.contains("999999"),
        "a plan-time row count must not be reported as a fact: {text}"
    );
}

/// ⚠ **The dropped orphan keys are the PLAN's count and must stay so.** They are dropped BY the
/// rebuild, so a post-write reading would answer zero every time — a loss that erases its own
/// evidence. The outcome therefore still names them.
#[test]
fn dropped_orphan_keys_survive_into_the_outcome() {
    let mut p = plan();
    p.orphan_commits = 13;
    let text = p.outcome_lines().join("\n");
    assert!(text.contains("13 orphan key(s) dropped"), "{text}");
    assert!(text.contains("verdict: LOSSY"), "{text}");
}

/// The base-less-with-log state renders as what it IS, so the rendering matches the refusal
/// that sent the operator here.
#[test]
fn the_base_less_state_renders_both_halves() {
    let mut p = plan();
    p.base_present = false;
    p.delta_frames = Some(9);
    p.current_error = Some("has a delta log but NO base".to_string());
    let text = p.lines().join("\n");
    assert!(text.contains("_manifest.json MISSING"), "{text}");
    assert!(text.contains("9 frame(s)"), "{text}");
    assert!(text.contains("index today: UNREADABLE"), "{text}");
}

/// The critical section is reported as a COUNT, because it is what a live writer pays and
/// "this may take a while" is not something an operator can weigh.
#[test]
fn the_critical_section_is_reported_as_a_part_count() {
    let mut p = plan();
    p.report = RebuildReport {
        parts_recovered: 10,
        parts_superseded: 4,
        parts_unreadable: 1,
        parts_unpublished_merge: 1,
        parts_without_keys: 0,
    };
    assert_eq!(p.parts_seen(), 16, "every footer opened, whatever became of it");
    let text = p.lines().join("\n");
    assert!(text.contains("16 part footer(s) read with this series' lock HELD"), "{text}");
}
