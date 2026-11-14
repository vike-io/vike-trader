//! Rust mode's Build -> sha -> Run ordering: the spec it resolves (the FILE), Run refused until a
//! Build has answered, the staleness guard around an edit, the guard that the editor holds the
//! server's file, a failed Build, and a reloaded sha.

use super::super::*;
use super::support::{answered_build, seeded_store, state_new};
use std::assert_matches;

/// The file `st` holds as opened from the server: `rust_file` and the editor's current text, as an
/// Open would have left them.
fn opened(st: &mut StudioState, file: &str) {
    st.rust_file = file.to_string();
    st.opened_rust_file = Some((file.to_string(), st.editor.source.clone()));
}

/// Flipping to Rust makes `current_spec` resolve the strategy FILE the buffer is built as: a hub
/// runs a `.rs` file in a sandboxed child and loads no artifact by its sha (plan 2b), so the sha
/// never crosses into a run.
#[test]
fn rust_mode_makes_current_spec_resolve_the_file() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Rust;
    st.editor.source =
        "pub fn build<B>(p: &vike_model::params::Params) -> Box<dyn Strategy<B>> { todo!() }"
            .to_string();
    st.rust_file = "my/strat.rs".to_string();
    st.rust_sha = Some("a".repeat(64));
    match st.current_spec() {
        StrategySpec::File { file, params } => {
            assert_eq!(file, "my/strat.rs");
            assert!(params.is_empty());
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
    st.strategy_source = StrategySource::Rust;
    opened(&mut st, "my/strat.rs");
    st.rust_sha = None;
    assert!(st.run_blocked_reason().is_some(), "Run must be blocked with no sha");

    st.rust_sha = Some("a".repeat(64));
    assert!(st.run_blocked_reason().is_none(), "a returned sha must clear the refusal");
}

/// **Run runs the server's FILE, so the editor must hold it as it was opened.** A buffer typed in,
/// or one edited and built since the Open, would report on the server's text instead of the code
/// in front of the author: refused in words, and cleared by the file's own text.
#[test]
fn a_rust_run_is_refused_while_the_editor_is_not_the_opened_file() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Rust;
    st.rust_file = "my/strat.rs".to_string();
    st.editor.source = "// typed in".to_string();
    st.build_rx = answered_build("// typed in", Ok("c".repeat(64)));
    st.poll();
    assert_eq!(st.run_blocked_reason(), Some(RUST_RUN_NEEDS_THE_FILE), "no file was opened");

    opened(&mut st, "my/strat.rs");
    assert_eq!(st.run_blocked_reason(), None, "the buffer is the file as opened, and built");

    st.editor.source = "// edited".to_string();
    st.build_rx = answered_build("// edited", Ok("d".repeat(64)));
    st.poll();
    assert_eq!(st.run_blocked_reason(), Some(RUST_RUN_NEEDS_THE_FILE), "built, but not the file");

    st.rust_file = "other/strat.rs".to_string();
    st.editor.source = "// typed in".to_string();
    st.build_rx = answered_build("// typed in", Ok("c".repeat(64)));
    st.poll();
    assert_eq!(
        st.run_blocked_reason(),
        Some(RUST_RUN_NEEDS_THE_FILE),
        "the opened text belongs to another path"
    );
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
    st.strategy_source = StrategySource::Rust;
    st.editor.source = "// v1".to_string();
    opened(&mut st, "my/strat.rs");
    assert!(st.run_blocked_reason().is_some(), "no sha yet");

    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    assert!(st.any_running(), "a build in flight is an in-flight worker like any other");
    st.poll();

    assert_eq!(st.rust_sha.as_deref(), Some("c".repeat(64).as_str()));
    assert_eq!(st.strategy_file_built_source.as_deref(), Some("// v1"));
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
    st.strategy_source = StrategySource::Rust;
    st.editor.source = "// v1".to_string();
    opened(&mut st, "my/strat.rs");
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert!(st.run_blocked_reason().is_none());

    st.editor.source = "// v2".to_string();
    let reason = st.run_blocked_reason().expect("an edit must re-block Run");
    assert!(reason.contains("changed"), "{reason}");

    // Building again clears the staleness, with the NEW source recorded; v2 is not the file.
    st.build_rx = answered_build("// v2", Ok("d".repeat(64)));
    st.poll();
    assert_eq!(st.run_blocked_reason(), Some(RUST_RUN_NEEDS_THE_FILE));
    assert_eq!(st.rust_sha.as_deref(), Some("d".repeat(64).as_str()));
}

