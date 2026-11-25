//! **`vike-desktop --mcp` IS `vike-mcp`**: the built binary, spawned over pipes, answers `vike-mcp`'s own
//! goldens (`crates/vike-mcp/tests/fixtures/mcp_wire/`, written by
//! `crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot.rs` from the shipped `vike-mcp`): `initialize`,
//! `tools/list` and the tool calls that need no node, `list_templates` among them, answer for answer,
//! with the same exit and the same (empty) stderr. The goldens are that crate's; this file replays each
//! case's requests and normalises each answer the way its harness does
//! (`crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot/harness.rs`'s `normalized`, the parts these
//! cases reach): a tool text that is its structured body pretty-printed becomes one marker,
//! `serverInfo.version` becomes `<version>`, a multi-line string `{"<lines>": [..]}`, and the
//! `instructions` are held to their own text golden.
//!
//! It also holds that the mode opens no window: the child's environment is cleared, so it has no
//! display to open one on, and stderr must stay as empty as `vike-mcp`'s.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

/// `vike-mcp`'s goldens.
const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../vike-mcp/tests/fixtures/mcp_wire");

/// The cases replayed: every one recorded with nothing but an empty `settings/` and no daemon (each
/// dials port 1, or no node at all).
const CASES: [&str; 12] = [
    "initialize.profile-full",
    "initialize.profile-offline",
    "tools_list.full",
    "tools_list.read-only",
    "tools_list.offline",
    "tools.offline",
    "tools.errors",
    "tools.node_reads_no_node",
    "tools.dead_daemons",
    "withheld.offline",
    "resources.full",
    "prompts.full",
];

/// How long one answer may take before the case FAILS rather than hangs.
const WAIT: Duration = Duration::from_secs(60);

/// The harness's marker for a tool text that is exactly its `structuredContent`, pretty-printed.
const RENDERED: &str = "<structuredContent, serde_json pretty>";

/// The harness's key for a multi-line string.
const LINES: &str = "<lines>";

#[test]
fn the_desktop_in_mcp_mode_answers_vike_mcps_goldens() {
    for case in CASES {
        replay(case);
    }
}

/// Spawn `vike-desktop --mcp` with the case's flags in the case's world, send its requests, and hold
/// every answer, the exit and stderr to the golden.
fn replay(case: &str) {
    let golden: Value = serde_json::from_str(&read(&format!("{GOLDENS}/{case}.json")))
        .unwrap_or_else(|e| panic!("{case}: the golden is not JSON: {e}"));
    assert_eq!(
        golden["env"],
        json!({ "VIKE_SETTINGS_DIR": "<scratch>/settings" }),
        "{case}: recorded in the world this replay builds"
    );
    let scratch = tempfile::tempdir().expect("a scratch project");
    let settings = scratch.path().join("settings");
    std::fs::create_dir_all(&settings).expect("the scratch settings directory");
    let stderr_path = scratch.path().join("server-stderr.txt");
    let argv: Vec<&str> = golden["argv"]
        .as_array()
        .expect("the golden's argv")
        .iter()
        .map(|word| word.as_str().expect("an argv word"))
        .collect();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-desktop"))
        .arg("--mcp")
        .args(&argv)
        .env_clear()
        .env("VIKE_SETTINGS_DIR", &settings)
        .current_dir(scratch.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&stderr_path).expect("the server's stderr file"))
        .spawn()
        .unwrap_or_else(|e| panic!("{case}: spawn vike-desktop --mcp: {e}"));
    let mut stdin = child.stdin.take().expect("a piped stdin");
    let lines = read_lines(child.stdout.take().expect("a piped stdout"));
    let root = scratch.path().display().to_string();
    let mut answers = 0;
    for entry in golden["transcript"].as_array().expect("the golden's transcript") {
        if let Some(message) = entry.get("send") {
            writeln!(stdin, "{}", joined(message.clone()))
                .unwrap_or_else(|e| panic!("{case}: write a request: {e}"));
            continue;
        }
        let line = lines.recv_timeout(WAIT).unwrap_or_else(|e| panic!("{case}: {entry}: {e}"));
        let mut answer: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("{case}: an answer is not JSON ({e}): {line}"));
        held_instructions(case, &mut answer, entry);
        assert_eq!(normalized(answer, &root), entry["recv"], "{case}: an answer differs");
        answers += 1;
    }
    assert!(answers > 0, "{case}: the golden holds no answer, so this replay compared nothing");
    drop(stdin);
    let status = child.wait().expect("the server's exit");
    assert_eq!(json!(status.code()), golden["exit"], "{case}: the exit code");
    assert_eq!(json!(read(&stderr_path.display().to_string())), golden["stderr"], "{case}: stderr");
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Every line of the server's stdout, read on a thread of its own so every wait is bounded.
fn read_lines(stdout: ChildStdout) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

