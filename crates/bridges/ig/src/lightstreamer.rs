//! Minimal TLCP (Text Lightstreamer Client Protocol 2.1.0) codec — the PURE half of IG's
//! Lightstreamer MARKET-DATA feed (`crate::market_feed`). No I/O lives here: request-building and
//! frame/field decoding only, so every wire shape is unit-testable offline.
//!
//! The subset is deliberately bounded to what MERGE-mode subscriptions on one WS session need:
//! `create_session` / `control (LS_op=add)` / `control (LS_op=delete)` requests, and the `CONOK`/`CONERR`/`SUBOK`/`REQOK`/
//! `REQERR`/`U`/`PROBE`/`LOOP`/`END` notifications. Everything else the server can say
//! (`CONF`/`OV`/`CS`/`EOS`/`SYNC`/`NOOP`/`PROG`/`CONS`/`SERVNAME`/`CLIENTIP`/`UNSUB`) parses as
//! [`LsFrame::Other`] and is ignored by the driver — none of it affects a MERGE quote/candle lane
//! (`EOS` is never even sent in MERGE mode; the snapshot is a single `U`).
//!
//! Authority: the TLCP 2.1.0 specification ("TLCP Specifications", lightstreamer.com,
//! ls-generic-client 2.1.0). Two spec rules that are easy to get wrong, both pinned by tests here:
//!
//! - **The update grammar is `U,<subscription-ID>,<item>,<f1>|<f2>|...`** — the field list is the
//!   COMMA-separated third argument; only fields among themselves are pipe-separated. (The exec
//!   trade lane's `crate::exec::event_mapper::parse_update_line` expected a PIPE after `<item>` until a
//!   raw capture of the TRADE item on the demo gateway, 2026-10-30, showed the same grammar as here; it
//!   now splits the same way, and the crate CLAUDE.md has the three captured shapes.)
//! - **Field values are DELTAS**: empty = unchanged, `#` = null, `$` = empty string, `^n` = the
//!   next n fields unchanged, and actual content is percent-encoded (`#`/`$`/`^` only when
//!   leading; `|`, CR, LF, `%` always). [`MergeItemState`] owns the fold.

use std::fmt::Write as _;

/// TLCP protocol version this codec speaks (the `LS_protocol` query value for HTTP transports).
pub const LS_PROTOCOL: &str = "TLCP-2.1.0";
/// The WS subprotocol (`Sec-WebSocket-Protocol`) TLCP 2.1.0 requires on a WebSocket transport.
pub const LS_WS_SUBPROTOCOL: &str = "TLCP-2.1.0.lightstreamer.com";
/// `LS_cid` value the spec mandates for ALL custom-developed clients (TLCP 2.1.0, create_session:
/// "Must be set with the special string ... for all custom developed clients").
pub const LS_CID: &str = "mgQkwtwdysogQz2BJ4Ji kOj2Bg";

/// One field slot of a TLCP `U` notification, AFTER `^n` expansion and percent-decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldDelta {
    /// Empty segment (or covered by a `^n` run): keep the previous value of this field.
    Unchanged,
    /// `#`: the field is now null.
    Null,
    /// Actual content (`$` decodes to an empty string).
    Value(String),
}

/// One parsed `U` (real-time update) notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsUpdate {
    pub sub_id: u32,
    pub item: u32,
    /// Field deltas in schema order (`^n` runs already expanded).
    pub fields: Vec<FieldDelta>,
}

/// One parsed TLCP notification line — the bounded subset the market feed consumes, with every
/// unconsumed tag preserved as [`LsFrame::Other`] so a driver can log it rather than lose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LsFrame {
    Conok {
        session_id: String,
        request_limit: u64,
        keepalive_ms: u64,
        control_link: String,
    },
    Conerr {
        code: String,
        message: String,
    },
    Subok {
        sub_id: u32,
        num_items: u32,
        num_fields: u32,
    },
    Reqok,
    Reqerr {
        req_id: String,
        code: String,
        message: String,
    },
    Update(LsUpdate),
    Probe,
    /// `LOOP,<expected-delay>` — the session must be rebound (this client reconnects instead).
    Loop,
    End {
        code: String,
        message: String,
    },
    Other(String),
}

