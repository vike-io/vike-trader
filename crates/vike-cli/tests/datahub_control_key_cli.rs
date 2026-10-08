//! End-to-end tests for `vike-cli datahub control-key`, driving the SHIPPED binary.
//!
//! The verb hands this box's datahub CONTROL key to a PIPE — the one form in which a key value
//! leaves `vike-cli` — so what matters is what reaches each STREAM: the value and nothing else on
//! stdout, never the value on stderr, and nothing at all on stdout when it refuses. Its unit tests
//! beside the module cover the grammar and the terminal refusal (a test harness has no terminal to
//! hand the binary); these cover the streams and the store.
//!
//! ⚠ **Every invocation points `VIKE_SETTINGS_DIR` at the case's own temp directory**, on the CHILD
//! through `Command::env`, exactly as `tests/node_cli.rs` does and for its reason: without it a run
//! on a developer box resolves the REPO's settings directory and would hand out a REAL key.
//!
//! The values planted here are placeholders, and none of them reaches an assertion MESSAGE.

use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// A project whose `settings/` holds a node-key FILE (the pre-database shape `resolve_node_keys`
/// still reads when no database exists).
struct Case {
    root: tempfile::TempDir,
}

impl Case {
    fn new(tag: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike-cli-control-key-{tag}-"))
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(root.path().join("settings")).expect("settings dir");
        Case { root }
    }

    fn settings(&self) -> std::path::PathBuf {
        self.root.path().join("settings")
    }

    fn write_node_store(&self, pairs: &[(&str, &str)]) {
        let text: String = pairs.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        std::fs::write(self.settings().join(vike_secrets::NODE_FILE), text).expect("write store");
        // A FILE is not a store since 2026-10-07: carry it into the `node_key` table, as
        // `vike-cli secrets migrate` does.
        vike_secrets::migrate(
            self.settings().to_str(),
            vike_model::credential_keys::is_platform_key,
            &vike_bridge_core::credentials::classify_credential_name,
            vike_secrets::WhenNothingToCarry::CreateEmptyStore,
        )
        .expect("carry the node-key file into the store");
    }

    /// `vike-cli datahub control-key`, stdout and stderr PIPED — which is also what makes the
    /// verb's terminal refusal not fire, exactly as it does not under `ssh host '…'`.
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(BIN)
            .arg("datahub")
            .arg("control-key")
            .args(extra)
            .env("VIKE_SETTINGS_DIR", self.settings())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("the vike-cli binary must run")
    }
}

fn observe_name() -> &'static str {
    vike_node_proto::auth::DATAHUB_OBSERVE_KEY_ENV
}

fn control_name() -> &'static str {
    vike_node_proto::auth::DATAHUB_CONTROL_KEY_ENV
}

/// ⚠ **stdout carries the CONTROL value and nothing else; stderr carries its id and never the
/// value; the OBSERVE value appears on neither.** This is the whole contract a launcher that
/// captures stdout depends on — a stray line on stdout would be read AS the key.
#[test]
fn control_key_hands_the_control_value_to_a_pipe_and_nothing_else() {
    let c = Case::new("hand");
    let (observe, control) = ("placeholder-observe-value", "placeholder-control-value");
    c.write_node_store(&[(observe_name(), observe), (control_name(), control)]);

    let out = c.run(&[]);
    assert!(out.status.success(), "exit {:?}", out.status.code());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout == format!("{control}\n"),
        "stdout must be exactly the CONTROL value and one newline"
    );
    assert!(!stderr.contains(control), "the CONTROL value reached stderr");
    assert!(
        !stdout.contains(observe) && !stderr.contains(observe),
        "the OBSERVE value was emitted"
    );
    let id = vike_node_proto::auth::key_fingerprint(control.as_bytes());
    assert!(
        stderr.contains(&id),
        "stderr names the handed key's id, for comparison with the daemon's log"
    );
}

/// A store with no CONTROL key is a refusal naming the key and the verb that mints it — and
/// stdout stays EMPTY, so a launcher that reads it gets nothing rather than a sentence it would
/// take for a key.
#[test]
fn control_key_refuses_a_store_without_one_and_prints_nothing() {
    let c = Case::new("absent");
    c.write_node_store(&[(observe_name(), "placeholder-observe-value")]);

    let out = c.run(&[]);
    assert!(!out.status.success(), "a store without a CONTROL key must be refused");
    assert!(out.stdout.is_empty(), "a refusal must print NOTHING on stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(control_name()), "names the key: {stderr}");
    assert!(stderr.contains("datahub setup"), "names the verb that mints it: {stderr}");
    assert!(!stderr.contains("placeholder-observe-value"), "the OBSERVE value reached stderr");
}

/// A value that is only whitespace is no key — refused the same way, rather than handed out as an
/// empty line the launcher would then have to recognise.
#[test]
fn a_blank_control_key_is_refused_like_an_absent_one() {
    let c = Case::new("blank");
    c.write_node_store(&[(observe_name(), "placeholder-observe-value"), (control_name(), "   ")]);
    let out = c.run(&[]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "a refusal must print NOTHING on stdout");
}

/// `control-key` takes no option: a flag is a usage error, and nothing is read or printed.
#[test]
fn control_key_takes_no_option() {
    let c = Case::new("flag");
    c.write_node_store(&[(control_name(), "placeholder-control-value")]);
    let out = c.run(&["--rotate"]);
    assert_eq!(out.status.code(), Some(2), "a flag on `control-key` is a usage error");
    assert!(out.stdout.is_empty(), "a usage error must print NOTHING on stdout");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("placeholder-control-value"),
        "the value reached stderr on a usage error"
    );
}
