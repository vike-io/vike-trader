use super::*;
use std::assert_matches;

/// ⚠ `toml::from_str`, NOT `src.parse()`. Under the workspace's pinned `toml = "1.1"`,
/// `FromStr for Value` parses a single VALUE rather than a DOCUMENT, so a recipe with more than
/// one key comes back as `unexpected content, expected nothing` pointing at the first space.
/// `crates/vike-studio-core/src/study_run.rs`'s own test helper already spells it this way; a
/// second spelling here is what made this test red on the merged tree.
fn params(src: &str) -> toml::Value {
    toml::from_str(src).expect("the test's own TOML")
}

fn empty_params() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

/// A context over the workspace's own store double — enough to compile, call and return, which
/// is all these unit tests need. The seeded, learner-backed pipeline lives in
/// `crates/vike-studio-core/tests/rhai_study_pipeline.rs`.
fn bare_ctx() -> StudyContext {
    StudyContext::new(
        std::sync::Arc::new(vike_data::MemHistStore::new()),
        vike_data::TsRange::of(0, 1_000),
        // Nothing here ever CREATES this — the interpreted tier has no file verb at all, so a
        // study cannot write into scratch. Uniquified anyway, for the reason
        // `crates/vike-ops/tests/hygiene/temp_path_gate.rs` gives.
        std::env::temp_dir().join(format!("vike-rhai-study-{}", std::process::id())),
    )
}

/// [`RhaiStudy::run`] takes `&self`, and that is only worth anything if a caller may actually
/// hold a compiled study across threads. Written as a compile-time assertion rather than as a
/// sentence in a doc comment, so the day some field stops being `Sync` the claim fails with it.
#[test]
fn a_compiled_study_is_send_and_sync_so_run_may_be_called_concurrently() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RhaiStudy>();
    assert_send_sync::<StudyFit>();
}

#[test]
fn a_study_returns_the_outcome_it_built() {
    let s = RhaiStudy::new(
        "smoke",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("answer", 42);
                 out.artifact("note.txt", "hello");
                 return out;
               }"#,
    )
    .unwrap();
    let out = s.run(&bare_ctx(), &empty_params()).unwrap();
    assert_eq!(out.metric_value("answer"), Some(42.0));
    assert_eq!(out.artifacts(), [("note.txt".to_string(), "hello".to_string())]);
}

/// The window is the run's, handed in — not something a study invents. It is also the default
/// range of every read verb, which is what keeps a study measuring the range it is filed under.
#[test]
fn the_window_reaches_the_script_and_reads_back_as_its_bounds() {
    let s = RhaiStudy::new(
        "win",
        r#"fn run(ctx, params) {
                 let w = ctx.window();
                 let out = outcome();
                 out.metric("start", w.start);
                 out.metric("end", w.end);
                 out.metric("all_start_is_unit", if ts_range_all().start == () { 1 } else { 0 });
                 return out;
               }"#,
    )
    .unwrap();
    let out = s.run(&bare_ctx(), &empty_params()).unwrap();
    assert_eq!(out.metric_value("start"), Some(0.0));
    assert_eq!(out.metric_value("end"), Some(1000.0));
    assert_eq!(out.metric_value("all_start_is_unit"), Some(1.0));
}

/// The recipe arrives as a document a script can index — every TOML shape, in one study.
#[test]
fn a_toml_recipe_arrives_as_an_indexable_rhai_value() {
    let s = RhaiStudy::new(
        "recipe",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("seed", params.seed);
                 out.metric("rate", params.rate);
                 out.metric("on", if params.on { 1 } else { 0 });
                 out.metric("first_symbol_len", params.symbols[0].len());
                 out.metric("nested", params.nested.depth);
                 out.artifact("tag.txt", params.tag);
                 return out;
               }"#,
    )
    .unwrap();
    let p = params(
        "seed = 7\nrate = 0.25\non = true\nsymbols = [\"BTCUSDT\"]\ntag = \"v1\"\n\
             [nested]\ndepth = 3\n",
    );
    let out = s.run(&bare_ctx(), &p).unwrap();
    assert_eq!(out.metric_value("seed"), Some(7.0));
    assert_eq!(out.metric_value("rate"), Some(0.25));
    assert_eq!(out.metric_value("on"), Some(1.0));
    assert_eq!(out.metric_value("first_symbol_len"), Some(7.0));
    assert_eq!(out.metric_value("nested"), Some(3.0));
    assert_eq!(out.artifacts()[0].1, "v1");
}

