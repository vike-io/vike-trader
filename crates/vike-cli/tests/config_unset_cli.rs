//! End-to-end tests for `vike-cli config unset`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a real settings directory in a temp dir (decision 0161).
//!
//! The unit tests beside the module cover the argv grammar and the report wording, and
//! `crates/vike-config/src/write_tests.rs` covers the planner and its validator against real
//! databases. These cover what an operator actually does — and above all **the thing that
//! commissioned the verb**: a store the loader refuses over one stale row is bricked for
//! `vike-tradehub`, `trade`, `mcp`, `config check` and `config set` alike, and this verb is the one
//! thing that cures it without a restore from backup.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory, passed through
//! `Command::env` on the CHILD (never `std::env::set_var`, which is unsafe under threads), and an
//! otherwise EMPTY environment. This verb WRITES, so the redirect is what keeps a test out of a real
//! settings database.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// A throwaway PROJECT and its `settings/` child — the fixture shape of `config_set_cli.rs`'s
/// `Case`: a BOUND `tempfile::TempDir` so a kill signal cannot leave a foreign-owned directory
/// behind, and `$VIKE_SETTINGS_DIR` naming the `settings/` CHILD.
struct Case {
    /// Held only for its `Drop`.
    #[expect(dead_code)]
    root: tempfile::TempDir,
    dir: PathBuf,
}

fn row(section: &str, key: &str, value: &str) -> vike_secrets::SettingRow {
    vike_secrets::SettingRow {
        section: section.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    }
}