/// A FAILED build must not leave the previous sha standing. The buffer the author is looking
/// at is the one that failed to compile, so an unchanged "built abc123" chip beside it would
/// invite a Run over the LAST artifact and report it as this code.
#[test]
fn a_failed_build_clears_the_sha_rather_than_leaving_a_stale_one() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Rust;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert!(st.rust_sha.is_some());

    st.editor.source = "// v2 (does not compile)".to_string();
    st.build_rx = answered_build(
        "// v2 (does not compile)",
        Err("error[E0425]: cannot find value `x`".to_string()),
    );
    st.poll();

    assert!(st.rust_sha.is_none(), "a failed build must not leave the old sha standing");
    assert!(st.run_blocked_reason().is_some(), "and Run must be blocked again");
    match &st.build_last {
        // rustc's diagnostics reach the author VERBATIM — the design's `Err(<rustc
        // diagnostics as text>)`.
        Some(Err(e)) => assert!(e.contains("E0425"), "{e}"),
        other => panic!("expected the diagnostics, got {other:?}"),
    }
}

/// A sha RELOADED from a saved row is not blocked by STALENESS, because this shell never held the
/// source that artifact was built from — a saved row carries a path and a sha and no text. It
/// still runs only over the file as opened: Run runs the server's file.
#[test]
fn a_reloaded_saved_sha_is_not_refused_as_stale() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Rust;
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();

    // What the saved-row arm does: path + sha in, provenance cleared.
    st.rust_sha = Some("e".repeat(64));
    st.strategy_file_built_source = None;
    st.editor.source = "// something else entirely".to_string();
    opened(&mut st, "reloaded.rs");
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
    st.strategy_source = StrategySource::Rust;
    st.rust_file = "my/strat.rs".to_string();
    st.editor.source = "// v1".to_string();
    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.start_strategy_file_build();
    st.poll();
    assert_eq!(
        st.rust_sha.as_deref(),
        Some("c".repeat(64).as_str()),
        "the in-flight build's answer must survive a second press"
    );
}

/// **The staleness guard across the BUILD's own window — the defect the guard was blind to.**
/// The author presses Build on v1, keeps typing while cargo runs (a build takes minutes), and
/// the sha for v1 lands while the buffer holds v2. That sha names v1's artifact, so Run must stay
/// blocked with the staleness reason: running it would report v1's behaviour as v2's.
///
/// Driven through the REAL dispatch (`dispatch_strategy_file_build`, the body of
/// `start_strategy_file_build`) with a receiver whose sender this test holds, so the recorded source
/// is whatever the dispatch captured — not a value the test planted beside the answer. The
/// sent source and FILE are asserted too, so the test also pins WHAT was handed to the builder.
#[test]
fn an_edit_made_while_the_build_runs_is_not_recorded_as_built() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Rust;
    st.editor.source = "// v1".to_string();
    opened(&mut st, "my/strat.rs");

    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let mut handed_to_builder: Option<(String, String)> = None;
    st.dispatch_strategy_file_build(|_addr, _keys, file, source| {
        handed_to_builder = Some((file, source));
        rx
    });
    assert_eq!(
        handed_to_builder,
        Some(("my/strat.rs".to_string(), "// v1".to_string())),
        "the builder is sent the file's path and the buffer"
    );

    // The author keeps typing while the build runs; frames keep polling a pending build.
    st.editor.source = "// v2".to_string();
    st.poll();
    assert!(st.rust_sha.is_none(), "nothing has answered yet");

    tx.send(Ok("c".repeat(64))).expect("the dispatch holds the receiver");
    st.poll();
    assert_eq!(st.rust_sha.as_deref(), Some("c".repeat(64).as_str()), "the sha is accepted");
    assert_eq!(
        st.strategy_file_built_source.as_deref(),
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

/// **Save writes the buffer to its server file, and its answer makes that text the file a Run
/// runs** (the owner's ruling of 2026-11-13: the backtesthub manages the strategy files). Folded
/// through the real `poll`: the answer's one line names the file, the sha and the state, the list is
/// fetched again, and in Rust mode the saved text is the opened file — so after its Build, Run goes.
#[test]
fn a_save_answer_names_the_file_sha_and_state_and_the_saved_text_is_the_file_run_runs() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    // A dial nothing answers: the list fetch the save starts must reach no real daemon.
    st.backend = crate::backend::remote::Backend::Remote { addr: "127.0.0.1:1".to_string() };
    st.strategy_source = StrategySource::Rust;
    st.rust_file = "my/strat.rs".to_string();
    st.editor.source = "// edited".to_string();
    st.build_rx = answered_build("// edited", Ok("c".repeat(64)));
    st.poll();
    assert_eq!(st.run_blocked_reason(), Some(RUST_RUN_NEEDS_THE_FILE), "not on the server yet");

    let (tx, rx) = std::sync::mpsc::channel();
    st.save_file_rx = Some((("my/strat.rs".to_string(), "// edited".to_string()), rx));
    let entry = vike_backtesthub_client::proto::WireStrategyFile {
        file: "my/strat.rs".to_string(),
        build: format!("built {}", "c".repeat(8)),
        broken: String::new(),
        edited: false,
    };
    tx.send(Ok(("c".repeat(64), entry))).unwrap();
    st.poll();
    assert_eq!(
        st.save_file_last,
        Some(Ok("saved my/strat.rs · sha256 cccccccc · built cccccccc".to_string()))
    );
    assert!(st.strategy_files_rx.is_some(), "the list is fetched again");
    assert_eq!(st.run_blocked_reason(), None, "the server's file is now this built buffer");
}

