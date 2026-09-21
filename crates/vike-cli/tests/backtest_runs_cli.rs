//! The three JUDGING verbs — `backtest tag | diff | gate` — over the SHIPPED binary, against a runs
//! tree planted on disk.
//!
//! ⚠ These are spawn tests on purpose. A unit test of a parser sees a `Result`; the EXIT CODE and
//! the choice of STREAM are only observable from OUTSIDE the process, and both are this stage's
//! product — `gate`'s whole deliverable is a rung a CI step branches on, and its verdict document
//! must be on stdout so `--json` can be piped while the rung still applies.
//!
//! ⚠ Every case pins the CHILD's environment (`Command::env` / `env_remove`, never
//! `std::env::set_var`, which is unsafe under threads and leaks across parallel cases), and points
//! `VIKE_USER_DATA_DIR` at a throwaway tree so no developer's real runs are read or written. The
//! four `env_remove` calls are not decoration: `vike_config::refuse_removed_env` runs before any
//! verb is routed, so an exported `VIKE_MAX_ORDER_NOTIONAL` on a developer box would send every case
//! below down the startup-refusal path instead of the rung it is testing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A project root with a `settings/` directory and a `user_data/` beside it, planted from scratch.
fn project(scratch: &Path) -> (PathBuf, PathBuf) {
    let settings = scratch.join("settings");
    let user_data = scratch.join("user_data");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::create_dir_all(user_data.join("runs")).unwrap();
    (settings, user_data)
}

/// Plant a FINISHED run: `report.json` first and `manifest.json` LAST, which is the writer's order
/// and the completion marker every scan of this tree keys on.
fn plant_run(user_data: &Path, run_id: &str, sharpe: f64, max_dd: f64, profile: &str) {
    let dir = user_data.join("runs").join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("report.json"),
        format!(
            r#"{{"name":"m","final_equity":10500.0,"total_return":0.05,"n_trades":412,
                 "win_rate":0.51,"sharpe":{sharpe},"max_drawdown":{max_dd},
                 "profit_factor":1.4,"funding_paid":0.0,"per_symbol_pnl":[["BTCUSDT",500.0]]}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        format!(
            r#"{{"run_id":"{run_id}","kind":"backtest","produced_by":"backtest",
                 "started_at":"2025-08-24T01:46:40Z","finished_at":"2025-08-24T01:47:00Z",
                 "git_sha":null,"config":{{"path":"{profile}","name":"momentum"}},
                 "detail":{{"strategy":"momentum","data":{{"kind":"bar","interval":"1h",
                 "from":"2025-01-01","to":"2025-06-30","series":["binance:BTCUSDT"]}},
                 "store":"/srv/store"}}}}"#
        ),
    )
    .unwrap();
}