impl Case {
    fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-config-unset-{tag}-"))
            .tempdir()
            .expect("tempdir");
        let dir = root.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        Case { root, dir }
    }

    /// Plant a settings database holding `rows` (unsealed, exactly as `secrets init` plus a
    /// whole-table plant leaves one).
    fn seed(&self, rows: Vec<vike_secrets::SettingRow>) {
        let rows = vike_secrets::StoredSettings { settings: rows, ..Default::default() };
        vike_secrets::plant_settings_rows(&self.dir, &rows).expect("seed");
    }

    /// Seal the store the way a real box is sealed: one sanctioned `config set` through the binary.
    fn seal(&self) {
        let out = self.cli(&["config", "set", "preferences.log_level", "info"]);
        assert!(out.status.success(), "the sealing write must land: {}", both(&out));
    }

    /// Plant `section.key = value` as ONE more row of a SEALED store, moving the seal's count with
    /// it — what a key retired by a later release leaves behind (no writer can produce it now).
    fn plant_stale_row(&self, section: &str, key: &str, value: &str) {
        let source = vike_secrets::read_settings_in(&self.dir).expect("read");
        let mut rows = source.rows().expect("rows").clone();
        rows.settings.push(row(section, key, value));
        vike_secrets::write_settings_in(&self.dir, &rows.sorted()).expect("plant the stale row");
    }

    fn read_setting(&self, key: &str) -> Option<String> {
        let (section, leaf) = key.split_once('.')?;
        let rows = vike_secrets::read_settings_in(&self.dir).ok()?;
        rows.rows()?
            .settings
            .iter()
            .find(|r| r.section == section && r.key == leaf)
            .map(|r| r.value.clone())
    }

    fn db_bytes(&self) -> Vec<u8> {
        std::fs::read(vike_secrets::db_path_in(&self.dir)).expect("the database")
    }

    /// Run `vike-cli <args>` with the case's directory as THE settings directory and an otherwise
    /// EMPTY environment. Stdin is `/dev/null`.
    fn cli(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", &self.dir);
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }

    fn unset(&self, key: &str) -> Output {
        self.cli(&["config", "unset", key])
    }

    /// Everything the change journal wrote under `<settings>/state`, concatenated.
    fn journal(&self) -> String {
        let mut out = String::new();
        let mut stack = vec![self.dir.join("state")];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(t) = std::fs::read_to_string(&p) {
                    out.push_str(&t);
                }
            }
        }
        out
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn both(o: &Output) -> String {
    format!("{}{}", stdout(o), stderr(o))
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// **An unset removes exactly the named row, says what it did and that nothing is live, and
/// journals it — without the value, in the report or the ledger.**
#[test]
fn an_unset_removes_the_named_row_journals_it_and_never_shows_its_value() {
    let c = Case::new("basic");
    c.seed(vec![
        row("config", "tradehub_addr", "\"<host>:1234\""),
        row("config", "log_dir", "\"/var/tmp/vike-logs\""),
    ]);

    let out = c.unset("config.tradehub_addr");
    assert!(out.status.success(), "{}", both(&out));
    let text = stdout(&out);
    assert!(text.contains("removed config.tradehub_addr"), "{text}");
    assert!(text.contains("its value is not shown"), "{text}");
    assert!(text.contains("compiled-in default"), "what it resolves to now: {text}");
    assert!(text.contains("not live") && text.contains("restart"), "{text}");
    assert!(text.contains("store: boots clean"), "{text}");
    assert!(!text.contains("<host>"), "the removed row's value was printed: {text}");

    assert_eq!(c.read_setting("config.tradehub_addr"), None, "the row is gone");
    assert_eq!(
        c.read_setting("config.log_dir").as_deref(),
        Some("\"/var/tmp/vike-logs\""),
        "a sibling row is untouched"
    );

    let ledger = c.journal();
    assert!(ledger.contains("set_setting"), "no record: {ledger}");
    assert!(ledger.contains("config.tradehub_addr"), "{ledger}");
    assert!(ledger.contains("<unset>") && ledger.contains("<set>"), "{ledger}");
    assert!(ledger.contains("pending_restart"), "a local removal reaches no applier: {ledger}");
    assert!(!ledger.contains("<host>"), "the removed row's value reached the ledger: {ledger}");
}

/// A removed policy CEILING is called out, and a default of "no value" is said as such.
#[test]
fn removing_a_ceiling_says_the_default_applies_and_carries_no_value() {
    let c = Case::new("ceiling");
    c.seed(vec![row("policy", "max_notional_per_order", "250")]);
    let out = c.unset("policy.max_notional_per_order");
    assert!(out.status.success(), "{}", both(&out));
    let text = stdout(&out);
    assert!(text.contains("policy ceiling row is gone"), "{text}");
    assert!(text.contains("UNSET") && text.contains("ABSENT ceiling"), "{text}");
    assert_eq!(c.read_setting("policy.max_notional_per_order"), None);
}

/// **THE BRICK CURE, through the shipped binary.** A sealed store carries a row the loader refuses
/// (a key a later release retired, count still sound). Every consumer refuses it: `config check`
/// reports it, `trade` will not run, `config set` will not write over it. `config check` prints the
/// exact line, `config unset` runs it, and the store is whole again — no backup, no hand DELETE.
#[test]
fn a_store_bricked_by_a_retired_row_is_cured_by_unset_alone() {
    let c = Case::new("brick");
    c.seed(vec![row("config", "tradehub_addr", "\"<host>:1234\"")]);
    c.seal();
    c.plant_stale_row("policy", "max_total_exposure", "500.0");

    // The state the verb exists for. `config check` names the row AND the line that removes it.
    let check = c.cli(&["config", "check"]);
    assert!(!check.status.success(), "a refused row must fail the pre-flight: {}", both(&check));
    assert!(
        stdout(&check).contains("Remove it with: vike-cli config unset policy.max_total_exposure"),
        "check must print the exact removal line: {}",
        both(&check)
    );
    // `trade` refuses, and its text now carries the cure instead of denying there is one.
    let trade = c.cli(&["trade", "status"]);
    assert!(!trade.status.success(), "{}", both(&trade));
    let refusal = stderr(&trade);
    assert!(refusal.contains("settings seal is unsound"), "{refusal}");
    assert!(
        refusal.contains("vike-cli config unset <section>.<key>"),
        "the cure is named: {refusal}"
    );
    assert!(!refusal.contains("no command repairs it"), "a row HAS a command: {refusal}");
    // …and `config set` is refused over the unsound store (the validator demands one that boots).
    let set = c.cli(&["config", "set", "preferences.log_level", "warn"]);
    assert_eq!(code(&set), 2, "{}", both(&set));
    assert!(stderr(&set).contains("does not boot clean"), "{}", stderr(&set));

    // The cure.
    let out = c.unset("policy.max_total_exposure");
    assert!(
        out.status.success(),
        "the narrow validator must let the repair through: {}",
        both(&out)
    );
    let text = stdout(&out);
    assert!(text.contains("does not know this key"), "a retired key has no default: {text}");
    assert!(text.contains("rows") && text.contains("->"), "the seal's move is reported: {text}");
    assert!(text.contains("store: boots clean"), "{text}");
    assert_eq!(c.read_setting("policy.max_total_exposure"), None);

    // The store is whole again: the pre-flight no longer reports a refused row, and a write lands.
    let check = c.cli(&["config", "check"]);
    assert!(
        !stdout(&check).contains("keeps this store from loading"),
        "the row is gone, so is its finding: {}",
        both(&check)
    );
    let set = c.cli(&["config", "set", "preferences.log_level", "warn"]);
    assert!(set.status.success(), "an ordinary write works again: {}", both(&set));
}

/// A credential-shaped key is refused on the usage rung, worded as a removal, with the store
/// byte-identical and no ledger row.
#[test]
fn a_credential_shaped_key_is_refused_and_removes_nothing() {
    let c = Case::new("secret");
    c.seed(vec![row("config", "tradehub_addr", "\"<host>:1234\"")]);
    let before = c.db_bytes();
    let out = c.unset("config.bot_token");
    assert_eq!(code(&out), 2, "{}", both(&out));
    let err = stderr(&out);
    assert!(err.contains("nothing was removed"), "{err}");
    assert!(err.contains("secrets set") && err.contains("stdin"), "{err}");
    assert!(err.contains("hand INSERT"), "the declared residual is said, not implied: {err}");
    assert_eq!(c.db_bytes(), before, "the store must be byte-identical");
    assert!(!c.journal().contains("bot_token"), "a verb-level refusal writes no ledger row");
}

/// A key with no row is refused BY NAME on the usage rung, recorded as a refusal, the store
/// untouched.
#[test]
fn a_key_with_no_row_is_refused_by_name_and_recorded() {
    let c = Case::new("missing");
    c.seed(vec![row("config", "tradehub_addr", "\"<host>:1234\"")]);
    let before = c.db_bytes();
    let out = c.unset("policy.max_levrage");
    assert_eq!(code(&out), 2, "{}", both(&out));
    let err = stderr(&out);
    assert!(err.contains("policy.max_levrage") && err.contains("nothing was removed"), "{err}");
    assert_eq!(c.db_bytes(), before, "the store must be byte-identical");
    let ledger = c.journal();
    assert!(ledger.contains("refused") && ledger.contains("policy.max_levrage"), "{ledger}");
}

/// Tokens that might be a pasted value are refused QUOTING NOTHING: `KEY=VALUE`, and a second token
/// after the key.
#[test]
fn a_pasted_value_is_refused_without_being_repeated_back() {
    let c = Case::new("pasted");
    c.seed(vec![row("config", "tradehub_addr", "\"<host>:1234\"")]);
    let before = c.db_bytes();

    let out = c.unset("BINANCE_LIVE_API_KEY=sk_live_deadbeef");
    assert_eq!(code(&out), 2, "{}", both(&out));
    assert!(!both(&out).contains("sk_live_deadbeef"), "THE VALUE was echoed: {}", both(&out));
    assert!(!both(&out).contains("BINANCE_LIVE_API_KEY"), "the token was echoed: {}", both(&out));

    let out = c.cli(&["config", "unset", "config.tradehub_addr", "hunter2-not-for-the-log"]);
    assert_eq!(code(&out), 2, "{}", both(&out));
    assert!(!both(&out).contains("hunter2"), "a second token was echoed: {}", both(&out));
    assert_eq!(c.db_bytes(), before, "neither refusal may touch the store");
}

/// A `venue.*` key is another table; it is named as such and nothing is removed.
#[test]
fn a_venue_key_is_refused_as_another_table() {
    let c = Case::new("venue");
    c.seed(Vec::new());
    let out = c.unset("venue.polymarket.proxy_host");
    assert_eq!(code(&out), 2, "{}", both(&out));
    let err = stderr(&out);
    assert!(err.contains("venue_setting") && err.contains("nothing was removed"), "{err}");
}

/// A key naming no settings section is refused with the menu, before any database is needed.
#[test]
fn a_key_naming_no_settings_section_is_refused_with_the_menu() {
    let c = Case::new("section");
    let out = c.unset("polciy.max_leverage");
    assert_eq!(code(&out), 2, "{}", both(&out));
    let err = stderr(&out);
    for spelling in ["policy.", "config.", "preferences.", "flags."] {
        assert!(err.contains(spelling), "{err}");
    }
}

/// **A seal whose row COUNT is off is the one cause this verb refuses**, because moving the count
/// would hide the erase. The refusal says why and names the backup; the store is byte-identical.
#[test]
fn an_unset_over_a_seal_count_that_is_off_is_refused_and_names_the_backup() {
    let c = Case::new("count");
    c.seed(vec![row("config", "tradehub_addr", "\"<host>:1234\"")]);
    c.seal();
    // Rows left by "some other route": the seal says seven, the table holds two.
    vike_secrets::hold_write_lock(&c.dir)
        .execute_batch("UPDATE settings_adoption SET setting_rows = 7; COMMIT;")
        .expect("the count is rewritten");
    let before = c.db_bytes();

    let out = c.unset("config.tradehub_addr");
    assert_eq!(code(&out), 2, "{}", both(&out));
    let err = stderr(&out);
    assert!(err.contains("already unsound"), "{err}");
    assert!(err.contains("nightly backup"), "the cure that is left is named: {err}");
    assert_eq!(c.db_bytes(), before, "the store must be byte-identical");
}

/// `config unset` is one of the verbs that RUNS in a refusing state — the repair for a refused row
/// must not be taken down by the refusal it cures (the JSON incident, in `seal_enforcement.rs`'s
/// words) — and it is not on the seal-refusing list.
#[test]
fn unset_runs_while_the_seal_is_unsound() {
    let c = Case::new("runs");
    c.seed(vec![row("config", "tradehub_addr", "\"<host>:1234\"")]);
    c.seal();
    c.plant_stale_row("preferences", "rate_utilization", "0.5");
    let out = c.unset("preferences.rate_utilization");
    assert!(out.status.success(), "the repair must run in the state it repairs: {}", both(&out));
}
