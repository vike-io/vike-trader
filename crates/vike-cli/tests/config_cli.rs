//! End-to-end tests for `vike-cli config show`, driving the SHIPPED binary
//! (`CARGO_BIN_EXE_vike-cli`) against a real settings directory in a temp dir.
//!
//! The unit tests beside the module cover the resolver and the argument grammar. These cover the
//! thing an operator actually does — point the binary at a settings directory and ask *did my
//! `policy.toml` get read, and what did it resolve to?* — which nothing answered before, and which
//! no unit test can answer because the answer involves the DISPATCHER resolving the directory, the
//! loader reading the file, and the printer disclosing both.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory. Without it a run on
//! a developer box would resolve the REPO's settings directory and print a real credential store's
//! key count into a test's assertions. The redirect is isolation AND a safety property — and it is
//! passed through `Command::env` on the child, never `std::env::set_var`, which is unsafe under
//! threads and would leak across this binary's parallel cases.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// A throwaway PROJECT and its `settings/` child.
///
/// ⚠ **Both halves of this fixture were repaired together, because they are one defect wearing two
/// faces.**
///
/// 1. The root is a BOUND `tempfile::TempDir`, not `<system-temp>/vike-cli-config-<tag>-<pid>`. A
///    fixed-tag-plus-pid directory in the shared system temp root is unique enough on one box and
///    not owned by anybody: `Drop` cleaned it up on the ordinary path, but a SIGKILL, an OOM or a
///    Ctrl-C — all of which the CI box has seen — leaves it behind forever, `/tmp` being 1777 sticky.
///    The RECEIVING end is what flips a verdict: given any foreign-owned leftover of the same name,
///    the old `remove_dir_all` failed `EACCES` and `let _ =` swallowed it, `create_dir_all`
///    returned **Ok** because the directory already existed, and the first `write` then panicked
///    `PermissionDenied` naming a `/tmp` path and no property of any test.
/// 2. `$VIKE_SETTINGS_DIR` names the `settings/` CHILD. The resolver returns that value verbatim
///    and `crates/vike-cli/src/lib.rs`'s `resolve_policy` takes its PARENT as `<project>`, so the
///    old fixture made the child's project the shared system temp ROOT — putting
///    `<project>/user_data`, `<project>/bin`, `<project>/tmp` and `vike_config::load`'s layer-0
///    probe of `<settings-dir>/../vike.toml` in a directory every user on the box shares. That last
///    one is armed by a single `/tmp/vike.toml` dropped by anything, and it fails EVERY success
///    case in this file at once.
///
/// `crates/vike-cli/tests/backtest_cli.rs`'s `project_with_settings` is the same repair.
struct Case {
    /// The project — BOUND, so its `Drop` removes the tree even on the panic path.
    root: tempfile::TempDir,
    /// `<project>/settings` — what `$VIKE_SETTINGS_DIR` names and what `config show` reports.
    dir: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        // The tag survives as the directory's PREFIX: it is what makes a leftover (from a kill
        // signal, which `Drop` cannot answer) attributable to a case rather than anonymous.
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-config-{tag}-"))
            .tempdir()
            .expect("tempdir");
        let dir = root.path().join("settings");
        std::fs::create_dir_all(&dir).unwrap();
        Case { root, dir }
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    /// Run `vike-cli config show …` with the case's directory as THE settings directory and an
    /// otherwise EMPTY environment — no ambient log level or operator flag from the developer's
    /// shell can move a row, which would otherwise make these assertions machine-dependent (an
    /// exported `RUST_LOG` alone changes `preferences.log_level`'s origin and the "configured"
    /// count). The command needs nothing else inherited: it opens one directory and prints.
    ///
    /// Cleared through `Command::env_clear` on the CHILD, never `std::env::set_var`, which is
    /// unsafe under threads and would leak across this binary's parallel cases.
    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("config").arg("show").args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", &self.dir);
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// **The POSITIVE proof of both halves of [`Case`]**, because a green run of the cases below proves
/// neither: they pass with the old fixture too, on every box where nobody has yet left a leftover
/// of the same name or dropped a `/tmp/vike.toml`.
///
/// The project the binary infers is asserted BEHAVIOURALLY — `vike_config::load`'s layer 0 stats
/// `<settings-dir>/../vike.toml` and refuses hard on anything but `NotFound`, so planting that file
/// inside THIS case's own root must be what the shipped binary refuses, naming the planted path.
/// With the old one-level fixture the plant would have landed in the shared system temp root, where
/// it would fail this whole suite for every user on the box instead.
///
/// The ownership half is asserted by dropping the handle: the tree is gone afterwards, which is the
/// property a `Drop` impl calling `remove_dir_all` also had on the PASSING path and neither had on
/// the killed one.
#[test]
fn the_case_fixture_is_owned_and_its_project_is_its_own_tempdir() {
    let root_path;
    {
        let c = Case::new("hermetic");
        root_path = c.root.path().to_path_buf();
        assert_eq!(
            c.dir.parent(),
            Some(root_path.as_path()),
            "the project the binary infers must be this case's own TempDir"
        );

        // Unplanted: the binary runs normally.
        assert!(c.run(&["--section", "files"]).status.success());

        // Planted inside this case's own root: the binary must refuse, naming the planted file.
        let planted = root_path.join("vike.toml");
        std::fs::write(&planted, "").expect("plant the refused file");
        let o = c.run(&["--section", "files"]);
        assert!(!o.status.success(), "a `vike.toml` beside the settings directory is refused");
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(
            err.contains(&planted.display().to_string()),
            "the refusal must name the file this case planted — that is what proves the probe \
             resolved inside this case's own TempDir: {err}"
        );
    }
    assert!(
        !root_path.exists(),
        "the fixture must remove itself when it goes out of scope: {} survived",
        root_path.display()
    );
}

/// **The headline case.** An armed ceiling in `policy.toml` is visible: the directory is named, the
/// file is reported present, and the row says the value and the file it came from.
///
/// Until this half existed the output contained no TOML row at all — `max_notional_per_order` was
/// unprintable anywhere, and it is the ONE setting with no environment override by design, so a
/// file being read was its only possible proof of effect.
#[test]
fn an_armed_policy_ceiling_is_visible_with_the_file_it_came_from() {
    let c = Case::new("policy");
    c.write("policy.toml", "max_notional_per_order = 250\n");

    let o = c.run(&["--section", "files"]);
    assert!(o.status.success(), "{o:?}");
    let out = stdout(&o);

    assert!(out.contains(&format!("settings directory: {}", c.dir.display())), "{out}");
    assert!(out.contains("policy.toml") && out.contains("present, 1 key(s) set"), "{out}");
    assert!(out.contains("config.toml") && out.contains("absent"), "{out}");

    let row = out
        .lines()
        .find(|l| l.starts_with("policy.max_notional_per_order"))
        .unwrap_or_else(|| panic!("no ceiling row in:\n{out}"));
    assert!(row.contains("250.0"), "the EFFECTIVE f64, not the file text: {row}");
    assert!(row.contains("policy.toml"), "the row must name the FILE it came from: {row}");
}

/// The same command with NO file: the ceiling row still exists and honestly reports `default` with
/// no value. "The file is missing" and "the file says nothing" are now distinguishable, which is the
/// whole point — a silently uncapped node used to look exactly like a configured one.
#[test]
fn an_unconfigured_ceiling_reports_default_rather_than_going_missing() {
    let c = Case::new("nofile");
    let out = stdout(&c.run(&["--section", "files"]));
    let row = out
        .lines()
        .find(|l| l.starts_with("policy.max_notional_per_order"))
        .unwrap_or_else(|| panic!("no ceiling row in:\n{out}"));
    assert!(row.contains("default"), "{row}");
    assert!(out.contains("policy.toml") && out.contains("absent"), "{out}");
}

/// A key set to the SAME number as the code default still reports the file. A value-diff would say
/// `default` here and tell the operator their file did nothing.
#[test]
fn a_file_that_sets_the_default_value_still_reports_the_file() {
    let c = Case::new("samevalue");
    // `trace` IS `Preferences::log_file_level`'s compiled-in default. (This case used to use
    // `rate_utilization = 0.4`, likewise its own default — that preference was removed because
    // NOTHING read it.) The POLICY twin lives in `crates/vike-config/tests/provenance.rs`, and has
    // to: naming a `Consumed::No` ceiling key outside that crate trips `policy_is_consumed.rs`'s
    // own gate.
    c.write("preferences.toml", "log_file_level = \"trace\"\n");
    let out = stdout(&c.run(&["--section", "files", "--filter", "log_file_level"]));
    let row = out
        .lines()
        .find(|l| l.starts_with("preferences.log_file_level"))
        .unwrap_or_else(|| panic!("no row in:\n{out}"));
    assert!(row.contains("preferences.toml"), "{row}");
    assert!(row.contains("trace"), "{row}");
}

/// `--changed-only` is the "what have I actually set?" view, and it works across both halves.
#[test]
fn changed_only_shows_exactly_what_was_configured() {
    let c = Case::new("changed");
    c.write("policy.toml", "max_notional_per_order = 250\n");
    c.write("flags.toml", "poly_exec = true\n");

    let out = stdout(&c.run(&["--section", "files", "--changed-only"]));
    assert!(out.contains("policy.max_notional_per_order"), "{out}");
    assert!(out.contains("flags.poly_exec"), "{out}");
    assert!(!out.contains("flags.reconcile"), "an untouched key must not appear: {out}");
    assert!(out.contains("2 setting(s) shown, 2 configured"), "{out}");
}

/// ⚠ `a_clamped_preference_is_marked_and_its_warning_printed` stood here, driving the model's only
/// policy-binds-preference clamp. Both of that clamp's fields were removed as a ceiling bounding a
/// value nothing read, so what this command must do with them now is REFUSE them — end to end,
/// through the real binary, which is where an operator meets the change.
#[test]
fn the_removed_rate_keys_fail_the_command_naming_the_file_and_the_key() {
    let c = Case::new("removedrate");
    c.write("policy.toml", "[rate]\nmax_utilization = 0.5\n");

    let out = c.run(&["--section", "files"]);
    assert!(!out.status.success(), "a removed ceiling must not print a happy table");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("policy.toml"), "{err}");
    assert!(err.contains("rate.max_utilization"), "{err}");
    assert!(err.contains("no longer a policy key"), "{err}");

