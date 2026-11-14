//! The harness: a throwaway project ([`World`]), one spawned server driven over its stdio
//! ([`Session`]), one server run to its exit ([`run_process`]), a fake backtesthub ([`FakeDaemon`]),
//! and the golden compare ([`check`], [`bless`]).
//!
//! The transport is the shape of `crates/vike-agent-evaluation/src/world/mcp.rs`'s `McpServer` (a reader
//! thread, so every wait is bounded and a dead server is a finding rather than a hang). It is not that
//! type because that type can only send what it numbered itself, and these cases must send malformed
//! lines, reused ids and notifications. The fake daemon is the shape of
//! `crates/vike-mcp/src/tests/cancellation.rs`'s `fake_connection`, spoken in the wire's own
//! JSON so it names no client type: that is what the server's bytes meet whichever crate sends them.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use vike_hub_protocol::auth::{DATAHUB_DOMAIN, HubKeys, Scope};

use super::{LAUNCH, SELF_NAMES};

/// How long one answer, or the process's exit after its stdin closed, may take before the case FAILS
/// rather than hangs. The server's own longest wait here is a preview's two ~2 s reads of a node that
/// has not folded.
const WAIT: Duration = Duration::from_secs(60);

/// Every dial goes here unless a case says otherwise: a real datahub or backtesthub may be listening
/// on the compiled-in defaults, and these goldens must never reach one. Port 1 on loopback refuses at
/// once, and the same way on every run.
const DEAD_HUBS: [(&str, &str); 2] =
    [("--addr", "127.0.0.1:1"), ("--backtest-addr", "127.0.0.1:1")];

/// What a tool's `content[0].text` is stored as when it is exactly the pretty rendering of its
/// `structuredContent`, so a body is read once. A different rendering would not match, and would show.
pub(super) const RENDERED: &str = "<structuredContent, serde_json pretty>";

/// The key of a multi-line string, split so a person can read it.
const LINES: &str = "<lines>";

/// The program and the argv head one launch starts with. See `super::LAUNCH`.
pub(super) struct Launch {
    pub(super) program: &'static str,
    pub(super) head: &'static [&'static str],
}

/// A throwaway project: an empty `settings/` named by `VIKE_SETTINGS_DIR`, and NOTHING else in the
/// child's environment (`env_clear`), so no exported variable on any box can change what is pinned.
pub(super) struct World {
    scratch: tempfile::TempDir,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    scrubs: Vec<(String, String)>,
}

impl World {
    /// `settings/` exists and holds nothing: no store, no keys.
    pub(super) fn empty() -> Self {
        let scratch = tempfile::tempdir().expect("a scratch project");
        let settings = scratch.path().join("settings");
        std::fs::create_dir_all(&settings).expect("the scratch settings directory");
        let root = scratch.path().display().to_string();
        Self {
            cwd: scratch.path().to_path_buf(),
            env: vec![("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())],
            scrubs: vec![(root, "<scratch>".to_string())],
            scratch,
        }
    }

    /// A store created the way a fresh box creates it, holding `rows`.
    pub(super) fn with_store(rows: &[(&str, &str)]) -> Self {
        let world = Self::empty();
        crate::common::seed_store(&world.settings(), rows);
        world
    }

    /// No `VIKE_SETTINGS_DIR` and a working directory no project sits above: the root of the box.
    pub(super) fn no_project() -> Self {
        let world = Self::empty();
        Self { cwd: PathBuf::from("/"), env: Vec::new(), ..world }
    }

    pub(super) fn settings(&self) -> PathBuf {
        self.path("settings")
    }

    /// A path under the scratch project, as a string for an argv.
    pub(super) fn arg(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }

    pub(super) fn path(&self, rel: &str) -> PathBuf {
        self.scratch.path().join(rel)
    }

    pub(super) fn env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.to_string(), value.to_string()));
        self
    }

    /// Show `actual` as `label` in the golden (an ephemeral address).
    pub(super) fn scrub(mut self, actual: &str, label: &str) -> Self {
        self.scrubs.push((actual.to_string(), label.to_string()));
        self
    }

    fn command(&self, argv: &[String]) -> Command {
        let mut cmd = Command::new(LAUNCH.program);
        cmd.args(LAUNCH.head).args(argv).env_clear().current_dir(&self.cwd);
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }
}

