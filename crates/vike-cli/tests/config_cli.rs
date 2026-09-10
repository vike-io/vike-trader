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

struct Case {
    dir: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("vike-cli-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Case { dir }
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

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
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
    // `poly_exec` is read inside the Polymarket bridge, from the environment; a file cannot set it.
    // `tradehub_control` is the wired twin, and is what makes this test prove a DISTINCTION rather
    // than just "the column exists".
    c.write("flags.toml", "poly_exec = true\ntradehub_control = true\n");

    let out = stdout(&c.run(&["--section", "files"]));
    let unread =
        out.lines().find(|l| l.starts_with("flags.poly_exec ")).unwrap_or_else(|| panic!("{out}"));
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
    assert!(out.contains("flags.poly_exec = true"), "{out}");
    assert!(out.contains("poly_exec_enabled"), "the reason must name the real reader: {out}");
    // The wired one must NOT appear in that warning block.
    let warned = out.split("CONFIGURED and read by NOTHING").nth(1).unwrap_or("");
    assert!(!warned.contains("flags.tradehub_control"), "{out}");
}

/// …and the machine view carries the same fact, so a tool does not have to parse the table.
#[test]
fn the_json_view_carries_the_consumed_flag() {
    let c = Case::new("unreadjson");
    let o = c.run(&["--json", "--section", "files"]);
    assert!(o.status.success(), "{o:?}");
    let doc: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("valid JSON");
    let rows = doc["settings"].as_array().unwrap();

    let unread = rows.iter().find(|r| r["key"] == "flags.poly_exec").expect("the poly_exec row");
    assert_eq!(unread["consumed"], serde_json::Value::Bool(false));
    assert!(unread["why_unread"].as_str().is_some_and(|w| w.contains("poly_exec_enabled")));

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

    // ⚠ `config.state_dir` was the third key here until 2026-09-09. It is NOT dropped because it
    // stopped mattering — it is dropped because it stopped being READ AT ALL: the desktop cut
    // deleted `state_dir_path`, its one reader, so it has no binary to name. Its row is now a
    // `Consumer::Not` carrying the admission, and `config show` must print it as unread rather than
    // naming a program. The assertion below covers that.
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
    assert_eq!(cell("flags.poly_exec"), "NO", "an unread row still says NO\n{out}");

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
    // ⚠ `config.state_dir` used to be the GUI example here and now reads NULL — not because the
    // column broke, but because the desktop cut deleted its one reader (`state_dir_path`), so there
    // is no binary to name. It is asserted alongside the genuinely-unread row below precisely so
    // this stays visible: an operator setting it today configures nothing.
    assert_eq!(read_by("config.state_dir"), serde_json::Value::Null);
    // An unread row has no binary to name — `consumed` is what distinguishes this `null` from the
    // library case, and both are honest answers rather than "unknown".
    assert_eq!(read_by("flags.poly_exec"), serde_json::Value::Null);
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
