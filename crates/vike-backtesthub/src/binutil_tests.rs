use super::*;

/// **The marker is PRESENT and it NAMES the convention actually in force.** Compared to the
/// constant rather than to a second copy of the string: `vike_analytics::metrics` carries the
/// one literal pin, beside the body being named, so a convention change is typed once.
#[test]
fn the_provenance_block_names_the_current_percentile_method() {
    let v = stats_provenance();
    assert_eq!(
        v["percentile_method"],
        serde_json::json!(vike_analytics::metrics::PERCENTILE_METHOD),
        "the block must publish the method the shared percentile actually computes"
    );
    assert_eq!(v["note"], serde_json::json!(PERCENTILE_NOTE));
    // Exactly these two keys: an added one is a deliberate act, and a reader of an OLD file has
    // to be able to trust that what this block does NOT say, it never said.
    let obj = v.as_object().expect("the block is an object");
    assert_eq!(obj.len(), 2, "unexpected keys in the provenance block: {obj:?}");
}

/// The note must carry the ABSENCE rule, because that is the half a reader cannot derive: an
/// old file says nothing at all, so the new file is the only place the comparison can be
/// refused. Asserted on the substance, not the punctuation — the sentence may be reworded, and
/// may not quietly lose a fact.
#[test]
fn the_note_states_the_absence_rule_and_names_both_conventions() {
    for fact in [
        // the key whose absence IS the signal, spelled so a reader can grep for it
        "stats_provenance",
        // ...and what its absence means
        "nearest-rank",
        // the new convention, by name and by algorithm
        "percentile",
        "interpolat",
        // the reason a conversion is not on offer
        "no scale factor",
        "re-run",
    ] {
        assert!(PERCENTILE_NOTE.contains(fact), "the note dropped {fact:?}: {PERCENTILE_NOTE}");
    }
    assert!(
        PERCENTILE_NOTE.is_ascii(),
        "the note is pasted into terminals and grepped; keep it ASCII"
    );
}

/// `CommaList` is the one splitter every list flag of the bins shares: parts are trimmed, empty
/// parts are skipped (`1,,2`, a trailing comma), and a part that is not a `T` is a refusal naming
/// it — where the hand-rolled splitters dropped it (`--sizes`, `--delays-ms`) or refused by hand.
#[test]
fn a_comma_list_trims_skips_empties_and_refuses_a_malformed_part() {
    let ok = |raw: &str| raw.parse::<CommaList<i64>>().map(|l| l.0);
    assert_eq!(ok("954,4229"), Ok(vec![954, 4229]));
    assert_eq!(ok(" 1 , 2 ,"), Ok(vec![1, 2]));
    assert_eq!(ok("1,,2"), Ok(vec![1, 2]));
    assert_eq!(ok(","), Ok(vec![]));
    assert_eq!(ok("-5"), Ok(vec![-5]), "a sign is the caller's rule, not the splitter's");
    let err = ok("1,x,3").expect_err("a malformed part is refused");
    assert!(err.contains("\"x\""), "{err}");
    assert_eq!("0.5,2".parse::<CommaList<f64>>().map(|l| l.0), Ok(vec![0.5, 2.0]));
}

#[test]
fn a_bin_version_line_is_the_bin_then_the_package_version() {
    assert_eq!(
        bin_version_line("cheap_np_run"),
        format!("cheap_np_run {}", env!("CARGO_PKG_VERSION"))
    );
}