    let c = Case::new("removedpref");
    c.write("preferences.toml", "rate_utilization = 0.9\n");
    let out = c.run(&["--section", "files"]);
    assert!(!out.status.success(), "a removed preference must not print a happy table");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("preferences.toml"), "{err}");
    assert!(err.contains("rate_utilization"), "{err}");
    assert!(err.contains("NOTHING read it"), "{err}");
}

/// A broken settings file FAILS the command, naming the file — a `config show` that quietly reported
/// defaults over an unparseable `policy.toml` would be the most misleading output it could produce.
#[test]
fn a_broken_settings_file_fails_loudly() {
    let c = Case::new("broken");
    c.write("policy.toml", "max_leverage = \"not a number\"\n");
    let o = c.run(&["--section", "files"]);
    assert!(!o.status.success(), "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("policy.toml"), "{err}");
}

/// …and it fails WITHOUT echoing the line it rejected.
///
/// `secrets.env` sits in this very directory, so `api_key = "…"` in `config.toml` is a plausible
/// first-time mistake. `deny_unknown_fields` refuses the key correctly; the refusal used to render
/// `toml`'s annotated snippet, which is the file's own line, VALUE INCLUDED — on the stderr of
/// every `vike-cli` invocation, in the exact path whose output gets pasted into a bug report.
///
/// The end-to-end twin of `vike_config::error`'s own unit test: that one pins the RENDERER, this one
/// pins the SURFACE, because the value has to survive being formatted into a `ConfigError`, printed
/// by the dispatcher and written to a pipe before anyone would ever see it.
#[test]
fn a_rejected_unknown_key_never_echoes_its_value() {
    const CANARY: &str = "DUMMY-TOML-APIKEY-SHOULD-NEVER-PRINT";
    let c = Case::new("nokeyecho");
    c.write("config.toml", &format!("store_root = \"/tmp/s\"\napi_key = \"{CANARY}\"\n"));

    let o = c.run(&["--section", "files"]);
    assert!(!o.status.success(), "an unknown key must still be refused: {o:?}");
    let all = format!("{}{}", stdout(&o), String::from_utf8_lossy(&o.stderr));
    assert!(!all.contains(CANARY), "the rejected line's VALUE leaked: {all}");
    // The diagnosis must survive the redaction, or the fix trades one bad outcome for another.
    assert!(all.contains("config.toml"), "{all}");
    assert!(all.contains("api_key"), "the offending KEY must still be named: {all}");
    assert!(all.contains("line 2"), "the location must still be named: {all}");
}

/// `--json` is one OBJECT carrying both halves plus the header, and the settings directory it names
/// is the one the dispatcher resolved.
#[test]
fn the_json_view_is_one_object_with_both_halves() {
    let c = Case::new("json");
    c.write("policy.toml", "max_notional_per_order = 250\n");

    let o = c.run(&["--json"]);
    assert!(o.status.success(), "{o:?}");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");

    assert_eq!(doc["settings_dir"].as_str(), Some(c.dir.display().to_string().as_str()));
    // FOUR: the `settings/*.toml` the loader consults, and nothing else. A file the loader consults
    // must be disclosed here, and a file it does NOT must not appear — a row is what makes an
    // operator treat a file as part of the answer.
    assert_eq!(doc["files"].as_array().unwrap().len(), 4);
    assert!(!doc["settings"].as_array().unwrap().is_empty());
    assert!(!doc["env"].as_array().unwrap().is_empty());

    let row = doc["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "policy.max_notional_per_order")
        .expect("the ceiling row");
    assert_eq!(row["value"], "250.0", "the EFFECTIVE f64, not the file text");
    assert_eq!(row["origin"], "file");
    assert_eq!(row["origin_detail"], "policy.toml");
}

/// A setting nothing reads is reported as UNREAD — and, when the operator actually configured it,
/// called out under the table with what reads the variable instead.
///
/// This is the defect the `READ` column exists for: attributing a value to a file and printing its
/// origin ASSERTS the value is in force, and doing that for a key nothing reads is positive
/// confirmation of something false. Both halves are asserted together deliberately — a `NO` column
/// with no explanation would leave the operator knowing their file does nothing and not knowing what
/// does.
#[test]
fn an_unread_setting_is_reported_as_unread_not_confirmed() {
    let c = Case::new("unread");
    // `poly_heartbeat` is read inside the Polymarket bridge, from the environment; a file cannot
    // set it. `tradehub_control` is the wired twin, and is what makes this test prove a DISTINCTION
    // rather than just "the column exists".
    //
    // ⚠ It was `poly_exec` until the unread-settings sweep WIRED that flag — at which point this
    // test asserted `NO` about a key that had just started saying `tradehub`, which is the test
    // doing its job rather than a spelling to patch. `poly_heartbeat` is one of the seven keys the
    // owner deliberately DEFERRED (its consumer exists and no running binary mounts it), so it is
    // the honest unread example today.
    c.write("flags.toml", "poly_heartbeat = true\ntradehub_control = true\n");

    let out = stdout(&c.run(&["--section", "files"]));
    let unread = out
        .lines()
        .find(|l| l.starts_with("flags.poly_heartbeat "))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(unread.trim_end().ends_with("NO"), "a setting nothing reads must say NO: {unread}");
    let wired = out
        .lines()
        .find(|l| l.starts_with("flags.tradehub_control "))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        wired.trim_end().ends_with("tradehub"),
        "a WIRED setting must name the BINARY that reads it, not merely `yes`: {wired}"
    );

    // …and the configured-but-unread one is called out, with its reason.
    assert!(out.contains("CONFIGURED and read by NOTHING"), "{out}");
    assert!(out.contains("flags.poly_heartbeat = true"), "{out}");
    assert!(out.contains("heartbeat_enabled"), "the reason must name the real reader: {out}");
    // The wired one must NOT appear in that warning block.
    let warned = out.split("CONFIGURED and read by NOTHING").nth(1).unwrap_or("");
    assert!(!warned.contains("flags.tradehub_control"), "{out}");
}

/// **The VERDICT line, which is the operator-facing half of the unread-reader fix and had no test.**
///
/// The `why` paragraph alone was the defect: for six flags it names a library that reads the
/// environment variable without saying that nothing calls that library, so it reads as *export the
/// variable instead* while exporting it does nothing. `vike_config::Reader::verdict` is the
/// one-line answer, printed ABOVE the paragraph because an operator may stop after one line.
///
/// Three things are asserted, and the ORDER is one of them — a verdict printed under the paragraph
/// would be read by nobody who had already acted on it.
#[test]
fn an_unread_key_gets_the_env_verdict_above_its_reason() {
    let c = Case::new("verdict");
    // `poly_heartbeat` is `Reader::Uncalled`: nothing spawns the poller that reads
    // `POLY_HEARTBEAT`, so NEITHER spelling does anything.
    //
    // ⚠ `sweep_threads` was configured here as the COUNTEREXAMPLE — the table's one
    // `Reader::Live`, where exporting the variable genuinely worked — so that this test proved a
    // DISTINCTION rather than the presence of a string. That key is now WIRED, no row is `Live`
    // any more, and the distinction has no example left to make at THIS level. It is still made,
    // one level down, by `vike_config::consumed`'s
    // `the_three_reader_states_tell_an_operator_three_different_things`, which constructs one
    // `Reader` of each variant and asserts the three verdicts are distinct strings — a check that
    // cannot go vacuous when the table changes. It is still written here so the SECOND key stays
    // present in the render (a `Consumer::At` key must not acquire an unread verdict).
    c.write("flags.toml", "poly_heartbeat = true\n");
    c.write("preferences.toml", "sweep_threads = 2\n");

    let out = stdout(&c.run(&["--section", "files"]));
    let warned =
        out.split("CONFIGURED and read by NOTHING").nth(1).unwrap_or_else(|| panic!("{out}"));

    assert!(
        warned.contains("NEITHER SPELLING DOES ANYTHING"),
        "an operator who configured a dead flag must be told the VARIABLE is dead too — that \
         sentence is the whole fix: {out}"
    );
    // …and the WIRED key must not appear in that block at all: a `Consumer::At` row has no unread
    // verdict to render, so `sweep_threads` showing up here would mean the wiring was undone.
    assert!(
        !warned.contains("sweep_threads"),
        "a WIRED key must carry no unread verdict — `preferences.sweep_threads` is \
         `Consumer::At` now: {out}"
    );

    // THE ORDER. Both rows print `<verdict>` then `<why>`; the heartbeat row's reason names
    // `heartbeat_enabled`, so the verdict must come first in the text.
    let dead_verdict = warned.find("NEITHER SPELLING DOES ANYTHING").expect("verdict");
    let dead_reason = warned.find("heartbeat_enabled").expect("reason");
    assert!(
        dead_verdict < dead_reason,
        "the verdict must lead: a paragraph an operator may not finish is exactly what was \
         misleading them:\n{out}"
    );
}

/// …and the ENVIRONMENT half of the SAME invocation must not contradict it.
///
/// `config show` prints a variable table headed *"READS = what the reader consults"*. `READS` says
/// WHERE the read is and nothing about whether anything runs it, so the six dead flags' variables
/// sat in that table looking exactly like live knobs — in the same command whose file section had
/// just said neither spelling does anything. Derived from `vike_config::env_verdict`, so this can
/// only go stale by a `CONSUMPTION` row changing, which is gated.
#[test]
fn the_env_table_names_the_variables_that_do_nothing() {
    let c = Case::new("envverdict");
    let out = stdout(&c.run(&["--section", "env"]));

    assert!(
        out.contains("EXPORTING THEM CHANGES NOTHING"),
        "the env half must carry the same verdict as the file half: {out}"
    );
    let block =
        out.split("EXPORTING THEM CHANGES NOTHING").nth(1).unwrap_or_else(|| panic!("{out}"));
    assert!(block.contains("POLY_HEARTBEAT"), "a dead variable must be NAMED: {out}");
    // ⚠ The reason this assertion holds CHANGED and the assertion did not, which is worth saying
    // rather than leaving to be inferred. It used to hold because `VIKE_SWEEP_THREADS` was the
    // table's one `Reader::Live` variable: exporting it genuinely worked, so listing it would have
    // made this block the new false claim. It now holds because the key is WIRED — a
    // `Consumer::At` row contributes no verdict at all. Either way, naming this variable here
    // would be telling an operator that a lever they have is a lever they do not.
    assert!(
        !block.contains("VIKE_SWEEP_THREADS"),
        "exporting this variable genuinely caps sweep concurrency; listing it as dead would make \
         this block the new false claim: {out}"
    );
}

/// …and the machine view carries it too, so an agent reading `--json` is told the same thing.
#[test]
fn the_json_env_rows_carry_the_reader_verdict() {
    let c = Case::new("envverdictjson");
    let o = c.run(&["--json", "--section", "env"]);
    assert!(o.status.success(), "{o:?}");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
    let rows = doc["env"].as_array().expect("env rows");
    let verdict = |name: &str| -> serde_json::Value {
        rows.iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("no env row for {name}"))["reader_verdict"]
            .clone()
    };

    assert!(
        verdict("POLY_HEARTBEAT").as_str().is_some_and(|v| v.contains("NEITHER SPELLING")),
        "{:?}",
        verdict("POLY_HEARTBEAT")
    );
    // A variable with nothing gated to say about it stays null rather than gaining a sentence.
    assert_eq!(verdict("VIKE_SWEEP_THREADS"), serde_json::Value::Null);
}

