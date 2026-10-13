use super::*;

#[test]
fn an_enforced_ceiling_reads_and_an_unknown_key_does_not() {
    // `policy.*` is carried by `POLICY_VERDICTS`; answering `false` for an ENFORCED ceiling would
    // make `config show` warn about a limit that binds.
    assert!(is_consumed("policy.max_notional_per_order"));
    assert_eq!(
        read_status("policy.max_notional_per_order"),
        ReadStatus::Read { binary: None },
        "a ceiling is enforced across several programs, so no single binary is named"
    );
    // ⚠ …but a key NO table carries is not "consumed". It answered `true` here until a ceiling
    // nothing enforces printed `READ: yes`; an unknown key says so instead of borrowing that answer.
    assert!(!is_consumed("no.such.key"));
    assert_eq!(read_status("no.such.key"), ReadStatus::Unknown);
    assert_eq!(consumer_of("no.such.key"), None);
    // A misspelt ceiling is not a ceiling: the policy table does not answer for a prefix.
    assert_eq!(read_status("policy.max_levrage"), ReadStatus::Unknown);
    assert!(policy_verdict_of("policy.max_levrage").is_none());
}

/// **`policy.max_leverage` is ENFORCED, and `config show` says `READ: yes` for the right reason.**
/// It narrows the run profile's leverage (`vike_mount::make_engine_for_account`), and
/// `policy_is_consumed.rs`'s `POLICY_CONSUMERS` holds the read to the tree.
#[test]
fn the_leverage_ceiling_reads_as_enforced() {
    assert_eq!(read_status("policy.max_leverage"), ReadStatus::Read { binary: None });
    assert!(is_consumed("policy.max_leverage"));
    assert_eq!(read_status("policy.max_leverage").word(), "read");
    assert_eq!(read_status("policy.max_notional_per_order").word(), "read");
    assert_eq!(read_status("no.such.key").word(), "unknown");
}

/// **A ceiling the shipped table ADMITS nothing enforces says so.** No shipped row is `Unenforced`
/// today (the last was `policy.max_leverage`), so the mapping is held on a planted verdict: the
/// admission reads as `Unread`, leads with "NOTHING ENFORCES THIS CEILING", and carries the row's
/// own paragraph — the property that kept `config show` from printing `READ: yes` under it.
#[test]
fn an_unenforced_verdict_reads_as_unread_and_says_nothing_enforces_it() {
    const WHY: &str = "the run profile enforces the concept instead, so this row limits nothing";
    let ReadStatus::Unread { verdict, why } =
        policy_status(&PolicyVerdict::Unenforced { why: WHY })
    else {
        panic!("an Unenforced verdict must read as Unread");
    };
    assert!(verdict.contains("NOTHING ENFORCES"), "{verdict}");
    assert_eq!(why, WHY);
    assert_eq!(policy_status(&PolicyVerdict::Unenforced { why: WHY }).word(), "unread");
    assert_eq!(policy_status(&PolicyVerdict::Enforced), ReadStatus::Read { binary: None });
}

/// **The default of the question is not "yes".** Every key `setting_keys` lists — what `config show`
/// prints, what the wire carries, what `config set` accepts — must have a row in one of the two
/// tables. This is what keeps [`ReadStatus::Unknown`] unreachable from the shipped commands: a key
/// added without its row fails HERE, by name, instead of printing an answer nobody gave.
#[test]
fn every_setting_key_has_an_explicit_read_status() {
    let unknown: Vec<String> = setting_keys()
        .into_iter()
        .map(|k| k.key)
        .filter(|k| read_status(k) == ReadStatus::Unknown)
        .collect();
    assert!(
        unknown.is_empty(),
        "these settings keys are in NO consumption table, so `config show` could only answer \
         `unknown` for them. Add a `CONSUMPTION` row (config, preferences, flags) or a \
         `POLICY_VERDICTS` row (policy): {unknown:?}"
    );
}

/// The policy table names exactly the `policy.*` keys `setting_keys` lists, each once — the other
/// half of the gate above (`read_status` answers for a policy key from this table alone).
#[test]
fn the_policy_table_names_exactly_the_policy_keys_once_each() {
    let mut table: Vec<&str> = POLICY_VERDICTS.iter().map(|row| row.0).collect();
    let mut keys: Vec<String> =
        setting_keys().into_iter().map(|k| k.key).filter(|k| k.starts_with("policy.")).collect();
    table.sort_unstable();
    keys.sort();
    assert!(table.windows(2).all(|w| w[0] != w[1]), "a policy key appears twice: {table:?}");
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    assert_eq!(table, keys, "POLICY_VERDICTS and the policy keys `setting_keys` lists disagree");
}

