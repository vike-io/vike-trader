use super::seal_gate;

/// A sound seal is silent on BOTH dispositions — this arm may not cost a healthy box anything.
#[test]
fn a_sound_seal_starts_under_either_disposition() {
    assert!(seal_gate(true, None).is_ok());
    assert!(seal_gate(false, None).is_ok());
}

/// The refusing root refuses, and the message carries the operator's way out: no repair COMMAND
/// exists, so it names the nightly backup, and `config check` to see which row.
#[test]
fn an_unsound_seal_refuses_the_ordering_root_and_names_the_repair() {
    let err = seal_gate(true, Some("the settings rows have CHANGED since this box was adopted"))
        .expect_err("a root that places orders must not start on an unsound ceiling");
    assert!(err.contains("REFUSING TO START"), "{err}");
    assert!(err.contains("CHANGED since this box was adopted"), "the CAUSE survives: {err}");
    assert!(err.contains("NO SIZE CAP"), "…and what it would have cost: {err}");
    assert!(err.contains("nightly backup"), "{err}");
    assert!(err.contains("config check"), "…and how to see WHICH row: {err}");
}

/// The complement, and it is the half that keeps the box repairable: every OTHER root carries
/// the same unsound seal and still boots, so `vike-cli config check` stays reachable to name
/// the row.
///
/// Without this assertion the test above is satisfied by making every root refuse, which is
/// exactly the failure the mark was introduced to remove.
#[test]
fn the_same_unsound_seal_does_not_stop_a_reporting_root() {
    assert!(
        seal_gate(false, Some("the settings rows have CHANGED since this box was adopted")).is_ok(),
        "a root that reports the mark must still boot — `config check` lives there"
    );
}