/// Save is refused, with the reason on the button, where the shell can already see it would be:
/// a strategy-FILE pick (it writes the editor buffer), no file name, or a name of the other
/// language. The server judges the rest of the path itself.
#[test]
fn save_says_why_it_is_refused_before_anything_is_sent() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = crate::backend::remote::Backend::Remote { addr: "127.0.0.1:1".to_string() };
    st.strategy_source = StrategySource::Native;
    assert!(st.save_blocked_reason().is_some_and(|w| w.contains("editor buffer")));
    st.strategy_source = StrategySource::Rhai;
    st.rhai_file.clear();
    assert_eq!(st.save_blocked_reason(), Some("Name the file first."));
    st.rhai_file = "trend/x.rs".to_string();
    assert_eq!(st.save_blocked_reason(), Some("A Rhai buffer saves as a .rhai file."));
    st.rhai_file = "trend/x.rhai".to_string();
    assert_eq!(st.save_blocked_reason(), None);
    st.strategy_source = StrategySource::Rust;
    st.rust_file = "trend/x.rhai".to_string();
    assert_eq!(st.save_blocked_reason(), Some("A Rust buffer saves as a .rs file."));

    // A refused dial comes back as the save's error, in words, and nothing else moves.
    st.rust_file = "trend/x.rs".to_string();
    st.start_save_strategy_file();
    assert_eq!(st.save_blocked_reason(), Some("A save is running."));
    for _ in 0..200 {
        st.poll();
        if st.save_file_last.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        matches!(&st.save_file_last, Some(Err(e)) if e.contains("127.0.0.1:1")),
        "{:?}",
        st.save_file_last
    );
    assert!(st.opened_rust_file.is_none(), "a refused save makes nothing the server's file");
}

/// A Rust file is not on the Named backend's roster any more than a script is — `named_spec`
/// falls through to `current_spec` rather than inventing a substitution.
///
/// ⚠ **`named_strategy` is set DELIBERATELY.** The arm being covered is
/// `(StrategySource::Rust, _)` — a WILDCARD in the second position — and with `named_strategy`
/// left `None` every assertion below is equally satisfied by a rule reading "anything with no name
/// picked falls through", which is what the neighbouring `(StrategySource::Native, None)` arm
/// already says. Only a Rust buffer that ALSO has a name selected distinguishes the two: a
/// substitution rule keyed on the name alone would swap in `buy_hold` here.
#[test]
fn rust_mode_falls_through_named_spec_to_current_spec() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StrategySource::Rust;
    st.named_strategy = Some("buy_hold".to_string());
    st.rust_file = "my/strat.rs".to_string();
    st.rust_sha = Some("a".repeat(64));
    assert_eq!(st.named_spec(), st.current_spec());
    // ...and what it resolved to is still the Rust FILE, not the named strategy the line above
    // would have diverted it to.
    assert_matches!(
        st.named_spec(),
        StrategySpec::File { ref file, .. } if file == "my/strat.rs",
        "a picked native name must not divert a Rust file: {:?}",
        st.named_spec()
    );
}