/// …and the machine view carries the same fact, so a tool does not have to parse the table.
#[test]
fn the_json_view_carries_the_consumed_flag() {
    let c = Case::new("unreadjson");
    let o = c.run(&["--json", "--section", "files"]);
    assert!(o.status.success(), "{o:?}");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
    let rows = doc["settings"].as_array().unwrap();

    // ⚠ `poly_heartbeat`, not `poly_exec` — see the table test above for why the example moved.
    let unread =
        rows.iter().find(|r| r["key"] == "flags.poly_heartbeat").expect("the poly_heartbeat row");
    assert_eq!(unread["consumed"], serde_json::Value::Bool(false));
    assert!(unread["why_unread"].as_str().is_some_and(|w| w.contains("heartbeat_enabled")));

    let wired =
        rows.iter().find(|r| r["key"] == "config.tradehub_addr").expect("the tradehub_addr row");
    assert_eq!(wired["consumed"], serde_json::Value::Bool(true));
    assert_eq!(wired["why_unread"], serde_json::Value::Null);

    // A `policy.*` key is NOT reported as unread — it has its own gate, and warning about an
    // enforced ceiling would be the same false signal in the other direction.
    let ceiling =
        rows.iter().find(|r| r["key"] == "policy.max_notional_per_order").expect("the ceiling row");
    assert_eq!(ceiling["consumed"], serde_json::Value::Bool(true));
}