/// One JSON-RPC request line.
pub(super) fn request(id: impl Into<Value>, method: &str, params: Value) -> Value {
    let id: Value = id.into();
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A spawned server, every line in both directions recorded in order.
pub(super) struct Session {
    case: String,
    world: Option<World>,
    argv: Vec<String>,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    log: Vec<Value>,
    next_id: i64,
    sentinels: u32,
    stderr_path: PathBuf,
}

impl Session {
    /// Spawn the server with `argv` after the launch head. [`DEAD_HUBS`] fills in any address `argv`
    /// does not name.
    pub(super) fn start(case: impl Into<String>, world: World, argv: &[&str]) -> Self {
        let case = case.into();
        let argv = with_dead_hubs(argv);
        let stderr_path = world.path("server-stderr.txt");
        let stderr = File::create(&stderr_path).expect("the server's stderr file");
        let mut cmd = world.command(&argv);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(stderr));
        let mut child = cmd.spawn().unwrap_or_else(|e| panic!("{case}: spawn the server: {e}"));
        let stdin = child.stdin.take().expect("a piped stdin");
        let stdout = child.stdout.take().expect("a piped stdout");
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            case,
            world: Some(world),
            argv,
            child,
            stdin: Some(stdin),
            lines,
            log: Vec::new(),
            next_id: 0,
            sentinels: 0,
            stderr_path,
        }
    }

    /// The next id, then the answer to `method`.
    pub(super) fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        self.call(request(self.next_id, method, params))
    }

    /// `tools/call` under the next id.
    pub(super) fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({ "name": name, "arguments": arguments }))
    }

    /// Send one message and read one line back.
    pub(super) fn call(&mut self, message: Value) -> Value {
        self.send(message);
        self.recv()
    }

    /// Send one message and read nothing.
    pub(super) fn send(&mut self, message: Value) {
        self.write_line(message.to_string().as_bytes());
        self.log.push(json!({ "send": message }));
    }

    /// Send one line exactly as given, whatever it is.
    pub(super) fn send_raw(&mut self, line: &str) {
        self.write_line(line.as_bytes());
        self.log.push(json!({ "send_raw": line }));
    }

    /// Send bytes that are not text.
    pub(super) fn send_bytes(&mut self, bytes: &[u8]) {
        self.write_line(bytes);
        self.log.push(json!({ "send_bytes": bytes }));
    }

    /// Read one line. A line that is not JSON is recorded as the string it is: stdout is a protocol,
    /// and the golden must show a stray line rather than hide it.
    pub(super) fn recv(&mut self) -> Value {
        let line = match self.lines.recv_timeout(WAIT) {
            Ok(line) => line,
            Err(e) => panic!("{}: no line from the server ({e}); {}", self.case, self.stderr_now()),
        };
        let message = serde_json::from_str::<Value>(&line).unwrap_or(Value::String(line));
        self.log.push(json!({ "recv": message }));
        message
    }

    /// Prove nothing is owed: the server handles one line at a time, so the answer to a `ping` sent
    /// now is the next line unless an earlier line got an answer it should not have.
    pub(super) fn expect_silence(&mut self) {
        self.sentinels += 1;
        let id = format!("sentinel-{}", self.sentinels);
        let answer = self.call(json!({ "jsonrpc": "2.0", "id": id, "method": "ping" }));
        assert_eq!(answer["id"], json!(id), "{}: a line that owed nothing was answered", self.case);
    }

    /// One exchange that is not part of the golden: a wait whose number of rounds varies.
    pub(super) fn unrecorded(&mut self, message: Value) -> Value {
        let mark = self.log.len();
        let answer = self.call(message);
        self.log.truncate(mark);
        answer
    }

    /// Record something the wire cannot show, in its place in the order.
    pub(super) fn note(&mut self, text: &str) {
        self.log.push(json!({ "observed": text }));
    }

    /// Close stdin, and leave everything else to [`Session::finish`].
    pub(super) fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Close stdin, read to the end of stdout, wait for the exit. Every line after the last one a
    /// case asked for lands in the transcript, so an answer nobody expected fails the golden.
    pub(super) fn finish(mut self) -> Doc {
        self.close_stdin();
        let deadline = Instant::now() + WAIT;
        loop {
            match self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) => {
                    let message =
                        serde_json::from_str::<Value>(&line).unwrap_or(Value::String(line));
                    self.log.push(json!({ "recv": message }));
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "{}: stdout still open {WAIT:?} after stdin closed; {}",
                        self.case,
                        self.stderr_now()
                    )
                }
            }
        }
        let status = loop {
            match self.child.try_wait().expect("wait for the server") {
                Some(status) => break status,
                None if Instant::now() > deadline => {
                    panic!("{}: alive {WAIT:?} after stdin closed", self.case)
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let stderr = self.stderr_now();
        let mut doc = Doc::new(&self.case, std::mem::take(&mut self.argv), stderr, status.code());
        doc.body.insert("transcript".to_string(), Value::Array(std::mem::take(&mut self.log)));
        doc.world = self.world.take();
        doc
    }

    fn write_line(&mut self, bytes: &[u8]) {
        let result = {
            let stdin = self.stdin.as_mut().expect("stdin is still open");
            stdin
                .write_all(bytes)
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.flush())
        };
        if let Err(e) = result {
            panic!("{}: the server stopped reading ({e}); {}", self.case, self.stderr_now());
        }
    }

    fn stderr_now(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `argv` with [`DEAD_HUBS`] added for each address it does not name.
fn with_dead_hubs(argv: &[&str]) -> Vec<String> {
    let mut full: Vec<String> = argv.iter().map(ToString::to_string).collect();
    for (flag, addr) in DEAD_HUBS {
        if !argv.contains(&flag) {
            full.extend([flag.to_string(), addr.to_string()]);
        }
    }
    full
}

/// Run the server to its exit with stdin closed: the process-level cases, which answer nothing.
pub(super) fn run_process(case: &str, world: World, argv: &[&str]) -> Doc {
    let argv: Vec<String> = argv.iter().map(ToString::to_string).collect();
    let out = world
        .command(&argv)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{case}: spawn the server: {e}"));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let mut doc = Doc::new(case, argv, stderr, out.status.code());
    let stdout = String::from_utf8_lossy(&out.stdout);
    doc.body.insert("stdout".to_string(), Value::String(self_named(&stdout)));
    doc.world = Some(world);
    doc
}

/// What the program says about ITSELF becomes [`LAUNCH`]-independent: the expected deltas of the
/// move (`super::SELF_NAMES`), and the whole `--version` line, whose build identity varies per build.
fn self_named(text: &str) -> String {
    let stem = Path::new(LAUNCH.program).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let version_line = format!("{stem} {} (", env!("CARGO_PKG_VERSION"));
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if line.starts_with(&version_line) {
            out.push("<program> <version> (<build identity>)".to_string());
        } else {
            out.push(line.to_string());
        }
    }
    SELF_NAMES.iter().fold(out.join("\n"), |text, (from, to)| text.replace(*from, to))
}

/// One finished run, on its way to being a golden.
pub(super) struct Doc {
    case: String,
    what: String,
    body: Map<String, Value>,
    world: Option<World>,
}

impl Doc {
    fn new(case: &str, argv: Vec<String>, stderr: String, exit: Option<i32>) -> Self {
        let mut body = Map::new();
        body.insert("argv".to_string(), json!(argv));
        body.insert("stderr".to_string(), Value::String(self_named(&stderr)));
        body.insert("exit".to_string(), json!(exit));
        Self { case: case.to_string(), what: String::new(), body, world: None }
    }

    /// One line saying what this golden pins, for the person reading the file.
    pub(super) fn what(mut self, what: &str) -> Self {
        self.what = what.to_string();
        self
    }

    /// One more top-level field: what a case read off disk.
    pub(super) fn with(mut self, key: &str, value: Value) -> Self {
        self.body.insert(key.to_string(), value);
        self
    }

    /// The scratch project this run used. It lives until the golden is built.
    pub(super) fn world(&self) -> &World {
        self.world.as_ref().expect("a finished run keeps its world")
    }

    /// The JSON golden, normalised (the module doc's list).
    pub(super) fn golden(mut self) -> Golden {
        let world = self.world.take().expect("a finished run keeps its world");
        let mut body = std::mem::take(&mut self.body);
        body.insert("case".to_string(), Value::String(self.case.clone()));
        body.insert("what".to_string(), Value::String(std::mem::take(&mut self.what)));
        let env: Map<String, Value> =
            world.env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        body.insert("env".to_string(), Value::Object(env));
        Golden::Json {
            file: format!("{}.json", self.case),
            body: normalized(Value::Object(body), &world.scrubs),
        }
    }

    /// [`Doc::golden`] with each `initialize` answer's `instructions` moved into the plain-text golden
    /// `instructions.<tag>.txt`.
    pub(super) fn golden_with_instructions(mut self, tag: &str) -> Vec<Golden> {
        let file = format!("instructions.{tag}.txt");
        let mut goldens = Vec::new();
        let entries = self.body.get_mut("transcript").and_then(Value::as_array_mut);
        for entry in entries.into_iter().flatten() {
            if let Some(field) = entry.pointer_mut("/recv/result/instructions") {
                let text = std::mem::replace(field, json!(format!("<{file}>")));
                goldens.push(Golden::Text {
                    file: file.clone(),
                    body: text.as_str().expect("instructions are text").to_string(),
                });
            }
        }
        goldens.push(self.golden());
        goldens
    }
}

/// One committed file.
pub(super) enum Golden {
    /// Compared as parsed JSON.
    Json { file: String, body: Value },
    /// Compared byte for byte.
    Text { file: String, body: String },
}

fn golden_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mcp_wire"))
}