/// Parse one TLCP line (already CR-LF-trimmed). `None` only for an empty line.
pub fn parse_frame(line: &str) -> Option<LsFrame> {
    if line.is_empty() {
        return None;
    }
    let (tag, rest) = match line.split_once(',') {
        Some((t, r)) => (t, r),
        None => (line, ""),
    };
    Some(match tag {
        "U" => match parse_update(rest) {
            Some(u) => LsFrame::Update(u),
            None => LsFrame::Other(line.to_string()),
        },
        "CONOK" => {
            let mut p = rest.splitn(4, ',');
            let session_id = p.next().unwrap_or("").to_string();
            let request_limit = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let keepalive_ms = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let control_link = percent_decode(p.next().unwrap_or(""));
            LsFrame::Conok { session_id, request_limit, keepalive_ms, control_link }
        }
        "CONERR" => {
            let (code, message) = code_message(rest);
            LsFrame::Conerr { code, message }
        }
        "SUBOK" => {
            let mut p = rest.splitn(3, ',');
            let sub_id = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let num_items = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let num_fields = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            LsFrame::Subok { sub_id, num_items, num_fields }
        }
        "REQOK" => LsFrame::Reqok,
        "REQERR" => {
            let mut p = rest.splitn(3, ',');
            let req_id = p.next().unwrap_or("").to_string();
            let code = p.next().unwrap_or("").to_string();
            let message = percent_decode(p.next().unwrap_or(""));
            LsFrame::Reqerr { req_id, code, message }
        }
        "PROBE" => LsFrame::Probe,
        "LOOP" => LsFrame::Loop,
        "END" => {
            let (code, message) = code_message(rest);
            LsFrame::End { code, message }
        }
        _ => LsFrame::Other(line.to_string()),
    })
}

/// `<code>,<message>` tail shared by CONERR and END (message percent-decoded).
fn code_message(rest: &str) -> (String, String) {
    match rest.split_once(',') {
        Some((c, m)) => (c.to_string(), percent_decode(m)),
        None => (rest.to_string(), String::new()),
    }
}

/// Upper bound on a single `^n` unchanged-run, far above any real schema width — refuses a
/// corrupt/hostile count before it becomes an allocation.
const MAX_UNCHANGED_RUN: u32 = 4096;

/// Parse the `U` tail `<subId>,<item>,<f1>|<f2>|...` (spec grammar: the field list is the
/// comma-separated THIRD argument). `None` on a malformed head, a `^n` run, or a field list
/// wider than [`MAX_UNCHANGED_RUN`].
fn parse_update(rest: &str) -> Option<LsUpdate> {
    let mut p = rest.splitn(3, ',');
    let sub_id = p.next()?.parse().ok()?;
    let item = p.next()?.parse().ok()?;
    let tail = p.next().unwrap_or("");
    let mut fields = Vec::new();
    for seg in tail.split('|') {
        match seg {
            "" => fields.push(FieldDelta::Unchanged),
            "#" => fields.push(FieldDelta::Null),
            "$" => fields.push(FieldDelta::Value(String::new())),
            _ if seg.starts_with('^') => {
                let n: u32 = seg[1..].parse().ok()?;
                if n == 0 || n > MAX_UNCHANGED_RUN {
                    return None;
                }
                fields.extend(std::iter::repeat_n(FieldDelta::Unchanged, n as usize));
            }
            _ => fields.push(FieldDelta::Value(percent_decode(seg))),
        }
        // the per-run cap alone lets one line chain many runs: bound the TOTAL width too
        if fields.len() > MAX_UNCHANGED_RUN as usize {
            return None;
        }
    }
    Some(LsUpdate { sub_id, item, fields })
}

/// Percent-decode a TLCP value (UTF-8). Lenient: a malformed escape is kept literally rather than
/// dropped — a decoder that eats bytes hides more than it fixes.
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && s.is_char_boundary(i + 1)
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Fold state for ONE item of a MERGE subscription: current value per schema field. TLCP updates
/// are deltas against this ("unchanged" keeps, `#` nulls, content replaces); the first update of a
/// subscription (the snapshot, with `LS_snapshot=true`) never carries "unchanged", so the fold is
/// correct from the first frame — including across a reconnect, where the fresh snapshot rewrites
/// every field.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeItemState {
    values: Vec<Option<String>>,
}

