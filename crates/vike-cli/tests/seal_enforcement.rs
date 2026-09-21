//! **What a box in a REFUSING seal state can and cannot still do** — driven through the SHIPPED
//! binary (`CARGO_BIN_EXE_vike-cli`), never through a library call.
//!
//! ⚠ **This file exists because its library-level predecessor inverted the answer it reported.**
//! `crates/vike-config/tests/mirror.rs`'s
//! `a_stale_format_row_marks_a_seal_refusal_on_an_adopted_box` was called
//! `..._and_the_repairs_still_run`, and it asserted no such thing: what it calls is
//! `rows_from_files(dir)` — a function that resolves the FILES and never opens the store. The
//! repairs are VERBS, and a verb in this binary is reached through `vike_cli::run`, which calls
//! `resolve_policy` before `dispatch` for every subcommand. So the library function returned `Ok`
//! while every real binary on the box exited 1, and the test read green. It has been renamed to
//! what it does measure, and the claim it used to make lives HERE, where a process is started.
//! Measured at `650907a37`:
//! `config show`, `config mirror`, `config adopt --undo`, `config check`, `secrets list`,
//! `config compare` and even `--help` all exited 1, and `sqlite3` is installed on neither
//! deployment box — so the store could not be repaired out of band either.
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

    /// Mirror the files into rows and SEAL the box, exactly as an operator does. Both through the
    /// binary, so the fixture is a state the shipped tool can actually produce — a hand-built store
    /// would prove nothing about a box anybody has.
    fn adopt(&self) {
        // ⚠ The database has to exist FIRST, and `config mirror` deliberately will not create it:
        // the mere existence of that file is what makes it — rather than `secrets.env` — answer
        // for every CREDENTIAL on the box, so a settings command that created one would take every
        // venue to paper. `secrets migrate` is the verb that owns that decision, and it is the
        // route a real operator takes, which is the only reason this fixture is a real state.
        //
        // ⚠ ONE obviously-fake DEMO key, and it is load-bearing rather than incidental: `migrate`
        // refuses to create a database when there is nothing to move ("nothing to migrate, so NO
        // database was created — that is the correct outcome for a box with no credentials yet"),
        // so an empty file leaves no store for `config mirror` to write into. The value is not a
        // credential in any sense — it never leaves this temp directory, and a DEMO key authorises
        // nothing anywhere even if it were real.
        self.write("secrets.env", "BINANCE_DEMO_API_KEY=fixture-not-a-real-key\n");
        let g = self.run(&["secrets", "migrate"]);
        assert!(g.status.success(), "the fixture's migrate must succeed: {}", both(&g));
        let m = self.run(&["config", "mirror"]);
        assert!(m.status.success(), "the fixture's mirror must succeed: {}", both(&m));
        let a = self.run(&["config", "adopt"]);
        assert!(a.status.success(), "the fixture's adopt must succeed: {}", both(&a));
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
    let mut rows = src.rows().expect("the fixture must have mirrored rows").clone();
    let row = rows.settings.first_mut().expect("the fixture must have at least one setting row");
    row.value = r"'C:\vike\state'".to_string();
    vike_secrets::write_settings_in(&dir, &rows).expect("planting the row must succeed");
}

// ---------------------------------------------------------------------------------------------
// Half one — the repairs RUN
// ---------------------------------------------------------------------------------------------

