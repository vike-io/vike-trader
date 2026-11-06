use super::*;

fn live(tradehub_live: bool) -> Flags {
    Flags { tradehub_live, ..Default::default() }
}

/// **The residual this function exists to close, driven directly.** A box whose ONLY arming is
/// `flags.tradehub_live` reads as ARMED — the shape of a box live on a SWITCHLESS venue
/// (deribit/alpaca/aster/…), which has no venue row of its own to say so. Before this source
/// existed `vike-cli config check` degraded an unreadable credential store on such a live node.
#[test]
fn the_node_scoped_source_arms_with_no_venue_row_anywhere() {
    let v = armed_for_live(Some(live(true)));
    assert_eq!(v, LiveArmingVerdict::Armed(vec![TRADEHUB_LIVE_ARMING]), "{v:?}");
    assert!(v.refuses());
    assert_eq!(v.evidence()[0].scope, ArmingScope::Node, "it speaks for the whole mount");
}

/// The flag OFF is the paper box `docs/decisions/0013-degrade-vs-refuse.md` protects: nothing
/// armed, so an unreadable store stays a degrade. Same tree, one boolean apart — and
/// `Unarmed` is the ONLY verdict that does not refuse.
#[test]
fn the_node_scoped_source_is_silent_on_a_paper_box() {
    let v = armed_for_live(Some(live(false)));
    assert_eq!(v, LiveArmingVerdict::Unarmed);
    assert!(!v.refuses(), "a paper box must keep starting — ADR 0013's degrade");
    assert!(v.evidence().is_empty());
}

/// ⚠ **The hole one level up: a tree that did not LOAD is not a paper box.** `None` must never
/// answer `Unarmed`, because that is the same "no signal ⇒ assume paper" inference the
/// node-scoped source was added to end. It refuses instead.
#[test]
fn an_unresolvable_settings_tree_is_undetermined_and_still_refuses() {
    let v = armed_for_live(None);
    assert_eq!(v, LiveArmingVerdict::Undetermined);
    assert!(v.refuses(), "'I could not tell' must not be spelled 'not armed'");
    assert!(v.evidence().is_empty(), "…and it must not invent evidence either");
}

/// ⚠ `Flags::default()` is a REAL answer (a store with no `flags` row), not a stand-in for "unknown"
/// — the two are one `Option` apart and only one of them degrades. Pinned because passing the
/// default for an unresolved tree is the single call-site mistake that reopens the hole.
#[test]
fn default_flags_are_an_answer_and_none_is_not() {
    assert_eq!(armed_for_live(Some(Flags::default())), LiveArmingVerdict::Unarmed);
    assert_eq!(armed_for_live(None), LiveArmingVerdict::Undetermined);
}

/// Every source at once, each carrying its own scope — the shape a refusal message iterates so an
/// operator fixing a box is handed the whole list: the node-scoped flag first, then the Polymarket
/// rows in table order.
#[test]
fn every_source_is_reported_together_node_first() {
    let f =
        Flags { tradehub_live: true, poly_exec: true, poly_reconcile: true, ..Default::default() };
    let v = armed_for_live(Some(f));
    let ev = v.evidence();
    assert_eq!(ev.len(), 3, "{v:?}");
    assert_eq!(ev[0], TRADEHUB_LIVE_ARMING);
    assert!(ev[1].source.contains("flags.poly_exec"), "{v:?}");
    assert!(ev[2].source.contains("flags.poly_reconcile"), "{v:?}");
    assert!(ev[1..].iter().all(|a| a.scope == ArmingScope::Venue), "{v:?}");
}

/// The node-scoped source names the ONE place it resolves from — the `flags.tradehub_live` row —
/// and never the retired variable, which a booted root refuses (decision 0111): naming it would
/// send an operator to set a line that stops the daemon starting.
#[test]
fn the_node_scoped_source_names_the_row_and_no_variable() {
    assert!(TRADEHUB_LIVE_ARMING.source.contains("tradehub_live"), "the file KEY");
    assert!(TRADEHUB_LIVE_ARMING.source.contains("settings database"), "…and the row's home");
    assert!(!TRADEHUB_LIVE_ARMING.source.contains(".toml"), "no settings file is a source");
    assert!(
        !TRADEHUB_LIVE_ARMING.source.contains(crate::flags::TRADEHUB_LIVE_ENV),
        "the retired variable is not a source (decision 0111)"
    );
    assert!(TRADEHUB_LIVE_ARMING.arms.contains("SWITCHLESS"), "…and WHY it is the new source");
}

/// ⚠ **The two Polymarket flags ARE evidence**, and this pins the FACT the fold rests on rather
/// than the prose — the same shape as the exclusion it replaced, pointed the other way.
///
/// Its predecessor (`the_polymarket_flags_are_excluded_because_the_file_layer_arms_nothing`)
/// asserted `!is_consumed` for both keys and FAILED on the commit that wired them, which is the
/// whole reason the exclusion was gated instead of written down. If either is ever UN-wired,
/// this fails in turn and the fold has to be revisited.
#[test]
fn the_polymarket_flags_are_file_evidence_now() {
    for key in ["flags.poly_exec", "flags.poly_reconcile"] {
        assert!(
            crate::is_consumed(key),
            "{key} stopped being consumed — a row arms nothing again, so counting it as \
                 evidence would refuse a box over a setting with no effect (see POLY_FILE_ARMING)"
        );
    }
    // A Polymarket arm is evidence on its own, with the node-scoped flag off…
    let f = Flags { poly_exec: true, poly_reconcile: true, ..Default::default() };
    let v = armed_for_live(Some(f));
    assert_eq!(v.evidence().len(), 2, "{v:?}");
    assert!(v.evidence().iter().all(|a| a.scope == ArmingScope::Venue), "{v:?}");
    assert!(v.evidence()[0].source.contains("flags.poly_exec"), "it must name the setting: {v:?}");
    assert!(v.refuses());
    // …and each names the row, never the retired variable of the same gate (decision 0095), which
    // a booted root refuses at startup.
    for a in v.evidence() {
        assert!(a.source.contains("settings database"), "{a:?}");
        assert!(
            !a.source.contains(crate::flags::POLY_EXEC_ENV)
                && !a.source.contains(crate::flags::POLY_RECONCILE_ENV),
            "the retired variable is not a source: {a:?}"
        );
    }
}