fn run_cli(settings: &Path, user_data: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// Two runs and a baseline mark over the newer-is-worse pair every gate case needs.
fn two_runs_and_a_mark(
    settings: &Path,
    user_data: &Path,
    baseline_sharpe: f64,
    subject_sharpe: f64,
) {
    plant_run(user_data, "1755900000-1-0", baseline_sharpe, 0.12, "profiles/m.toml");
    plant_run(user_data, "1756000000-1-0", subject_sharpe, 0.12, "profiles/m.toml");
    let out =
        run_cli(settings, user_data, &["backtest", "tag", "1755900000-1-0", "--as", "baseline/m"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}

// ─── tag ────────────────────────────────────────────────────────────────────────────────────────

/// A mark set by `tag --as` is what a later verb names. This is the round trip that makes `gate`
/// writable at all — a run id moves every time you run, and a mark does not.
#[test]
fn a_mark_set_by_tag_resolves_afterwards() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1756000000-1-0", 1.82, 0.12, "profiles/m.toml");

    let out = run_cli(
        &settings,
        &user_data,
        &["backtest", "tag", "1756000000-1-0", "--as", "baseline/momentum", "--add", "ci"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let mark = user_data.join("marks").join("baseline").join("momentum.json");
    assert!(mark.is_file(), "the mark is a file NAMED for the mark, beside runs/ not inside it");
    assert!(
        !user_data.join("runs").join("marks").exists(),
        "marks may never live under the runs root — every scan would read that as an unfinished run"
    );

    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(user_data.join("runs/1756000000-1-0/meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["schema"], 1, "the schema key ships with the document");
    assert_eq!(meta["tags"][0], "ci");

    // …and the mark RESOLVES, in both spellings, through a verb that takes a selector.
    for spelling in ["@baseline/momentum", "baseline/momentum"] {
        let out = run_cli(&settings, &user_data, &["backtest", "path", spelling]);
        assert_eq!(out.status.code(), Some(0), "{spelling}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("1756000000-1-0"),
            "{spelling}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// A `tag` that would write nothing is a USAGE error, not a silent success: a script that meant to
/// set a mark and typed the flag wrong must not read as green.
#[test]
fn a_tag_with_nothing_to_write_is_a_usage_error() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1756000000-1-0", 1.82, 0.12, "profiles/m.toml");
    let out = run_cli(&settings, &user_data, &["backtest", "tag", "1756000000-1-0"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--add") && err.contains("--as"), "names what it wanted: {err}");
}

/// An AMBIGUOUS prefix refuses and NAMES the candidates. "Be more specific" without the list is a
/// message nobody can act on.
#[test]
fn an_ambiguous_selector_names_the_candidates() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1756000000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-1", 1.80, 0.13, "profiles/m.toml");

    let out = run_cli(&settings, &user_data, &["backtest", "tag", "17560", "--add", "x"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1756000000-1-0") && err.contains("1756000000-1-1"), "{err}");
}

/// ⚠ Re-marking is EXPLICIT AND RECORDED (§7.2), which is only true if somebody is told: the move is
/// rendered, and the previous pointer is kept in the file.
#[test]
fn re_marking_prints_the_move_and_keeps_the_previous_pointer() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.80);

    let out = run_cli(
        &settings,
        &user_data,
        &["backtest", "tag", "1756000000-1-0", "--as", "baseline/m"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("1755900000-1-0"), "the move names where it pointed: {doc}");
    assert!(doc.contains("1756000000-1-0"), "…and where it points now: {doc}");

    let mark: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(user_data.join("marks/baseline/m.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(mark["run_id"], "1756000000-1-0");
    assert_eq!(mark["history"][0]["run_id"], "1755900000-1-0");
}

// ─── gate ───────────────────────────────────────────────────────────────────────────────────────

/// A gate that PASSES is a zero, and the document still lands on stdout — a CI step that prints the
/// verdict on success is the normal case, not the exception.
#[test]
fn a_gate_inside_its_tolerances_is_a_zero_and_still_prints_its_verdict() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.80);

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpe:-5%,max_dd:+10%",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("sharpe"), "every criterion is named: {doc}");
    assert!(doc.contains("max_dd"), "…including the aliased one, as typed: {doc}");
    assert!(doc.contains("1755900000-1-0"), "…and so is the run the mark points at: {doc}");
}

/// ⚠ THE RUNG THIS VERB EXISTS FOR. A breach is not a crash and not a usage error: the command did
/// exactly what was asked and the answer was no. A CI step branches on this number.
#[test]
fn a_breached_threshold_is_its_own_rung() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.40);

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpe:-5%",
        ],
    );
    assert_eq!(out.status.code(), Some(6), "a declared threshold was breached");
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("1.8200") && doc.contains("1.4000"), "both numbers: {doc}");
    assert!(doc.contains("BREACH"), "{doc}");
}

/// ⚠ AND ITS TWIN. A gate that evaluated nothing must not read as a pass — "green means nothing ran"
/// is the failure every gate in this repository is organised against, and from `0` a CI step cannot
/// tell the difference.
#[test]
fn a_gate_that_evaluated_nothing_is_not_a_pass() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.80);

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpee:-5%",
        ],
    );
    assert_eq!(out.status.code(), Some(7), "a typo'd metric evaluated nothing");
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("sharpe"), "…and the document names keys it DOES carry: {doc}");
    assert!(doc.contains("unevaluated"), "{doc}");
}

/// A BREACH outranks an unevaluated criterion: a real failure must never be masked by a typo
/// elsewhere in the same expression.
#[test]
fn a_breach_outranks_an_unevaluated_criterion() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.40);

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpe:-5%,nonsuch:-5%",
        ],
    );
    assert_eq!(out.status.code(), Some(6));
}

/// `--json` is a MACHINE document on stdout, and the rung still applies — the two are not
/// alternatives.
#[test]
fn the_json_verdict_is_parseable_and_the_rung_still_applies() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.40);

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpe:-5%",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(6));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["verdict"], "breach");
    assert_eq!(v["criteria"][0]["metric"], "sharpe");
    assert_eq!(v["criteria"][0]["verdict"], "breach");
    assert_eq!(v["against"]["run"], "1755900000-1-0");
}

/// A gate with no `--against` is a USAGE error naming how to make a mark. A gate with no baseline
/// cannot be spelled in this grammar, and guessing one is not an option.
#[test]
fn a_gate_with_no_baseline_names_how_to_make_one() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1756000000-1-0", 1.80, 0.12, "profiles/m.toml");
    let out = run_cli(
        &settings,
        &user_data,
        &["backtest", "gate", "1756000000-1-0", "--fail-if", "sharpe:-5%"],
    );
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--against") && err.contains("tag"), "{err}");
}