/// A file with the wrong entry is a study nothing will ever call, so it is refused at COMPILE
/// naming the contract — never accepted as an empty one.
#[test]
fn a_script_with_no_run_entry_is_refused_naming_the_contract() {
    // ⚠ NOT `fn go()` — `go` is a RESERVED KEYWORD in Rhai, so that script fails at the
    // tokenizer and never reaches the missing-entry check this test exists for. It was written
    // that way and passed nothing: the refusal it asserted on said "'go' is a reserved
    // keyword", not the contract. Any ordinary identifier exercises the real path.
    let e = RhaiStudy::new("wrong", "fn compute() { 1 }").unwrap_err();
    assert_matches!(e, StudyError::Study(ref m) if m.contains("fn run(ctx, params)"), "{e}");
    // ...and the arity is part of the contract: a one-argument `run` is not this entry.
    let e = RhaiStudy::new("arity", "fn run(ctx) { 1 }").unwrap_err();
    assert_matches!(e, StudyError::Study(_), "{e}");
}

#[test]
fn a_compile_error_names_the_study_and_carries_rhai_s_own_words() {
    let e = RhaiStudy::new("broken", "fn run(ctx, params) { this. }").unwrap_err();
    assert_matches!(e, StudyError::Study(ref m) if m.contains("broken"), "{e}");
}

/// A study that forgets to return an outcome gets a sentence naming what to do, not a cast
/// panic and not an empty result that would list as a finished run with no metrics.
#[test]
fn a_study_that_returns_something_else_is_refused_naming_what_it_returned() {
    let s = RhaiStudy::new("wrongret", "fn run(ctx, params) { 1 }").unwrap();
    let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
    assert_matches!(e, StudyError::Study(ref m) if m.contains("outcome()"), "{e}");
}

/// THE property [`fault`] exists for, proven end to end through a real script: a store read
/// that fails comes back as `StudyError::Data`, not as a sentence a caller would have to parse.
///
/// `depth` is the verb used because `MemHistStore` holds no depth lane and inherits
/// `HistStore`'s REFUSING default for it — a real refusal from the real double, rather than a
/// bespoke failing store written to make this test pass.
#[test]
fn a_store_read_failure_comes_back_as_the_typed_data_variant() {
    let s = RhaiStudy::new("read", r#"fn run(ctx, params) { ctx.depth("binance", "BTCUSDT") }"#)
        .unwrap();
    let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
    assert_matches!(e, StudyError::Data(_), "got {e}");

    // ...and a read that SUCCEEDS is an ordinary array, so the fault path is not the only path
    // this store exercises.
    let s = RhaiStudy::new(
        "ok",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("n", ctx.bars("binance", "BTCUSDT", "1m").len());
                 return out;
               }"#,
    )
    .unwrap();
    assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("n"), Some(0.0));
}

/// The top level runs during [`RhaiStudy::new`], which is why a top-level failure is a COMPILE
/// refusal rather than a surprise on the first run — and why it cannot recur per run.
#[test]
fn a_failing_top_level_statement_is_refused_at_compile_not_at_run() {
    let e = RhaiStudy::new("boom", r#"throw "no"; fn run(ctx, params) { outcome() }"#).unwrap_err();
    assert_matches!(e, StudyError::Study(ref m) if m.contains("top-level"), "{e}");
}

/// ...and the host ceiling: no learner is `NoLearner`, which a UI renders as "run this
/// somewhere else" rather than as a stack of prose.
#[test]
fn fitting_with_no_learner_comes_back_as_the_typed_no_learner_variant() {
    let s =
        RhaiStudy::new("fitless", r#"fn run(ctx, params) { ctx.fit([1.0, 2.0], [0.0], 2, #{}) }"#)
            .unwrap();
    let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
    assert_matches!(e, StudyError::NoLearner(_), "got {e}");
    // ...and a study can ASK first rather than failing, which is what makes the ceiling
    // something an author can branch on.
    let s = RhaiStudy::new(
        "asks",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("can_fit", if ctx.can_fit() { 1 } else { 0 });
                 return out;
               }"#,
    )
    .unwrap();
    assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("can_fit"), Some(0.0));
}

/// A refused metric or artifact STOPS the study. Returning the `Result` as a value would let a
/// script that never checks a return file a half-recorded outcome.
#[test]
fn the_outcome_rules_are_enforced_inside_the_script() {
    let s = RhaiStudy::new(
        "dup",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("sharpe", 1.0);
                 out.metric("sharpe", 2.0);
                 return out;
               }"#,
    )
    .unwrap();
    let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
    assert!(e.to_string().contains("duplicate metric"), "{e}");

    let s = RhaiStudy::new(
        "escape",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.artifact("../../secrets.env", "x");
                 return out;
               }"#,
    )
    .unwrap();
    let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
    assert!(e.to_string().contains("secrets.env"), "{e}");
}