/// **`READ` names the BINARY, and a headless box can tell that three of these are the GUI's.**
///
/// The reported defect: `config.state_dir`, `config.store_root` and `preferences.chart_style` all
/// said `READ: yes` on a tradehub or recorder host, whose only reader is `vike-app`. Setting
/// `config.state_dir` there does nothing — correct behaviour, reported as if it had worked.
///
/// Asserted through the SHIPPED binary and against BOTH scopes: a column that named one binary for
/// everything would pass an app-only check, and the distinction is the entire content of the fix.
#[test]
fn the_read_column_names_which_binary_reads_each_setting() {
    let c = Case::new("readscope");
    let out = stdout(&c.run(&["--section", "files"]));

    let cell = |key: &str| -> String {
        let line = out
            .lines()
            .find(|l| l.starts_with(&format!("{key} ")))
            .unwrap_or_else(|| panic!("no row for {key}\n{out}"));
        line.split_whitespace().next_back().unwrap_or("").to_string()
    };

    // ⚠ `config.state_dir` was the third key here until 2026-09-09, when it stopped being READ AT
    // ALL (the desktop cut deleted `state_dir_path`, its one reader) and became a written
    // admission. It is not a key at all now: the unread-settings sweep DELETED it, and both
    // spellings refuse — `vike_config::REMOVED_ENV` for `VIKE_STATE_DIR`,
    // `vike_config::config::ConfigPatch::state_dir` for the file key. There is nothing left for
    // this command to print about it, which is the end state its `CONSUMPTION` row asked for.
    for key in ["config.store_root", "preferences.chart_style"] {
        assert_eq!(
            cell(key),
            "desktop",
            "{key}'s only reader is the GUI — a headless operator must be able to see that\n{out}"
        );
    }
    for key in ["config.log_dir", "config.tradehub_addr", "flags.tradehub_control"] {
        assert_eq!(cell(key), "tradehub", "{key} is the daemon's\n{out}");
    }
    // ⚠ The unread example is `flags.poly_heartbeat`, NOT `flags.poly_exec`: the unread-settings
    // sweep wired `poly_exec` into the daemon's mount, so it names `tradehub` now and would have
    // made this assertion prove the opposite of what it says. The heartbeat flag's read is still
    // the venue adapter's own (`crates/bridges/polymarket/src/heartbeat.rs`), and its row is one of
    // the seven the sweep's owner ruling deliberately DEFERRED.
    assert_eq!(cell("flags.poly_heartbeat"), "NO", "an unread row still says NO\n{out}");

    // The legend, so the column is readable without this test.
    assert!(out.contains("READ names the BINARY"), "the column needs a legend: {out}");
    // ⚠ `desktop`, not `app`: the GUI binary was renamed and this assertion would otherwise pass
    // VACUOUSLY — "app" is a substring of enough incidental output that it stops proving the legend
    // names a real binary, which is the one thing this line is for.
    assert!(out.contains("desktop") && out.contains("tradehub"), "{out}");
    assert!(
        out.contains("does nothing on a headless box"),
        "the legend must say what a mismatch COSTS, which is the whole point: {out}"
    );
}

