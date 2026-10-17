#![cfg(feature = "trade")]
//! **Text gates of the trade half: the fold thread's cost, and what this crate may read.**
//!
//! 1. **The fold thread does a channel send and a stat.** `RemoteExecClient`'s methods are called ON the
//!    core's single-writer fold; the core-hop latency gate (`crates/vike-core/tests/runtime_latency.rs`)
//!    measures the hop with the order path OUTSIDE it, so the order path must stay an enqueue. The CODE
//!    (comments and string literals masked) of every `ExecutionClient` method that runs on the fold
//!    thread may not name `serde_json`, anything that writes, a lock, a join, a sleep or a receive. The
//!    helpers those methods call are where a stat, a channel send and the failure-path synthesis live;
//!    they are deliberately outside the list - the rule is about what the fold thread does, and a helper
//!    that serialized would be a defect of its own caller's body the day it is named there.
//! 2. **This crate reads no setting.** Libraries take configuration as parameters; only binaries read the
//!    process environment or the credential store (root `CLAUDE.md`). Nothing under `src/trade/` may name
//!    `std::env`, `vike_secrets` or `vike_config`.
//!
//! Each gate is shown to be able to FAIL: a planted violation in a synthetic source is reported.

use std::path::PathBuf;

const EXEC_SRC: &str = "crates/vike-gatewayhub-client/src/trade/exec.rs";
const TRADE_DIR: &str = "crates/vike-gatewayhub-client/src/trade";

/// The `ExecutionClient` methods that run on the fold thread.
const FOLD_METHODS: &[&str] = &[
    "submit",
    "cancel",
    "cancel_with_intent",
    "cancel_batch",
    "cancel_batch_with_intent",
    "modify",
    "submit_batch",
    "confirm",
    "begin_detach",
    "adopt_restored",
];

fn read_repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel} could not be read ({e})"))
}

/// `src` with comments and the CONTENTS of string and char literals blanked (same length), so a word in
/// prose or in a log message is not code.
fn mask(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                out.push(if b[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            out.push_str("  ");
            i += 2;
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                out.push(if b.get(i) == Some(&'\n') { '\n' } else { ' ' });
                i += 1;
            }
            out.push('"');
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The body (between the braces) of `fn name(` inside `impl_header`'s impl block of `masked`.
fn body_of(masked: &str, impl_header: &str, name: &str) -> Option<String> {
    let start = masked.find(impl_header)?;
    let tail = &masked[start..];
    let needle = format!("fn {name}(");
    let at = tail.find(&needle)?;
    let after = &tail[at..];
    let open = after.find('{')?;
    let mut depth = 0usize;
    for (i, c) in after[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[open + 1..open + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The identifiers of `code`.
fn idents(code: &str) -> Vec<&str> {
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .collect()
}

fn forbidden(token: &str) -> bool {
    token == "serde_json"
        || token == "lock"
        || token == "Mutex"
        || token == "RwLock"
        || token == "join"
        || token.starts_with("write")
        || token.starts_with("sleep")
        || token.starts_with("recv")
}

/// Every forbidden token in the fold-thread methods of `src`, as `method: token`.
fn fold_thread_violations(src: &str) -> Vec<String> {
    let masked = mask(src);
    let mut out = Vec::new();
    for method in FOLD_METHODS {
        let Some(body) = body_of(&masked, "impl ExecutionClient for RemoteExecClient", method)
        else {
            out.push(format!("{method}: the method is not in the ExecutionClient impl"));
            continue;
        };
        for token in idents(&body) {
            if forbidden(token) {
                out.push(format!("{method}: {token}"));
            }
        }
    }
    out
}

#[test]
fn the_fold_thread_methods_serialize_nothing_write_nothing_and_take_no_lock() {
    let violations = fold_thread_violations(&read_repo(EXEC_SRC));
    assert!(
        violations.is_empty(),
        "the order path on the fold thread must stay an enqueue + a stat: {violations:?}"
    );
}

#[test]
fn every_fold_thread_method_exists_in_the_impl_so_the_gate_cannot_pass_by_finding_nothing() {
    let masked = mask(&read_repo(EXEC_SRC));
    for method in FOLD_METHODS {
        let body = body_of(&masked, "impl ExecutionClient for RemoteExecClient", method)
            .unwrap_or_else(|| panic!("{method} is not in the impl"));
        assert!(!body.trim().is_empty(), "{method} has an empty body");
    }
    // And the two bodies the NEVER-SENT story rests on do call the channel, not a socket.
    let submit =
        body_of(&masked, "impl ExecutionClient for RemoteExecClient", "submit").expect("submit");
    assert!(submit.contains("enqueue") && submit.contains("clone"), "{submit}");
}

#[test]
fn the_gate_reports_a_planted_serializer_a_write_and_a_lock() {
    let planted = r#"
impl ExecutionClient for RemoteExecClient {
    fn submit(&mut self, request: &OrderRequest) {
        let bytes = serde_json::to_vec(request).unwrap();
        self.stream.write_all(&bytes).unwrap();
        let _g = self.state.lock().unwrap();
    }
    fn cancel(&mut self, _c: &str) { /* serde_json in a comment */ let _ = "write in a string"; }
}
"#;
    let found = fold_thread_violations(planted);
    assert!(found.iter().any(|v| v == "submit: serde_json"), "{found:?}");
    assert!(found.iter().any(|v| v == "submit: write_all"), "{found:?}");
    assert!(found.iter().any(|v| v == "submit: lock"), "{found:?}");
    assert!(
        !found.iter().any(|v| v.starts_with("cancel:") && !v.contains("not in")),
        "prose in a comment or a string is not code: {found:?}"
    );
}

#[test]
fn the_trade_half_reads_no_environment_and_names_no_store() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/trade");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the trade source dir") {
        let path = entry.expect("an entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        let code = mask(&text);
        for needle in ["std::env", "vike_secrets", "vike_config", "env::var"] {
            if code.contains(needle) {
                offenders.push(format!("{} names {needle}", path.display()));
            }
        }
    }
    assert!(offenders.is_empty(), "{TRADE_DIR} must read no setting: {offenders:?}");
}
