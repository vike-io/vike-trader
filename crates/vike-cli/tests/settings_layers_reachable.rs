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
//! resolves the REPO's settings directory, and a stale retired variable would refuse the run.

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
///
/// ⚠ **A BOUND `tempfile::TempDir`, not `<system-temp>/vike-cli-layers-<tag>-<pid>`.** A
/// fixed-tag-plus-pid directory in the shared system temp root is unique enough on one box and
/// OWNED by nobody: the `Drop` impl that stood here cleaned up on the ordinary path, but a SIGKILL,
/// an OOM or a Ctrl-C — all of which the CI box has seen — leaves it there forever, `/tmp` being 1777
/// sticky. The verdict flip is at the RECEIVING end: given a foreign-owned leftover of the same
/// name, the old `remove_dir_all` failed `EACCES` and `let _ =` swallowed it, `create_dir_all`
/// returned **Ok** because the directory already existed, and the first `write` then panicked
/// `PermissionDenied` naming a `/tmp` path and no property of any test.
///
/// It matters more here than anywhere: this fixture's whole subject is the settings directory's
/// PARENT, and one `vike.toml` in a shared parent is a HARD refusal for every scenario at once.
struct Case {
    /// BOUND, so its `Drop` removes the tree even on the panic path.
    root: tempfile::TempDir,
}