/// …and the machine view carries the same distinction, so an agent need not parse the table.
#[test]
fn the_json_view_carries_which_binary_reads_each_setting() {
    let c = Case::new("readscopejson");
    let o = c.run(&["--json", "--section", "files"]);
    assert!(o.status.success(), "{o:?}");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
    let rows = doc["settings"].as_array().unwrap();
    let read_by = |key: &str| -> serde_json::Value {
        rows.iter().find(|r| r["key"] == key).unwrap_or_else(|| panic!("no row for {key}"))
            ["read_by"]
            .clone()
    };

    assert_eq!(read_by("config.store_root"), serde_json::json!("desktop"));
    assert_eq!(read_by("config.tradehub_addr"), serde_json::json!("tradehub"));
    // ⚠ `config.state_dir` used to be the GUI example here, then read NULL once the desktop cut
    // deleted its one reader, and is now not a key at all — the unread-settings sweep deleted it
    // and refuses both spellings. `rows` carries no entry for it, so asserting on it would panic in
    // `read_by`'s `unwrap_or_else` rather than prove anything.
    //
    // An unread row has no binary to name — `consumed` is what distinguishes this `null` from the
    // library case, and both are honest answers rather than "unknown". `poly_heartbeat` rather than
    // `poly_exec`: the same sweep WIRED `poly_exec`, so it names a binary now.
    assert_eq!(read_by("flags.poly_heartbeat"), serde_json::Value::Null);
}

