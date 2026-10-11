//! End-to-end tests for `vike-cli config check`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a real settings directory in a temp dir.
//!
//! The unit tests beside `crates/vike-cli/src/cmd/config/check.rs` cover the disposition table and
//! the argument grammar against a synthesized report. These cover the thing the shipped
//! `deploy/*.service` units actually depend on: **the EXIT CODE of the real process**, which no unit
//! test can observe — the verdict has to survive the dispatcher resolving the directory, the loader
//! reading the files, `vike_secrets::resolve` opening the store, and `main` returning an
//! `ExitCode`. A systemd `ExecStartPre=` reads exactly one bit of all of that, so exactly one bit is
//! what these assert first.
//!
//! ⚠ **Not every test here reaches the verb, and the ones that do not say so in their names.**
//! `vike_cli::run` resolves the settings tree BEFORE it routes a subcommand, so a tree that will not
//! load exits from the dispatcher and `config check` never runs. Those cases still belong here —
//! what a systemd `ExecStartPre=` observes is the PROCESS's exit code, whichever layer produced it —
//! but they are labelled, and they assert WHICH layer answered so the ordering cannot change
//! silently. See the "refusals that fire ONE LAYER UP" section below.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory and clears the rest
//! of the environment, for the reason `crates/vike-cli/tests/config_cli.rs` gives: without it a run
//! on a developer box resolves the REPO's settings directory and puts a real credential store's key
//! count into a test's assertions. Passed through `Command::env`/`env_clear` on the CHILD, never
//! `std::env::set_var`, which is unsafe under threads and would leak across this binary's parallel
//! cases.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

mod common;

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

struct Case {
    /// The PROJECT root. The settings directory is its `settings/` child, so a case can also plant
    /// the things that live BESIDE it — `<project>/.env`, whose presence beside an absent store is
    /// a finding in its own right.
    root: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("vike-cli-check-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Case { root }
    }

    fn settings(&self) -> PathBuf {
        self.root.join("settings")
    }

    /// Create the settings directory. Deliberately NOT done by `new`: "the directory an operator
    /// named is not there" is one of the cases under test.
    fn with_settings_dir(self) -> Self {
        std::fs::create_dir_all(self.settings()).unwrap();
        self
    }

    /// The credential STORE holding `body`'s `KEY=value` lines — the settings DATABASE, seeded by
    /// `common::seed_store` (created as `vike-cli secrets init` creates it, each row
    /// written through the production writer).
    fn store(&self, body: &str) {
        let rows: Vec<(&str, &str)> = body.lines().filter_map(|l| l.split_once('=')).collect();
        common::seed_store(&self.settings(), &rows);
    }

