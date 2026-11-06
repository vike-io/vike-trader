use super::*;

/// The table, verbatim: the CLI's `method_knobs` roster row renders `flag => owners` from it, so
/// a reorder or an owner change is a visible change to a published asset.
#[test]
fn the_knob_table_is_pinned_verbatim() {
    assert_eq!(
        METHOD_KNOBS,
        &[
            ("--euler-depth", &["euler"][..]),
            ("--trials", &["tpe"][..]),
            ("--seed", &["tpe", "genetic"][..]),
        ][..]
    );
}