impl MergeItemState {
    pub fn new(num_fields: usize) -> Self {
        MergeItemState { values: vec![None; num_fields] }
    }

    /// Fold one update's deltas in. Extra fields beyond the schema width are ignored; missing
    /// trailing fields mean "unchanged" (both shapes are tolerated, neither is expected).
    pub fn apply(&mut self, fields: &[FieldDelta]) {
        for (slot, delta) in self.values.iter_mut().zip(fields) {
            match delta {
                FieldDelta::Unchanged => {}
                FieldDelta::Null => *slot = None,
                FieldDelta::Value(v) => *slot = Some(v.clone()),
            }
        }
    }

    /// Current value of field `idx` (`None` = null or never delivered).
    pub fn get(&self, idx: usize) -> Option<&str> {
        self.values.get(idx).and_then(|v| v.as_deref())
    }

    /// Current value of field `idx` parsed as f64.
    pub fn get_f64(&self, idx: usize) -> Option<f64> {
        self.get(idx).and_then(|v| v.parse().ok())
    }

    /// Current value of field `idx` parsed as i64.
    pub fn get_i64(&self, idx: usize) -> Option<i64> {
        self.get(idx).and_then(|v| v.parse().ok())
    }
}

/// The `create_session` WS request (TLCP 2.1.0 WS transport: request name on its own line, then
/// one `&`-joined parameter line; sent as ONE text message). `user`/`password` are IG's
/// Lightstreamer credentials (accountId / `CST-…|XST-…`); the adapter set is IG's `DEFAULT`.
pub fn create_session_request(user: &str, password: &str) -> String {
    format!(
        "create_session\r\n{}",
        form(&[
            ("LS_adapter_set", "DEFAULT"),
            ("LS_cid", LS_CID),
            ("LS_user", user),
            ("LS_password", password),
        ])
    )
}

/// A `control` `LS_op=add` WS request subscribing `group` with `schema` (space-separated fields)
/// in `mode`, served by `data_adapter` (IG: `Pricing` for `PRICE:`, `DEFAULT` for `CHART:`).
/// `LS_session` is deliberately omitted: on a WS transport it defaults to the session
/// bound to this very connection (TLCP 2.1.0, control common parameters).
pub fn subscribe_request(
    req_id: u32,
    sub_id: u32,
    data_adapter: &str,
    mode: &str,
    group: &str,
    schema: &str,
    snapshot: bool,
) -> String {
    format!(
        "control\r\n{}",
        form(&[
            ("LS_reqId", &req_id.to_string()),
            ("LS_op", "add"),
            ("LS_subId", &sub_id.to_string()),
            ("LS_data_adapter", data_adapter),
            ("LS_group", group),
            ("LS_schema", schema),
            ("LS_mode", mode),
            ("LS_snapshot", if snapshot { "true" } else { "false" }),
        ])
    )
}

/// A `control` `LS_op=delete` WS request ending subscription `sub_id` of the session bound to this connection
/// (the session itself, and every other subscription on it, stay). `req_id` is the request's own id, which the
/// server echoes in its `REQOK`/`REQERR`.
pub fn unsubscribe_request(req_id: u32, sub_id: u32) -> String {
    format!(
        "control\r\n{}",
        form(&[
            ("LS_reqId", &req_id.to_string()),
            ("LS_op", "delete"),
            ("LS_subId", &sub_id.to_string()),
        ])
    )
}

/// `application/x-www-form-urlencoded` body from key/value pairs (shared with the exec trade
/// stream's HTTP transport — `crate::exec::stream`).
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        let _ = write!(
            out,
            "{}={}",
            vike_util::percent::percent_encode(k),
            vike_util::percent::percent_encode(v)
        );
    }
    out
}

#[path = "lightstreamer_tests.rs"]
#[cfg(test)]
mod tests;