/// The credential store is disclosed by COUNT, never by a key name and never by a value — this
/// command is meant to be pasted into an issue.
#[test]
fn the_credential_store_is_disclosed_by_count_only() {
    let c = Case::new("secrets");
    c.write("secrets.env", "BINANCE_LIVE_API_KEY=key-abcd1234\nOKX_DEMO_API_PASSPHRASE=s3cr3t\n");

    let out = stdout(&c.run(&["--section", "files"]));
    assert!(out.contains("secrets.env") && out.contains("present, 2 key(s)"), "{out}");
    assert!(!out.contains("key-abcd1234"), "a credential VALUE leaked: {out}");
    assert!(!out.contains("s3cr3t"), "a credential VALUE leaked: {out}");
    assert!(!out.contains("BINANCE_LIVE_API_KEY"), "a credential NAME leaked into the header");
}

/// ...and in the env half, a store-held credential prints as `<set>` with its true source — never
/// the value, in either view.
#[test]
fn a_credential_value_never_reaches_either_view() {
    let c = Case::new("redact");
    c.write("secrets.env", "BINANCE_LIVE_API_KEY=key-abcd1234\n");

    let human = stdout(&c.run(&["--section", "env", "--filter", "BINANCE_LIVE_API_KEY"]));
    assert!(human.contains("<set>"), "{human}");
    assert!(!human.contains("key-abcd1234"), "{human}");

    let json = stdout(&c.run(&["--json", "--filter", "BINANCE_LIVE_API_KEY"]));
    assert!(!json.contains("key-abcd1234"), "{json}");
    assert!(json.contains("\"secret\": true"), "{json}");
}

