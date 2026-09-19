//! Minimal TLCP (Text Lightstreamer Client Protocol 2.1.0) codec — the PURE half of IG's
//! Lightstreamer MARKET-DATA feed (`crate::market_feed`). No I/O lives here: request-building and
//! frame/field decoding only, so every wire shape is unit-testable offline.
//!
//! The subset is deliberately bounded to what one MERGE-mode subscription per WS session needs:
//! `create_session` / `control (LS_op=add)` requests, and the `CONOK`/`CONERR`/`SUBOK`/`REQOK`/
//! `REQERR`/`U`/`PROBE`/`LOOP`/`END` notifications. Everything else the server can say
//! (`CONF`/`OV`/`CS`/`EOS`/`SYNC`/`NOOP`/`PROG`/`CONS`/`SERVNAME`/`CLIENTIP`/`UNSUB`) parses as
//! [`LsFrame::Other`] and is ignored by the driver — none of it affects a MERGE quote/candle lane
//! (`EOS` is never even sent in MERGE mode; the snapshot is a single `U`).
//!
//! Authority: the TLCP 2.1.0 specification ("TLCP Specifications", lightstreamer.com,
//! ls-generic-client 2.1.0). Two spec rules that are easy to get wrong, both pinned by tests here:
//!
//! - **The update grammar is `U,<subscription-ID>,<item>,<f1>|<f2>|...`** — the field list is the
//!   COMMA-separated third argument; only fields among themselves are pipe-separated. (⚠ The exec
//!   trade lane's `crate::event_mapper::parse_update_line` expects a PIPE after `<item>`; see the
//!   crate CLAUDE.md — this module implements the spec's grammar, which the spec's own worked
//!   examples and third-party TLCP clients against IG both use.)
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
/// comma-separated THIRD argument). `None` on a malformed head or `^n` run.
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
/// in `mode`. `LS_session` is deliberately omitted: on a WS transport it defaults to the session
/// bound to this very connection (TLCP 2.1.0, control common parameters).
pub fn subscribe_request(
    req_id: u32,
    sub_id: u32,
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
            ("LS_data_adapter", "DEFAULT"),
            ("LS_group", group),
            ("LS_schema", schema),
            ("LS_mode", mode),
            ("LS_snapshot", if snapshot { "true" } else { "false" }),
        ])
    )
}

/// `application/x-www-form-urlencoded` body from key/value pairs (shared with the exec trade
/// stream's HTTP transport — `crate::stream`).
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        let _ = write!(out, "{}={}", urlencode(k), urlencode(v));
    }
    out
}

