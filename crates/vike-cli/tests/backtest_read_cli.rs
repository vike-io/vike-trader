//! The `backtest` READING verbs, driven as the SHIPPED binary — the convention every exit-code and
//! stream assertion in this crate follows (`crate`-internal `#[cfg(test)]` modules cover grammar;
//! these cover rungs and streams).
//!
//! ⚠ The child's environment is PINNED. Without `VIKE_SETTINGS_DIR` a case picks up the developer's
//! real `policy.toml`, and without the two `env_remove`s a box with a stale `VIKE_MAX_ORDER_NOTIONAL`
//! exits on `vike_config::refuse_removed_env` — the startup refusal, not the rung under test.
//! `VIKE_USER_DATA_DIR` is pinned for the same reason: these verbs read a runs directory, and the one
//! they must read is the case's own.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;

use vike_backtest::compute_server::serve;
use vike_data::{HistStore, MemHistStore};

fn manifest_json(run_id: &str, kind: &str, started: &str) -> String {
    format!(
        r#"{{
  "run_id": "{run_id}",
  "kind": "{kind}",
  "produced_by": "backtest",
  "started_at": "{started}",
  "finished_at": "{started}",
  "git_sha": null,
  "config": {{ "path": "profiles/sma.toml", "name": "sma cross" }},
  "detail": {{ "strategy": "sma_cross", "store": "/tmp/store" }}
}}
"#
    )
}

fn report_json(sharpe: f64, trades: usize) -> String {
    format!(
        r#"{{"name":"sma cross","final_equity":10500.0,"total_return":0.05,"n_trades":{trades},"win_rate":0.55,"sharpe":{sharpe},"max_drawdown":-0.12,"profit_factor":null,"funding_paid":0.0,"per_symbol_pnl":[["BTCUSDT",500.0]]}}
"#
    )
}

/// A project with a runs directory. Returns `(settings_dir, user_data_dir)`.
fn project(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let settings = tmp.join("settings");
    let user_data = tmp.join("user_data");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::create_dir_all(user_data.join("runs")).unwrap();
    (settings, user_data)
}

fn seed_run(user_data: &Path, run_id: &str, kind: &str, started: &str, sharpe: f64, trades: usize) {
    let dir = user_data.join("runs").join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.json"), report_json(sharpe, trades)).unwrap();
    std::fs::write(dir.join("manifest.json"), manifest_json(run_id, kind, started)).unwrap();
}

