//! End-to-end tests for `vike-cli config check`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a real settings directory in a temp dir.
//!
//! The unit tests beside `crates/vike-cli/src/cmd/config_check.rs` cover the disposition table and
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

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.settings().join(name), body).unwrap();
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
    c.write("policy.toml", "max_notional_per_order = 250\n");

    let o = c.run(&[]);
    assert!(o.status.success(), "a paper deployment must start: {}", both(&o));
    let out = stdout(&o);
    assert!(out.contains(&format!("settings directory: {}", c.settings().display())), "{out}");
    assert!(out.contains("named outright by VIKE_SETTINGS_DIR"), "the RUNG must be named: {out}");
    assert!(out.contains("policy.toml"), "{out}");
    assert!(out.contains("0 error(s)"), "{out}");

    let strict = c.run(&["--strict"]);
    assert!(strict.status.success(), "not even --strict may fail this: {}", both(&strict));
}

// ---------------------------------------------------------------------------------------------
// The verb's own dispositions — each one a row of the module doc's table
// ---------------------------------------------------------------------------------------------

/// **The headline case, both branches, through the real process.** A store that EXISTS and cannot
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
/// A DIRECTORY where the file should be is the portable stand-in for an unreadable file — a
/// `chmod 000` proves nothing when the test runs as root, which CI does. The arming variable comes
/// FROM `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` rather than being spelled, so this asserts
/// the mechanism (whatever that table holds is what counts as live) and not one venue's flag.
#[test]
fn an_unreadable_credential_store_warns_on_paper_and_refuses_when_armed_for_live() {
    let c = Case::new("unreadable").with_settings_dir();
    std::fs::create_dir_all(c.settings().join("secrets.env")).unwrap();

    let o = c.run(&[]);
    assert!(o.status.success(), "a paper box must still start: {}", both(&o));
    let all = both(&o);
    assert!(all.contains("secrets.env"), "the finding must name the file: {all}");
    assert!(all.contains("WARN"), "{all}");
    // …and the audit audience still gets a failure out of it.
    let strict = c.run(&["--strict"]);
    assert!(!strict.status.success(), "--strict counts it: {}", both(&strict));

    // The SAME tree, on a box armed for live.
    let armer = vike_config::CREDENTIAL_FILE_ARMING_REFUSED[0].var;
    let armed = c.run_with_env(&[], &[(armer, "1")]);
    assert!(!armed.status.success(), "an armed box must refuse: {}", both(&armed));
    let all = both(&armed);
    assert!(all.contains("FAIL"), "{all}");
    assert!(all.contains(armer), "the refusal must name what armed it: {all}");
    // …and the one-line stderr summary an `ExecStartPre=` failure leaves in `journalctl -p err`.
    assert!(
        String::from_utf8_lossy(&armed.stderr).contains("FAILED"),
        "a refusal must say so on stderr too: {all}"
    );
}