/// ⚠ **A MISSING BASELINE IS NOT EVIDENCE THAT THE INPUTS CHANGED.** A mark pointing at a run that
/// was pruned is DANGLING: the message says which run is gone and tells the operator to re-point the
/// name. Crucially the rung is NOT `Exit::Breach` — a gate that read "I could not find the baseline"
/// as "the numbers moved" would turn a store hiccup into a red build with a false explanation.
#[test]
fn a_dangling_mark_names_the_run_that_is_gone_and_never_reads_as_a_breach() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.80);
    std::fs::remove_dir_all(user_data.join("runs/1755900000-1-0")).unwrap();

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpe:-5%",
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1755900000-1-0"), "names the missing run: {err}");
    assert!(err.contains("baseline/m"), "…and the mark that points at it: {err}");
    assert_ne!(out.status.code(), Some(6), "a missing baseline is NEVER a breach: {err}");
    assert_ne!(out.status.code(), Some(0), "…and it is never a pass either: {err}");
}

/// A run with no `report.json` has nothing to judge, and that is its OWN rung rather than a failure:
/// the gate did not break, it was handed an unfinished run.
#[test]
fn a_gate_over_a_run_with_no_report_is_nothing_to_evaluate() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    two_runs_and_a_mark(&settings, &user_data, 1.82, 1.80);
    std::fs::remove_file(user_data.join("runs/1756000000-1-0/report.json")).unwrap();

    let out = run_cli(
        &settings,
        &user_data,
        &[
            "backtest",
            "gate",
            "1756000000-1-0",
            "--against",
            "@baseline/m",
            "--fail-if",
            "sharpe:-5%",
        ],
    );
    assert_eq!(out.status.code(), Some(7));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("report.json"), "{err}");
}

// ─── diff ───────────────────────────────────────────────────────────────────────────────────────

/// The INPUT half diffs what the manifest actually RECORDS — today, the profile the run was driven
/// by. That is the comparison §6.3 exists for: a metric move attributable to a config change without
/// re-running anything.
#[test]
fn a_diff_shows_the_config_change_beside_the_metric_change() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.40, 0.12, "profiles/m-v2.toml");

    let out =
        run_cli(&settings, &user_data, &["backtest", "diff", "1755900000-1-0", "1756000000-1-0"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("config.path"), "the INPUT that moved: {doc}");
    assert!(doc.contains("m-v2.toml"), "…named: {doc}");
    assert!(doc.contains("sharpe"), "the OUTPUT that moved: {doc}");
    assert!(doc.contains("1.82") && doc.contains("1.4"), "…with both numbers: {doc}");
}

/// ⚠ `run_id`, `started_at` and `finished_at` differ between ANY two runs, so they are excluded from
/// the input diff and rendered in the HEADER. Otherwise three guaranteed rows drown the real signal
/// in the view whose whole job is removing noise.
#[test]
fn the_always_different_keys_are_in_the_header_not_the_diff() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.40, 0.12, "profiles/m.toml");

    let out =
        run_cli(&settings, &user_data, &["backtest", "diff", "1755900000-1-0", "1756000000-1-0"]);
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("1755900000-1-0") && doc.contains("1756000000-1-0"), "header: {doc}");
    // ⚠ THE POSITIVE CONTROL FOR THE TWO NEGATIVES BELOW, and it is load-bearing. They search for
    // the `~ ` changed-row prefix; if the renderer ever stopped using it, BOTH would pass while
    // testing nothing at all. This proves the prefix is the thing being searched for.
    assert!(doc.contains("~ sharpe"), "a changed row IS rendered with `~ `: {doc}");
    assert!(!doc.contains("~ run_id"), "run_id is not a diff row: {doc}");
    assert!(!doc.contains("~ started_at"), "started_at is not a diff row: {doc}");
}