/// Compare every golden, and report every difference at once, by JSON path.
pub(super) fn check(goldens: Vec<Golden>) {
    let mut failures = Vec::new();
    for golden in goldens {
        let (file, differences) = match &golden {
            Golden::Json { file, body } => (file, json_differences(&golden_dir().join(file), body)),
            Golden::Text { file, body } => (file, text_differences(&golden_dir().join(file), body)),
        };
        if !differences.is_empty() {
            failures.push(format!("{file}:\n  {}", differences.join("\n  ")));
        }
    }
    assert!(
        failures.is_empty(),
        "the server's wire answers moved:\n{}\n\nIf this is intended: cargo test -p vike-mcp --test \
         shipped_mcp -- --ignored bless_  (in a lane), then read the diff.",
        failures.join("\n")
    );
}

/// Wipe and rewrite every golden.
pub(super) fn bless(goldens: Vec<Golden>) {
    let dir = golden_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear the golden directory");
    }
    std::fs::create_dir_all(&dir).expect("the golden directory");
    let count = goldens.len();
    for golden in goldens {
        let (file, text) = match golden {
            Golden::Json { file, body } => {
                let pretty = serde_json::to_string_pretty(&body).expect("a golden is JSON");
                (file, format!("{pretty}\n"))
            }
            Golden::Text { file, body } => (file, body),
        };
        std::fs::write(dir.join(&file), text).unwrap_or_else(|e| panic!("write {file}: {e}"));
    }
    eprintln!("wrote {count} goldens under {}", dir.display());
}