#[test]
fn a_wired_key_and_an_unwired_one_answer_differently() {
    assert!(is_consumed("config.log_dir"));
    // ⚠ `flags.poly_heartbeat`, not `flags.poly_exec`: the unread-settings sweep WIRED
    // `poly_exec`, and an example that has moved to the other column proves the opposite of
    // what it says. The heartbeat flag is one of the seven the owner deliberately DEFERRED.
    assert!(!is_consumed("flags.poly_heartbeat"));
    assert!(consumer_of("config.log_dir").unwrap().file().is_some());
    assert!(consumer_of("flags.poly_heartbeat").unwrap().why_not().is_some());
}

/// **The keys a headless box was told it could set.** `config.store_root` and
/// `preferences.chart_style` reported `READ: yes` while their only reader is the GUI — so on a
/// tradehub or recorder box, setting one did nothing and the output said it would work.
///
/// ⚠ `config.state_dir` WAS the third. It left this list when the desktop cut deleted
/// `state_dir_path`, its one reader, and it has now left the TABLE: the unread-settings sweep
/// deleted the key and refuses both spellings.
/// `the_deleted_state_dir_key_is_gone_from_this_table_entirely` is what holds it gone —
/// dropping it silently would have turned a key that configures NOTHING into a key this suite
/// simply stopped asking about.
#[test]
fn a_gui_only_setting_names_the_gui() {
    for key in [
        "config.store_root",
        "preferences.chart_style",
        "preferences.theme",
        "preferences.market_colors",
        "preferences.header_gradient",
        "preferences.density",
        "preferences.text_size",
        "preferences.export_dir",
        // The desktop's tuning rows and the Studio chat's: read through `main.rs`'s accessors.
        "preferences.observe_poll_ms",
        "preferences.chart_history_bars",
        "preferences.backfill_lookback_days",
        "preferences.ai_anthropic_model",
        "preferences.ai_cerebras_model",
        "preferences.ai_timeout_secs",
        "preferences.vsync",
    ] {
        let c = consumer_of(key).unwrap_or_else(|| panic!("{key} must have a row"));
        assert!(c.is_consumed(), "{key} is read — that part was never wrong");
        assert_eq!(
            c.binary(),
            Some("desktop"),
            "{key}'s only reader is vike-desktop, and the column has to say so"
        );
    }
}

/// The END of `config.state_dir`. It was a `Consumer::At` on the GUI, then a written admission
/// that its one reader had gone, and it is now **no key at all** — which is what that
/// admission's own text asked for ("deletion plus a `REMOVED_ENV` refusal, as its own change").
///
/// This test replaces `a_gui_only_setting_that_lost_its_reader_says_so` and is the same
/// property from the other side: what must never happen is the key coming back WITHOUT a
/// reader. A row here would put it back in `vike-cli config show`'s table, and the refusals in
/// `crate::removed` and `crate::config::ConfigPatch` would then contradict it.
#[test]
fn the_deleted_state_dir_key_is_gone_from_this_table_entirely() {
    assert!(
        consumer_of("config.state_dir").is_none(),
        "config.state_dir was DELETED — both spellings refuse now. A row here would make \
             `config show` list a key the loader will not accept"
    );
    assert!(
        !unconsumed_keys().contains(&"config.state_dir"),
        "...and it must not be warned about either: there is nothing left to set"
    );
}

/// …and a daemon-side key names the daemon, or the column would be a constant.
#[test]
fn a_daemon_setting_names_the_daemon() {
    for key in ["config.log_dir", "config.tradehub_addr", "flags.tradehub_control"] {
        assert_eq!(consumer_of(key).and_then(Consumer::binary), Some("tradehub"), "{key}");
    }
}

