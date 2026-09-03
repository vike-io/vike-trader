//! **Every advertised settings layer, proven to take effect through the SHIPPED binary** — the
//! end-to-end half of the layer-reachability gate.
//!
//! `crates/vike-config/tests/layers_are_reachable.rs` holds the other half: it pins
//! [`vike_config::PRECEDENCE`] against the resolver's own variants and order, and walks the
//! workspace to prove no shipped file reads the REMOVED per-project override file. Both of those
//! reason about CODE. This file reasons about BEHAVIOUR: it points the real `vike-cli` binary at a
//! real project directory, arms one layer at a time, and demands the origin it reports be the layer
//! that was armed.
//!
//! Only the pair is worth trusting. A source walk proves an argument is no longer `None`; it cannot
//! prove the dispatcher resolves the directory the loader then reads, which is the whole chain that
//! was broken. `<project>/vike.toml` was implemented, unit-tested, given a provenance variant and
//! NAMED in this very command's precedence header, and a `vike.toml` on disk still did nothing —
//! every unit test in `vike-config` passed throughout, because each one passed the project
//! directory the binaries did not.
//!
//! ⚠ That layer is now REMOVED (a fifth settings file above `<project>/settings/` reintroduces
//! "which file won?"), so this file gates the removal from the same end:
//! [`a_present_project_file_is_refused_by_the_shipped_binary`] proves a leftover one produces a
//! loud, actionable startup error naming the file — never a silent no-op, which would leave an
//! operator believing a setting is in force that is not. A refusal nobody has run is exactly as
//! trustworthy as a layer nobody has read.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` to the case's own temp directory and clears the rest
//! of the environment, for the reason `config_cli.rs` gives: without it a run on a developer box
//! resolves the REPO's settings directory, and an exported `RUST_LOG` alone would move a row.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use vike_config::{PRECEDENCE, REMOVED_PROJECT_FILE};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// The key every scenario measures. One key is enough because it is settable from both non-default
/// layers — which is exactly the property under test — and `config.log_dir` is one of the keys whose
/// silent loss has a recorded cost (`vike_log`'s trace file, 341 GB).
const KEY: &str = "config.log_dir";

/// A project directory laid out the way the loader expects: `<project>/settings/`. The settings
/// directory's PARENT is the project — the relationship the removed-file probe derives with one
/// `Path::parent` call, and the reason a `vike.toml` dropped beside `settings/` is found at all.
struct Case {
    project: PathBuf,
    env_layer: bool,
}