impl Case {
    fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-layers-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).unwrap();
        Case { root }
    }

    /// `<project>` — the settings directory's PARENT, which is what this file is about.
    fn project(&self) -> &std::path::Path {
        self.root.path()
    }

    /// Arm one layer so that it, and only layers below it, can win.
    ///
    /// The catch-all PANICS rather than skipping: a layer added to [`PRECEDENCE`] with no scenario
    /// here would otherwise make this gate quietly stop covering it, which is the same silence the
    /// gate exists to remove.
    fn arm(&self, kind: &str) {
        match kind {
            "db" => self.arm_db(),
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
        std::fs::write(self.project().join(rel), body).unwrap();
    }

    /// **Arm the settings DATABASE as [`PRECEDENCE`]'s `db` layer means it** — a row present in the
    /// store, the only layer above `default`: `docs/decisions/0086` deleted the settings files
    /// outright and decision 0111 the environment layer. [`vike_secrets::plant_settings_rows`] creates the store from
    /// scratch when none exists, so there is no need to run `secrets init` or plant a credential
    /// file first — the `test-support` fixture every other row-writer test in this tree already
    /// uses.
    fn arm_db(&self) {
        vike_secrets::plant_settings_rows(
            &self.project().join("settings"),
            &vike_secrets::StoredSettings {
                settings: vec![vike_secrets::SettingRow {
                    section: "config".to_string(),
                    key: "log_dir".to_string(),
                    value: "\"/from/db\"".to_string(),
                }],
                ..Default::default()
            },
        )
        .expect("planting the db row must succeed");
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut with_verb = vec!["config", "show"];
        with_verb.extend_from_slice(args);
        self.run_args(&with_verb)
    }

    /// Any subcommand of the shipped binary, in this case's own project and with the rest of the
    /// environment cleared — see this file's module doc for why the clear is load-bearing.
    fn run_args(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args);
        cmd.env_clear();
        cmd.env("VIKE_SETTINGS_DIR", self.project().join("settings"));
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

/// **The POSITIVE proof that [`Case`] OWNS its tree**, which no green run of the scenarios below
/// can make: they passed with the old fixture too, and its `Drop` also removed the directory on the
/// PASSING path. What neither had is removal on the KILLED path, and what a leftover buys is the
/// cross-user `PermissionDenied` in this fixture's own constructor.
///
/// The other half of this file's hermeticity — that the project the binary infers is this case's
/// own root — is already proven behaviourally, and by a test that is not about hermeticity at all:
/// [`a_present_project_file_is_refused_by_the_shipped_binary`] plants `vike.toml` in
/// `Case::project` and requires the binary's refusal to name that exact path.
#[test]
fn the_case_fixture_removes_itself() {
    let project;
    {
        let case = Case::new("owned");
        project = case.project().to_path_buf();
        case.write("settings/config.toml", "log_dir = \"/from/file\"\n");
        assert!(project.join("settings").join("config.toml").is_file());
    }
    assert!(
        !project.exists(),
        "the fixture must remove itself when it goes out of scope: {} survived",
        project.display()
    );
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
        let case = Case::new(target.kind);
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

/// The settings block still names every typed key with NO settings-file block beside it any more —
/// `docs/decisions/0086` deletes the settings files outright, so `config show --json`'s top level
/// carries no `files` array at all (there is nothing on disk to report presence or absence of).
///
/// The per-project override file had a row here for one change while it was wired, and this test
/// used to hold that row absent; now there is no row of any kind to hold absent, only the array
/// itself.
#[test]
fn the_json_document_carries_no_settings_file_presence_block() {
    let case = Case::new("presence");
    let doc = case.show_json();
    assert!(doc.get("files").is_none(), "there are no settings files to report: {doc:#?}");

    // ...and the human header does not name a settings-file location either.
    let human = case.run(&["--section", "files"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("policy."), "the human block lost its rows entirely:\n{text}");
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
    let path = case.project().join(REMOVED_PROJECT_FILE);
    assert!(err.contains(&path.display().to_string()), "the exact file, not a name: {err}");
    assert!(err.contains("NO LONGER READ"), "{err}");
    assert!(err.contains("vike-cli config set config."), "where [config] goes: {err}");
    assert!(err.contains("vike-cli config set preferences."), "where [preferences] goes: {err}");

    // ⚠ THE rule: refuse and INSTRUCT. This is the operator's only copy of whatever they wrote.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BODY, "the binary edited the file");
}

/// **The live pre-trade ceilings become readable with no daemon anywhere** — decision 0057's
/// Phase 2, end to end through the SHIPPED binary, which is the only place the claim can be
/// checked.
///
/// # Why this cannot be a unit test
///
/// The whole finding is that a ceiling judging every live order was printable by nothing: resolving
/// it means parsing a `vike_core::RunProfile`, and `vike-cli` links neither `vike-core` nor
/// `vike-exec` on purpose. A library test can prove a struct round-trips; only running the binary
/// proves the number reaches an operator's terminal. Nothing in this test starts a daemon, opens a
/// socket or mounts a venue — the point is that none of that is needed.
///
/// # What it demands, and the order matters
///
/// 1. the ceiling's VALUE is printed, machine-readably and to a human;
/// 2. the document says whether it is IN FORCE — the active `run` row is what the daemon builds its
///    ceilings from (decision 0111: the only place), and a stored-but-inactive body must never be
///    mistakable for a ceiling in force;
/// 3. writing the run profile leaves the settings database's resolved rows BYTE-IDENTICAL — a run
///    body is a profile, not a settings layer;
/// 4. an unset mount-refusing ceiling is NAMED as unset, because that is the case where an
///    operator's box will not start live and the reason is invisible today.
///
/// The human block is printed on purpose (run with `--nocapture` to read it): it is the transcript
/// this phase is judged by, and a transcript nobody has looked at is a claim.
#[test]
fn the_run_profile_ceilings_are_readable_with_the_daemon_down() {
    let case = Case::new("profile-risk");
    let migrated = case.run_args(&["secrets", "init"]);
    assert!(
        migrated.status.success(),
        "`secrets init` must create the store: {}",
        String::from_utf8_lossy(&migrated.stderr)
    );

    // The settings rows BEFORE the run profile is written — the baseline for demand 3.
    let before = case.show_json()["settings"].clone();

    // A PAPER profile that sets one of the two mount-refusing ceilings and NOT the other, so both
    // halves of the disclosure are exercised by one case (a `live` one must carry both, and the
    // writer refuses it otherwise).
    let written = case.run_args(&[
        "config",
        "bootstrap-run",
        "run-live",
        "--mode",
        "paper",
        "--risk.max_notional_per_order",
        "5000",
        "--risk.max_leverage",
        "2",
    ]);
    assert!(
        written.status.success(),
        "`config bootstrap-run` must write the rows: {}",
        String::from_utf8_lossy(&written.stderr)
    );
    let report = String::from_utf8_lossy(&written.stdout);
    assert!(
        report.contains("stored and ACTIVATED run profile `run-live`"),
        "the writer stores AND selects, and says so: {report}"
    );
    assert!(report.contains("CROSSING"), "nothing was selected before: {report}");

    let doc = case.show_json();

    // 3 — nothing this box RESOLVES as a SETTING changed. Asserted first because a failure here
    // invalidates every other reading in this test.
    assert_eq!(
        doc["settings"], before,
        "writing a run profile must change no resolved settings row: a run body enters no layer"
    );

    let block = &doc["profile_risk"];
    assert_eq!(block["state"], "rows", "the stored rows must be readable: {block}");
    // 2 — the active body IS the ceiling in force, and the document says so.
    assert_eq!(block["active"], "run-live", "{block}");
    assert_eq!(block["enforced"], true, "{block}");
    assert!(!block["read_by"].is_null(), "{block}");

    let profiles = block["profiles"].as_array().expect("profiles is an array");
    let p = profiles.iter().find(|p| p["profile"] == "run-live").expect("the stored profile");
    // 1 — THE payoff: the number.
    let notional = p["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "max_notional_per_order")
        .expect("the per-order ceiling must be readable");
    assert_eq!(notional["value"], "5000.0");
    // 4 — and the one this profile does not set, which is why a live mount would refuse to start.
    let unset = p["unset"].as_array().unwrap();
    assert!(
        unset.iter().any(|k| k.as_str() == Some("max_total_exposure")),
        "an unset mount-refusing ceiling must be named: {unset:?}"
    );

    // ...and the human view a person actually reads. PRINTED — see this test's doc.
    let human = case.run(&["--section", "files", "--filter", "risk"]);
    assert!(human.status.success(), "{}", String::from_utf8_lossy(&human.stderr));
    let text = String::from_utf8_lossy(&human.stdout);
    println!("----- `vike-cli config show --section files --filter risk`, daemon down -----");
    println!("{text}");
    assert!(text.contains("5000.0"), "the human block must print the number:\n{text}");
    assert!(text.contains("ENFORCEABLE: `run-live`"), "the active body is in force:\n{text}");
    assert!(
        text.contains("REFUSES TO START"),
        "the unset mount-refusing ceiling must be called out where a human reads:\n{text}"
    );

    // ── THE ROLLBACK, which is one column and no data — and leaves NO run profile ─────────────
    let back = case.run_args(&["config", "deactivate", "run"]);
    assert!(back.status.success(), "{}", String::from_utf8_lossy(&back.stderr));
    let rolled = case.show_json();
    assert!(rolled["profile_risk"]["active"].is_null(), "no run profile is in force");
    assert_eq!(rolled["profile_risk"]["enforced"], false);
    assert_eq!(
        rolled["profile_risk"]["profiles"][0]["rows"],
        p["rows"].clone(),
        "the BODY is untouched by the rollback — that is why re-crossing needs no re-write"
    );
    let human = case.run(&["--section", "files", "--filter", "risk"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("READABLE, NOT ENFORCEABLE"),
        "with NO row active, the human block must say what a stored ceiling is not:\n{text}"
    );
    assert!(
        text.contains("NO run profile"),
        "…and that no file stands in for it any more (decision 0111):\n{text}"
    );

    // ── RE-CROSSING through `config activate`, whose `--proves` fence still holds ────────────
    //
    // The stored rows must render back to a document that parses EQUAL to the file named.
    case.write("settings/not-the-profile.toml", "mode = \"paper\"\n");
    let decoy = case.project().join("settings").join("not-the-profile.toml");
    let decoy = decoy.to_string_lossy().into_owned();
    let refused = case.run_args(&["config", "activate", "run", "run-live", "--proves", &decoy]);
    assert!(!refused.status.success(), "a proof against the WRONG file must be refused");
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("REFUSING to activate"), "{err}");

    case.write(
        "settings/run-live.toml",
        "mode = \"paper\"\n\n[risk]\nmax_notional_per_order = 5000.0\nmax_leverage = 2.0\n",
    );
    let proof = case.project().join("settings").join("run-live.toml");
    let proof = proof.to_string_lossy().into_owned();
    let crossed = case.run_args(&["config", "activate", "run", "run-live", "--proves", &proof]);
    assert!(crossed.status.success(), "{}", String::from_utf8_lossy(&crossed.stderr));
    let activated = case.show_json();
    assert_eq!(
        activated["profile_risk"]["active"], "run-live",
        "after the crossing the document must name the body this box READS"
    );
    assert_eq!(activated["profile_risk"]["enforced"], true);
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