/// `--changed-only` is DEFAULT ON and `--all` is its negation. A default-on behaviour with no
/// negation cannot be turned off, which is why the negation is named here.
#[test]
fn changed_only_is_the_default_and_all_is_its_negation() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.40, 0.12, "profiles/m.toml");

    let quiet =
        run_cli(&settings, &user_data, &["backtest", "diff", "1755900000-1-0", "1756000000-1-0"]);
    let loud = run_cli(
        &settings,
        &user_data,
        &["backtest", "diff", "1755900000-1-0", "1756000000-1-0", "--all"],
    );
    let (q, l) = (String::from_utf8_lossy(&quiet.stdout), String::from_utf8_lossy(&loud.stdout));
    assert!(l.lines().count() > q.lines().count(), "--all shows unchanged leaves too");
    // ⚠ THE POSITIVE CONTROL. Without it, a quiet run that printed NOTHING AT ALL would satisfy both
    // the line-count comparison and the `!q.contains(...)` below — a green over an empty document,
    // which is the failure mode this whole programme keeps paying for.
    assert!(q.contains("sharpe"), "the default view still shows what CHANGED: {q}");
    assert!(!q.contains("win_rate"), "an unchanged metric is hidden by default: {q}");
    assert!(l.contains("win_rate"), "…and shown under --all: {l}");
    // Spelling the default explicitly is accepted and changes nothing, so a script that says what it
    // means is not refused.
    let explicit = run_cli(
        &settings,
        &user_data,
        &["backtest", "diff", "1755900000-1-0", "1756000000-1-0", "--changed-only"],
    );
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&explicit.stdout),
        q,
        "the default, spelled out, is byte-identical"
    );
}

/// ⚠ A run with no report has NOTHING TO DIFF, and that is its own rung rather than an empty
/// success. An empty diff and an impossible diff must not look the same to a script.
#[test]
fn a_run_with_no_report_is_nothing_to_evaluate() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.40, 0.12, "profiles/m.toml");
    std::fs::remove_file(user_data.join("runs/1756000000-1-0/report.json")).unwrap();

    let out =
        run_cli(&settings, &user_data, &["backtest", "diff", "1755900000-1-0", "1756000000-1-0"]);
    assert_eq!(out.status.code(), Some(7));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1756000000-1-0") && err.contains("report"), "{err}");
}

/// Two IDENTICAL runs diff to nothing and that is a SUCCESS — "no change" is an answer, not a
/// failure, and a CI step that diffs after a no-op refactor must read green.
#[test]
fn two_identical_runs_diff_to_nothing_and_exit_zero() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.82, 0.12, "profiles/m.toml");

    let out =
        run_cli(&settings, &user_data, &["backtest", "diff", "1755900000-1-0", "1756000000-1-0"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(doc.contains("no change") || doc.contains("identical"), "{doc}");
}

/// `--json` opens with its schema and separates the two halves, so a CI step can act on the input
/// half without parsing a table.
#[test]
fn the_json_diff_separates_inputs_from_outputs() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.40, 0.12, "profiles/m-v2.toml");

    let out = run_cli(
        &settings,
        &user_data,
        &["backtest", "diff", "1755900000-1-0", "1756000000-1-0", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["schema"], 1);
    assert!(v["inputs"].as_array().unwrap().iter().any(|r| r["key"] == "config.path"));
    assert!(v["outputs"].as_array().unwrap().iter().any(|r| r["key"] == "sharpe"));
}

/// ⚠ A diff that cannot attribute a move SAYS SO. Without this, a diff showing only a config change
/// reads as "the config is why" when the truth is that the other two causes were never recorded —
/// and an ABSENT fingerprint means "the producer could not inventory its store", never "the data
/// changed".
#[test]
fn a_diff_names_the_attributions_the_record_cannot_support() {
    let scratch = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(scratch.path());
    plant_run(&user_data, "1755900000-1-0", 1.82, 0.12, "profiles/m.toml");
    plant_run(&user_data, "1756000000-1-0", 1.40, 0.12, "profiles/m.toml");

    let out =
        run_cli(&settings, &user_data, &["backtest", "diff", "1755900000-1-0", "1756000000-1-0"]);
    let doc = String::from_utf8_lossy(&out.stdout);
    assert!(doc.contains("no build stamp"), "{doc}");
    assert!(doc.contains("no data fingerprint"), "{doc}");
    assert!(doc.contains("not a different one"), "an absent address is not a changed one: {doc}");
    // ⚠ …and the note names every cause a null has, rather than one of them. The planted manifests
    // carry no `fingerprint` key at all, which is the shape a producer with no address and a store
    // fault both produce — a reader cannot tell them apart, so the message may not claim to.
    //
    // ⚠ A SEARCH PARENT was a third cause and is not one any more: it carried no address only
    // because addressing it cost a second manifest parse per series, and
    // `vike_data::DataFusionHist::series_facts` removed that parse. The note stops naming it
    // rather than keeping it "for old documents", because this text is advice about what a null
    // might mean NOW and a cause the producer can no longer exhibit sends the reader hunting a
    // sweep that is not there.
    for cause in ["computes no address", "could not be inventoried"] {
        assert!(doc.contains(cause), "the note omits `{cause}`: {doc}");
    }
    assert!(!doc.contains("search parent"), "a search run addresses its inputs now: {doc}");
}