fn run(settings: &Path, user_data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// Bind an ephemeral loopback listener and spawn the REAL compute server over an in-memory store —
/// the exact pattern `crates/vike-cli/tests/backtest_cli.rs`'s `spawn_server` uses.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// `path` prints ONE line and nothing else, which is the whole contract: `$(…)` substitution.
#[test]
fn path_prints_one_absolute_line_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "1789213143-8821-00", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);

    let out = run(&settings, &user_data, &["backtest", "path", "@last"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert!(Path::new(lines[0]).is_absolute(), "{}", lines[0]);
    assert!(lines[0].ends_with("1789213143-8821-00"), "{}", lines[0]);

    let out = run(&settings, &user_data, &["backtest", "path", "@last", "report"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).trim().ends_with("report.json"), "{}", stdout(&out));
}

/// The exit ladder this stage takes, all three rungs, in one place.
#[test]
fn the_reading_verbs_take_the_declared_exit_rungs() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "1789213143-8821-00", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);
    seed_run(&user_data, "1789213999-8821-00", "search", "2026-08-24T09:20:04Z", 0.9, 3);

    // AMBIGUOUS prefix → 2, naming both candidates.
    let out = run(&settings, &user_data, &["backtest", "path", "1789213"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("1789213143-8821-00"), "{}", stderr(&out));
    assert!(stderr(&out).contains("1789213999-8821-00"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "a failure writes nothing to stdout: {}", stdout(&out));

    // NO MATCH → 1.
    let out = run(&settings, &user_data, &["backtest", "path", "nope"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));

    // ⚠ A MARK that was never set → 1, not 2, and the rung MOVED when marks shipped. This row used
    // to assert `@baseline/momentum` was a deferred FORM on the usage rung; the tag/diff/gate stage
    // made the form real, so the command line is now well-formed and the failure is a fact about the
    // store — the same classification a mistyped run id already had. The message still names `tag`,
    // because that is the verb that sets one.
    let out = run(&settings, &user_data, &["backtest", "path", "@baseline/momentum"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("tag"), "{}", stderr(&out));
    assert!(stderr(&out).contains("baseline/momentum"), "{}", stderr(&out));

    // …and the one form that IS still deferred → 2, naming the form and what has to persist first.
    let out = run(&settings, &user_data, &["backtest", "path", "1789213143-8821-00#12"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains('#'), "{}", stderr(&out));

    // `--help` → 0, on STDOUT.
    let out = run(&settings, &user_data, &["backtest", "--help"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    // ⚠ Assert the USAGE LINE, not the bare verb. `"ls"` is a substring of `--cols` and `"params"`
    // of `--list-params`, so a `contains("ls")` passes with the whole `backtest ls …` line deleted —
    // the assertion would then pin nothing while reading as coverage. All EIGHT reading sub-verbs,
    // each by the line the usage actually advertises.
    for line in [
        "backtest ls [",
        "backtest show <id>",
        "backtest path <id>",
        "backtest tag <run>",
        "backtest diff <a> <b>",
        "backtest gate <run>",
        "backtest params [",
        "backtest strategies [",
    ] {
        assert!(stdout(&out).contains(line), "`backtest --help` must carry `{line}`");
    }
}

/// ⚠ The RUNS ROOT honours `VIKE_USER_DATA_DIR`, which is the whole of D3: the writer does too
/// (`vike_model::state_path::user_runs_dir_from`), so the two halves cannot answer with different
/// directories. Before that, an override moved the indicators and not the runs.
#[test]
fn the_runs_root_follows_the_user_data_override() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, _ignored) = project(tmp.path());
    let elsewhere = tmp.path().join("somewhere-else");
    std::fs::create_dir_all(elsewhere.join("runs")).unwrap();
    seed_run(&elsewhere, "1789213143-8821-00", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);

    let out = run(&settings, &elsewhere, &["backtest", "path", "@last"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("somewhere-else"), "{}", stdout(&out));
}

/// An EMPTY listing is an ANSWER, on rung 0, with a note that says which tree was looked in. There
/// is no exit rung for "nothing was evaluated" — spec §14 proposes `7` and stages it elsewhere, and
/// `crates/vike-cli/src/exit.rs` has no such variant.
#[test]
fn an_empty_ls_exits_zero_with_a_note_naming_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    let out = run(&settings, &user_data, &["backtest", "ls"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("no runs"), "{text}");
    // ⚠ Assert the PATH, not the word "runs" — which the line above already guarantees through
    // "no runs", so the old assertion could not fail for the reason it named. The tree is the half
    // an operator needs the moment `VIKE_USER_DATA_DIR` is in play, and it is what must be there.
    let root = user_data.join("runs");
    assert!(
        text.contains(&root.display().to_string()),
        "the note must name the tree it looked in ({}): {text}",
        root.display()
    );
}

#[test]
fn ls_lists_every_run_and_a_selector_narrows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "1789213143-8821-00", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);
    seed_run(&user_data, "1789213999-8821-00", "search", "2026-08-24T09:20:04Z", 0.9, 3);

    let out = run(&settings, &user_data, &["backtest", "ls"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("1789213143-8821-00"));
    assert!(stdout(&out).contains("1789213999-8821-00"));

    let out = run(&settings, &user_data, &["backtest", "ls", "@last:search", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["count"], serde_json::json!(1));
    assert_eq!(doc["runs"][0]["kind"], serde_json::json!("search"));
}

#[test]
fn ls_cols_and_sort_and_limit_compose() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 0.5, 2);
    seed_run(&user_data, "b-1-0", "backtest", "2026-08-24T09:16:04Z", 3.5, 9);
    seed_run(&user_data, "c-1-0", "backtest", "2026-08-24T09:17:04Z", 1.5, 4);

    let out = run(
        &settings,
        &user_data,
        &[
            "backtest",
            "ls",
            "--cols",
            "run_id,sharpe",
            "--sort",
            "sharpe",
            "--limit",
            "2",
            "--json",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["count"], serde_json::json!(2), "--limit bounds the document too");
    assert_eq!(doc["runs"][0]["run_id"], serde_json::json!("b-1-0"), "best first");
    assert_eq!(doc["runs"][1]["run_id"], serde_json::json!("c-1-0"));
    assert!(doc["runs"][0].get("kind").is_none(), "--cols is the whole column set");
}

/// A broken run is a NAMED problem on stderr while the good rows still render on stdout — and under
/// `--json` the document stays parseable, which is the whole reason problems go to the other stream.
#[test]
fn a_broken_run_is_named_on_stderr_and_does_not_break_the_json() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 0.5, 2);
    let broken = user_data.join("runs").join("b-1-0");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("manifest.json"), "not json at all").unwrap();

    let out = run(&settings, &user_data, &["backtest", "ls", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("still one document");
    assert_eq!(doc["count"], serde_json::json!(1));
    assert!(stderr(&out).contains("b-1-0"), "the broken run is NAMED: {}", stderr(&out));
}

#[test]
fn ls_where_filters_on_a_metric_from_the_report() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 0.5, 2);
    seed_run(&user_data, "b-1-0", "backtest", "2026-08-24T09:16:04Z", 3.5, 90);

    let out = run(
        &settings,
        &user_data,
        &["backtest", "ls", "--where", "sharpe>1,n_trades>=30", "--json"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["count"], serde_json::json!(1));
    assert_eq!(doc["runs"][0]["run_id"], serde_json::json!("b-1-0"));
}

#[test]
fn show_renders_a_stored_run_and_its_json_is_one_document() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);

    let out = run(&settings, &user_data, &["backtest", "show", "@last"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("a-1-0"));
    assert!(stdout(&out).contains("sharpe"));

    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["manifest"]["kind"], serde_json::json!("backtest"));
    assert_eq!(doc["report"]["n_trades"], serde_json::json!(7));
}