fn json_differences(path: &Path, now: &Value) -> Vec<String> {
    let Ok(committed) = std::fs::read_to_string(path) else {
        return vec!["no committed golden".to_string()];
    };
    let golden: Value = serde_json::from_str(&committed).expect("a committed golden is JSON");
    let mut out = Vec::new();
    json_diff("", &golden, now, &mut out);
    out
}

fn text_differences(path: &Path, now: &str) -> Vec<String> {
    let Ok(committed) = std::fs::read_to_string(path) else {
        return vec!["no committed golden".to_string()];
    };
    if committed == now {
        return Vec::new();
    }
    let (golden, current): (Vec<&str>, Vec<&str>) =
        (committed.split('\n').collect(), now.split('\n').collect());
    let mut out = Vec::new();
    for i in 0..golden.len().max(current.len()) {
        let (a, b) = (golden.get(i), current.get(i));
        if a != b {
            out.push(format!("line {}: golden {a:?} / now {b:?}", i + 1));
        }
    }
    out
}

/// Every difference between two JSON values, by JSON path, each side cut short for reading.
fn json_diff(path: &str, golden: &Value, now: &Value, out: &mut Vec<String>) {
    match (golden, now) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let at = format!("{path}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => json_diff(&at, x, y, out),
                    (Some(x), None) => out.push(format!("{at}: GONE, golden has {}", short(x))),
                    (None, Some(y)) => out.push(format!("{at}: NEW, now {}", short(y))),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for i in 0..a.len().max(b.len()) {
                let at = format!("{path}/{i}");
                match (a.get(i), b.get(i)) {
                    (Some(x), Some(y)) => json_diff(&at, x, y, out),
                    (Some(x), None) => out.push(format!("{at}: GONE, golden has {}", short(x))),
                    (None, Some(y)) => out.push(format!("{at}: NEW, now {}", short(y))),
                    (None, None) => {}
                }
            }
        }
        _ if golden != now => {
            out.push(format!("{path}: golden {} / now {}", short(golden), short(now)));
        }
        _ => {}
    }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    match text.char_indices().nth(400) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