/// A golden request as it was sent: each `{"<lines>": [..]}` joined back into its string.
fn joined(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) if map.len() == 1 && map.contains_key(LINES) => {
            let lines: Vec<&str> = map[LINES]
                .as_array()
                .expect("the lines of a string")
                .iter()
                .map(|line| line.as_str().expect("a line"))
                .collect();
            return json!(lines.join("\n"));
        }
        Value::Object(map) => map.values_mut().for_each(|v| *v = joined(v.take())),
        Value::Array(items) => items.iter_mut().for_each(|v| *v = joined(v.take())),
        _ => {}
    }
    value
}

/// An `initialize` answer's `instructions`, which the golden keeps in a text file of their own
/// (`<instructions.TAG.txt>`): held to that file byte for byte, then replaced by the marker.
fn held_instructions(case: &str, answer: &mut Value, entry: &Value) {
    let Some(marker) = entry.pointer("/recv/result/instructions").and_then(Value::as_str) else {
        return;
    };
    let file = format!("{GOLDENS}/{}", marker.trim_matches(['<', '>']));
    let field = answer
        .pointer_mut("/result/instructions")
        .unwrap_or_else(|| panic!("{case}: no instructions"));
    assert_eq!(field.as_str(), Some(read(&file).as_str()), "{case}: the instructions ({file})");
    *field = json!(marker);
}

/// An answer the way the golden stores it.
fn normalized(mut answer: Value, root: &str) -> Value {
    collapse_rendered(&mut answer);
    if let Some(version) = answer.pointer_mut("/result/serverInfo/version") {
        assert_eq!(*version, json!(env!("CARGO_PKG_VERSION")), "the server's own version");
        *version = json!("<version>");
    }
    scrub_and_split(&mut answer, root);
    answer
}

/// A tool answer whose text is exactly its structured body pretty-printed: the text becomes
/// [`RENDERED`].
fn collapse_rendered(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let rendered = map
                .get("structuredContent")
                .and_then(|body| serde_json::to_string_pretty(body).ok());
            if let (Some(rendered), Some(text)) =
                (rendered, map.get_mut("content").and_then(|c| c.pointer_mut("/0/text")))
                && text.as_str() == Some(rendered.as_str())
            {
                *text = json!(RENDERED);
            }
            map.values_mut().for_each(collapse_rendered);
        }
        Value::Array(items) => items.iter_mut().for_each(collapse_rendered),
        _ => {}
    }
}

/// The scratch directory as `<scratch>`, then every string holding a newline as `{"<lines>": [..]}`.
fn scrub_and_split(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => {
            if text.contains(root) {
                *text = text.replace(root, "<scratch>");
            }
            if text.contains('\n') {
                let lines: Vec<&str> = text.split('\n').collect();
                *value = json!({ LINES: lines });
            }
        }
        Value::Object(map) => map.values_mut().for_each(|v| scrub_and_split(v, root)),
        Value::Array(items) => items.iter_mut().for_each(|v| scrub_and_split(v, root)),
        _ => {}
    }
}
