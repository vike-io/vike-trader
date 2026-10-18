//! The profile -> `CoreConfig` / `JournalConfig` glue, `[guards.margin_call]` included, pinned. The schema and its
//! validation are `vike_config::run_profile`'s tests.

use vike_config::run_profile::samples;

use super::*;

// ---------------------------------------------------------------------------------------------
// Mapping to the real runtime types (the compile-checked converters).
// ---------------------------------------------------------------------------------------------

#[test]
fn maps_to_real_margin_call() {
    let p = samples::live().unwrap();
    let mut cfg = vike_core::CoreConfig::default();
    apply_guards_and_sinks(&p, &mut cfg);
    let mc = cfg.margin_call.expect("margin call present");
    assert_eq!(mc.mm_requirement, 0.05);
    assert_eq!(mc.warn_fraction, 0.05);
    assert_eq!(mc.buffer, 0.10);
}

/// The profile's `[sinks.journal]` TUNES the journal the row names: each key it sets lands on the
/// real [`vike_core::JournalConfig`], and the directory is the caller's (the `config.journal_dir` row).
#[test]
fn maps_to_real_journal_config() {
    let toml = r#"
[sinks.journal]
segment_bytes = 4096
flush_every = 8
snapshot_every = 64
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    let jc = journal_config_from(Some(&p), Some("/srv/wal".into()), Some(7))
        .expect("the row names a directory");
    assert_eq!(jc.dir, std::path::PathBuf::from("/srv/wal"), "the directory is the row's");
    assert_eq!(jc.file.segment_bytes, 4096);
    assert_eq!(jc.file.flush_every, 8);
    assert_eq!(jc.snapshot_every, 64, "a key the profile names beats the cadence row");
}

#[test]
fn margin_call_defaults_apply() {
    // omit warn_fraction/buffer -> LEAN defaults
    let toml = r#"
[guards.margin_call]
mm_requirement = 0.04
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    let mut cfg = vike_core::CoreConfig::default();
    apply_guards_and_sinks(&p, &mut cfg);
    let mc = cfg.margin_call.expect("margin call present");
    assert_eq!(mc.mm_requirement, 0.04);
    assert_eq!(mc.warn_fraction, 0.05);
    assert_eq!(mc.buffer, 0.10);
}

// ---------------------------------------------------------------------------------------------
// journal_config_from — the directory is the caller's row, the tuning is layered over it.
// ---------------------------------------------------------------------------------------------

#[test]
fn no_directory_row_is_journaling_off() {
    assert!(journal_config_from(None, None, None).is_none());
}

/// ⚠ **A run profile cannot turn the journal on.** The live sample carries a `[sinks.journal]`
/// tuning table; with no `config.journal_dir` row there is still no journal.
#[test]
fn a_profile_with_a_journal_table_and_no_directory_row_is_off() {
    let p = samples::live().unwrap();
    assert!(p.sinks.journal.is_some(), "precondition: the profile carries tuning");
    assert!(journal_config_from(Some(&p), None, Some(64)).is_none());
}

#[test]
fn a_directory_row_alone_uses_the_default_cadence() {
    let jc = journal_config_from(None, Some("data/journal".into()), None)
        .expect("a dir enables the journal");
    assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
    let at = vike_core::JournalConfig::at("data/journal");
    assert_eq!(jc.file.segment_bytes, at.file.segment_bytes);
    assert_eq!(jc.file.flush_every, at.file.flush_every);
    assert_eq!(jc.snapshot_every, at.snapshot_every);
}

#[test]
fn the_snapshot_cadence_row_applies() {
    let jc = journal_config_from(None, Some("d".into()), Some(64)).unwrap();
    assert_eq!(jc.snapshot_every, 64);
    // a 0 override is ignored (0 would snapshot on every record — a p99 cliff the CoreThread
    // asserts against)
    let jc0 = journal_config_from(None, Some("d".into()), Some(0)).unwrap();
    assert_eq!(jc0.snapshot_every, vike_core::JournalConfig::at("d").snapshot_every);
}

/// ⚠ **A run profile cannot turn the journal off either.** A profile with NO `[sinks.journal]`
/// leaves the row's journal on, at the row's cadence — the case the old rule (a resolved profile
/// decides alone) turned OFF.
#[test]
fn a_profile_without_a_journal_table_leaves_the_row_in_charge() {
    let p = samples::paper().unwrap();
    assert!(p.sinks.journal.is_none(), "precondition");
    let jc = journal_config_from(Some(&p), Some("data/journal".into()), Some(64))
        .expect("the row turns the journal on whatever the profile says");
    assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
    assert_eq!(jc.snapshot_every, 64, "the cadence row stands");
}

