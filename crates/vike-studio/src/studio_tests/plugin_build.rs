//! Plugin mode's Build -> sha -> Run ordering: the spec it resolves, Run refused until a Build has
//! answered, the staleness guard around an edit, a failed Build, and a reloaded sha.

use super::super::*;
use super::support::{answered_build, seeded_store, state_new};
use std::assert_matches;

// ---- plugin strategies --------------------------------------------------------------------

/// Flipping to Plugin makes `current_spec` resolve a [`StrategySpec::Plugin`] naming the
/// declared plugin and its last-built sha — the editor buffer itself never joins the spec
/// (unlike Rhai): it is BUILT by the builder service (`start_plugin_build`) and only the
/// resulting sha crosses into a run.
#[test]
fn plugin_mode_makes_current_spec_resolve_a_plugin() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source =
        "pub fn build<B>(p: &toml::Value) -> Box<dyn Strategy<B>> { todo!() }".to_string();
    st.plugin_name = "my_strat".to_string();
    st.plugin_sha = Some("a".repeat(64));
    match st.current_spec() {
        StrategySpec::Plugin { name, sha, .. } => {
            assert_eq!(name, "my_strat");
            assert_eq!(sha.len(), 64);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// The ordering rule from the spec, as a test: Run must not be reachable while no sha is held,
/// because the server would be asked for an artifact that does not exist yet.
#[test]
fn run_is_refused_until_a_build_has_returned_a_sha() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_sha = None;
    assert!(st.run_blocked_reason().is_some(), "Run must be blocked with no sha");

    st.plugin_sha = Some("a".repeat(64));
    assert!(st.run_blocked_reason().is_none(), "a returned sha must clear the refusal");
}

/// **THE SEQUENCING CONTRACT, folded through the real `poll`.** A build's `Ok(sha)` arrives on
/// the same worker channel every other dispatch uses, and delivering it does exactly two
/// things: it sets the sha, and it records the source that sha was built FROM. Nothing here
/// dispatches a run — Run is what the operator presses next, and this delivery is what stops
/// `run_blocked_reason` refusing it.
#[test]
fn a_delivered_build_sha_unblocks_run_and_nothing_else_is_dispatched() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();
    assert!(st.run_blocked_reason().is_some(), "no sha yet");

    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    assert!(st.any_running(), "a build in flight is an in-flight worker like any other");
    st.poll();

    assert_eq!(st.plugin_sha.as_deref(), Some("c".repeat(64).as_str()));
    assert_eq!(st.plugin_built_source.as_deref(), Some("// v1"));
    assert!(st.run_blocked_reason().is_none(), "the sha must unblock Run");
    assert!(st.build_rx.is_none(), "the receiver is consumed");
    // ⚠ The half that would be easy to leave out: a build must NOT start a run.
    assert!(!st.running, "a build answers with a sha and dispatches nothing");
    assert!(st.run_rx.is_none(), "a build must not have queued a run");
    assert!(st.last.is_none(), "and must not have fabricated a result");
}

/// **The STALENESS guard.** Editing after a successful build re-blocks Run: the held sha names
/// the PREVIOUS artifact, so running would report on code the author already replaced — with
/// plausible numbers and no error, which is the worst shape a defect can take here.
#[test]
fn an_edit_after_a_build_blocks_run_until_it_is_rebuilt() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert!(st.run_blocked_reason().is_none());

    st.editor.source = "// v2".to_string();
    let reason = st.run_blocked_reason().expect("an edit must re-block Run");
    assert!(reason.contains("changed"), "{reason}");

    // Building again clears it, with the NEW source recorded.
    st.build_rx = answered_build("// v2", Ok("d".repeat(64)));
    st.poll();
    assert!(st.run_blocked_reason().is_none(), "a fresh build clears the staleness refusal");
    assert_eq!(st.plugin_sha.as_deref(), Some("d".repeat(64).as_str()));
}

/// A FAILED build must not leave the previous sha standing. The buffer the author is looking
/// at is the one that failed to compile, so an unchanged "built abc123" chip beside it would
/// invite a Run over the LAST artifact and report it as this code.
#[test]
fn a_failed_build_clears_the_sha_rather_than_leaving_a_stale_one() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert!(st.plugin_sha.is_some());

    st.editor.source = "// v2 (does not compile)".to_string();
    st.build_rx = answered_build(
        "// v2 (does not compile)",
        Err("error[E0425]: cannot find value `x`".to_string()),
    );
    st.poll();

    assert!(st.plugin_sha.is_none(), "a failed build must not leave the old sha standing");
    assert!(st.run_blocked_reason().is_some(), "and Run must be blocked again");
    match &st.build_last {
        // rustc's diagnostics reach the author VERBATIM — the design's `Err(<rustc
        // diagnostics as text>)`.
        Some(Err(e)) => assert!(e.contains("E0425"), "{e}"),
        other => panic!("expected the diagnostics, got {other:?}"),
    }
}

