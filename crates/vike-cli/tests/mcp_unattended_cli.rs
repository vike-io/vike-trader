//! `vike-cli mcp --unattended` — the flag decision 0040's runner passes on every launch, proven
//! against the SHIPPED binary over its real stdio transport.
//!
//! # The ruling
//!
//! The owner, on `docs/decisions/0040-the-unattended-agent-runner-is-local-read-only-and-os-scheduled.md`
//! (`policy.*` on 2026-09-28, every other key on 2026-09-29): **an UNATTENDED run changes no
//! setting at all.** An ATTENDED agent changes a live setting only after the owner said yes in chat
//! (`docs/decisions/0086` point 6), and a session nobody attends has no chat to say it in — so the
//! operator changes settings with `vike-cli config set` or the GUI, and the server an unattended
//! runner launches refuses every `set_setting` outright: before a preview, before a token, before
//! anything reaches a node.
//!
//! # Why the SHIPPED binary, and why here
//!
//! `crates/vike-agent-eval/src/unattended.rs`'s `mcp_argv` is the argv a scheduled run launches, and
//! its own test pins that the argv always carries `--unattended`. What that crate cannot see is
//! whether the server HONOURS it: it has no `vike-*` dependency by design, and it drives
//! pre-built binaries a lane may not have rebuilt. This file closes that half where cargo
//! guarantees a fresh `vike-cli`: the same flags the runner passes, the real argument parser, the
//! real router, the real JSON-RPC transport. The ATTENDED control is what keeps the refusal from
//! passing on a server that refused every policy write for some other reason.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// A policy RISK CEILING — the key class the ruling is about.
const CEILING: &str = "policy.max_notional_per_order";

/// What one `vike-cli mcp` session answered, by JSON-RPC id, plus its stderr for failure messages.
struct Session {
    by_id: HashMap<i64, Value>,
    stderr: String,
}

impl Session {
    /// The `result` of the `tools/call` sent under `id`, or a panic naming what the server said.
    fn result(&self, id: i64) -> &Value {
        self.by_id.get(&id).map(|r| &r["result"]).unwrap_or_else(|| {
            panic!(
                "the server answered nothing to request {id} — did it start at all?\n\
                 answered: {:?}\n--- stderr ---\n{}",
                self.by_id.keys().collect::<Vec<_>>(),
                self.stderr
            )
        })
    }
}

/// Run the shipped `vike-cli mcp` with `flags`, send `initialize` and then one `tools/call` per
/// `calls` entry (ids from 2), close stdin, and collect every answer.
///
/// Hermetic: the settings directory is an EMPTY throwaway one (so the walk cannot find the checkout
/// the test runs in), the node keys are scrubbed, and no `--node` is named — every call below is
/// answered before a node would ever be dialled, which is part of what is proven.
fn session(flags: &[&str], calls: &[(&str, Value)]) -> Session {
    let settings = tempfile::tempdir().expect("an empty settings directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("mcp")
        .args(flags)
        .env("VIKE_SETTINGS_DIR", settings.path())
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vike-cli mcp");
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut requests = vec![json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" }
        })];
        for (i, (name, arguments)) in calls.iter().enumerate() {
            requests.push(json!({
                "jsonrpc": "2.0", "id": i as i64 + 2, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }));
        }
        for request in requests {
            // A server that refused its own argv has already exited; the answers (none) and its
            // stderr then carry the verdict, so a broken pipe here is not the failure to report.
            if writeln!(stdin, "{request}").is_err() {
                break;
            }
        }
    }
    let out = child.wait_with_output().expect("wait for vike-cli mcp");
    let by_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| v["id"].as_i64().map(|id| (id, v)))
        .collect();
    Session { by_id, stderr: String::from_utf8_lossy(&out.stderr).to_string() }
}

/// The text of a tool result's first content block.
fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

/// One key of every settings class: a risk ceiling and an arming switch (`policy.*`), the
/// live-trading and reconcile-off flags (`flags.*`), a node address (`config.*`) and a preference.
const EVERY_CLASS: [(&str, &str); 6] = [
    (CEILING, "250"),
    ("policy.venues.binance", "live"),
    ("flags.tradehub_live", "true"),
    ("flags.reconcile_off", "true"),
    ("config.tradehub_addr", "127.0.0.1:7879"),
    ("preferences.log_file_level", "warn"),
];

/// **An unattended session refuses EVERY `set_setting`** — whatever the key's class — before any
/// preview is taken or any token minted, and the refusal says who changes settings instead.
#[test]
fn an_unattended_session_refuses_every_settings_write_and_names_who_changes_them() {
    let calls: Vec<(&str, Value)> = EVERY_CLASS
        .iter()
        .map(|(key, value)| ("set_setting", json!({ "key": key, "value": value })))
        .collect();
    let s = session(&["--profile", "full", "--unattended"], &calls);

    for (i, (key, _)) in EVERY_CLASS.iter().enumerate() {
        let id = i as i64 + 2;
        let r = s.result(id);
        assert_eq!(r["isError"], json!(true), "`{key}` must be REFUSED unattended: {r}");
        let why = text(r);
        assert!(why.contains("vike-cli config set"), "`{key}`: name the CLI route: {why}");
        assert!(why.contains("GUI"), "`{key}`: …and the GUI route: {why}");
        assert!(why.contains("unattended"), "`{key}`: say WHY this session may not: {why}");
        assert!(
            r["structuredContent"].is_null(),
            "`{key}`: a refusal is not a preview and hands back no preview_token: {r}"
        );
    }
}

/// **The control: an ATTENDED session keeps `set_setting` for every class**, behind the preview
/// gate and the owner's-yes rule. Without this, the refusal above would also pass on a server that
/// refused settings writes for some other reason — the retype it replaced, say.
#[test]
fn an_attended_session_still_previews_a_settings_write() {
    let calls: Vec<(&str, Value)> = EVERY_CLASS
        .iter()
        .map(|(key, value)| ("set_setting", json!({ "key": key, "value": value })))
        .collect();
    let s = session(&["--profile", "full"], &calls);
    for (i, (key, _)) in EVERY_CLASS.iter().enumerate() {
        let r = s.result(i as i64 + 2);
        assert_eq!(r["isError"], json!(false), "an attended `{key}` write previews: {r}");
        assert!(r["structuredContent"]["preview_token"].is_string(), "`{key}`: {r}");
        assert_eq!(r["structuredContent"]["change"]["key"], json!(key), "`{key}`: {r}");
    }
}
