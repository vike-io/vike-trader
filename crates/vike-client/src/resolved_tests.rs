use super::*;

/// A loader warning is SURFACED, never swallowed. [`Resolved::from_booted`] emits exactly what
/// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
/// dispatcher prints it — the failure this guards is the loader resolving something the
/// operator did not write while they believe their own file is in force. (Warnings were
/// swallowed here until Phase 6c.)
///
/// ⚠ The warning is HAND-STUFFED, which it deliberately was not before. This drove
/// `Settings::clamp_to_policy` — `policy.rate.max_utilization` over
/// `preferences.rate_utilization` — and that clamp was removed with both of its fields: a
/// ceiling that bounded a value nothing read. The loader HAS a producer again
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`), and this test deliberately does not use it:
/// driving the real one would test the producer, where what this gates is the half that lives in
/// THIS crate and was the actual Phase-6c defect — a warning that exists reaches the operator,
/// verbatim.
#[test]
fn a_loader_warning_is_surfaced_not_swallowed() {
    let mut settings = vike_config::Settings::default();
    settings.warnings.push("preferences.something was resolved to 0.5".to_string());

    let lines = settings_warning_lines("a-program", &settings);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("a-program: settings: "), "the program's name: {}", lines[0]);
    assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
    assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
}

/// The quiet path stays quiet: nothing to resolve ⇒ nothing printed, so a line on stderr always
/// means something actually happened. Load-bearing for `vike-mcp`, whose stdout is a
/// protocol and whose stderr an agent may still read.
///
/// ⚠ The second half used to drive `load(None, …)` and assert silence, and that is no longer a
/// clean load — it is the "no project resolved" case, which now says so
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`). Both halves are kept and the second is
/// INVERTED rather than deleted: a `vike-cli` run from outside any project must still print
/// exactly one line, on stderr, and never on the stdout `mcp` speaks a protocol over.
#[test]
fn a_clean_load_prints_nothing_and_a_projectless_one_prints_exactly_one_line() {
    assert!(settings_warning_lines("a-program", &vike_config::Settings::default()).is_empty());
    let settings = vike_config::load(None).unwrap();
    let lines = settings_warning_lines("a-program", &settings);
    assert_eq!(lines.len(), 1, "no project ⇒ exactly one line: {lines:?}");
    assert!(lines[0].contains("no settings directory resolved"), "{}", lines[0]);
}

/// The refusal reads as the command the operator typed, and carries the mark's reason whole.
#[test]
fn the_seal_refusal_names_the_command_it_refuses() {
    let text = seal_refusal_text("a-program a-verb", "the seal is broken");
    assert!(text.starts_with("REFUSED: `a-program a-verb` will not run"), "{text}");
    assert!(text.contains("the seal is broken"), "{text}");
    assert!(text.ends_with(vike_config::UNSOUND_STORE_REPAIR), "{text}");
}
