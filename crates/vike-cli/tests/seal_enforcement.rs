//! **What a box in a REFUSING seal state can and cannot still do** — driven through the SHIPPED
//! binary (`CARGO_BIN_EXE_vike-cli`), never through a library call.
//!
//! ⚠ **This file exists because its library-level predecessor inverted the answer it reported.**
//! The test that became `crates/vike-config/src/mirror.rs`'s
//! `a_stale_row_is_marked_by_name_rather_than_stopping_the_boot` was called
//! `..._and_the_repairs_still_run`, and it asserted no such thing: what it called is
//! `rows_from_files(dir)` — a function that resolved the FILES and never opened the store (both the
//! function and the settings-FILE layer it read are gone, per `docs/decisions/0086`). The
//! repairs are VERBS, and a verb in this binary is reached through `vike_cli::run`, which calls
//! `resolve_policy` before `dispatch` for every subcommand. So the library function returned `Ok`
//! while every real binary on the box exited 1, and the test read green. It has been renamed to
//! what it does measure, and the claim it used to make lives HERE, where a process is started.
//!
//! The two halves below are each other's necessary complement, and either alone is satisfied by
//! doing nothing:
//!
//! * **The repairs RUN.** A refusal that takes down the command its own text names as the repair is
//!   the 2026-09-18 JSON incident, and this is that incident's regression test in a second column.
//! * **The actors REFUSE.** Marking without enforcing is silencing. `vike_config::Settings`'s
//!   `store_refusal` doc and `vike_cli::resolve_policy`'s `settings:` arm have BOTH claimed since
//!   the mark was introduced that "`trade` and `mcp` refuse on it", and until
//!   `Resolved::seal_refusal` existed nothing in this crate read either mark and every verb ran.
//!
//! ⚠ **REWRITTEN for `docs/decisions/0086` (settings live only in the database).** `config mirror`'s
//! four-settings-file half, `config compare` and `config adopt`/`--undo` are all RETIRED — there is
//! no file-vs-row "adoption" any more, and the seal is created and moved by
//! `vike_config::write_setting_row` itself, on the FIRST row it ever writes to a box. So the fixture
//! that used to be `secrets migrate` -> `config mirror` -> `config adopt` is now
//! `secrets migrate` -> `config set <key> <value>`, and the repair for an unsound seal is
//! restoring `vike.db` from the box's nightly backup rather than a second CLI verb.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// A project root with a settings directory, driven only through the binary.
struct Case {
    root: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("vike-seal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("settings")).unwrap();
        Case { root }
    }

    fn settings(&self) -> PathBuf {
        self.root.join("settings")
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.settings().join(name), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        // `env_clear` for the reason `config_check_cli.rs` gives: without it a run on a developer
        // box resolves the REPO's settings directory and reads a real credential store.
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", self.settings());
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }

    /// **Create the database and write ONE row through the shipped binary** — the whole of what it
    /// takes to seal a box now (0086): `write_setting_row_in` creates the seal on the first row it
    /// ever writes. Both steps through the binary, so the fixture is a state a real operator can
    /// actually produce — a hand-built store would prove nothing about a box anybody has.
    fn seed(&self, key: &str, value: &str) {
        // ⚠ ONE obviously-fake DEMO key, and it is load-bearing rather than incidental: `migrate`
        // refuses to create a database when there is nothing to move ("nothing to migrate, so NO
        // database was created — that is the correct outcome for a box with no credentials yet"),
        // so an empty file leaves no store for `config set` to write into. The value is not a
        // credential in any sense — it never leaves this temp directory, and a DEMO key authorises
        // nothing anywhere even if it were real.
        self.write("secrets.env", "BINANCE_DEMO_API_KEY=fixture-not-a-real-key\n");
        let g = self.run(&["secrets", "migrate"]);
        assert!(g.status.success(), "the fixture's migrate must succeed: {}", both(&g));
        let s = self.run(&["config", "set", key, value]);
        assert!(s.status.success(), "the fixture's seed write must succeed: {}", both(&s));
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn both(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// Plant a row the loader cannot parse, WITHOUT moving the counts the seal recorded.
///
/// ⚠ The count is held deliberately: it routes the fixture through `section_values`'
/// `RowRefusal::Unreadable` arm rather than through the erase detector, so this proves the arm that
/// the JSON incident actually travelled. `'C:\vike\state'` is the real value from that incident —
/// valid TOML, and not JSON.
fn plant_unreadable_row(c: &Case) {
    let dir = c.settings();
    let src = vike_secrets::read_settings_in(&dir).expect("the fixture store must open");
    let mut rows = src.rows().expect("the fixture must have rows").clone();
    let row = rows.settings.first_mut().expect("the fixture must have at least one setting row");
    row.value = r"'C:\vike\state'".to_string();
    vike_secrets::write_settings_in(&dir, &rows).expect("planting the row must succeed");
}

// ---------------------------------------------------------------------------------------------
// Half one — the repairs RUN
// ---------------------------------------------------------------------------------------------

/// **The JSON incident's regression test.** Every verb an operator is TOLD to reach for must still
/// work in the refusing state, because there is no other way back: `sqlite3` is on neither box, and
/// (0086) there is no `config adopt --undo` or `config compare` left to reach for either — the
/// repair is restoring the database from the nightly backup, out of band. A failure here is the
/// incident, not a test nit.
#[test]
fn a_broken_seal_still_runs_every_repair_and_disclosure_verb() {
    let c = Case::new("repairs");
    c.seed("policy.max_notional_per_order", "250");
    plant_unreadable_row(&c);

    // Each of these is named BY a refusal message somewhere in this tree, or is the only way to
    // see what is wrong. A failure here is the incident, not a test nit.
    for verb in [
        &["config", "check"][..],
        &["config", "show"][..],
        &["secrets", "list"][..],
        &["--help"][..],
    ] {
        let o = c.run(verb);
        let text = both(&o);
        assert!(
            !text.contains("could not load settings") && !text.is_empty(),
            "`vike-cli {}` produced the loader's refusal instead of running: {text}",
            verb.join(" ")
        );
        // ⚠ **`config check` exits NON-ZERO by design, so the exit code is not the property under
        // test here — RUNNING is.** It REPORTS the fault (that is the whole point of it). What
        // every row here shares, and what actually distinguishes a working box from the bricked
        // one measured at `650907a37`, is that the verb PRODUCED ITS OWN OUTPUT rather than the
        // loader's refusal. So that is what is asserted for all of them, and the exit code only
        // where it means what it looks like it means.
        if verb == ["config", "check"] {
            assert!(
                text.contains("settings directory") || text.contains("store:"),
                "`vike-cli {}` must render its own report in a refusing state: {text}",
                verb.join(" ")
            );
        } else {
            assert!(
                o.status.success(),
                "`vike-cli {}` must still run in a refusing state — this is the repair, and a \
                 refusal that takes it down is the JSON incident: {text}",
                verb.join(" ")
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Half two — the actors REFUSE
// ---------------------------------------------------------------------------------------------

/// The complement. Without this the first test is satisfied by deleting the refusal outright.
#[test]
fn a_broken_seal_refuses_the_verbs_that_act_on_a_ceiling() {
    let c = Case::new("actors");
    c.seed("policy.max_notional_per_order", "250");
    plant_unreadable_row(&c);

    for verb in [&["trade", "status"][..], &["mcp"][..]] {
        let o = c.run(verb);
        assert!(
            !o.status.success(),
            "`vike-cli {}` ACTS on the ceilings and must refuse while the seal is unsound: {}",
            verb.join(" "),
            both(&o)
        );
        assert!(
            both(&o).contains("settings seal is unsound"),
            "the refusal must say WHY and name the repairs: {}",
            both(&o)
        );
        assert!(
            both(&o).contains("nightly backup"),
            "the refusal must name the ONLY repair left (0086: no config adopt --undo any \
             more): {}",
            both(&o)
        );
    }
}

/// **A settings write must never re-bless an erased ceiling.** `config set` refuses OUTRIGHT while
/// the current store does not boot clean, rather than letting a second write paper over the first —
/// the row-native writer's own precondition (`docs/decisions/0086` point 2), proven here at the CLI.
#[test]
fn a_write_over_an_unsound_seal_is_refused_and_changes_nothing() {
    let c = Case::new("write-over-unsound");
    c.seed("policy.max_notional_per_order", "250");
    plant_unreadable_row(&c);

    let before = std::fs::read(c.settings().join("db").join("vike.db")).expect("db bytes");
    let o = c.run(&["config", "set", "policy.max_notional_per_order", "300"]);
    assert!(!o.status.success(), "a write over an unsound store must be refused: {}", both(&o));
    let after = std::fs::read(c.settings().join("db").join("vike.db")).expect("db bytes");
    assert_eq!(before, after, "a refused write must leave the database byte-identical");
}

/// **A box with a seal and no rows at all** — the state `config check` reported as `OK` at
/// `650907a37` because its store finding renders `SettingsSource`'s `Display` and had no arm for
/// it.
#[test]
fn config_check_fails_a_sealed_box_that_carries_no_rows() {
    let c = Case::new("emptyseal");
    c.seed("policy.max_notional_per_order", "250");

    let dir = c.settings();
    let empty = vike_secrets::StoredSettings::default();
    vike_secrets::write_settings_in(&dir, &empty).expect("emptying the tables must succeed");

    let o = c.run(&["config", "check"]);
    assert!(
        !o.status.success(),
        "a sealed box resolving from NOTHING has no ceiling and must not read as healthy: {}",
        both(&o)
    );
    assert!(
        both(&o).contains("no ceiling"),
        "the finding must name what is actually lost: {}",
        both(&o)
    );
}
