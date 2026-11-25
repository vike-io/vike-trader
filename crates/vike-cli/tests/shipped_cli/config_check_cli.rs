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
//! of the environment, for the reason `crates/vike-cli/tests/shipped_cli/config_cli.rs` gives: without it a run
//! on a developer box resolves the REPO's settings directory and puts a real credential store's key
//! count into a test's assertions. Passed through `Command::env`/`env_clear` on the CHILD, never
//! `std::env::set_var`, which is unsafe under threads and would leak across this binary's parallel
//! cases.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use crate::common;

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
/// * nothing armed for live ⇒ a WARNING, exit 0. No order was going to reach a venue anyway, and a
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
/// `crates/vike-tradehub-core/src/node.rs` `assemble_runtime` mount, which takes live every venue whose credentials
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
/// `vike-mcp`'s stdout is a protocol.
///
/// The two conditions cover each other exactly. The verb's ONE stdout-free exit is a bad argument
/// (`args::finish`), and every message it and `config_check::run` emit is prefixed
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
/// `crates/vike-secrets/src/store/warnings.rs`'s `StorePermissionWarning` argues why this cannot be a
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
fn an_unknown_verb_is_refused_and_the_nearest_verb_is_named() {
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
    assert!(err.contains("unrecognized subcommand") && err.contains("check"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// The ACTIVE daemon profile — judged by the daemon, `vike-tradehub --check-profile`
// ---------------------------------------------------------------------------------------------
//
// The daemon itself is not built in this crate's lane (`CARGO_BIN_EXE_vike-tradehub` exists only in
// its own), so a stand-in answers `--check-profile` with the REAL loader's verdict on the stored
// body (`vike_tradehub::profile_rows::rows_to_daemon_profile`, what the daemon's boot runs): its
// refusal on stderr and exit 1, or a line on stdout and exit 0. The real binary's half of that
// contract is `crates/vike-tradehub/tests/daemon_spawn/check_profile.rs`.
//
// ⚠ Each stand-in is run ONCE by this process, through the `ETXTBSY` retry, before `vike-cli` is
// pointed at it: the race is a forked child still holding the plant's write descriptor, and once one
// exec of the file has succeeded no process can hold one again (this process closed its own). The
// shipped binary's spawn of it then cannot lose that race, and a lost race there would read as a
// NOT JUDGED warning rather than a spawn error any retry could see.

#[cfg(unix)]
mod daemon_profile {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use vike_secrets::profile_store::{
        MountRow, ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileRow, set_active,
        store_profile,
    };

    use super::{Case, both, stdout};

    /// A one-mount polymarket body: a `symbol` is all a paper mount needs.
    fn mount() -> MountRow {
        let mut m = MountRow::new(0, "polymarket", "PredictionMarket");
        m.symbol = Some("TOK".to_string());
        m
    }

    /// The case's tree: one well-formed settings row, then `mount` (with `params`) stored as the
    /// ACTIVE daemon profile `name` through the profile writer — never through `ProfileDaemon`, which
    /// would refuse the retired shape before it reached the store.
    fn store_active(
        c: &Case,
        name: &str,
        mount: MountRow,
        params: BTreeMap<(i64, String), String>,
    ) -> ProfileRecord {
        c.seed(&[("policy", "max_notional_per_order", "250")]);
        let db = vike_secrets::db_path_in(&c.settings());
        let stored = ProfileRecord {
            row: ProfileRow {
                name: name.to_string(),
                kind: ProfileKind::Daemon,
                active: false,
                note: None,
            },
            mounts: vec![mount],
            params,
            settings: BTreeMap::new(),
            recorder: None,
        };
        let write = ProfileOperatorWrite::claim("vike-cli config check test");
        store_profile(&db, &stored, &write, 1, vike_model::AssetClass::SQL_WORDS).unwrap();
        set_active(&db, ProfileKind::Daemon, name, &write, 1, vike_model::AssetClass::SQL_WORDS)
            .unwrap();
        stored
    }

    /// Plant `body` as a stand-in `vike-tradehub` under `<root>/stub/`, settled (see the section
    /// note). With no argument it exits 0 at once, which is what the settling run passes; with any
    /// argument but `--check-profile`, or a `VIKE_SETTINGS_DIR` other than the case's, it exits 9.
    /// Shell builtins only: the shipped binary runs with a cleared environment, so no `PATH`.
    fn plant(c: &Case, body: &str) -> PathBuf {
        let dir = c.root.join("stub");
        std::fs::create_dir_all(&dir).unwrap();
        let script = format!(
            "#!/bin/sh\n[ $# -eq 0 ] && exit 0\n\
             [ \"$*\" = \"--check-profile\" ] || {{ echo \"stub: argv $*\" >&2; exit 9; }}\n\
             [ \"$VIKE_SETTINGS_DIR\" = \"{}\" ] || {{ echo \"stub: dir $VIKE_SETTINGS_DIR\" >&2; exit 9; }}\n\
             {body}\n",
            c.settings().display()
        );
        let planted = vike_cli::test_support::plant_engine(&dir, "vike-tradehub", &script);
        let settled =
            vike_cli::test_support::output_retrying_etxtbsy("settle the stand-in", || {
                Command::new(planted.path())
            });
        assert!(settled.status.success(), "the stand-in's settling run: {settled:?}");
        planted.path().to_path_buf()
    }

    /// A stand-in that answers as the daemon's own loader does for `stored`.
    fn judge_like_the_daemon(c: &Case, stored: &ProfileRecord) -> PathBuf {
        let body = match vike_tradehub::profile_rows::rows_to_daemon_profile(stored) {
            Ok(_) => format!(
                "echo \"vike-tradehub: the ACTIVE daemon profile {} loads\"\nexit 0",
                stored.row.name
            ),
            Err(refusal) => format!(
                "while IFS= read -r l; do printf '%s\\n' \"$l\"; done >&2 <<'WORDS'\n\
                 vike-tradehub: bad profile row `{}`: {refusal}\nWORDS\nexit 1",
                stored.row.name
            ),
        };
        plant(c, &body)
    }

    fn check(c: &Case, tradehub: &Path, extra: &[&str]) -> std::process::Output {
        let mut args = vec!["--tradehub", tradehub.to_str().unwrap()];
        args.extend_from_slice(extra);
        c.run(&args)
    }

    /// Every file under `dir` with its bytes, in path order.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    out.push((path, bytes));
                }
            }
        }
        out.sort();
        out
    }

    /// **The deploy question.** A stored ACTIVE profile still setting the retired top-level `qty`
    /// FAILS the check, quoting the daemon's own refusal and naming the cure — in both views.
    #[test]
    fn an_active_daemon_profile_with_a_retired_top_level_quantity_fails_in_the_daemons_own_words() {
        let c = Case::new("daemon-retired").with_settings_dir();
        let mut m = mount();
        m.qty = Some(5.0);
        let stored = store_active(&c, "old-shape", m, BTreeMap::new());
        let refusal = vike_tradehub::profile_rows::rows_to_daemon_profile(&stored)
            .expect_err("the daemon's loader refuses a top-level qty");
        assert!(refusal.contains("this profile sets: qty"), "{refusal}");
        let tradehub = judge_like_the_daemon(&c, &stored);

        let o = check(&c, &tradehub, &[]);
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("FAIL  daemon profile"), "{out}");
        assert!(out.contains("the ACTIVE daemon profile `old-shape` is REFUSED"), "{out}");
        assert!(out.contains(&refusal), "the daemon's words, verbatim: {out}");
        assert!(out.contains("vike-cli config profile create daemon old-shape"), "the cure: {out}");
        assert!(out.contains("`[strategy.params]`"), "{out}");

        let o = check(&c, &tradehub, &["--json"]);
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
        let row = doc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["subject"] == "daemon profile")
            .unwrap_or_else(|| panic!("no daemon profile finding: {doc}"));
        assert_eq!(row["level"], "fail", "{doc}");
        assert!(row["detail"].as_str().unwrap().contains(&refusal), "{doc}");
    }

    /// The same knob under `[strategy.params]` of the mount's strategy FILE loads, and the check
    /// passes, with no retired-selection finding.
    #[test]
    fn the_same_knob_under_strategy_params_passes() {
        let c = Case::new("daemon-params").with_settings_dir();
        let mut m = mount();
        m.strategy_file = Some("market_making/spread_maker.rs".to_string());
        let params = BTreeMap::from([((0, "qty".to_string()), "5.0".to_string())]);
        let stored = store_active(&c, "new-shape", m, params);
        vike_tradehub::profile_rows::rows_to_daemon_profile(&stored)
            .expect("the daemon's loader takes the knob under [strategy.params]");
        let tradehub = judge_like_the_daemon(&c, &stored);

        let o = check(&c, &tradehub, &[]);
        assert!(o.status.success(), "{}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("OK    daemon profile"), "{out}");
        assert!(out.contains("`new-shape` loads through the daemon's own loader"), "{out}");
        assert!(!out.contains("strategy selection"), "no row names a retired column: {out}");
    }

    /// **A RETIRED `run` row (decision 0297) WARNS, never fails**: the trading daemon refuses to
    /// start on an ACTIVE one, and the check prints the fold line rendered from its own rows; a FAIL
    /// would hold every daemon the supervisor runs this verb before.
    #[test]
    fn an_active_retired_run_row_warns_with_the_fold_line() {
        let c = Case::new("daemon-retired-run").with_settings_dir();
        let mut m = mount();
        m.strategy_file = Some("market_making/spread_maker.rs".to_string());
        let stored = store_active(&c, "tradehub", m, BTreeMap::new());
        let db = vike_secrets::db_path_in(&c.settings());
        let run = ProfileRecord {
            row: ProfileRow {
                name: "run-live".to_string(),
                kind: ProfileKind::Run,
                active: false,
                note: None,
            },
            mounts: Vec::new(),
            params: BTreeMap::new(),
            settings: BTreeMap::from([
                ("risk.max_notional_per_order".to_string(), "100000.0".to_string()),
                ("risk.max_total_exposure".to_string(), "1000000.0".to_string()),
            ]),
            recorder: None,
        };
        let write = ProfileOperatorWrite::claim("vike-cli config check test");
        store_profile(&db, &run, &write, 1, vike_model::AssetClass::SQL_WORDS).unwrap();
        set_active(&db, ProfileKind::Run, "run-live", &write, 1, vike_model::AssetClass::SQL_WORDS)
            .unwrap();
        let tradehub = judge_like_the_daemon(&c, &stored);

        let o = check(&c, &tradehub, &[]);
        assert!(o.status.success(), "a WARN, never a FAIL: {}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("WARN  run profile"), "{out}");
        assert!(
            out.contains(
                "vike-cli config profile create run tradehub --risk.max_notional_per_order 100000.0 \
                 --risk.max_total_exposure 1000000.0"
            ),
            "the fold line, from the row's own values: {out}"
        );
        assert!(out.contains("vike-cli config profile deactivate run"), "{out}");
    }

    /// **A mount row still holding a RETIRED selection column FAILS, read-only, with the fix**: a
    /// strategy is a file, no load reads `strategy_name`/`strategy_rhai`, so the row mounts nothing by
    /// it. The row is planted by SQL (no writer writes those columns any more) on a profile that
    /// otherwise loads, so the finding is the column's alone; storing the profile again clears it.
    #[test]
    fn a_row_holding_a_retired_strategy_column_fails_with_the_fix() {
        let c = Case::new("daemon-retired-column").with_settings_dir();
        let mut m = mount();
        m.strategy_file = Some("market_making/spread_maker.rs".to_string());
        let stored = store_active(&c, "named", m, BTreeMap::new());
        vike_secrets::hold_write_lock(&c.settings())
            .execute_batch(
                "UPDATE mount SET strategy_name = 'spread_maker' WHERE profile = 'named'; COMMIT;",
            )
            .expect("the retired column is planted");
        let tradehub = judge_like_the_daemon(&c, &stored);
        let before = snapshot(&c.settings());

        let o = check(&c, &tradehub, &[]);
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("FAIL  strategy selection"), "{out}");
        assert!(out.contains("profile `named` mount 0"), "names the row: {out}");
        assert!(out.contains("`strategy_name`"), "names the column: {out}");
        assert!(out.contains("\"spread_maker\""), "names the value: {out}");
        assert!(out.contains("--strategy"), "the fix: {out}");
        assert!(out.contains("OK    daemon profile"), "the profile itself still loads: {out}");
        assert_eq!(snapshot(&c.settings()), before, "the check rewrote nothing");

        let o = check(&c, &tradehub, &["--json"]);
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
        let row = doc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["subject"] == "strategy selection")
            .unwrap_or_else(|| panic!("no strategy selection finding: {doc}"));
        assert_eq!(row["level"], "fail", "{doc}");

        // Storing the profile again (any writer) clears the column, and the finding goes.
        store_active(&c, "named", stored.mounts[0].clone(), BTreeMap::new());
        let o = check(&c, &tradehub, &[]);
        assert!(o.status.success(), "{}", both(&o));
        assert!(!stdout(&o).contains("strategy selection"), "{}", stdout(&o));
    }

    /// The retired script path the two tests below plant; its file name is the cure's.
    const RETIRED_SCRIPT: &str = "/srv/rhai/b7_donothing/b7_donothing.rhai";

    /// Plant `RETIRED_SCRIPT` in `profile`'s retired `strategy_rhai` column, by SQL: no writer
    /// writes it any more.
    fn plant_retired_script(c: &Case, profile: &str) {
        vike_secrets::hold_write_lock(&c.settings())
            .execute_batch(&format!(
                "UPDATE mount SET strategy_rhai = '{RETIRED_SCRIPT}' WHERE profile = '{profile}'; \
                 COMMIT;"
            ))
            .expect("the retired column is planted");
    }

    /// The `strategy selection` finding of the JSON view.
    fn retired_finding(o: &std::process::Output) -> serde_json::Value {
        let doc: serde_json::Value = serde_json::from_str(&stdout(o)).expect("valid JSON");
        doc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["subject"] == "strategy selection")
            .unwrap_or_else(|| panic!("no strategy selection finding: {doc}"))
            .clone()
    }

    /// **The ACTIVE profile's answer, pinned word for word** (the `strategy_rhai` shape STOP 4 of the
    /// v0.1.102 deploy met): a FAIL, exit 1, with the re-store cure and nothing about a delete.
    #[test]
    fn an_active_profile_naming_a_retired_script_fails_word_for_word() {
        let c = Case::new("daemon-retired-script").with_settings_dir();
        let mut m = mount();
        m.strategy_file = Some("market_making/spread_maker.rs".to_string());
        let stored = store_active(&c, "shadow", m, BTreeMap::new());
        plant_retired_script(&c, "shadow");
        let tradehub = judge_like_the_daemon(&c, &stored);

        let o = check(&c, &tradehub, &[]);
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        assert!(stdout(&o).contains("FAIL  strategy selection"), "{}", stdout(&o));
        let o = check(&c, &tradehub, &["--json"]);
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        let row = retired_finding(&o);
        assert_eq!(row["level"], "fail", "{row}");
        assert_eq!(
            row["detail"],
            format!(
                "profile `shadow` mount 0 names its strategy by the retired `strategy_rhai` column \
                 (\"{RETIRED_SCRIPT}\"), which is no longer read: a strategy is a FILE under the \
                 project's `strategies/` folder now. Store the profile again naming the file, \
                 which clears the column: `vike-cli config profile create daemon shadow … \
                 --strategy <folder>/b7_donothing.rhai` (or any stored-profile writer with \
                 `[strategy] file = \"<folder>/b7_donothing.rhai\"`)"
            ),
            "{row}"
        );
    }

    /// **"Retired names just stop being read": the same row in an INACTIVE profile WARNS and the
    /// check passes**, naming the profile, the row and both cures — so a leftover profile no load
    /// reads cannot hold every daemon behind the supervisor's pre-flight (STOP 4 of the v0.1.102
    /// deploy). `--strict` still counts it, and activating the profile turns it back into the FAIL.
    #[test]
    fn an_inactive_profile_naming_a_retired_script_warns_and_passes() {
        let c = Case::new("daemon-retired-inactive").with_settings_dir();
        let mut m = mount();
        m.strategy_file = Some("market_making/spread_maker.rs".to_string());
        store_active(&c, "tradehub-oanda-shadow", m.clone(), BTreeMap::new());
        plant_retired_script(&c, "tradehub-oanda-shadow");
        // Activating `tradehub` deactivates the shadow: one active row per kind.
        store_active(&c, "tradehub", m, BTreeMap::new());
        let tradehub = plant(&c, "echo \"vike-tradehub: the ACTIVE daemon profile loads\"\nexit 0");

        let o = check(&c, &tradehub, &[]);
        assert!(o.status.success(), "an inactive leftover holds no daemon: {}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("WARN  strategy selection"), "{out}");
        assert!(!out.contains("FAIL"), "{out}");
        let o = check(&c, &tradehub, &["--json"]);
        assert!(o.status.success(), "{}", both(&o));
        let row = retired_finding(&o);
        assert_eq!(row["level"], "warn", "{row}");
        let detail = row["detail"].as_str().unwrap();
        let value = format!("\"{RETIRED_SCRIPT}\"");
        for needle in [
            "INACTIVE daemon profile `tradehub-oanda-shadow` mount 0",
            "`strategy_rhai`",
            value.as_str(),
            "--strategy <folder>/b7_donothing.rhai",
            "`vike-cli config profile rm daemon tradehub-oanda-shadow --dry-run`, then `--yes`",
        ] {
            assert!(detail.contains(needle), "missing {needle:?}: {detail}");
        }
        assert_eq!(
            check(&c, &tradehub, &["--strict"]).status.code(),
            Some(1),
            "--strict counts it"
        );

        let db = vike_secrets::db_path_in(&c.settings());
        let write = ProfileOperatorWrite::claim("vike-cli config check test");
        set_active(
            &db,
            ProfileKind::Daemon,
            "tradehub-oanda-shadow",
            &write,
            1,
            vike_model::AssetClass::SQL_WORDS,
        )
        .unwrap();
        let o = check(&c, &tradehub, &["--json"]);
        assert_eq!(o.status.code(), Some(1), "the ACTIVE profile still fails: {}", both(&o));
        assert_eq!(retired_finding(&o)["level"], "fail");
    }

    /// A store with no daemon profile is no finding, as before, and no daemon is asked: the
    /// stand-in would fail the check and leave a mark.
    #[test]
    fn a_store_with_no_daemon_profile_asks_no_daemon_and_reports_nothing() {
        let c = Case::new("daemon-none").with_settings_dir();
        c.seed(&[("policy", "max_notional_per_order", "250")]);
        let mark = c.root.join("asked");
        let tradehub = plant(&c, &format!(": > \"{}\"\nexit 1", mark.display()));

        let o = check(&c, &tradehub, &["--strict"]);
        assert!(o.status.success(), "{}", both(&o));
        assert!(!stdout(&o).contains("daemon profile"), "{}", stdout(&o));
        assert!(!mark.exists(), "the daemon was asked about a store that holds no daemon profile");
    }

    /// **Read-only.** A check of a store holding an ACTIVE daemon profile leaves every byte of the
    /// settings directory as it found it — the profile read, the settings read and the credential
    /// read alike.
    #[test]
    fn the_check_writes_nothing_into_the_settings_directory() {
        let c = Case::new("daemon-readonly").with_settings_dir();
        let mut m = mount();
        m.qty = Some(5.0);
        let stored = store_active(&c, "old-shape", m, BTreeMap::new());
        let tradehub = judge_like_the_daemon(&c, &stored);
        let before = snapshot(&c.settings());
        assert!(!before.is_empty(), "the store is there to be read");

        for args in [&[][..], &["--json"][..], &["--strict"][..]] {
            let o = check(&c, &tradehub, args);
            assert_eq!(o.status.code(), Some(1), "{}", both(&o));
            assert_eq!(snapshot(&c.settings()), before, "`config check {args:?}` wrote");
        }
    }

    /// A daemon that cannot be run leaves the profile NOT JUDGED: a warning, which `--strict`
    /// counts, never a pass that says it was looked at.
    #[test]
    fn a_daemon_that_cannot_be_run_leaves_the_profile_not_judged() {
        let c = Case::new("daemon-missing").with_settings_dir();
        let mut m = mount();
        m.qty = Some(5.0);
        store_active(&c, "old-shape", m, BTreeMap::new());
        let missing = c.root.join("no-such-dir").join("vike-tradehub");

        let o = check(&c, &missing, &[]);
        assert!(o.status.success(), "not judged is not refused: {}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("WARN  daemon profile"), "{out}");
        assert!(out.contains("`old-shape` is NOT JUDGED"), "{out}");
        assert_eq!(check(&c, &missing, &["--strict"]).status.code(), Some(1));

        // …and so does a daemon older than the flag, whose argument parser refuses it (exit 2).
        let old =
            plant(&c, "echo \"error: unexpected argument '--check-profile' found\" >&2\nexit 2");
        let o = check(&c, &old, &[]);
        assert!(o.status.success(), "{}", both(&o));
        let out = stdout(&o);
        assert!(out.contains("NOT JUDGED") && out.contains("exited 2"), "{out}");
        assert!(out.contains("unexpected argument '--check-profile'"), "its words: {out}");
    }
}