/// The normalisation: the module doc's list, and nothing else.
fn normalized(mut body: Value, scrubs: &[(String, String)]) -> Value {
    collapse_rendered_text(&mut body);
    stamp_what_varies(&mut body);
    let mut replacements = scrubs.to_vec();
    let mut minted = Vec::new();
    collect_minted_order_client_ids(&body, &mut minted);
    for (n, order_client_id) in minted.into_iter().enumerate() {
        replacements.push((order_client_id, format!("<minted-order-client-id-{}>", n + 1)));
    }
    // Longest first: `<session>1` is a prefix of `<session>10`.
    replacements.sort_by_key(|(actual, _)| std::cmp::Reverse(actual.len()));
    for_each_string(&mut body, &mut |text: &mut String| {
        for (actual, label) in &replacements {
            if text.contains(actual.as_str()) {
                *text = text.replace(actual.as_str(), label);
            }
        }
    });
    split_lines(&mut body);
    body
}

/// A `tools/call` answer whose text is exactly its structured body rendered: see [`RENDERED`].
fn collapse_rendered_text(value: &mut Value) {
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
            map.values_mut().for_each(collapse_rendered_text);
        }
        Value::Array(items) => items.iter_mut().for_each(collapse_rendered_text),
        _ => {}
    }
}

/// The fields that vary by build, instant and process.
fn stamp_what_varies(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(version) = map.get_mut("serverInfo").and_then(|s| s.get_mut("version")) {
                assert_eq!(*version, json!(env!("CARGO_PKG_VERSION")), "the server's own version");
                *version = json!("<version>");
            }
            if let Some(ts) = map.get_mut("ts_ms") {
                *ts = json!("<ts_ms>");
            }
            if let Some(process) = map.get_mut("proc") {
                *process = json!({ "bin": "<program>", "pid": "<pid>", "ver": "<version>" });
            }
            for clock_reading in ["frame_age_ms", "frame_arrived_after_ms", "frame_stale"] {
                map.remove(clock_reading);
            }
            map.values_mut().for_each(stamp_what_varies);
        }
        Value::Array(items) => items.iter_mut().for_each(stamp_what_varies),
        _ => {}
    }
}