/// **THE SWITCHLESS-VENUE RESIDUAL, closed and proved through the real process.**
///
/// `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` names a variable per venue, and only four roster
/// venues HAVE one. A box live on any of the other nine — deribit, oanda, ig, fxcm, dukascopy,
/// ctrader, alpaca, ibkr, aster — picks its tier from the credential PREFIX, inside the very file
/// that cannot be read, so the env table answers nothing and this exact tree used to WARN and start
/// all-paper while every surface said live.
///
/// The node-scoped `flags.tradehub_live` is what answers instead: it selects the twelve-venue
/// `crates/vike-run/src/node.rs` `build_node` mount, which takes live every venue whose credentials
/// resolve. **Both spellings are driven**, because the whole property is that the signal lives
/// OUTSIDE the store — a file the operator can read, and the environment variable a systemd
/// `EnvironmentFile=` supplies (which is how the one real deployment sets it).
///
/// ⚠ Note what is NOT in this environment: no `{VENUE}_MAINNET`, no `POLY_*`. `env_clear` guarantees
/// it, so a pass here cannot be the old signal answering under a new name.
#[test]
fn an_unreadable_store_refuses_on_a_box_armed_only_by_the_node_scoped_flag() {
    let c = Case::new("switchless").with_settings_dir();
    std::fs::create_dir_all(c.settings().join("secrets.env")).unwrap();

    // (a) the FILE layer — `<project>/settings/flags.toml`.
    c.write("flags.toml", "tradehub_live = true\n");
    let o = c.run(&[]);
    assert!(!o.status.success(), "a live node with an unreadable store must refuse: {}", both(&o));
    let all = both(&o);
    assert!(all.contains("FAIL"), "{all}");
    assert!(all.contains("tradehub_live"), "the refusal must name the source: {all}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("FAILED"),
        "…and say so on stderr, which is what `journalctl -p err` shows an operator: {all}"
    );

    // (b) the ENV layer — the same flag as a systemd `EnvironmentFile=` line, file silent. The
    // variable comes FROM `vike_config::flags::TRADEHUB_LIVE_ENV` rather than being spelled, the
    // convention the armed-store case above follows: it keys the assertion on the MECHANISM, and a
    // bare env-shaped literal is harvested as a sighting by `crates/vike-ops/src/scan.rs`'s
    // `find_map_lookups`, which would attribute a read of this variable to `vike-cli`.
    c.write("flags.toml", "# nothing about the live gate here\n");
    let env_armed = c.run_with_env(&[], &[(vike_config::flags::TRADEHUB_LIVE_ENV, "1")]);
    assert!(!env_armed.status.success(), "the env spelling must arm too: {}", both(&env_armed));
    assert!(both(&env_armed).contains("FAIL"), "{}", both(&env_armed));

    // (c) ⚠ THE ADR 0013 GUARD, same tree, flag off: a PAPER box with an unreadable store must
    // still WARN and still START. This is what every shipped unit is, and narrowing the old
    // unconditional FAIL is the whole reason the computed level exists — closing one hole must not
    // reopen the other.
    let paper = c.run(&[]);
    assert!(paper.status.success(), "a paper box must keep starting: {}", both(&paper));
    assert!(both(&paper).contains("WARN"), "{}", both(&paper));
    // …and the audit audience still gets a failure out of it.
    assert!(!c.run(&["--strict"]).status.success());
}