/// Minimal percent-encoding for form values (RFC 3986 unreserved kept; everything else encoded).
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd(line: &str) -> LsUpdate {
        match parse_frame(line) {
            Some(LsFrame::Update(u)) => u,
            other => panic!("expected Update for {line:?}, got {other:?}"),
        }
    }

    /// The TLCP 2.1.0 spec's own worked example (chapter 4, "Notification Examples"): a 10-field
    /// stock-quote schema driven through the exact update lines the spec prints, asserting the
    /// folded state the spec's tables show after each line. This is the codec's conformance
    /// fixture — the bytes are the spec's, not ours.
    #[test]
    fn spec_worked_example_folds_exactly() {
        // schema: timestamp price change min max bid ask open close status
        let mut st = MergeItemState::new(10);

        st.apply(&upd("U,3,1,20:00:33|3.04|0.0|2.41|3.67|3.03|3.04|#|#|$").fields);
        assert_eq!(st.get(0), Some("20:00:33"));
        assert_eq!(st.get_f64(1), Some(3.04));
        assert_eq!(st.get(7), None, "open is null");
        assert_eq!(st.get(8), None, "close is null");
        assert_eq!(st.get(9), Some(""), "status is empty");

        st.apply(&upd("U,3,1,20:00:54|3.07|0.98|||3.06|3.07|||Suspended").fields);
        assert_eq!(st.get_f64(3), Some(2.41), "min unchanged");
        assert_eq!(st.get_f64(5), Some(3.06));
        assert_eq!(st.get(9), Some("Suspended"));

        st.apply(&upd("U,3,1,20:04:16|3.02|-0.65|||3.01|3.02|||$").fields);
        assert_eq!(st.get(9), Some(""), "status back to empty via $");

        st.apply(&upd("U,3,1,20:04:40|^4|3.02|3.03|||").fields);
        assert_eq!(st.get(0), Some("20:04:40"));
        assert_eq!(st.get_f64(1), Some(3.02), "price covered by ^4 run");
        assert_eq!(st.get_f64(6), Some(3.03), "ask updated after the run");

        st.apply(&upd("U,3,1,20:06:10|3.05|0.32|^7").fields);
        assert_eq!(st.get_f64(1), Some(3.05));
        assert_eq!(st.get_f64(6), Some(3.03), "ask inside the trailing ^7 run");

        st.apply(&upd("U,3,1,20:06:49|3.08|1.31|||3.08|3.09|||").fields);
        assert_eq!(st.get_f64(5), Some(3.08));
        assert_eq!(st.get(9), Some(""), "status still empty");
    }

    #[test]
    fn update_head_is_comma_separated_per_spec() {
        let u = upd("U,7,2,1.0855|1.0856");
        assert_eq!((u.sub_id, u.item), (7, 2));
        assert_eq!(
            u.fields,
            vec![FieldDelta::Value("1.0855".into()), FieldDelta::Value("1.0856".into())]
        );
        // A single trailing empty segment is one Unchanged, not zero fields.
        assert_eq!(upd("U,1,1,").fields, vec![FieldDelta::Unchanged]);
    }

    #[test]
    fn update_values_percent_decode_and_run_bounds_hold() {
        let u = upd("U,1,1,a%7Cb|%23leading|c%2Cd");
        assert_eq!(
            u.fields,
            vec![
                FieldDelta::Value("a|b".into()),
                FieldDelta::Value("#leading".into()),
                FieldDelta::Value("c,d".into()),
            ]
        );
        // ^0 and an absurd run are refused (whole line demoted to Other, never a panic).
        assert!(matches!(parse_frame("U,1,1,^0|x"), Some(LsFrame::Other(_))));
        assert!(matches!(parse_frame("U,1,1,^99999"), Some(LsFrame::Other(_))));
    }

    #[test]
    fn control_frames_parse() {
        assert_eq!(
            parse_frame("CONOK,S1a2b3c,50000,5000,*"),
            Some(LsFrame::Conok {
                session_id: "S1a2b3c".into(),
                request_limit: 50000,
                keepalive_ms: 5000,
                control_link: "*".into(),
            })
        );
        assert_eq!(
            parse_frame("CONERR,2,Requested%20Adapter%20Set%20not%20available"),
            Some(LsFrame::Conerr {
                code: "2".into(),
                message: "Requested Adapter Set not available".into()
            })
        );
        assert_eq!(
            parse_frame("SUBOK,1,1,4"),
            Some(LsFrame::Subok { sub_id: 1, num_items: 1, num_fields: 4 })
        );
        assert_eq!(parse_frame("REQOK,1"), Some(LsFrame::Reqok));
        assert_eq!(
            parse_frame("REQERR,1,17,Data%20Adapter%20not%20found"),
            Some(LsFrame::Reqerr {
                req_id: "1".into(),
                code: "17".into(),
                message: "Data Adapter not found".into()
            })
        );
        assert_eq!(parse_frame("PROBE"), Some(LsFrame::Probe));
        assert_eq!(parse_frame("LOOP,0"), Some(LsFrame::Loop));
        assert_eq!(
            parse_frame("END,31,Session%20closed"),
            Some(LsFrame::End { code: "31".into(), message: "Session closed".into() })
        );
        assert_eq!(
            parse_frame("SERVNAME,Lightstreamer"),
            Some(LsFrame::Other("SERVNAME,Lightstreamer".into()))
        );
        assert_eq!(parse_frame(""), None);
    }

    #[test]
    fn request_builders_pin_the_wire_bytes() {
        assert_eq!(
            create_session_request("ABC123", "CST-c|XST-x"),
            "create_session\r\nLS_adapter_set=DEFAULT&LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg\
             &LS_user=ABC123&LS_password=CST-c%7CXST-x"
        );
        assert_eq!(
            subscribe_request(2, 1, "MERGE", "MARKET:CS.D.EURUSD.MINI.IP", "BID OFFER", true),
            "control\r\nLS_reqId=2&LS_op=add&LS_subId=1&LS_data_adapter=DEFAULT\
             &LS_group=MARKET%3ACS.D.EURUSD.MINI.IP&LS_schema=BID%20OFFER&LS_mode=MERGE\
             &LS_snapshot=true"
        );
    }
}