/// Every `wire_command.Submit.order_client_id` the server minted, in order of appearance. A case
/// supplies its own only as `c-…`.
fn collect_minted_order_client_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let order_client_id = map.get("Submit").and_then(|s| s["order_client_id"].as_str());
            if let Some(order_client_id) =
                order_client_id.filter(|c| !c.starts_with("c-") && !c.is_empty())
                && !out.iter().any(|seen| seen == order_client_id)
            {
                out.push(order_client_id.to_string());
            }
            map.values().for_each(|v| collect_minted_order_client_ids(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_minted_order_client_ids(v, out)),
        _ => {}
    }
}

fn for_each_string(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => f(text),
        Value::Object(map) => map.values_mut().for_each(|v| for_each_string(v, f)),
        Value::Array(items) => items.iter_mut().for_each(|v| for_each_string(v, f)),
        _ => {}
    }
}

/// Every string holding a newline becomes `{"<lines>": [..]}`, on both sides of the compare.
fn split_lines(value: &mut Value) {
    match value {
        Value::String(text) if text.contains('\n') => {
            let lines: Vec<&str> = text.split('\n').collect();
            *value = json!({ LINES: lines });
        }
        Value::Object(map) => map.values_mut().for_each(split_lines),
        Value::Array(items) => items.iter_mut().for_each(split_lines),
        _ => {}
    }
}

/// What the fake daemon saw of one connection.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Seen {
    /// The request arrived, carrying this profile text.
    Request(String),
    /// The peer closed the connection that carried this profile text.
    PeerClosed(String),
    /// A keyed dial: the scope the client asked for, its mac verified against the daemon's pair.
    Authed(String),
}

/// A fake hub on an ephemeral port: it answers the handshake and reads one request. A run's profile
/// text says what to do: `hold…` is never answered, `answer…` is answered at once, `gated…` when the
/// case calls [`FakeDaemon::open_gate`]. `list_strategies` and `list_series` are answered at once.
pub(super) struct FakeDaemon {
    pub(super) addr: String,
    seen: Receiver<Seen>,
    gate: Sender<()>,
}

impl FakeDaemon {
    /// A key-less daemon.
    pub(super) fn keyless() -> Self {
        Self::spawn(None)
    }

    /// A daemon holding this pair: it issues a nonce and verifies the mac, as both real hubs do.
    pub(super) fn keyed(observe: &str, control: &str) -> Self {
        let keys = HubKeys::new(observe.as_bytes().to_vec(), control.as_bytes().to_vec());
        Self::spawn(Some(keys))
    }

    fn spawn(keys: Option<HubKeys>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("the fake daemon's address").to_string();
        let (seen_tx, seen) = channel();
        let (gate, gate_rx) = channel();
        let gate_rx = Arc::new(Mutex::new(gate_rx));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (seen_tx, gate_rx) = (seen_tx.clone(), Arc::clone(&gate_rx));
                let keys = keys.clone();
                std::thread::spawn(move || {
                    fake_connection(stream, keys.as_ref(), &seen_tx, &gate_rx);
                });
            }
        });
        Self { addr, seen, gate }
    }

    pub(super) fn next_seen(&self) -> Seen {
        self.seen.recv_timeout(WAIT).expect("the fake daemon reports within the wait")
    }

    /// [`Self::next_seen`], past the closes of connections that are over.
    pub(super) fn next_open(&self) -> Seen {
        loop {
            match self.next_seen() {
                Seen::PeerClosed(_) => {}
                seen => return seen,
            }
        }
    }

    pub(super) fn open_gate(&self) {
        self.gate.send(()).expect("the fake daemon holds the gate");
    }
}