/// ⚠ The unbuilt renderers are USAGE refusals that say what is missing — not "unknown option".
/// `--trades` is deliberately NOT among them: the run record grew a ledger, so that flag SHIPS.
///
/// ⚠ **`--html` LEFT this list too, and for a different reason than `--trades` did.** `--trades`
/// left because the run record grew the document it renders; `--html` left because this crate
/// gained the vike-report EDGE its refusal had always blamed. Its shipping behaviour is driven
/// over this same binary by
/// [`html_over_the_shipped_binary_writes_a_document_and_refuses_a_second_one`] below, so promoting
/// it out of here did not cost a binary-level assertion — it moved one.
#[test]
fn the_unbuilt_show_renderers_are_named_refusals_and_trades_is_not_one() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);

    for (flag, needle) in [("--drawdowns", "renderer"), ("--breakdown", "PER-BAR")] {
        let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", flag]);
        assert_eq!(out.status.code(), Some(2), "{flag}: {}", stderr(&out));
        assert!(stderr(&out).contains(needle), "{flag}: {}", stderr(&out));
        assert!(!stderr(&out).contains("unknown option"), "{flag}: {}", stderr(&out));
    }

    // ...and `--trades` is accepted and rendered, over a run whose producer wrote no ledger.
    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--trades"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no trades.json"), "{}", stdout(&out));
}