/// A key the profile's table leaves off keeps the row's (or the default) value: silence is
/// silence, the `Guards::submit_ack_confirm_grace_ms` shape.
#[test]
fn a_tuning_key_left_off_keeps_the_rung_below() {
    let toml = "[sinks.journal]\nflush_every = 8\n";
    let p = RunProfile::from_toml_str(toml).unwrap();
    let jc = journal_config_from(Some(&p), Some("d".into()), Some(64)).unwrap();
    assert_eq!(jc.file.flush_every, 8, "the key the profile names");
    assert_eq!(jc.snapshot_every, 64, "the cadence row, which the profile did not name");
    assert_eq!(jc.file.segment_bytes, vike_core::JournalConfig::at("d").file.segment_bytes);
}

// ---------------------------------------------------------------------------------------------
// `guards.initial_trading_state`: the command the daemon sends the spawned core.
// ---------------------------------------------------------------------------------------------

/// A profile naming only `guards.initial_trading_state = "<word>"`.
fn profile_starting(word: &str) -> RunProfile {
    RunProfile::from_toml_str(&format!("[guards]\ninitial_trading_state = \"{word}\"\n"))
        .expect("a lone trading state is a loadable profile")
}

/// **`active` — the default, named or not — sends NOTHING**, whatever the journal restored. The
/// rule that keeps every profile that never heard of the key byte-identical to before it was wired:
/// no command, so nothing journaled and no engine touched.
#[test]
fn an_active_profile_sends_nothing_whatever_the_journal_restored() {
    use vike_model::TradingState::{Active, Halted, Reducing};
    let unset = RunProfile::from_toml_str("").expect("the empty profile loads");
    for profile in [unset, profile_starting("active")] {
        for restored in [vec![], vec![Active], vec![Reducing], vec![Active, Halted]] {
            assert_eq!(
                initial_trading_state_to_send(&profile, restored.iter().copied()),
                None,
                "an `active` profile must send nothing over {restored:?}"
            );
        }
    }
}

/// **A `halted` or `reducing` profile sends its state — unless the journal restored something
/// STRICTER, in which case the journal's state stands and nothing is sent.** The last rows are the
/// rule this exists for: a profile must never LOOSEN a restored halt (only an operator's
/// `SetTradingState` does), and the equal and stricter-profile rows pin the boundary on both sides
/// of it.
#[test]
fn a_halted_or_reducing_profile_is_sent_unless_the_journal_is_stricter() {
    use vike_model::TradingState::{Active, Halted, Reducing};
    // (the profile's word, the restored engines' states, what is sent)
    let matrix: [(&str, Vec<vike_model::TradingState>, Option<vike_model::TradingState>); 8] = [
        ("halted", vec![], Some(Halted)),
        ("reducing", vec![], Some(Reducing)),
        ("halted", vec![Active], Some(Halted)),
        ("halted", vec![Reducing], Some(Halted)),
        ("halted", vec![Halted], Some(Halted)),
        ("reducing", vec![Active, Reducing], Some(Reducing)),
        ("reducing", vec![Halted], None),
        ("reducing", vec![Active, Halted], None),
    ];
    for (word, restored, want) in matrix {
        assert_eq!(
            initial_trading_state_to_send(&profile_starting(word), restored.iter().copied()),
            want,
            "profile `{word}` over a journal that restored {restored:?}"
        );
    }
}

/// [`send_initial_trading_state`] hands the lane exactly the command the decision names — one
/// `SetTradingState`, and only then — and returns what it sent.
#[test]
fn the_sender_puts_one_set_trading_state_on_the_lane_when_and_only_when_it_sends() {
    use vike_model::TradingState::{Active, Halted};
    let mut lane: Vec<vike_exec::Command> = Vec::new();

    let sent = send_initial_trading_state(&profile_starting("halted"), &[Active], |c| lane.push(c));
    assert_eq!(sent, Some(Halted));
    assert!(
        matches!(lane.as_slice(), [vike_exec::Command::SetTradingState(Halted)]),
        "exactly one SetTradingState(Halted): {lane:?}"
    );

    lane.clear();
    assert_eq!(
        send_initial_trading_state(&profile_starting("active"), &[], |c| lane.push(c)),
        None
    );
    assert_eq!(
        send_initial_trading_state(&profile_starting("reducing"), &[Halted], |c| lane.push(c)),
        None,
        "a restored halt is not loosened by a `reducing` profile"
    );
    assert!(lane.is_empty(), "nothing was sent in either case: {lane:?}");
}