fn fake_connection(
    mut stream: TcpStream,
    keys: Option<&HubKeys>,
    seen: &Sender<Seen>,
    gate: &Mutex<Receiver<()>>,
) {
    use vike_hub_protocol::frame::{read_frame, write_frame};

    /// Fixed: nothing here replays a handshake, and the golden holds no nonce.
    const NONCE: [u8; 32] = [7; 32];
    let Ok(hello) = read_frame::<_, Value>(&mut stream) else { return };
    let proto_version = &hello["Hello"]["proto_version"];
    let welcome = match keys {
        Some(_) => json!({ "Welcome": {
            "proto_version": proto_version, "features": ["auth"], "nonce": NONCE,
        }}),
        None => json!({ "Welcome": { "proto_version": proto_version, "features": [] } }),
    };
    if write_frame(&mut stream, &welcome).is_err() {
        return;
    }
    if let Some(keys) = keys {
        let Ok(auth) = read_frame::<_, Value>(&mut stream) else { return };
        let scope: Scope =
            serde_json::from_value(auth["Auth"]["scope"].clone()).expect("an Auth frame's scope");
        let mac: Vec<u8> =
            serde_json::from_value(auth["Auth"]["mac"].clone()).expect("an Auth frame's mac");
        let version = proto_version.as_u64().expect("a protocol version") as u32;
        if !keys.verify(DATAHUB_DOMAIN, scope, &NONCE, version, &mac) {
            let _ = write_frame(&mut stream, &json!({ "AuthDenied": { "reason": "bad mac" } }));
            return;
        }
        if write_frame(&mut stream, &json!({ "AuthOk": { "scope": scope } })).is_err() {
            return;
        }
        let _ = seen.send(Seen::Authed(format!("{scope:?}")));
    }
    let Ok(request) = read_frame::<_, Value>(&mut stream) else { return };
    if let Some((label, answer)) = metadata_answer(&request) {
        let _ = seen.send(Seen::Request(label.to_string()));
        let _ = write_frame(&mut stream, &answer);
        return;
    }
    // The wire's own tags, spelled as the wire spells them (since ruling 20 the paramscan tags are
    // their Rust names, `RunParamscanProfile` and its answer `ParamscanReport`).
    let (tag, label) = if let Some(profile) = request["RunBacktest"].as_str() {
        ("Report", profile)
    } else if let Some(profile) = request["RunParamscanProfile"]["profile_toml"].as_str() {
        ("ParamscanReport", profile)
    } else if let Some(profile) = request["RunWalkforwardProfile"]["profile_toml"].as_str() {
        ("WalkforwardReport", profile)
    } else {
        panic!("the fake hub serves the three run verbs and two reads only, got {request}");
    };
    let label = label.to_string();
    let _ = seen.send(Seen::Request(label.clone()));
    let mut answer = Map::new();
    answer.insert(tag.to_string(), json!(json!({ "label": label }).to_string()));
    let answer = Value::Object(answer);
    let answer_it = label.starts_with("answer")
        || (label.starts_with("gated")
            && gate.lock().expect("the gate").recv_timeout(WAIT).is_ok());
    if answer_it {
        let _ = write_frame(&mut stream, &answer);
    }
    // Hold (or linger after answering) until the peer closes.
    let mut buf = [0u8; 256];
    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
    let _ = seen.send(Seen::PeerClosed(label));
}

/// The two metadata reads, by their wire tags: what [`Seen`] calls each, and its answer.
fn metadata_answer(request: &Value) -> Option<(&'static str, Value)> {
    match request.as_str()? {
        "ListStrategies" => Some(("ListStrategies", json!({ "Strategies": ["buy_hold"] }))),
        "Inventory" => {
            let id =
                json!({ "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h" });
            let cover = json!({ "first_timestamp": 1, "last_timestamp": 2, "rows": 3, "bytes": 4, "parts": 1, "dates": 1 });
            Some(("Inventory", json!({ "Inventory": [[id, cover]] })))
        }
        _ => None,
    }
}