/// **`show --metrics-list` prints the metric CATALOG over the SHIPPED BINARY, with no run named and
/// nothing on disk to read** — the whole chain, which no unit test reaches: the dispatcher,
/// `claim_subcommand`, `parse_read`'s selector exemption, `execute_read` and `run_show`'s
/// short-circuit.
///
/// ⚠ `VIKE_USER_DATA_DIR` names a directory that DOES NOT EXIST, and the honest statement of what
/// that buys is narrower than it looks: `vike_model::state_path::user_runs_dir_from` answers with
/// where the runs directory BELONGS whether or not it is there (its own doc says so), so
/// `Ctx::runs_root` here is `Some(<missing>/runs)` and NOT `None`. What this case therefore proves
/// is that the listing returns above the SELECTOR resolution — there is no selector to resolve —
/// and over a scan that finds nothing. The `runs_root == None` half is the unit test's:
/// `crates/vike-cli/src/cmd/runs/show.rs`'s
/// `the_metric_listing_prints_the_catalog_with_no_project_and_no_run` builds that `Ctx` directly,
/// because an environment variable cannot produce it.
///
/// ⚠ The mutations this fails on, in PRODUCTION, and which one each test owns. Deleting the
/// `ReadSub::Show if a.metrics_list` arm from `crates/vike-cli/src/cmd/backtest.rs`'s `parse_read`
/// → rung 2 demanding a run selector, caught HERE. Moving the `a.metrics_list` block in `run_show`
/// below its `resolve_one` call → non-zero rung and empty stdout, also caught here. Moving it below
/// the `ctx.runs_root` requirement → caught by the unit test named above and NOT by this one, which
/// is exactly why both exist.
#[test]
fn the_metric_listing_prints_the_catalog_over_the_shipped_binary_with_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    // NOT created: the listing must answer without one.
    let absent_user_data = tmp.path().join("nothing-here");

    let out = run(&settings, &absent_user_data, &["backtest", "show", "--metrics-list"]);
    assert!(out.status.success(), "exit {:?}; stderr: {}", out.status.code(), stderr(&out));
    let text = stdout(&out);
    // Every id the catalog holds, asserted against the catalog rather than a literal — so a door
    // that printed some other renderer cannot satisfy this.
    for m in vike_analytics::metric_catalog::METRICS {
        assert!(text.contains(m.id), "{} is missing from the printed catalog:\n{text}", m.id);
    }
    assert!(text.contains("core"), "the storage column is rendered: {text}");
    assert!(!text.contains("run     "), "no run header — this is not a `show` rendering: {text}");

    // …and the two refusals, over the same binary. `--metrics` beside it is rung 2 naming both;
    // `--json` beside it is rung 2 naming the asset that does carry the ids.
    let out =
        run(&settings, &absent_user_data, &["backtest", "show", "--metrics-list", "--metrics"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--metrics-list"), "{}", stderr(&out));
    assert!(stderr(&out).contains("two different documents"), "{}", stderr(&out));

    let out = run(&settings, &absent_user_data, &["backtest", "show", "--metrics-list", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("cli.json"), "{}", stderr(&out));

    // …and on a SIBLING reading verb it is refused by its own named rule rather than by the
    // `show_only` roster, whose sentence ("selects a SECTION of one stored run") is false of a
    // listing.
    let out = run(&settings, &absent_user_data, &["backtest", "ls", "--metrics-list"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("METRIC CATALOG"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("unknown option"), "{}", stderr(&out));
}

/// ⚠ **`--export` is refused per VALUE, not per flag.** Refusing the whole flag because ONE of its
/// three values is unstorable was inconsistent with its two neighbours, both refused narrowly —
/// and it withheld two documents the run record fully holds.
#[test]
fn export_serves_the_two_stored_values_and_refuses_fills_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);
    let dir = user_data.join("runs").join("a-1-0");
    std::fs::write(
        dir.join("trades.json"),
        r#"{"schema":1,"trades":[{"entry_price":100.0,"exit_price":110.0,"size":2.0,"pnl":20.0,"symbol":"BTCUSDT","is_long":true}],"source_len":9}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("series.json"),
        r#"{"schema":1,"equity":[100.0,101.0],"equity_ts":[1,2],"per_symbol_equity":[],"stride":1,"source_len":2}"#,
    )
    .unwrap();

    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--export", "trades"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["source_len"], serde_json::json!(9), "the TRUE closed count rides along");
    assert_eq!(doc["trades"][0]["symbol"], serde_json::json!("BTCUSDT"));

    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--export", "equity"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["equity"], serde_json::json!([100.0, 101.0]));
    // ⚠ `stride` is not decoration: above 1 the curve is a SAMPLE, and an export that dropped it
    // would hand somebody a curve with no way to know that.
    assert_eq!(doc["stride"], serde_json::json!(1));

    // `fills` is the ONE value the record cannot serve, refused BY NAME with the reason.
    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--export", "fills"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("not stored at ANY size"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("unknown option"), "{}", stderr(&out));

    // ...and a multi-value list, which one `--out` cannot honestly write.
    let out =
        run(&settings, &user_data, &["backtest", "show", "a-1-0", "--export", "trades,equity"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("one value at a time"), "{}", stderr(&out));

    // ...and it composes with `--out`, which is the whole point of a raw document.
    let dest = tmp.path().join("trades-export.json");
    let out = run(
        &settings,
        &user_data,
        &["backtest", "show", "a-1-0", "--export", "trades", "--out", dest.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
    assert_eq!(written["source_len"], serde_json::json!(9));
}

/// ⚠ `--trades --json` carries the ledger. Before this it accepted the flag and emitted a document
/// with no `trades` key at all, so `jq '.trades'` answered `null` on exit 0 — indistinguishable
/// from a run whose producer wrote none.
#[test]
fn trades_under_json_carries_the_ledger_over_the_shipped_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);
    std::fs::write(
        user_data.join("runs").join("a-1-0").join("trades.json"),
        r#"{"schema":1,"trades":[{"entry_price":100.0,"exit_price":110.0,"size":2.0,"pnl":20.0,"symbol":"BTCUSDT","is_long":true}],"source_len":9}"#,
    )
    .unwrap();

    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--trades", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["trades"]["closed"], serde_json::json!(9));
    assert_eq!(doc["trades"]["trades"][0]["symbol"], serde_json::json!("BTCUSDT"));

    // ...and without the flag the key is absent, not null: nobody asked, so nothing was opened.
    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(doc.get("trades").is_none(), "absent flag means absent key: {doc}");
}

/// ⚠ `ls --sort` over an EMPTY store is rung `0`, not a typo accusation. `all(Missing)` is
/// vacuously true over an empty slice, so the typo check fired on every fresh project.
#[test]
fn ls_sort_over_an_empty_store_is_an_answer_not_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());

    let out = run(&settings, &user_data, &["backtest", "ls", "--sort", "sharpe"]);
    assert_eq!(out.status.code(), Some(0), "an empty store is an ANSWER: {}", stderr(&out));
    assert!(stdout(&out).contains("no runs"), "{}", stdout(&out));

    // ...and the same one step along: a `--where` that legitimately matched nothing, over a store
    // that DOES carry the key.
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 0.5, 2);
    let out = run(
        &settings,
        &user_data,
        &["backtest", "ls", "--where", "sharpe>99", "--sort", "sharpe", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["count"], serde_json::json!(0));

    // ...while a genuine typo over a NON-empty store is still refused, which is the half a check
    // against the survivors could never do.
    let out = run(&settings, &user_data, &["backtest", "ls", "--sort", "sharp"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("no run carries that field"), "{}", stderr(&out));
}

/// ⚠ `params --strategy rhai` RESOLVES. `vike_strategy::SCRIPT_ONLY` exists to end exactly the
/// answer this verb was giving: "no built-in strategy named 'rhai'".
#[test]
fn params_resolves_the_script_only_name_rather_than_denying_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());

    let out = run(&settings, &user_data, &["backtest", "params", "--strategy", "rhai"]);
    assert!(out.status.success(), "`rhai` is a name this registry resolves: {}", stderr(&out));
    assert!(stdout(&out).contains("--script"), "it names the question to ask: {}", stdout(&out));

    // ...and a name NO roster carries is still refused, with `rhai` now IN the roster it prints.
    let out = run(&settings, &user_data, &["backtest", "params", "--strategy", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("rhai"), "the roster must advertise it: {}", stderr(&out));
}

/// `--out` writes the document and stdout stays empty — the property a pipeline needs.
#[test]
fn out_writes_the_document_and_prints_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);
    let dest = tmp.path().join("run.json");

    let out = run(
        &settings,
        &user_data,
        &["backtest", "show", "a-1-0", "--json", "--out", dest.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
    assert_eq!(written["run_id"], serde_json::json!("a-1-0"));
}

#[test]
fn params_lists_a_strategys_declared_keys_and_says_when_it_cannot() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());

    let out = run(&settings, &user_data, &["backtest", "params", "--strategy", "buy_hold"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("size"), "{}", stdout(&out));

    let out = run(&settings, &user_data, &["backtest", "params", "--strategy", "rotation_top_k"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("simulator"), "it says WHY, not 'no params': {}", stdout(&out));
}

#[test]
fn params_lists_a_scripts_knobs_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    let script = tmp.path().join("s.rhai");
    std::fs::write(&script, "let t = param(\"threshold\", 60.0);\nfn on_bar(b) { }\n").unwrap();

    let out = run(
        &settings,
        &user_data,
        &["backtest", "params", "--script", script.to_str().unwrap(), "--json"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["source"], serde_json::json!("script"));
    assert_eq!(doc["params"][0]["name"], serde_json::json!("threshold"));
}

// ⚠ **`the_old_list_params_flag_still_answers_and_names_params` WAS HERE, and it was DELETED
// rather than repaired.** It asserted that `backtest --list-params --script s.rhai` still
// SUCCEEDED and printed the knobs, with a deprecation note on stderr — true when this stage was
// written, and the opposite of what stage 3 shipped days later: decision 11 makes a sub-verb
// mandatory, so that command line is now an exit-2 that names `backtest params`. The two tests
// asserted opposite outcomes for the same argv and git merged both without a conflict, because
// they live in different files.
//
// Its ground is covered, and more of it: `crates/vike-cli/tests/backtest_cli.rs`'s
// `the_retired_list_params_flag_names_the_subcommand_that_replaced_it` drives BOTH spellings that
// used to work — the bare `backtest --list-params` (the router's arm) and
// `backtest run --list-params` (the run parser's) — and holds each to the exit rung, the flag's own
// name, the replacement's name, and the absence of "unknown argument".
//
// The half worth keeping is the WORRY, not the assertion: the old spelling was in every scaffolded
// README and in a published skill. Both were swept with the same change —
// `crates/vike-cli/src/cmd/init/content.rs` writes `backtest run` and `backtest params`, and
// `scripts/skills/run-a-backtest-locally.md` says the sub-verb is the spelling.

/// ⚠ The roster comes over the WIRE — that is the whole point of §8.3: the engine binary's own
/// `--list` answers about the LOCAL build, and an operator with `vike-cli` and a remote daemon could
/// not enumerate what `--strategy` may name, even though `ListStrategies` was already served.
#[test]
fn strategies_asks_the_daemon_rather_than_a_local_const() {
    let addr = spawn_server();
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());

    let out = run(
        &settings,
        &user_data,
        &["backtest", "strategies", "--addr", &addr.to_string(), "--json"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["addr"], serde_json::json!(addr.to_string()));
    let names = doc["strategies"].as_array().expect("an array");
    assert!(names.iter().any(|n| n == "buy_hold"), "{names:?}");
    // ⚠ `rhai` is deliberately NOT in the roster — it is the create->backtest keystone arm, not a
    // named strategy — so its absence is the assertion, not an oversight.
    assert!(!names.iter().any(|n| n == "rhai"), "{names:?}");
}

/// A daemon that is not there is the CONNECT rung, and the message names how to start one.
#[test]
fn strategies_with_no_daemon_is_the_connect_rung() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    // Port 1 is privileged and never listening; no bind race with a sibling test.
    let out = run(&settings, &user_data, &["backtest", "strategies", "--addr", "127.0.0.1:1"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("vike-backend backtest"), "{}", stderr(&out));
}

/// A flag that belongs to a SIBLING sub-verb is refused by name with the reason, never dropped.
#[test]
fn a_flag_from_a_sibling_subverb_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);

    let out = run(&settings, &user_data, &["backtest", "path", "a-1-0", "--json"]);
    // `--json` is shared, so it is NOT refused — this asserts the sibling-only one is.
    assert!(out.status.success(), "{}", stderr(&out));

    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--sort", "sharpe"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--sort"), "{}", stderr(&out));
    assert!(stderr(&out).contains("listing"), "{}", stderr(&out));
}

