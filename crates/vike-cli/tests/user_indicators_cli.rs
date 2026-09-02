//! `vike-cli indicators` — an indicator the USER wrote must be discoverable, through the shipped
//! binary, from a real `.rhai` file on disk.
//!
//! # Why this drives the real binary, and why the unit tests cannot replace it
//!
//! The installed user-indicator set is a process-wide `OnceLock`
//! (`crates/vike-script/src/engine.rs`'s `install_user_indicators` argues why), so a unit test in
//! `crates/vike-cli/src/cmd/indicators.rs` cannot install one without leaking into every sibling
//! test in the same binary. Those tests therefore drive the renderers with SYNTHETIC rows — which
//! means **every one of them would still pass with `user_rows()` hard-wired to return nothing.**
//! This file is what makes them non-vacuous: a subprocess, its own `OnceLock`, a real file, and the
//! whole chain under test —
//!
//! `VIKE_USER_DATA_DIR` -> the dispatcher's `resolve_policy` -> `crate::install_user_indicators` ->
//! `vike_script::load_and_install_user_indicators` -> `installed_user_indicators` -> the listing.
//!
//! Break any link and the name below stops appearing on stdout.
//!
//! # Nothing here is written down twice
//!
//! The indicator's name, its knobs and its defaults are chosen by the fixture this test writes, so
//! the assertions are about THAT file rather than about a roster. No built-in name appears anywhere
//! in this file — that roster is `indicators_cli.rs`'s subject, and it derives its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway `<user_data>` tree, removed on drop.
struct UserData(PathBuf);