    /// Plant real settings ROWS into a real database at this case's settings directory — the
    /// fixture every settings-file write above used to be. `docs/decisions/0086`: there are no
    /// settings files any more, so a case that wants a ROW present when the shipped binary runs
    /// writes it here, straight into `<settings>/db/vike.db`, rather than into a TOML file nothing
    /// reads. A whole-table REPLACE (`vike_secrets::plant_settings_rows`'s own doc), so calling it
    /// again with a DIFFERENT set of rows is how a case moves a tree from one state to the next —
    /// exactly the way overwriting a settings file used to.
    fn seed(&self, rows: &[(&str, &str, &str)]) {
        let stored = vike_secrets::StoredSettings {
            settings: rows
                .iter()
                .map(|(section, key, value)| vike_secrets::SettingRow {
                    section: section.to_string(),
                    key: key.to_string(),
                    value: value.to_string(),
                })
                .collect(),
            ..Default::default()
        };
        vike_secrets::plant_settings_rows(&self.settings(), &stored).unwrap();
        // `plant_settings_rows` creates the database at whatever the OS umask gives it — MEASURED
        // as group-readable on the the CI box lane — which is a real exposed-permission finding this
        // fixture does not want to exercise by accident.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let db = vike_secrets::db_path_in(&self.settings());
            std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("config").arg("check").args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", self.settings());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn both(o: &Output) -> String {
    format!("{}{}", stdout(o), String::from_utf8_lossy(&o.stderr))
}

// ---------------------------------------------------------------------------------------------
// The clean case — the one every shipped unit is in
// ---------------------------------------------------------------------------------------------

/// **The row that decides whether a unit may run this verb at all.** Every shipped daemon unit is a
/// PAPER deployment whose credential store is deliberately empty, so a `check` that failed on an
/// absent store would make a correct fresh install unstartable — the case
/// `docs/decisions/0013-degrade-vs-refuse.md` names under "what would reopen this".
///
/// Absent credentials ARE the live gate, so this is not even a warning: `--strict` passes too.
#[test]
fn a_paper_deployment_with_no_credentials_passes_even_under_strict() {
    let c = Case::new("paper").with_settings_dir();
    c.seed(&[("policy", "max_notional_per_order", "250")]);

    let o = c.run(&[]);
    assert!(o.status.success(), "a paper deployment must start: {}", both(&o));
    let out = stdout(&o);
    assert!(out.contains(&format!("settings directory: {}", c.settings().display())), "{out}");
    assert!(out.contains("named outright by VIKE_SETTINGS_DIR"), "the RUNG must be named: {out}");
    assert!(out.contains("0 error(s)"), "{out}");

    let strict = c.run(&["--strict"]);
    assert!(strict.status.success(), "not even --strict may fail this: {}", both(&strict));
}

/// **A credential row under a venue setting's old name is read by nothing, and the check does not
/// fail on it** (`docs/decisions/0117-there-are-no-migrations.md`: a retired name simply stops being
/// read). Through the real process, because the exit code is the product; the planted value and the
/// credential beside it must appear nowhere in the report.
#[test]
fn a_venue_setting_under_its_old_credential_name_does_not_fail_the_check() {
    const LEAK: &str = "sk-do-not-print-me";
    let old_name = concat!("IBKR", "_DEMO_PORT");
    let c = Case::new("old-name").with_settings_dir();
    c.store(&format!("{old_name}={LEAK}\nBINANCE_LIVE_API_KEY={LEAK}\n"));

    let o = c.run(&[]);
    let all = both(&o);
    assert!(o.status.success(), "a row nothing reads is no failure: {all}");
    assert!(!all.contains(LEAK), "a value leaked: {all}");
    assert!(!all.contains("BINANCE_LIVE_API_KEY"), "a credential NAME leaked: {all}");
}

// ---------------------------------------------------------------------------------------------
// The verb's own dispositions — each one a row of the module doc's table
// ---------------------------------------------------------------------------------------------

/// **The headline case, through the real process.** A store that EXISTS and cannot
/// be read is never "no credentials" — the two are indistinguishable downstream and only one of
/// them is a correct fresh install — but WHAT the verb does about it is computed, not fixed:
///
/// * nothing armed for live ⇒ a WARNING, exit 0. Every venue was already going to be paper, and a
///   pre-check that stopped a working paper daemon over a file it never opens would take down more
///   than it protects. That is the disposition `docs/decisions/0013-degrade-vs-refuse.md` records
///   for the daemon itself, and this verb does not overrule it.
/// * ARMED for live ⇒ a refusal, exit 1. The operator asked for a real venue and the process cannot
///   read the file it authenticates from, so it would place no orders while every surface reads
///   live — ADR question 2, set-but-unhonoured.
///
/// Bytes that are not a database where the database belongs are the portable stand-in for an
/// unreadable store — a `chmod 000` proves nothing when the test runs as root, which CI does.
///
/// ⚠ **The ARMED branch is no longer reachable through a tree like this one, and that is decision
/// 0111 rather than a gap.** Arming is the `flags.tradehub_live` row alone — no variable stands
/// beside it — and that row lives in the very database this tree cannot open, so an unreadable
/// store resolves this box as unarmed (or undetermined), never armed. The variable that drove the
/// armed half here is refused by the dispatcher, naming the row, before this verb runs.
#[test]
fn an_unreadable_credential_store_warns_on_paper() {
    let c = Case::new("unreadable").with_settings_dir();
    // Bytes that are not a database where the database belongs — the one store, present and
    // unopenable (`chmod 000` proves nothing as root).
    std::fs::create_dir_all(c.settings().join(vike_secrets::DB_DIR)).unwrap();
    std::fs::write(vike_secrets::db_path_in(&c.settings()), b"not a sqlite database at all")
        .unwrap();

    // ⚠ Since the credential FILE store was removed (2026-10-07) the credential store IS the settings
    // database, so an unreadable credential store is an unreadable settings database too — and THAT
    // finding fails the check on its own (`a_genuinely_unreadable_store_degrades_settings_and_
    // credentials_together`). What stays this test's to pin is the CREDENTIAL row's own
    // disposition: on paper it is a WARN, never the FAIL, so the box's only failure is the settings
    // one.
    let o = c.run(&[]);
    let all = both(&o);
    let credential_row = |all: &str| {
        all.lines().find(|l| l.contains("credential store")).map(str::to_string).unwrap_or_default()
    };
    let row = credential_row(&all);
    assert!(row.starts_with("WARN"), "on paper the credential finding is a WARNING: {all}");
    assert!(row.contains("vike.db"), "the finding must name the store: {all}");
    assert!(row.contains("Nothing on this box is ARMED FOR LIVE"), "{all}");
    let fails: Vec<&str> = all.lines().filter(|l| l.starts_with("FAIL")).collect();
    assert_eq!(
        fails.len(),
        1,
        "the one FAIL is the settings database's, not the credentials': {all}"
    );
    assert!(fails[0].contains("settings database"), "{all}");
}

/// **THE SWITCHLESS-VENUE RESIDUAL, closed and proved through the real process.**
///
/// Most roster venues have no switch of their own: a box live on deribit, oanda, ig, fxcm,
/// dukascopy, ctrader, alpaca, ibkr or aster picks its tier from the credential PREFIX, inside the
/// very file that cannot be read, so per-venue evidence answers nothing and this exact tree used to
/// WARN and start all-paper while every surface said live.
///
/// The node-scoped `flags.tradehub_live` row is what answers instead: it selects the twelve-venue
/// `crates/vike-mount/src/node.rs` `build_node` mount, which takes live every venue whose credentials
/// resolve. Since decision 0111 it is the ONLY spelling, and it lives in the same database as the
/// credentials — so what this pins is that a store which will not open fails the check outright.
///
/// ⚠ Note what is NOT in this environment: no `{VENUE}_MAINNET`, no `POLY_*`. `env_clear` guarantees
/// it, so a pass here cannot be the old signal answering under a new name.
#[test]
fn a_genuinely_unreadable_store_degrades_settings_and_credentials_together() {
    let c = Case::new("switchless").with_settings_dir();

    // The ROW layer — `flags.tradehub_live` in the settings database — planted, then the ONE file
    // it lives in corrupted AFTER the plant closed it, simulating a box whose database has since
    // gone bad rather than a fixture that never wrote one.
    c.seed(&[("flags", "tradehub_live", "true")]);
    std::fs::write(vike_secrets::db_path_in(&c.settings()), b"not a sqlite file").unwrap();

    // ⚠ **This used to prove the node-scoped ROW makes an unreadable store refuse.** Under one
    // database (0054/0086) the row and the credential table are the SAME file, so a store that
    // will not open loses both at once: the `tradehub_live` row that would have proven the box
    // armed is exactly as unreadable as the credential it would have armed. What survives is the
    // stronger, honest property — the settings half fails unconditionally (an unopenable database
    // is a hard refusal, armed or not), which is what still makes this exit non-zero.
    let o = c.run(&[]);
    assert!(!o.status.success(), "an unopenable settings database must refuse: {}", both(&o));
    let all = both(&o);
    assert!(all.contains("FAIL"), "{all}");
    assert!(!all.contains("tradehub_live"), "an unopenable store names no row from it: {all}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("FAILED"),
        "…and say so on stderr, which is what `journalctl -p err` shows an operator: {all}"
    );
}

/// …and the flag alone is NOT a finding. An armed box whose store reads fine is a correctly
/// configured live node — this verb DETECTS a false belief, it does not object to going live, and
/// `crates/vike-cli/src/cmd/config/check.rs`'s `unreadable_store_finding` is the only place arming
/// is consulted at all. Without this case the fix above would have quietly become a pre-check veto
/// on every live deployment.
#[test]
fn arming_the_node_is_not_itself_a_failure_when_the_store_reads() {
    let c = Case::new("armed-clean").with_settings_dir();
    c.seed(&[("flags", "tradehub_live", "true")]);
    c.store("BYBIT_DEMO_API_KEY=k\n");

    let o = c.run(&[]);
    assert!(o.status.success(), "arming is a configuration, not a defect: {}", both(&o));
    assert!(both(&o).contains("0 error(s)"), "{}", both(&o));
}

/// **Set-but-unhonoured is a refusal** (`docs/decisions/0013-degrade-vs-refuse.md`, question 2).
/// Every shipped unit sets `VIKE_SETTINGS_DIR`, so this is the pre-check's teeth: an install recipe
/// that forgot `install -d …/settings` now stops the unit instead of producing a daemon with no
/// policy ceiling and no credentials, silently, with nothing in any log.
#[test]
fn a_named_settings_directory_that_does_not_exist_fails_the_check() {
    // NOTE: no `with_settings_dir()` — that is the whole point of this case.
    let c = Case::new("nodir");

    let o = c.run(&[]);
    assert!(!o.status.success(), "a named-and-missing directory must refuse: {}", both(&o));
    let all = both(&o);
    assert!(all.contains("VIKE_SETTINGS_DIR"), "name the variable that promised it: {all}");
    assert!(all.contains("install -d"), "…and say how to fix it: {all}");
}

// ---------------------------------------------------------------------------------------------
// The refusal that fires ONE LAYER UP — the dispatcher's, not this verb's
// ---------------------------------------------------------------------------------------------
//
// ⚠ THE TEST BELOW DOES NOT REACH `config check`, AND THAT IS WHAT IT EXISTS TO PIN.
// `vike_cli::run` calls `resolve_policy` — `vike_config::refuse_removed_env`, then the real
// `vike_boot::boot` load — BEFORE `dispatch` routes a verb, so a REMOVED environment variable exits
// 1 from the dispatcher with the loader's own message, and no verb runs at all. Verified by
// construction (`crate::run`'s `?` on `resolve_policy`) and by [`from_the_dispatcher`] below, which
// keys on the wording only that layer produces.
//
// ⚠ **This section used to hold THREE tests, and the other two moved to the section below it.**
// `docs/decisions/0086` is why: a settings ROW that will not parse or apply no longer makes
// `resolve_policy` return `Err` at all — it is `Settings::seal_refusal`, a MARK, and the mark is
// checked by whichever verb JUDGES the tree (`config check`'s own `"settings rows"` finding), never
// by the dispatcher. So "a broken settings file/an unknown key refuses from the dispatcher" is no
// longer true of anything — there is no file, and an illegal row does not refuse at that layer any
// more. Only the REMOVED-VARIABLE refusal below still genuinely fires before a verb runs, because
// `refuse_removed_env` is Layer 0 and has nothing to do with the rows at all.
//
// It is still worth having, and worth having HERE rather than deleted:
//
//   * what a systemd `ExecStartPre=` observes is one bit — the exit code — and the unit comments'
//     claim ("it refuses a removed setting") is true OF THE PROCESS. This proves the process-level
//     property the units depend on, which no unit test can see.
//   * the ORDERING is load-bearing and was previously unpinned. If `resolve_policy` ever stopped
//     refusing first, `config check`'s own row would take over silently — a change this test now
//     makes visible instead, by asserting WHICH layer answered.

/// The dispatcher's own refusal — `crate::run`'s `eprintln!("vike-cli: {e}")` around
/// `resolve_policy`'s error. Asserting it is how a test says "this exited before the verb ran"
/// instead of merely "this exited 1".
///
/// ⚠ **That prefix alone does not say it, and this used to rest on the prefix alone.** Two other
/// things in `crate::run` print it: `install_user_indicators` emits `vike-cli: {message}` for every
/// rejected `<project>/user_data/indicators/*.rhai`, and `settings_warning_lines` yields
/// `vike-cli: settings: …`. Both run only AFTER `resolve_policy` SUCCEEDS, so neither can fire in
/// the three cases below — but that is the very ordering these tests exist to pin, so keying on a
/// string they share made the pin depend on the thing it was protecting. Worse, the indicator lane
/// reads a directory resolved from the CHILD's working directory, which no case here sets: it is
/// whatever tree the suite happens to run in, so the predicate's answer was partly a property of the
/// developer's checkout.
///
/// The second condition is what carries it, and it holds by construction rather than by wording:
/// **stdout is empty**. `config check` writes its report to stdout on EVERY path it reaches —
/// including the `--strict` failure, whose own test asserts the report is printed — so an empty
/// stdout means the verb never ran, whatever anyone prints on stderr, and it stays true if the
/// wording changes.
///
/// ⚠ Stated precisely, because the loose version ("nothing above `dispatch` writes to stdout") is
/// FALSE and is exactly the kind of sentence the next person keying a test on empty stdout would
/// repeat: `crate::run`'s NO-SUBCOMMAND arm calls `print_help`, which is `println!`. That arm
/// RETURNS from it — it never reaches `resolve_policy` or `dispatch` — and every case in this file
/// passes `config check`, so it cannot fire here. What holds is the narrower claim: between the
/// subcommand check and `dispatch`, nothing writes to stdout. `resolve_policy`'s error,
/// `settings_warning_lines` and `install_user_indicators` all print to STDERR, deliberately, because
/// `vike-cli mcp`'s stdout is a protocol.
///
/// The two conditions cover each other exactly. The verb's ONE stdout-free exit is a bad argument
/// (`args::exit_for_parse_error`), and every message it and `config_check::run` emit is prefixed
/// `"vike-cli config check: "` — which does not start with `"vike-cli: "`, the colon falling in a
/// different place. So the prefix excludes the verb's own failures and the empty stdout excludes
/// everything the verb reached.
fn from_the_dispatcher(o: &Output) -> bool {
    String::from_utf8_lossy(&o.stderr).starts_with("vike-cli: ") && o.stdout.is_empty()
}

// ---------------------------------------------------------------------------------------------
// A bad ROW is MARKED, not a dispatcher refusal — and only a JUDGING verb acts on the mark
// ---------------------------------------------------------------------------------------------
//
// ⚠ **These two used to be `..._refuses_from_the_dispatcher_before_the_verb_runs`, alongside the
// removed-variable test above.** `docs/decisions/0086` moved the ground under them: a settings ROW
// that will not parse or apply no longer makes `resolve_policy` (and therefore `vike_boot::boot`)
// return `Err` at all — `crate::mirror::apply_rows`'s own doc argues why, the JSON incident's
// lesson, that a refusal here would take `config show` and every `secrets` verb down with the box
// they exist to diagnose. So it is `Settings::seal_refusal`, a MARK, and the mark is checked by
// whichever verb JUDGES the tree — `config check`'s own `"settings rows"` finding — never by the
// dispatcher. `config show`, a DISCLOSING verb, keeps running and exits 0 on the identical tree.

/// A broken settings ROW stops `config check` — but from ITS OWN judging logic, not the
/// dispatcher, and `config show`'s exit code on the same tree is the proof of the difference.
#[test]
fn a_broken_settings_row_is_marked_and_only_the_judging_verb_fails() {
    let c = Case::new("brokenrow").with_settings_dir();
    c.seed(&[("policy", "max_leverage", "\"not a number\"")]);

    let o = c.run(&[]);
    assert!(!o.status.success(), "the JUDGING verb must still fail: {}", both(&o));
    assert!(!o.stdout.is_empty(), "the verb ran and printed its own report: {}", both(&o));
    assert!(
        !from_the_dispatcher(&o),
        "the layer that catches this MOVED — it is `config check`'s own finding now: {}",
        both(&o)
    );
    assert!(both(&o).contains("max_leverage"), "the offending key must be named: {}", both(&o));

    // …and the proof that the mark is not a hard load error: `config show`, which judges nothing,
    // keeps running on the identical tree and exits 0.
    let show = Command::new(BIN)
        .args(["config", "show"])
        .env_clear()
        .env("VIKE_SETTINGS_DIR", c.settings())
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    assert!(
        show.status.success(),
        "a DISCLOSING verb must not brick on the box it exists to diagnose: {}",
        both(&show)
    );
}

/// An unknown key is refused BY NAME — and, as everywhere else in this crate, without echoing the
/// row's VALUE. This is the same MARK as its sibling above, through `config check`'s judging logic
/// rather than the dispatcher: the REDACTION is the property worth proving at process level either
/// way, since it has to hold on every path that prints a `vike_config::ConfigError`.
#[test]
fn an_unknown_settings_key_is_marked_and_refused_by_the_judging_verb_without_echoing_its_value() {
    const CANARY: &str = "DUMMY-ROW-APIKEY-SHOULD-NEVER-PRINT";
    let c = Case::new("unknownkey").with_settings_dir();
    c.seed(&[("config", "api_key", &format!("\"{CANARY}\""))]);

    let o = c.run(&[]);
    assert!(!o.status.success(), "{}", both(&o));
    let all = both(&o);
    assert!(!all.contains(CANARY), "the rejected row's VALUE leaked: {all}");
    assert!(all.contains("api_key"), "the offending KEY must still be named: {all}");
    assert!(!from_the_dispatcher(&o), "the layer that catches this MOVED: {all}");
}

/// **The removed-variable refusal, end to end** — also one layer up, and disclosed as such since it
/// was written: `config_check`'s own row for it can never print anything but `OK`, so the
/// process-level behaviour is the only thing that proves the refusal exists at all.
///
/// It matters for a unit: `EnvironmentFile=-<project>/.env` reaches the pre-check too, so a stale
/// ceiling variable in that file stops the daemon instead of being silently ignored.
#[test]
fn a_removed_environment_variable_refuses_from_the_dispatcher() {
    let c = Case::new("removedenv").with_settings_dir();

    let o = c.run_with_env(&[], &[("VIKE_MAX_ORDER_NOTIONAL", "250")]);
    assert!(!o.status.success(), "a removed variable must refuse: {}", both(&o));
    let all = both(&o);
    assert!(all.contains("vike-cli config set policy."), "the way out is a ROW write: {all}");
    assert!(all.contains("max_notional_per_order"), "the replacement KEY: {all}");
    assert!(all.contains("250"), "…with the operator's own value pasted in: {all}");
    assert!(from_the_dispatcher(&o), "the LAYER moved — see this section's note: {all}");
}

// ---------------------------------------------------------------------------------------------
// The degrades — exit 0 by default, and what `--strict` is for
// ---------------------------------------------------------------------------------------------

/// A store readable beyond its owner is a WARNING, not a refusal, and the two dispositions of the
/// same tree are asserted together because the difference IS the `--strict` flag: a unit runs the
/// default and starts, an operator auditing the box runs `--strict` and does not.
///
/// `crates/vike-secrets/src/store/warnings.rs`'s `PermissionWarning` argues why this cannot be a
/// refusal. (This planted a pre-one-store `<project>/.env` until the credential FILE store was
/// removed on 2026-10-07; nothing reads that file now, so it is no finding at all.)
#[cfg(unix)]
#[test]
fn a_store_readable_beyond_its_owner_warns_but_only_strict_fails() {
    use std::os::unix::fs::PermissionsExt;
    let c = Case::new("perm").with_settings_dir();
    c.store("BINANCE_LIVE_API_KEY=k\n");
    let db = vike_secrets::db_path_in(&c.settings());
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();

    let o = c.run(&[]);
    assert!(o.status.success(), "a finding is never a refusal: {}", both(&o));
    let out = stdout(&o);
    assert!(out.contains("WARN"), "{out}");
    assert!(out.contains("warning(s)"), "{out}");

    let strict = c.run(&["--strict"]);
    assert!(!strict.status.success(), "--strict counts a warning as an error: {}", both(&strict));
    assert!(String::from_utf8_lossy(&strict.stderr).contains("--strict"), "{}", both(&strict));
}

// ---------------------------------------------------------------------------------------------
// The machine view, and redaction
// ---------------------------------------------------------------------------------------------

/// `--json` carries the verdict PRE-COMPUTED, so a monitor never has to re-implement the `--strict`
/// promotion rule to learn what the exit code was.
#[test]
fn the_json_view_carries_the_verdict_and_the_findings() {
    let c = Case::new("json").with_settings_dir();
    c.seed(&[("policy", "max_notional_per_order", "250")]);

    let o = c.run(&["--json"]);
    assert!(o.status.success(), "{}", both(&o));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");

    assert_eq!(doc["settings_dir"].as_str(), Some(c.settings().display().to_string().as_str()));
    assert_eq!(doc["settings_dir_origin"], serde_json::json!("named"));
    assert_eq!(doc["ok"], serde_json::Value::Bool(true));
    assert_eq!(doc["failures"], serde_json::json!(0));
    let findings = doc["findings"].as_array().expect("findings");
    // ⚠ There are no settings FILES to name a clean row after any more (`docs/decisions/0086`): a
    // well-formed row produces no finding at all — see `config_check.rs`'s own
    // `a_clean_tree_with_a_real_store_is_all_ok`. What this view must still carry is the credential
    // store's own row, which every tree names whether or not one is configured.
    assert!(
        findings.iter().any(|f| f["subject"] == "credential store" && f["level"] == "ok"),
        "{doc}"
    );

    // …and the same tree, judged by a caller who wanted the failure.
    let bad = Case::new("jsonbad");
    let o = bad.run(&["--json"]);
    assert!(!o.status.success(), "{}", both(&o));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
    assert_eq!(doc["ok"], serde_json::Value::Bool(false));
    assert_eq!(doc["failures"], serde_json::json!(1));
}

/// This output lands in a journal and in pasted issues: the store is disclosed by COUNT, never by a
/// key NAME and never by a VALUE, in either view.
#[test]
fn no_credential_reaches_either_view() {
    let c = Case::new("redact").with_settings_dir();
    c.store("BINANCE_LIVE_API_KEY=key-abcd1234\nOKX_DEMO_API_PASSPHRASE=s3cr3t\n");

    for args in [vec![], vec!["--json"]] {
        let o = c.run(&args);
        assert!(o.status.success(), "{}", both(&o));
        let all = both(&o);
        assert!(!all.contains("key-abcd1234"), "a credential VALUE leaked: {all}");
        assert!(!all.contains("s3cr3t"), "a credential VALUE leaked: {all}");
        assert!(!all.contains("BINANCE_LIVE_API_KEY"), "a credential NAME leaked: {all}");
        assert!(all.contains("2 key(s)"), "…and the count is what IS disclosed: {all}");
    }
}

// ---------------------------------------------------------------------------------------------
// The verb surface
// ---------------------------------------------------------------------------------------------

/// A misspelled verb names BOTH real ones — the list is the only discovery surface an operator
/// following a runbook has.
#[test]
fn an_unknown_verb_names_both_verbs() {
    let c = Case::new("badverb").with_settings_dir();
    let out = Command::new(BIN)
        .args(["config", "chekc"])
        .env_clear()
        .env("VIKE_SETTINGS_DIR", c.settings())
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("show") && err.contains("check"), "{err}");
}