/// **`show --html` over the SHIPPED BINARY: it writes a document, and a second document is
/// refused.** The whole chain no unit test reaches — the dispatcher, `claim_subcommand`,
/// `parse_read`, the two document guards, `run_show`'s selection, `html_document`, and the
/// vike-report edge that makes the renderer reachable at all.
///
/// ⚠ This is the assertion that moved out of
/// [`the_unbuilt_show_renderers_are_named_refusals_and_trades_is_not_one`] when `--html` was
/// promoted. It is the SAME binary and the SAME seeded run; only the expected outcome inverted,
/// which is what a promotion means.
///
/// ⚠ The mutation this fails on, in PRODUCTION: remove the `vike-report` line from
/// `crates/vike-cli/Cargo.toml`. The build breaks rather than the test, which is the point — the
/// edge is load-bearing and nothing else in this crate needs it.
#[test]
fn html_over_the_shipped_binary_writes_a_document_and_refuses_a_second_one() {
    let tmp = tempfile::tempdir().unwrap();
    let (settings, user_data) = project(tmp.path());
    seed_run(&user_data, "a-1-0", "backtest", "2026-08-24T09:15:04Z", 1.25, 7);

    // To stdout: an HTML document carrying the catalog's own metric ids.
    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--html"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let html = stdout(&out);
    assert!(html.contains("<html"), "an HTML document: {}", &html[..80.min(html.len())]);
    assert!(html.contains("max_drawdown"), "…carrying the catalog's ids: {html:.200}");

    // ...and to a file through the `--out` this verb already had, which is why the flag takes no
    // path of its own.
    let sheet = tmp.path().join("sheet.html");
    let out = run(
        &settings,
        &user_data,
        &["backtest", "show", "a-1-0", "--html", "--out", sheet.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let written = std::fs::read_to_string(&sheet).expect("--out wrote the document");
    assert!(written.contains("<html"), "the file is the document");

    // A second document is refused by NAME, with the reason neither wins.
    let out = run(&settings, &user_data, &["backtest", "show", "a-1-0", "--html", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--html") && stderr(&out).contains("--json"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "a refusal leaves stdout empty: {}", stdout(&out));

    // ...and so is the flag on a sub-verb that renders no run, by its own named rule.
    let out = run(&settings, &user_data, &["backtest", "ls", "--html"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("does not apply"), "{}", stderr(&out));
}