/// …and the flag alone is NOT a finding. An armed box whose store reads fine is a correctly
/// configured live node — this verb DETECTS a false belief, it does not object to going live, and
/// `crates/vike-cli/src/cmd/config_check.rs`'s `unreadable_store_finding` is the only place arming
/// is consulted at all. Without this case the fix above would have quietly become a pre-check veto
/// on every live deployment.
#[test]
fn arming_the_node_is_not_itself_a_failure_when_the_store_reads() {
    let c = Case::new("armed-clean").with_settings_dir();
    c.write("flags.toml", "tradehub_live = true\n");
    c.write("secrets.env", "BYBIT_DEMO_API_KEY=k\n");

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
// The refusals that fire ONE LAYER UP — the dispatcher's, not this verb's
// ---------------------------------------------------------------------------------------------
//
// ⚠ THE THREE TESTS BELOW DO NOT REACH `config check`, AND THAT IS WHAT THEY EXIST TO PIN.
// `vike_cli::run` calls `resolve_policy` — `vike_config::refuse_removed_env`, then
// `vike_config::load` — BEFORE `dispatch` routes a verb, so a settings tree that will not load
// exits 1 from the dispatcher with the loader's own message, and no verb runs at all. Verified by
// construction (`crate::run`'s `?` on `resolve_policy`) and by [`from_the_dispatcher`] below, which
// keys on the wording only that layer produces.
//
// They were named `..._fails_the_check` and read as coverage of the verb. They are not: the
// consequence is that `inspect`'s `Err` arm for `vike_config::describe` (the `settings files` FAIL
// row) is UNREACHABLE through the shipped binary, exactly like the removed-environment row above
// it, and `crates/vike-cli/src/cmd/config_check.rs` now says so beside both.
//
// They are still worth having, and worth having HERE rather than deleted:
//
//   * what a systemd `ExecStartPre=` observes is one bit — the exit code — and the unit comments'
//     claim ("it refuses a settings file that does not parse") is true OF THE PROCESS. These prove
//     the process-level property the units depend on, which no unit test can see.
//   * the ORDERING is load-bearing and was previously unpinned. If `resolve_policy` ever stopped
//     refusing first, the verb's own rows would take over silently — a change these tests now make
//     visible instead, by asserting WHICH layer answered.

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

/// A broken settings file stops the process — from the DISPATCHER, before `config check` runs — and
/// the message names the file. A `check` that passed here would contradict the binary it is
/// checking, so the exit code is what the unit needs and it is correct either way.
#[test]
fn a_broken_settings_file_refuses_from_the_dispatcher_before_the_verb_runs() {
    let c = Case::new("broken").with_settings_dir();
    c.write("policy.toml", "max_leverage = \"not a number\"\n");

    let o = c.run(&[]);
    assert!(!o.status.success(), "{}", both(&o));
    assert!(both(&o).contains("policy.toml"), "{}", both(&o));
    assert!(from_the_dispatcher(&o), "the LAYER moved — see this section's note: {}", both(&o));
    assert!(
        both(&o).contains("settings could not be loaded"),
        "`resolve_policy`'s own wording is the proof of WHICH layer refused: {}",
        both(&o)
    );
    // …and the proof that it is not this verb: `config show`, which judges nothing, refuses
    // identically. Anything the two verbs share was never the verb's doing.
    let show = Command::new(BIN)
        .args(["config", "show"])
        .env_clear()
        .env("VIKE_SETTINGS_DIR", c.settings())
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    assert!(!show.status.success(), "{}", both(&show));
    assert!(from_the_dispatcher(&show), "{}", both(&show));
}

/// An unknown key is refused BY NAME — and, as everywhere else in this crate, without echoing the
/// line's VALUE. `secrets.env` sits in this very directory, so `api_key = "…"` in `config.toml` is
/// a plausible first-time mistake, and this output lands in the journal.
///
/// Same layer as its sibling above: the loader refuses inside the dispatcher. The REDACTION is the
/// property worth proving at process level either way — it has to hold on every path that prints a
/// `vike_config::ConfigError`, and this is one of them.
#[test]
fn an_unknown_settings_key_refuses_from_the_dispatcher_without_echoing_its_value() {
    const CANARY: &str = "DUMMY-TOML-APIKEY-SHOULD-NEVER-PRINT";
    let c = Case::new("unknownkey").with_settings_dir();
    c.write("config.toml", &format!("api_key = \"{CANARY}\"\n"));

    let o = c.run(&[]);
    assert!(!o.status.success(), "{}", both(&o));
    let all = both(&o);
    assert!(!all.contains(CANARY), "the rejected line's VALUE leaked: {all}");
    assert!(all.contains("api_key"), "the offending KEY must still be named: {all}");
    assert!(from_the_dispatcher(&o), "the LAYER moved — see this section's note: {all}");
    assert!(all.contains("settings could not be loaded"), "WHICH layer refused: {all}");
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
    assert!(all.contains("policy.toml"), "{all}");
    assert!(all.contains("max_notional_per_order"), "the replacement KEY: {all}");
    assert!(all.contains("250"), "…with the operator's own value pasted in: {all}");
    assert!(from_the_dispatcher(&o), "the LAYER moved — see this section's note: {all}");
}

// ---------------------------------------------------------------------------------------------
// The degrades — exit 0 by default, and what `--strict` is for
// ---------------------------------------------------------------------------------------------

/// A pre-one-store `<project>/.env` beside an ABSENT store is a WARNING, not a refusal, and the two
/// dispositions of the same tree are asserted together because the difference IS the `--strict`
/// flag: a unit runs the default and starts, an operator auditing the box runs `--strict` and does
/// not.
///
/// `crates/vike-secrets/src/store.rs`'s `LegacyStoreWarning` argues why this cannot be a refusal —
/// a `.env` is also a legitimate systemd `EnvironmentFile`, and the CI box's recorder ships one.
#[test]
fn a_legacy_dotenv_beside_an_absent_store_warns_but_only_strict_fails() {
    let c = Case::new("legacy").with_settings_dir();
    std::fs::write(c.root.join(".env"), "POLY_PROXY_ENABLED=false\n").unwrap();

    let o = c.run(&[]);
    assert!(o.status.success(), "a finding is never a refusal: {}", both(&o));
    let out = stdout(&o);
    assert!(out.contains("WARN"), "{out}");
    assert!(out.contains("1 warning(s)"), "{out}");

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
    c.write("policy.toml", "max_notional_per_order = 250\n");

    let o = c.run(&["--json"]);
    assert!(o.status.success(), "{}", both(&o));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");

    assert_eq!(doc["settings_dir"].as_str(), Some(c.settings().display().to_string().as_str()));
    assert_eq!(doc["settings_dir_origin"], serde_json::json!("named"));
    assert_eq!(doc["ok"], serde_json::Value::Bool(true));
    assert_eq!(doc["failures"], serde_json::json!(0));
    let findings = doc["findings"].as_array().expect("findings");
    assert!(findings.iter().any(|f| f["subject"] == "policy.toml" && f["level"] == "ok"), "{doc}");

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
    c.write("secrets.env", "BINANCE_LIVE_API_KEY=key-abcd1234\nOKX_DEMO_API_PASSPHRASE=s3cr3t\n");

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
