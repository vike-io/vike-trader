use super::*;

/// Both outcomes are recorded under the caller's actor: a row the writer refuses (here: no database yet) is a
/// `Refused` record of the attempt carrying the writer's reason, and the refusal comes back; a row it writes is an
/// `AppliedPendingRestart` record of the cells the writer returned. The desktop's Server group records its dial rows
/// by this, the refusal included.
#[test]
fn a_written_and_a_refused_row_are_both_recorded_under_the_callers_actor() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let budget = Duration::from_millis(3_000);
    let (key, value) = ("config.node_addr", "127.0.0.1:17879");
    let mut seen = Vec::new();
    let refused =
        write_setting_journalled(dir.path(), key, value, budget, ChangeActor::Gui, |o, c| {
            seen.push((o, c.clone()));
        })
        .expect_err("there is no database to write");
    let attempt =
        Change::set_setting(ChangeOutcome::Refused, ChangeActor::Gui, "config", key, None, value)
            .with_reason(Some(&refused.to_string()));
    assert_eq!(seen, vec![(ChangeOutcome::Refused, attempt)]);

    vike_secrets::create_store(dir.path().to_str()).expect("an empty store");
    seen.clear();
    let report =
        write_setting_journalled(dir.path(), key, value, budget, ChangeActor::Gui, |o, c| {
            seen.push((o, c.clone()));
        })
        .expect("the row is written");
    let written = Change::set_setting(
        ChangeOutcome::AppliedPendingRestart,
        ChangeActor::Gui,
        "config",
        &report.key,
        None,
        &report.new_value,
    );
    assert_eq!(seen, vec![(ChangeOutcome::AppliedPendingRestart, written)]);
}