/// An integer metric and a float metric must both work — the `param`-shaped trap this
/// workspace shipped once already, at the one call site an author writes most often.
#[test]
fn a_metric_takes_an_integer_and_a_float_alike_and_refuses_text() {
    let s = RhaiStudy::new(
        "nums",
        r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("int", 3);
                 out.metric("float", 3.5);
                 return out;
               }"#,
    )
    .unwrap();
    let out = s.run(&bare_ctx(), &empty_params()).unwrap();
    assert_eq!(out.metric_value("int"), Some(3.0));
    assert_eq!(out.metric_value("float"), Some(3.5));

    let s = RhaiStudy::new(
        "text",
        r#"fn run(ctx, params) { let out = outcome(); out.metric("m", "nope"); return out; }"#,
    )
    .unwrap();
    assert!(s.run(&bare_ctx(), &empty_params()).unwrap_err().to_string().contains("NUMBER"));
}

/// The top level runs once, at compile. Two runs of one study must not differ, and a top-level
/// `const` must still be visible inside `run` — the pair of properties the persisted scope and
/// `eval_ast(false)` buy together.
#[test]
fn a_top_level_const_is_visible_and_two_runs_agree() {
    let s = RhaiStudy::new(
        "constant",
        r#"const K = 7.0;
               fn run(ctx, params) { let out = outcome(); out.metric("k", K); return out; }"#,
    )
    .unwrap();
    let first = s.run(&bare_ctx(), &empty_params()).unwrap();
    let second = s.run(&bare_ctx(), &empty_params()).unwrap();
    assert_eq!(first.metric_value("k"), Some(7.0));
    assert_eq!(first, second, "two runs of one study must not differ");
}

/// A run's own locals must not survive into the next run — the reason the scope is CLONED per
/// call rather than mutated in place.
#[test]
fn a_runs_locals_do_not_leak_into_the_next_run() {
    let s = RhaiStudy::new(
        "locals",
        r#"fn run(ctx, params) {
                 let seen = is_def_var("leaked");
                 let leaked = 1;
                 let out = outcome();
                 out.metric("seen", if seen { 1 } else { 0 });
                 return out;
               }"#,
    )
    .unwrap();
    assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("seen"), Some(0.0));
    assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("seen"), Some(0.0));
}

/// The entry file's stem must equal the folder name, and the EXTENSION is matched
/// case-insensitively — a Windows editor that saved `.RHAI` wrote a real study.
#[test]
fn the_entry_file_is_found_by_stem_with_a_case_insensitive_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("vol_clustering");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("vol_clustering.RHAI"),
        "fn run(ctx, params) { let o = outcome(); o.metric(\"n\", 1); return o; }",
    )
    .unwrap();
    let s = RhaiStudy::load(&dir).unwrap();
    assert_eq!(s.name(), "vol_clustering");
    assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("n"), Some(1.0));
}

/// The Rust tier's `[a-z][a-z0-9_]*` rule is a property of RUST MODULE NAMES. This tier has no
/// such constraint, and imposing one would refuse folders `list_studies` already lists — its
/// own test uses exactly this name.
#[test]
fn a_dashed_folder_name_loads_because_this_tier_generates_no_rust_module() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("vol-clustering");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("vol-clustering.rhai"),
        "fn run(ctx, params) { let o = outcome(); o.metric(\"n\", 1); return o; }",
    )
    .unwrap();
    assert_eq!(RhaiStudy::load(&dir).unwrap().name(), "vol-clustering");
}

/// A folder without its entry file is a study nothing would ever run. It is refused NAMING the
/// file it should have held — the `MissingEntry` ruling, mirrored.
#[test]
fn a_folder_with_no_matching_entry_file_is_refused_naming_the_file_it_wants() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("halfdone");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("scratch.rhai"), "fn run(ctx, params) { () }").unwrap();
    let e = RhaiStudy::load(&dir).unwrap_err();
    assert_matches!(e, StudyError::Study(ref m) if m.contains("halfdone.rhai"), "{e}");
}

/// No `user_data/` at all is the CI and fresh-clone state. Nothing is registered and nothing
/// panics — an absent folder is a named refusal, which is the answer a caller can render.
#[test]
fn an_absent_study_folder_is_a_named_refusal_rather_than_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let e = RhaiStudy::load(&tmp.path().join("user_data").join("nope")).unwrap_err();
    assert_matches!(e, StudyError::Study(ref m) if m.contains("nope"), "{e}");
}