/// `config.node_ack_wait_ms` is the CLIENT's wait for a node's verdict: `vike-cli`'s boot walk reads
/// it (a library file of that crate, not a `main.rs`), so it is READ and names no single binary —
/// exactly like its pair `config.node_addr`, and unlike the GUI-only rows above.
#[test]
fn the_client_ack_wait_is_read_by_the_cli_boot_and_names_no_single_binary() {
    let c = consumer_of("config.node_ack_wait_ms").expect("the key has a row");
    assert!(c.is_consumed());
    assert_eq!(c.file(), Some("crates/vike-cli/src/boot.rs"));
    assert_eq!(c.binary(), None, "a boot-walk read belongs to every binary that links it");
    assert_eq!(read_status("config.node_ack_wait_ms"), ReadStatus::Read { binary: None });
}

/// An UNREAD row has no binary — there is nothing to name — and neither does a hypothetical
/// library consumer, whose read belongs to every binary that links it rather than to one.
#[test]
fn an_unread_row_and_a_library_consumer_name_no_binary() {
    assert_eq!(consumer_of("flags.poly_heartbeat").and_then(Consumer::binary), None);

    let lib = Consumer::At { file: "crates/vike-tradehub/src/reconcile_config.rs", needle: "x" };
    assert_eq!(
        lib.binary(),
        None,
        "a library read has no single owning binary, so `config show` must keep saying `yes` \
             rather than invent one"
    );
    // A `src/bin/` entry point resolves like a `main.rs` one, prefix stripped either way.
    let b = Consumer::At { file: "crates/vike-backfill/src/bin/eod_backfill.rs", needle: "x" };
    assert_eq!(b.binary(), Some("eod_backfill"));
    let nested = Consumer::At { file: "crates/bridges/polymarket/src/main.rs", needle: "x" };
    assert_eq!(nested.binary(), Some("polymarket"));
}

/// Every consumed row resolves to SOMETHING renderable — a binary name or an honest `None`.
/// The rule must not panic or produce an empty string on any path in the real table.
#[test]
fn the_rule_survives_every_row_in_the_real_table() {
    for row in CONSUMPTION {
        if let Some(b) = row.by.binary() {
            assert!(!b.is_empty(), "{} produced an empty binary name", row.key);
            assert!(!b.contains('/'), "{} produced a path, not a name: {b}", row.key);
        }
    }
}

/// **The verdict an operator reads is DERIVED from the variant, and the three differ.**
///
/// The whole point of [`Reader`] is that two of these states must not render as the third.
/// `flags.poly_heartbeat` said "use the environment variable" while the variable did nothing;
/// a rendering rule that collapsed the variants would hand that defect straight back.
#[test]
fn the_three_reader_states_tell_an_operator_three_different_things() {
    let live = Reader::Live {
        file: "crates/vike-backtest/src/harness/sweep.rs",
        needle: "x",
        caller: "crates/vike-backtest/src/harness/optimize.rs",
        call: "y",
    };
    let uncalled = Reader::Uncalled { file: "f", needle: "n", entry: &["T::spawn"] };

    assert!(live.env_still_works(), "the one state in which exporting the variable helps");
    assert!(!uncalled.env_still_works());
    assert!(!Reader::Nothing.env_still_works());

    // …and the three verdicts are genuinely distinct strings, not one message with decoration.
    let verdicts = [live.verdict(), uncalled.verdict(), Reader::Nothing.verdict()];
    for (i, a) in verdicts.iter().enumerate() {
        for b in &verdicts[i + 1..] {
            assert_ne!(a, b, "two Reader states render identically");
        }
    }
    assert!(live.verdict().contains("still works"));
    assert!(uncalled.verdict().contains("NEITHER SPELLING"));
    assert!(Reader::Nothing.verdict().contains("NEITHER SPELLING"));
}

/// The rows this change was made for: they claim `Uncalled`, and an operator who set one must be
/// told that neither spelling configures anything (every one of their variables refuses startup).
/// There were six; `flags.record_chains` left the list when the data daemon mounted its recorder
/// (its row is a `Consumer::At` now), and its variable still refuses startup.
///
/// Pinned by NAME rather than by counting `Uncalled` rows, because the failure being prevented
/// is a specific row quietly going back to claiming a live variable — a count would stay green
/// through a swap.
#[test]
fn the_dead_variable_rows_say_so() {
    for key in [
        "flags.hl_outcome",
        "flags.pm_resolve",
        "flags.poly_auto_redeem",
        "flags.poly_heartbeat",
        "flags.poly_redeem_halt",
    ] {
        let c = consumer_of(key).unwrap_or_else(|| panic!("{key} must have a row"));
        let reader = c.reader().unwrap_or_else(|| panic!("{key} must be a Consumer::Not"));
        assert!(
            !reader.env_still_works(),
            "{key}: this row told operators to export a variable that nothing reachable reads"
        );
        assert!(
            c.unread_verdict().is_some_and(|v| v.contains("NEITHER SPELLING")),
            "{key}: `config show` must lead with the fact that neither spelling works"
        );
    }
}