/// **The JSON incident's regression test.** Every verb an operator is TOLD to reach for must still
/// work in the refusing state, because there is no other way back: `sqlite3` is on neither box.
#[test]
fn a_broken_seal_still_runs_every_repair_and_disclosure_verb() {
    let c = Case::new("repairs");
    c.write("policy.toml", "max_notional_per_order = 250\n");
    c.adopt();
    plant_unreadable_row(&c);

    // Each of these is named BY a refusal message somewhere in this tree, or is the only way to
    // see what is wrong. A failure here is the incident, not a test nit.
    for verb in [
        &["config", "check"][..],
        &["config", "show"][..],
        &["config", "compare"][..],
        &["config", "mirror", "--dry-run"][..],
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
        // ⚠ **Two of these exit NON-ZERO by design, so the exit code is not the property under
        // test here — RUNNING is.** `config check` REPORTS the fault (that is the whole point of
        // it), and `config compare` returns *identical* as its exit code, so a box whose two
        // sources disagree — which is exactly this fixture — exits 1 having done its job perfectly.
        // Asserting success on them would be asserting the tool is broken.
        //
        // What every row here shares, and what actually distinguishes a working box from the
        // bricked one measured at `650907a37`, is that the verb PRODUCED ITS OWN OUTPUT rather than
        // the loader's refusal. So that is what is asserted for all of them, and the exit code only
        // where it means what it looks like it means.
        let reports_a_verdict = verb == ["config", "check"] || verb == ["config", "compare"];
        if reports_a_verdict {
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

    // ...and the repair actually REPAIRS: re-deriving from the files clears the planted row.
    let fixed = c.run(&["config", "mirror"]);
    assert!(fixed.status.success(), "the repair must run: {}", both(&fixed));
    let after = c.run(&["config", "check"]);
    assert!(
        after.status.success(),
        "`config mirror` must leave a box that passes `config check` — a repair that runs and does \
         not repair is the shape `write.rs` shipped: {}",
        both(&after)
    );
}

// ---------------------------------------------------------------------------------------------
// Half two — the actors REFUSE
// ---------------------------------------------------------------------------------------------

/// The complement. Without this the first test is satisfied by deleting the refusal outright.
#[test]
fn a_broken_seal_refuses_the_verbs_that_act_on_a_ceiling() {
    let c = Case::new("actors");
    c.write("policy.toml", "max_notional_per_order = 250\n");
    c.adopt();
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
    }
}

// ---------------------------------------------------------------------------------------------
// The write paths that used to take the ceiling off quietly
// ---------------------------------------------------------------------------------------------

/// **`config mirror` on an adopted box whose file lost the ceiling.** Measured at `650907a37`:
/// `max_notional_per_order` went from `250.0` to unset, the command printed *"The files still win
/// — nothing this box resolves has changed"*, and it exited 0.
#[test]
fn mirroring_a_shrunken_file_onto_an_adopted_box_is_refused_by_name() {
    let c = Case::new("shrink");
    c.write("policy.toml", "max_notional_per_order = 250\nmax_leverage = 3\n");
    c.adopt();

    // The operator deletes a line, having been told by the drift banner that the file is inert.
    c.write("policy.toml", "max_leverage = 3\n");

    let o = c.run(&["config", "mirror"]);
    assert!(
        !o.status.success(),
        "re-deriving FEWER rows onto an adopted box takes the ceiling off and must be refused: {}",
        both(&o)
    );
    let text = both(&o);
    assert!(text.contains("NO CEILING"), "the refusal must name the consequence: {text}");
    assert!(
        text.contains("config adopt --undo"),
        "...and the verb that MEANS it, which still runs: {text}"
    );

    // And the refusal is not merely a message: the row is still there.
    let o = c.run(&["config", "show"]);
    assert!(both(&o).contains("250"), "the ceiling must survive the refused mirror: {}", both(&o));
}

/// **An ADOPTED box with no rows at all** — the state `config adopt` refuses to CREATE, which
/// `config check` reported as `OK` at `650907a37` because its store finding renders
/// `SettingsSource`'s `Display` and had no arm for it.
#[test]
fn config_check_fails_an_adopted_box_that_carries_no_rows() {
    let c = Case::new("emptyseal");
    c.write("policy.toml", "max_notional_per_order = 250\n");
    c.adopt();

    let dir = c.settings();
    let empty = vike_secrets::StoredSettings::default();
    vike_secrets::write_settings_in(&dir, &empty).expect("emptying the tables must succeed");

    let o = c.run(&["config", "check"]);
    assert!(
        !o.status.success(),
        "an adopted box resolving from NOTHING has no ceiling and must not read as healthy: {}",
        both(&o)
    );
    assert!(
        both(&o).contains("no ceiling"),
        "the finding must name what is actually lost: {}",
        both(&o)
    );
}