impl Case {
    fn new(tag: &str) -> Self {
        let project =
            std::env::temp_dir().join(format!("vike-cli-layers-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("settings")).unwrap();
        Case { project, env_layer: false }
    }

    /// Arm one layer so that it, and only layers below it, can win.
    ///
    /// The catch-all PANICS rather than skipping: a layer added to [`PRECEDENCE`] with no scenario
    /// here would otherwise make this gate quietly stop covering it, which is the same silence the
    /// gate exists to remove.
    fn arm(&mut self, kind: &str) {
        match kind {
            "env" => self.env_layer = true,
            "file" => self.write("settings/config.toml", "log_dir = \"/from/file\"\n"),
            // The compiled-in default is armed by writing nothing at all.
            "default" => {}
            other => panic!(
                "no scenario for the advertised layer {other:?}. Every layer in \
                 vike_config::PRECEDENCE must be proven to take effect end to end — add its arm \
                 here rather than letting this gate stop covering it."
            ),
        }
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.project.join(rel), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.arg("config").arg("show").args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", self.project.join("settings"));
        if self.env_layer {
            cmd.env("VIKE_LOG_DIR", "/from/env");
        }
        cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
    }

    /// `config show --json`, parsed.
    fn show_json(&self) -> serde_json::Value {
        let out = self.run(&["--json", "--section", "files"]);
        assert!(
            out.status.success(),
            "config show failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("--json emits JSON")
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.project);
    }
}

/// The `origin` the command reports for [`KEY`] — the machine word, i.e. a layer's `kind`.
fn reported_origin(v: &serde_json::Value) -> String {
    v["settings"]
        .as_array()
        .expect("settings is an array")
        .iter()
        .find(|r| r["key"] == KEY)
        .unwrap_or_else(|| panic!("no row for {KEY}"))["origin"]
        .as_str()
        .expect("origin is a string")
        .to_string()
}

/// **THE gate.** For each advertised layer, arm it and everything below it, then demand the shipped
/// binary attribute the value to that layer.
///
/// Driven by [`PRECEDENCE`] itself rather than by a written-out list, so a layer added to the
/// precedence header — the operator-facing claim — arrives here needing an end-to-end proof before
/// it can be advertised, and a layer removed from it takes its scenario with it.
#[test]
fn every_advertised_layer_takes_effect_through_the_shipped_binary() {
    for (i, target) in PRECEDENCE.iter().enumerate() {
        let mut case = Case::new(target.kind);
        // The target and every layer BELOW it. Peeling the higher ones off is what makes each
        // scenario a precedence measurement instead of an isolated "this layer parses" check.
        for lower in &PRECEDENCE[i..] {
            case.arm(lower.kind);
        }

        let origin = reported_origin(&case.show_json());
        assert_eq!(
            origin, target.kind,
            "the {:?} layer is advertised in `config show`'s precedence header but the shipped \
             binary attributed {KEY} to {origin:?} instead. An advertised layer no composition \
             root reaches gives the operator positive confirmation of something false.",
            target.label
        );
    }
}

/// The file-presence block lists EXACTLY the files the loader reads — four, and no fifth.
///
/// The per-project file had a row here for one change, wired alongside the layer. Both are gone:
/// a row for a file nothing reads is the same false positive as a header naming a layer nothing
/// applies, just rendered in a different block of the same command's output.
#[test]
fn the_file_presence_block_lists_only_the_files_the_loader_reads() {
    let case = Case::new("presence");
    let doc = case.show_json();
    let files = doc["files"].as_array().unwrap();

    assert_eq!(files.len(), 4, "{files:#?}");
    assert!(
        !files.iter().any(|f| f["name"] == REMOVED_PROJECT_FILE),
        "{REMOVED_PROJECT_FILE} is refused, not read — it must not be disclosed as a settings \
         file: {files:#?}"
    );

    // ...and the header a human reads does not name it either.
    let human = case.run(&["--section", "files"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("policy.toml"), "the human block lost its rows entirely:\n{text}");
    assert!(!text.contains(REMOVED_PROJECT_FILE), "the human header names it:\n{text}");
}

/// **A leftover `vike.toml` is REFUSED, loudly, by the shipped binary — never ignored.**
///
/// This is the migration half of the removal, and the reason it is a gate rather than a note. The
/// layer genuinely worked for the window between the change that wired it and the change that
/// removed it, so an operator can be holding a file that TOOK EFFECT. Dropping the reader in
/// silence would make their belief quietly false — the exact defect class this whole settings
/// program has been spent removing, and the one `refuse_removed_env` already answers for a
/// variable.
///
/// Four things are demanded of the refusal, all of them from the OUTSIDE: a non-zero exit, the
/// full path on stderr, both destination files named, and — the rule that outranks the others —
/// the operator's own file still sitting there, byte for byte.
#[test]
fn a_present_project_file_is_refused_by_the_shipped_binary() {
    let case = Case::new("removed-file");
    const BODY: &str = "[config]\nlog_dir = \"/from/project\"\n";

    // Absent: the ordinary path, exit 0.
    let before = case.run(&["--json", "--section", "files"]);
    assert!(before.status.success(), "{}", String::from_utf8_lossy(&before.stderr));

    case.write(REMOVED_PROJECT_FILE, BODY);
    let after = case.run(&["--json", "--section", "files"]);

    assert!(
        !after.status.success(),
        "a present {REMOVED_PROJECT_FILE} must FAIL startup, not be ignored — an ignored one is a \
         setting the operator believes is in force and is not.\nstdout: {}",
        String::from_utf8_lossy(&after.stdout)
    );
    let err = String::from_utf8_lossy(&after.stderr);
    let path = case.project.join(REMOVED_PROJECT_FILE);
    assert!(err.contains(&path.display().to_string()), "the exact file, not a name: {err}");
    assert!(err.contains("NO LONGER READ"), "{err}");
    assert!(err.contains("settings/config.toml"), "where [config] goes: {err}");
    assert!(err.contains("settings/preferences.toml"), "where [preferences] goes: {err}");

    // ⚠ THE rule: refuse and INSTRUCT. This is the operator's only copy of whatever they wrote.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BODY, "the binary edited the file");
}

/// Every table, including the two the file never accepted. There is no "some of it still works"
/// middle ground to explain, and an empty file refuses too — its mere presence is the belief.
#[test]
fn any_project_file_at_all_is_refused_whatever_it_contains() {
    for (tag, body) in [
        ("config", "[config]\nlog_dir = \"/from/project\"\n"),
        ("preferences", "[preferences]\nlog_file_level = \"warn\"\n"),
        ("policy", "[policy]\nmax_notional_per_order = 99999.0\n"),
        ("flags", "[flags]\nreconcile = true\n"),
        ("empty", ""),
    ] {
        let case = Case::new(&format!("removed-{tag}"));
        case.write(REMOVED_PROJECT_FILE, body);

        let out = case.run(&["--json", "--section", "files"]);
        assert!(!out.status.success(), "a [{tag}] {REMOVED_PROJECT_FILE} was accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(REMOVED_PROJECT_FILE), "the error must name the file: {err}");
    }
}
