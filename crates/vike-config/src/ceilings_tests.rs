use super::*;

/// The derivation answers from the TABLE, not from a written list — add a second home for a
/// name and it appears here with no edit.
#[test]
fn a_name_carried_by_two_homes_is_derived_as_shared() {
    let shared = shared_names();
    assert!(shared.contains(&"max_notional_per_order"), "{shared:?}");
    assert!(shared.contains(&"max_leverage"), "{shared:?}");
    assert!(
        !shared.contains(&"max_total_exposure"),
        "the policy settings cannot carry this name today — \
             `PolicyPatch::max_total_exposure` is a tombstone that hard-errors the load, so \
             exactly one home declares it: {shared:?}"
    );
    assert!(!shared.contains(&"max_account_exposure"), "{shared:?}");
}

/// **No home is labelled with a settings file's name.** `vike-cli config show` prints
/// [`CeilingHome::label`] in its LIVES IN column and carries it in the JSON document as
/// `home`. The policy home read `policy.toml` after decision 0086 had deleted every settings
/// file, so an operator who read the column went looking for a file no binary opens; the
/// value is a row in the settings database, and the label says so.
#[test]
fn a_home_label_names_no_settings_file() {
    for c in PRE_TRADE_CEILINGS {
        let label = c.home.label();
        assert!(
            !label.contains(".toml"),
            "`{}` is labelled {label:?}, a file name no binary reads (decision 0086)",
            c.name
        );
        if c.home == CeilingHome::PolicySettings {
            assert!(
                label.contains("settings database"),
                "`{}` lives in a settings row, and its label must say so: {label:?}",
                c.name
            );
        }
    }
}

/// Every shared name really does carry two DIFFERENT homes — the property that makes the word
/// "shared" mean what the renderer says it means.
#[test]
fn a_shared_name_spans_two_homes() {
    for name in shared_names() {
        let mut homes: Vec<CeilingHome> = ceilings_named(name).map(|c| c.home).collect();
        homes.sort_unstable();
        homes.dedup();
        assert!(homes.len() > 1, "{name} repeats within one home rather than spanning two");
    }
}

/// Rows are ordered so a rendering puts the two halves of a shared name side by side. Without
/// this the disclosure is two lines a screen apart, which is how the defect read on the box.
#[test]
fn rows_are_grouped_by_name_and_ordered_by_home() {
    let mut seen: Vec<&str> = Vec::new();
    for w in PRE_TRADE_CEILINGS.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.name == b.name {
            assert!(a.home < b.home, "{}: keep a name's homes in enum order", a.name);
        } else {
            assert!(!seen.contains(&b.name), "{} is split across the table", b.name);
            seen.push(a.name);
            assert!(a.name < b.name, "{} then {}: keep the table sorted by key", a.name, b.name);
        }
    }
}