/// **`flags.record_properties` is DECLARED AND UNWIRED, and its row says so.** It claimed
/// `Consumer::At` on the daemon's fold line, and the fold is a carrier, not a reader: no root opens
/// the properties recorder, so the flag arms nothing and `config show` must not print a binary under
/// it. Pinned by name for the reason `the_dead_variable_rows_say_so` is — the failure being
/// prevented is a specific row quietly going back to `At`.
#[test]
fn the_properties_recorder_flag_is_declared_and_unwired() {
    let key = "flags.record_properties";
    let c = consumer_of(key).expect("the key keeps its row");
    assert!(!c.is_consumed(), "{key} arms nothing — no root opens the recorder");
    assert_eq!(c.binary(), None);
    assert!(c.why_not().is_some_and(|why| why.contains("UNWIRED") && why.len() > 60));
    assert!(
        c.unread_verdict().is_some_and(|v| v.contains("NEITHER SPELLING")),
        "its variable is retired and the recorder is opened by nothing"
    );
    assert!(matches!(read_status(key), ReadStatus::Unread { .. }));
}

/// **NO row claims a live variable today, and that is a state worth asserting rather than
/// leaving to be noticed.**
///
/// ⚠ This test was `the_one_live_variable_row_says_so`, and its subject was
/// `preferences.sweep_threads` — the one key where exporting `VIKE_SWEEP_THREADS` genuinely
/// worked while the FILE key was inert. That key is now WIRED (`Consumer::At`, two composition
/// roots), so [`Reader::Live`] has no rows left, and the sibling test above stopped having a
/// row-level counterexample.
///
/// The variant STAYS, and this is the reason: its six `Uncalled` neighbours each become `Live`
/// the moment something calls the entry point they name, which is a real and expected
/// transition. What keeps `env_still_works` honest in the meantime is
/// [`the_three_reader_states_tell_an_operator_three_different_things`] above, which constructs
/// one of each variant — a unit-level check that cannot rot with the table.
///
/// So what this asserts is the FACT, not a policy: a row added in that state is a finding to
/// read, not a failure, and the assertion message says which.
#[test]
fn no_row_claims_a_live_variable_today() {
    let live: Vec<&str> = CONSUMPTION
        .iter()
        .filter(|c| c.by.reader().is_some_and(|r| r.env_still_works()))
        .map(|c| c.key)
        .collect();
    assert!(
        live.is_empty(),
        "a row is back in the `Reader::Live` state ({live:?}). That is legitimate — it means \
             something now reaches that key's env read — but it also means the sibling test's \
             `!env_still_works` assertion has a real counterexample again: re-point this test at \
             the row rather than deleting it."
    );
}

/// A CONSUMED row has no verdict to render — there is nothing unread to explain.
#[test]
fn a_consumed_row_has_no_reader_and_no_verdict() {
    let c = consumer_of("config.log_dir").expect("row");
    assert_eq!(c.reader(), None);
    assert_eq!(c.unread_verdict(), None);
}

#[test]
fn the_unconsumed_list_is_exactly_the_not_rows() {
    let listed = unconsumed_keys();
    assert_eq!(listed.len(), CONSUMPTION.iter().filter(|c| !c.by.is_consumed()).count());
    assert!(listed.contains(&"flags.pm_resolve"));
    assert!(listed.contains(&"flags.record_properties"), "no root opens the recorder");
    assert!(!listed.contains(&"flags.record_chains"), "the data daemon reads it now");
    assert!(!listed.contains(&"config.tradehub_addr"));
    // ⚠ `flags.record_dvol` was asserted HERE, as the row that went BACKWARDS. The key is
    // deleted (`crate::flags::DEAD_FLAG_KEYS`), so this table has nothing to say about it and
    // the assertion goes with it — asserting the ABSENCE of a deleted key would pin a fact
    // about nothing and would have to be deleted again the day the feed is re-mounted.
}