/// The `--section` filter is what keeps ~300 env rows from burying the files half.
#[test]
fn the_section_flag_picks_one_half() {
    let c = Case::new("section");
    let files = stdout(&c.run(&["--section", "files"]));
    assert!(files.contains("-- settings files"), "{files}");
    assert!(!files.contains("-- environment variables"), "{files}");

    let envs = stdout(&c.run(&["--section", "env"]));
    assert!(envs.contains("-- environment variables"), "{envs}");
    assert!(!envs.contains("-- settings files"), "{envs}");

    let all = stdout(&c.run(&[]));
    assert!(all.contains("-- settings files") && all.contains("-- environment variables"), "{all}");
}

/// The env half reports what each reader CONSULTS, and names the rows a store value may not reach —
/// the provenance fix. The motivating case was `VIKE_TRADEHUB_CONTROL_KEY`: it sits in the store,
/// and `vike-cli`'s own `trade`/`mcp` read it with `std::env::var`.
///
/// ⚠ The variable is picked FROM THE REGISTRY rather than named here, deliberately. A reader can
/// legitimately move to the credential map (there is work in flight doing exactly that to the node
/// keys), at which point its `SETTINGS` row becomes `Naming::MapLookup` — forced, because
/// `declared_layer_matches_the_path` fails on the stale `Layer::Library` and `LIBRARY_PIN` fails on
/// the vanished row — and the warning correctly stops naming it. Hard-coding the name would turn
/// THIS test red inside THAT change, for a reason that has nothing to do with it. What is being
/// asserted is the mechanism: whatever the registry says reads with `env::var`, a store-only value
/// for it is named.
#[test]
fn a_store_only_value_read_with_env_var_is_named_in_a_warning() {
    // The same rule `cmd::config`'s `Reads::of` applies, restated here because an integration test
    // sees the crate's public surface only. A registry with no direct-env reader at all would make
    // the warning unreachable — worth failing on, not skipping past.
    let row = vike_ops::settings::SETTINGS
        .iter()
        .find(|s| {
            matches!(
                s.naming,
                vike_ops::settings::Naming::Literal | vike_ops::settings::Naming::Konst(_)
            )
        })
        .expect("the registry must contain at least one direct `env::var` reader");

    let c = Case::new("reads");
    c.write("secrets.env", &format!("{}=store-value-not-exported\n", row.name));

    let out = stdout(&c.run(&["--section", "env", "--filter", row.name]));
    assert!(out.contains("READS"), "the column must exist: {out}");
    assert!(out.contains("dotenv"), "the store still won under the store precedence: {out}");
    assert!(out.contains("direct `env::var`"), "the warning block: {out}");
    assert!(out.contains(&format!("{} ({})", row.name, row.krate)), "the named row: {out}");
    // NOTE deliberately no leak assertion here: the row the registry hands back is whichever comes
    // first, and most direct-env readers are ORDINARY knobs whose value SHOULD print. Redaction is
    // a property of credential-shaped NAMES and is asserted on one, in
    // `a_credential_value_never_reaches_either_view`.
}