/// A sha RELOADED from a saved row is not blocked by staleness, because this shell never held
/// the source that artifact was built from — a saved row carries a name and a sha and no file.
/// Claiming a mismatch against an unrelated buffer would break the reload path to guard
/// against a drift nothing here can measure.
#[test]
fn a_reloaded_saved_sha_is_not_refused_as_stale() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();

    // What the saved-row arm does: name + sha in, provenance cleared.
    st.plugin_name = "reloaded".to_string();
    st.plugin_sha = Some("e".repeat(64));
    st.plugin_built_source = None;
    st.editor.source = "// something else entirely".to_string();
    assert!(
        st.run_blocked_reason().is_none(),
        "a reloaded row must run — its artifact's source was never in this buffer"
    );
}

/// A build in flight is not started twice: a second press would race the first, and whichever
/// finished last would decide the sha.
#[test]
fn a_second_build_press_while_one_is_in_flight_is_a_no_op() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.start_plugin_build();
    st.poll();
    assert_eq!(
        st.plugin_sha.as_deref(),
        Some("c".repeat(64).as_str()),
        "the in-flight build's answer must survive a second press"
    );
}

/// **The staleness guard across the BUILD's own window — the defect the guard was blind to.**
/// The author presses Build on v1, keeps typing while cargo runs (a build takes minutes), and
/// the sha for v1 lands while the buffer holds v2. That sha names v1's artifact, so Run must stay
/// blocked with the staleness reason: running it would report v1's behaviour as v2's.
///
/// Driven through the REAL dispatch (`dispatch_plugin_build`, the body of
/// `start_plugin_build`) with a receiver whose sender this test holds, so the recorded source
/// is whatever the dispatch captured — not a value the test planted beside the answer. The
/// sent source is asserted too, so the test also pins WHAT was handed to the builder.
#[test]
fn an_edit_made_while_the_build_runs_is_not_recorded_as_built() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.plugin_name = "my_strat".to_string();
    st.editor.source = "// v1".to_string();

    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let mut handed_to_builder: Option<String> = None;
    st.dispatch_plugin_build(|_addr, _keys, _name, source| {
        handed_to_builder = Some(source);
        rx
    });
    assert_eq!(handed_to_builder.as_deref(), Some("// v1"), "the builder is sent the buffer");

    // The author keeps typing while the build runs; frames keep polling a pending build.
    st.editor.source = "// v2".to_string();
    st.poll();
    assert!(st.plugin_sha.is_none(), "nothing has answered yet");

    tx.send(Ok("c".repeat(64))).expect("the dispatch holds the receiver");
    st.poll();
    assert_eq!(st.plugin_sha.as_deref(), Some("c".repeat(64).as_str()), "the sha is accepted");
    assert_eq!(
        st.plugin_built_source.as_deref(),
        Some("// v1"),
        "the sha is the build of what was SENT, not of the buffer when it landed"
    );
    let reason =
        st.run_blocked_reason().expect("an edit made during the build must leave Run blocked");
    assert!(reason.contains("changed since the last Build"), "the STALENESS reason: {reason}");

    // The comparison is against the sent bytes exactly: undoing the edit unblocks Run.
    st.editor.source = "// v1".to_string();
    assert!(st.run_blocked_reason().is_none(), "the buffer is v1 again, which is what was built");
}

/// A plugin is not on the Named backend's roster any more than a script is — `named_spec`
/// falls through to `current_spec` rather than inventing a substitution.
///
/// ⚠ **`named_strategy` is set DELIBERATELY, and this test did not set it.** The arm being
/// covered is `(StrategySource::Plugin, _)` — a WILDCARD in the second position — and with
/// `named_strategy` left `None` every assertion below is equally satisfied by a rule reading
/// "anything with no name picked falls through", which is what the neighbouring
/// `(StrategySource::Native, None)` arm already says. Only a plugin that ALSO has a name
/// selected distinguishes the two: a substitution rule keyed on the name alone would swap in
/// `buy_hold` here, and this assertion would fail. Picking a real roster entry rather than an
/// invented string matters for the same reason — a name nothing could resolve would leave a
/// substituting implementation no substitution to make.
#[test]
fn plugin_mode_falls_through_named_spec_to_current_spec() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Plugin;
    st.named_strategy = Some("buy_hold".to_string());
    st.plugin_name = "my_strat".to_string();
    st.plugin_sha = Some("a".repeat(64));
    assert_eq!(st.named_spec(), st.current_spec());
    // ...and what it resolved to is still the PLUGIN, not the named native strategy the line
    // above would have diverted it to.
    assert_matches!(
        st.named_spec(),
        StrategySpec::Plugin { .. },
        "a picked native name must not divert a Plugin: {:?}",
        st.named_spec()
    );
}
