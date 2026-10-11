use super::*;

#[test]
fn an_unknown_key_is_not_reported_as_unread() {
    // `policy.*` has its own gate; answering `false` here would make `config show` warn about
    // ceilings that ARE enforced.
    assert!(is_consumed("policy.max_notional_per_order"));
    assert!(is_consumed("no.such.key"));
    assert_eq!(consumer_of("no.such.key"), None);
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

/// The six rows this change was made for: they claim `Uncalled`, and an operator who set one
/// must be told that neither spelling configures anything (every one of the six variables refuses startup).
///
/// Pinned by NAME rather than by counting `Uncalled` rows, because the failure being prevented
/// is a specific row quietly going back to claiming a live variable — a count would stay green
/// through a swap.
#[test]
fn the_six_dead_variable_rows_say_so() {
    for key in [
        "flags.hl_outcome",
        "flags.pm_resolve",
        "flags.poly_auto_redeem",
        "flags.poly_heartbeat",
        "flags.poly_redeem_halt",
        "flags.record_chains",
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
    assert!(listed.contains(&"flags.record_chains"));
    assert!(!listed.contains(&"config.tradehub_addr"));
    // ⚠ `flags.record_dvol` was asserted HERE, as the row that went BACKWARDS. The key is
    // deleted (`crate::flags::DEAD_FLAG_KEYS`), so this table has nothing to say about it and
    // the assertion goes with it — asserting the ABSENCE of a deleted key would pin a fact
    // about nothing and would have to be deleted again the day the feed is re-mounted.
}