/// A key the operator PUT IN THE STORE that matches no registry row used to appear NOWHERE — both
/// halves of this command are catalog-driven, so an unrecognised key is not an `<unset>` row, it is
/// no row at all. A mis-spelled variable name was therefore indistinguishable from one never set,
/// which is the same silent-by-construction failure the command exists to remove (a clean-install
/// validation found `VIKE_NODE_CONTROL_KEY`, a plausible mis-spelling of
/// `VIKE_TRADEHUB_CONTROL_KEY`, sitting in a store and reported by nothing).
///
/// The credential-shaped half below now names that REAL mis-spelling. It could not when this
/// landed: the settings-registry gate's literal sweep harvested every prefixed env-shaped string
/// literal under `crates/` and demanded a `SETTINGS` row for it, so a `VIKE_NODE_CONTROL_KEY`
/// literal here failed THAT gate for a variable nothing reads — leaving the test to make do with an
/// invented stand-in and the realistic name in a comment. The gate's `read_evidence_literals` now
/// takes a test region out of direction-1 evidence for exactly this reason, so a test about a
/// mis-spelled variable can be written with the mis-spelling.
///
/// `ACME_NOT_A_SETTING` stays invented ON PURPOSE — it is the safe-shaped control, and its whole job
/// is to be a name that can never become a real setting and quietly stop being unmatched.
#[test]
fn a_store_key_that_matches_no_setting_is_surfaced_by_name() {
    let c = Case::new("unknown");
    c.write("secrets.env", "ACME_NOT_A_SETTING=1\n");

    let out = stdout(&c.run(&["--section", "env", "--filter", "ACME_NOT_A_SETTING"]));
    assert!(out.contains("ACME_NOT_A_SETTING"), "the unmatched key must be named: {out}");
    assert!(out.contains("match NO row above"), "…under an explanation of what it means: {out}");
}

/// …and the half that keeps it from becoming a store dump: an unmatched key whose NAME is
/// credential-shaped is disclosed by COUNT, never named — the same rule the header has always
/// followed. Its VALUE reaches neither view, in either spelling.
///
/// The fixture IS the case that prompted the feature: `VIKE_NODE_CONTROL_KEY`, the mis-spelling of
/// `VIKE_TRADEHUB_CONTROL_KEY` a clean-install validation found sitting in a store. That it is
/// credential-shaped, and therefore lands in the COUNTED bucket rather than the named one, is the
/// honest and slightly unsatisfying half of the design — asserting it on the real name is what makes
/// that visible instead of a claim about a stand-in whose shape somebody chose to match.
#[test]
fn an_unmatched_credential_shaped_key_is_counted_not_named() {
    let c = Case::new("unknown-secret");
    c.write("secrets.env", "VIKE_NODE_CONTROL_KEY=hmac-abcd1234\nACME_NOT_A_SETTING=1\n");

    let out = stdout(&c.run(&["--section", "env"]));
    assert!(!out.contains("VIKE_NODE_CONTROL_KEY"), "an unmatched credential NAME leaked: {out}");
    assert!(!out.contains("hmac-abcd1234"), "a credential VALUE leaked: {out}");
    assert!(out.contains("1 credential-shaped name(s)"), "…it is disclosed by count: {out}");
    assert!(out.contains("ACME_NOT_A_SETTING"), "the safe-shaped key is still named: {out}");

    let json = stdout(&c.run(&["--json"]));
    assert!(!json.contains("VIKE_NODE_CONTROL_KEY") && !json.contains("hmac-abcd1234"), "{json}");
    assert!(json.contains("\"credential_shaped\": 1"), "{json}");
}

/// A store whose every key IS a known setting produces no block at all, so a block on screen always
/// means something is genuinely unaccounted for.
#[test]
fn a_fully_recognised_store_prints_no_unmatched_block() {
    let known = vike_ops::settings::SETTINGS[0].name;
    let c = Case::new("unknown-none");
    c.write("secrets.env", &format!("{known}=x\n"));

    let out = stdout(&c.run(&["--section", "env"]));
    assert!(!out.contains("match NO row above"), "nothing is unaccounted for: {out}");
}