impl UserData {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-cli-user-ind-{tag}-{nanos}"));
        std::fs::create_dir_all(p.join("indicators")).expect("scratch");
        Self(p)
    }
    fn indicator(&self, name: &str, src: &str) -> &Self {
        std::fs::write(self.0.join("indicators").join(format!("{name}.rhai")), src).unwrap();
        self
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for UserData {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run the shipped binary against `user_data`, with settings pointed at a directory that does not
/// exist so the dispatcher cannot pick up this machine's real `policy.toml`.
fn run(user_data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env("VIKE_SETTINGS_DIR", std::env::temp_dir().join("vike_cli_user_ind_no_settings"))
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

fn stdout_of(user_data: &Path, args: &[&str]) -> String {
    let out = run(user_data, args);
    assert!(
        out.status.success(),
        "`vike-cli {}` must exit 0; stderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// ⚠ **The whole point.** A file the user wrote is listed, with the call form its own `param()`
/// declarations imply, and the built-in roster is still there beside it.
#[test]
fn a_users_own_indicator_is_listed_with_its_knobs() {
    let ud = UserData::new("listed");
    ud.indicator(
        "my_range",
        "let n = param(\"lookback\", 7.0);\n\
         fn on_bar(bar) { bar.high - bar.low }",
    );

    let listing = stdout_of(ud.path(), &["indicators"]);
    assert!(
        listing.contains("my_range(lookback=7)"),
        "the user's own indicator must be listed with its knob: {listing}"
    );
    assert!(
        listing.contains("your own"),
        "it must be in a block of its own, not filed under a built-in category: {listing}"
    );
    // The built-ins did not go anywhere: the footer states the split, both numbers computed.
    assert!(listing.contains("built in, 1 of your own"), "{listing}");

    // ...and the SAME binary run WITHOUT that user_data lists no such thing — which is what makes
    // the assertions above about this file rather than about the build.
    let empty = UserData::new("empty");
    let bare = stdout_of(empty.path(), &["indicators"]);
    assert!(!bare.contains("my_range"), "{bare}");
    assert!(!bare.contains("your own"), "an empty block must not be headed: {bare}");
}

/// `--json` must carry the distinction structurally — a separate key, and no `category` on a row
/// that has none — so an agent cannot file the author's own file under a family it is not in.
#[test]
fn the_json_answer_keeps_the_two_kinds_apart() {
    let ud = UserData::new("json");
    ud.indicator("my_flag", "fn on_bar(bar) { if bar.close > bar.open { 1.0 } else { 0.0 } }");

    let json: serde_json::Value =
        serde_json::from_str(&stdout_of(ud.path(), &["indicators", "--json"]))
            .expect("--json must print valid JSON");

    let users = json["user_indicators"].as_array().expect("a `user_indicators` array");
    assert_eq!(users.len(), 1, "{json}");
    assert_eq!(users[0]["name"], "my_flag");
    assert_eq!(users[0]["source"], "user_data/indicators");
    assert!(users[0].get("category").is_none(), "{}", users[0]);
    // A file declaring no `param()` still carries the key, as an empty array.
    assert_eq!(users[0]["params"].as_array().map(Vec::len), Some(0));

    // The built-in array is untouched, and does NOT contain the user's file.
    let builtins = json["indicators"].as_array().expect("an `indicators` array");
    assert!(!builtins.is_empty(), "a build binding nothing would make this vacuous");
    assert!(builtins.iter().all(|r| r["name"] != "my_flag"), "the two kinds must not merge");

    // ⚠ The key is present even with nothing installed: a consumer must not have to test for it.
    let empty = UserData::new("json-empty");
    let bare: serde_json::Value =
        serde_json::from_str(&stdout_of(empty.path(), &["indicators", "--json"])).unwrap();
    assert_eq!(bare["user_indicators"].as_array().map(Vec::len), Some(0), "{bare}");
}

/// `--name` is what a user runs when something they expected is missing. Answering "no indicator
/// named 'my_thing'" about a file on their own disk is not a miss — it is wrong.
#[test]
fn name_resolves_a_users_own_indicator() {
    let ud = UserData::new("byname");
    ud.indicator("my_thing", "fn on_bar(bar) { bar.close }");

    let one = stdout_of(ud.path(), &["indicators", "--name", "my_thing"]);
    assert!(one.contains("my_thing()"), "{one}");
    assert!(!one.contains("callable in this build"), "a single lookup is not a roster: {one:?}");

    let json: serde_json::Value =
        serde_json::from_str(&stdout_of(ud.path(), &["indicators", "--name=my_thing", "--json"]))
            .unwrap();
    assert_eq!(json["user_indicators"].as_array().map(Vec::len), Some(1), "{json}");
    assert_eq!(json["indicators"].as_array().map(Vec::len), Some(0), "{json}");

    // ...and the SAME name, with no such file, is still the honest "no indicator named" error.
    let empty = UserData::new("byname-empty");
    let out = run(empty.path(), &["indicators", "--name", "my_thing"]);
    assert!(!out.status.success(), "an unknown name must exit non-zero");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no indicator named"));
}

/// ⚠ A rejected file is REPORTED and never fatal — the property
/// `vike_script::load_and_install_user_indicators` exists to give every binary. One half-edited
/// indicator must not break a command that does not call it, and it must not be swallowed either:
/// from inside a strategy, "did not load" and "typo in the call" are the same error.
#[test]
fn a_broken_indicator_is_reported_on_stderr_and_the_command_still_succeeds() {
    let ud = UserData::new("broken");
    ud.indicator("my_good", "fn on_bar(bar) { bar.close }");
    ud.indicator("my_broken", "fn on_bar(bar) { this. }");

    let out = run(ud.path(), &["indicators"]);
    assert!(out.status.success(), "one bad file must not fail the command");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("my_broken.rhai"), "the rejection must name the file: {stderr}");
    assert!(stderr.contains("indicator not loaded"), "{stderr}");

    // ⚠ stdout stays a clean document: `--json` is machine-read, so a diagnostic there would make
    // it unparseable. The good file is listed; the broken one is not.
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(listing.contains("my_good()"), "{listing}");
    assert!(!listing.contains("my_broken"), "a file that did not load must not be advertised");
    assert!(!listing.contains("indicator not loaded"), "diagnostics belong on stderr: {listing}");
}

/// ⚠ **A user BAND indicator, end to end.** A file declaring `fn outputs()` whose line 0 is not its
/// namesake has NO bare name — the same refusal `bollinger` carries — so a listing that printed
/// `my_bands(width=2)` would be handing the author a call that raises on every bar and self-disables
/// the strategy after the consecutive-error cap. The per-line spellings are what it must print.
///
/// This is the end-to-end half of `crates/vike-cli/src/cmd/indicators.rs`'s
/// `a_user_band_indicator_is_printed_line_by_line_and_never_under_its_bare_name`, which drives the
/// renderer with a SYNTHETIC row and so would still pass with the whole
/// `installed_user_line_accessors` join hard-wired to nothing. Here the row comes from a real file
/// through the real `OnceLock`.
#[test]
fn a_users_own_band_indicator_is_listed_line_by_line_not_under_its_bare_name() {
    let ud = UserData::new("bands");
    ud.indicator(
        "my_bands",
        "let w = param(\"width\", 2.0);\n\
         fn outputs() { [\"upper\", \"mid\", \"lower\"] }\n\
         fn on_bar(bar) { [bar.close + w, bar.close, bar.close - w] }",
    );

    let listing = stdout_of(ud.path(), &["indicators"]);
    for line in ["upper", "mid", "lower"] {
        assert!(
            listing.contains(&format!("my_bands_{line}(width=2)")),
            "the {line} band must be listed with its knob: {listing}"
        );
    }
    assert!(
        !listing.contains("my_bands(width=2)"),
        "the bare name does not resolve, so printing it would advertise a call that raises: \
         {listing}"
    );

    // ...and the machine answer says the same thing structurally, so an agent does not have to
    // parse the block: no bare call, three spellings that work.
    let json: serde_json::Value =
        serde_json::from_str(&stdout_of(ud.path(), &["indicators", "--json"]))
            .expect("--json must print valid JSON");
    let users = json["user_indicators"].as_array().expect("a `user_indicators` array");
    assert_eq!(users.len(), 1, "{json}");
    assert_eq!(users[0]["bare_call"].as_bool(), Some(false), "{}", users[0]);
    let accessors = users[0]["accessors"].as_array().expect("its line accessors");
    assert_eq!(accessors.len(), 3, "{}", users[0]);
    assert_eq!(accessors[1]["call"], "my_bands_mid");

    // ...while a SINGLE-output file in the same shape of tree keeps the opposite pair, which is what
    // keeps the two assertions above about `fn outputs()` rather than about user files in general.
    let plain = UserData::new("bands-plain");
    plain.indicator("my_mid", "fn on_bar(bar) { bar.close }");
    let json: serde_json::Value =
        serde_json::from_str(&stdout_of(plain.path(), &["indicators", "--json"])).unwrap();
    let users = json["user_indicators"].as_array().unwrap();
    assert_eq!(users[0]["bare_call"].as_bool(), Some(true), "{}", users[0]);
    assert_eq!(users[0]["accessors"].as_array().map(Vec::len), Some(0), "{}", users[0]);
}
