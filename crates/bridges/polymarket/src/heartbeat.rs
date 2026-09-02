//! `heartbeat` — the OPT-IN, EXEC-side server-side dead-man heartbeat for a Polymarket account.
//!
//! Polymarket's CLOB exposes a **dead-man switch**: while an account keeps POSTing
//! `/v1/heartbeats`, its resting orders live; if **no valid beat lands within ~10 s** the VENUE
//! cancels **every** open order the account holds. This module owns the beat: an EXEC-side thread
//! (NOT the market-data pump) that L2-signs a POST to [`HEARTBEAT_PATH`] every [`DEFAULT_BEAT_INTERVAL`]
//! (5 s), carrying the rolling `heartbeat_id`, with a 400 resync arm.
//!
//! ## Venue mechanics (live-probed 2026-07-22)
//! - Host is [`CLOB_BASE`](crate::config::CLOB_BASE) = `https://clob.polymarket.com`; path
//!   `/v1/heartbeats`. (NOT `clob-v2.polymarket.com`, which is NXDOMAIN.)
//! - Auth is the SAME L2 HMAC every signed CLOB call uses ([`crate::auth`] via
//!   [`crate::exec::post_signed_raw`]) — `POLY_API_KEY`/`POLY_ADDRESS`/`POLY_SIGNATURE`/
//!   `POLY_PASSPHRASE`/`POLY_TIMESTAMP`, signing the EXACT body string sent.
//! - The FIRST beat sends `{"heartbeat_id": null}`; the venue replies
//!   `{"heartbeat_id":"<uuid>","error":null}`. Every SUBSEQUENT beat echoes that id.
//! - A wrong/expired id gets `HTTP 400 {"error":"Invalid Heartbeat ID","heartbeat_id":"<correct>"}`:
//!   **resync** to the id in the response and **retry — do NOT restart** the session
//!   ([`interpret_beat`] → [`BeatOutcome::Resync`], [`beat_once`] retries in-tick).
//! - Armed IMPLICITLY by starting to send; per-ACCOUNT scope (not per-market).
//!
//! ## Two hard constraints (read before wiring this anywhere)
//! 1. **Per-account ⇒ a single owner.** The switch is scoped to the whole account, so exactly ONE
//!    vike process may beat for a given key. Two instances sharing the same L2 creds racing beats
//!    against one account is UNDEFINED (the venue tracks one rolling id; interleaved beats resync
//!    each other into a fight). Own the beat from a single composition root, never two.
//! 2. **FAIL-CLOSED, and the ~10 s deadline is AGGRESSIVE vs vike's reconnect windows.** A stalled
//!    beat does not merely disconnect a feed — it **wipes every resting quote** at the venue. So the
//!    beat MUST run on its OWN cadence and OWN thread, wholly independent of WS reconnect state: a
//!    market-feed/user-channel reconnect storm must never pause the beat, and the beat must never
//!    wait on any WS lane. That is why this is a dedicated thread with its own timer (below), not a
//!    hook on the pump's reconnect loop. A single rejected beat is recovered WITHIN the tick by an
//!    immediate resync-retry (see [`beat_once`]) precisely so one bad id does not cost a whole 5 s
//!    cycle and breach the 10 s window.
//!
//! ## Opt-in, OFF by default (byte-identical when unset)
//! [`HeartbeatPoller::spawn`] returns `None` — spawning NOTHING, making NO network call — unless
//! creds are present AND [`heartbeat_enabled`] (`POLY_HEARTBEAT=1`, the EXACT string, the
//! `VIKE_RECONCILE` idiom, read from the process env OR the workspace `.env` map). Unset ⇒ the
//! account is NOT dead-man-protected and the build is byte-identical to before this module existed.
//!
//! The network op is a ONE-method [`HeartbeatTransport`] seam so the whole beat/resync brain is
//! unit-tested offline against a scripted stub (no network) — the crate's `RedeemDeps` idiom.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::poller::{sleep_stop_aware, spawn_poller, StopHandle, STOP_POLL_SLICE};

use crate::config::{first_token, PolymarketCreds, CLOB_BASE};
use crate::exec::post_signed_raw;

/// The dead-man heartbeat endpoint path (host is [`CLOB_BASE`]).
pub const HEARTBEAT_PATH: &str = "/v1/heartbeats";

/// Default beat cadence: every 5 s. The venue cancels ALL open orders if no valid beat lands within
/// ~10 s, so 5 s leaves one whole missed beat of slack before the switch fires.
pub const DEFAULT_BEAT_INTERVAL: Duration = Duration::from_secs(5);

/// The workspace-`.env` / env name of the heartbeat opt-in (see [`heartbeat_enabled`]).
pub const HEARTBEAT_ENV: &str = "POLY_HEARTBEAT";

/// The opt-in gate: the EXACT string `"1"` in the process env **or** in the workspace `.env` map,
/// default OFF — the `VIKE_RECONCILE` / [`crate::recon_client::poly_reconcile_enabled`] idiom.
///
/// Read from BOTH on purpose: every other Polymarket setting lives in the `.env`, and a flag
/// silently ignored there is the exact class of bug [`crate::egress::proxy_url`] was fixed for; a
/// shell-exported flag still WINS. Unset (the default) ⇒ [`HeartbeatPoller::spawn`] builds nothing
/// and makes no network call — byte-identical to before this existed.
pub fn heartbeat_enabled(vars: &HashMap<String, String>) -> bool {
    std::env::var(HEARTBEAT_ENV).as_deref() == Ok("1")
        || vars.get(HEARTBEAT_ENV).map(|v| first_token(v)) == Some("1")
}

// --- the rolling state + the pure resync brain --------------------------------------------------

/// The interpreted result of ONE beat POST — the pure classification [`beat_once`] acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BeatOutcome {
    /// A valid (2xx) beat; carries the venue's rolling id to echo on the next beat.
    Ok(String),
    /// A `400 {"error":"Invalid Heartbeat ID","heartbeat_id":"<correct>"}` (or any non-2xx that
    /// still hands back a usable id) — resync to that id and retry, never restart the session.
    Resync(String),
    /// Any other failure (a 2xx with no id, or a non-2xx with no corrected id). The rolling id is
    /// left UNCHANGED so the next beat retries with it — fail-soft, never fatal to the task.
    Err(String),
}

/// The rolling dead-man state: the venue-issued `heartbeat_id` to echo, or `None` before the first
/// beat (which sends `heartbeat_id: null` to OPEN a fresh session).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HeartbeatState {
    heartbeat_id: Option<String>,
}

impl HeartbeatState {
    /// A fresh state — the next [`body`](Self::body) sends `heartbeat_id: null`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The request body for the NEXT beat: `{"heartbeat_id": null}` before the first valid beat
    /// (opens the session), else `{"heartbeat_id": "<id>"}` echoing the venue's rolling id.
    pub fn body(&self) -> Value {
        match &self.heartbeat_id {
            Some(id) => serde_json::json!({ "heartbeat_id": id }),
            None => serde_json::json!({ "heartbeat_id": null }),
        }
    }

    /// The rolling id currently in effect (for the poller's observability + the offline tests).
    pub fn heartbeat_id(&self) -> Option<&str> {
        self.heartbeat_id.as_deref()
    }

    /// Fold one [`BeatOutcome`] into the rolling id. `Ok`/`Resync` adopt the venue's id; `Err`
    /// deliberately leaves the id unchanged so the next beat retries with it.
    fn apply(&mut self, outcome: &BeatOutcome) {
        match outcome {
            BeatOutcome::Ok(id) | BeatOutcome::Resync(id) => {
                self.heartbeat_id = Some(id.clone());
            }
            BeatOutcome::Err(_) => {}
        }
    }
}

/// Classify one beat's `(http_status, parsed_body)`. PURE — the resync brain of the dead-man.
///
/// - 2xx WITH a non-empty `heartbeat_id` → [`BeatOutcome::Ok`] (the venue's rolling id).
/// - non-2xx WITH a non-empty `heartbeat_id` → [`BeatOutcome::Resync`]: the documented
///   `400 Invalid Heartbeat ID` path hands back the correct id; any non-2xx that still supplies a
///   usable id is treated the same — adopt it and retry, keep the beat alive.
/// - anything else (2xx with no id, non-2xx with no id) → [`BeatOutcome::Err`].
fn interpret_beat(status: u16, body: &Value) -> BeatOutcome {
    let hb_id = body
        .get("heartbeat_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if (200..300).contains(&status) {
        match hb_id {
            Some(id) => BeatOutcome::Ok(id),
            None => BeatOutcome::Err(format!(
                "heartbeat {status}: 2xx response carried no heartbeat_id"
            )),
        }
    } else if let Some(id) = hb_id {
        BeatOutcome::Resync(id)
    } else {
        let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
        BeatOutcome::Err(format!("heartbeat {status}: {err}"))
    }
}

/// What one [`beat_once`] tick did — for the poller's logging + the offline tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeatReport {
    /// A valid beat landed; carries the rolling id now in effect.
    Beat(String),
    /// A resync happened AND the immediate in-tick retry also did not confirm (two rejections in a
    /// row — rare). The newest corrected id is stored; the next tick retries with it.
    ResyncPending,
    /// The beat failed (network error, or an unexpected body). The rolling id is unchanged and the
    /// next tick retries. NOT fatal — but if it PERSISTS the venue will wipe resting orders.
    Failed(String),
}

/// The one network op the beat needs — a test seam. [`ProdTransport`] L2-signs the real POST;
/// offline tests inject a scripted stub (no network). Returns `(http_status, parsed_body)` for ANY
/// status because the 400 resync body must be READ, not collapsed into an error.
pub trait HeartbeatTransport {
    fn post_heartbeat(&self, body: &Value) -> Result<(u16, Value), String>;
}

/// The production transport: an L2-signed POST to [`HEARTBEAT_PATH`] over the shared proxy-aware
/// `ureq` agent ([`crate::exec::post_signed_raw`]). No new HTTP client — the same transport every
/// signed CLOB call already uses.
pub struct ProdTransport {
    creds: PolymarketCreds,
    base: String,
}

impl ProdTransport {
    /// Build against the production CLOB host. `creds` must carry the L2 trio (api_key/secret/
    /// passphrase) — the beat is L2-signed.
    pub fn new(creds: PolymarketCreds) -> Self {
        ProdTransport { creds, base: CLOB_BASE.to_string() }
    }
}

impl HeartbeatTransport for ProdTransport {
    fn post_heartbeat(&self, body: &Value) -> Result<(u16, Value), String> {
        post_signed_raw(&self.base, HEARTBEAT_PATH, &self.creds, body)
    }
}

/// Send ONE beat, resyncing in-tick on a 400. Bounded to TWO POSTs: the beat, plus one immediate
/// retry if the venue rejected the id and handed back a corrected one — so a stale id is recovered
/// inside this tick rather than 5 s later, which would risk the ~10 s deadline (see the module doc).
/// Two rejections in a row give up for this tick (`ResyncPending`); the corrected id is stored and
/// the next tick retries. `state` is mutated in place with the newest id the venue acknowledged.
pub fn beat_once(deps: &dyn HeartbeatTransport, state: &mut HeartbeatState) -> BeatReport {
    for _ in 0..2 {
        let body = state.body();
        let (status, resp) = match deps.post_heartbeat(&body) {
            Ok(x) => x,
            Err(e) => return BeatReport::Failed(e),
        };
        let outcome = interpret_beat(status, &resp);
        state.apply(&outcome);
        match outcome {
            BeatOutcome::Ok(id) => return BeatReport::Beat(id),
            BeatOutcome::Err(e) => return BeatReport::Failed(e),
            // corrected id adopted by `apply`; loop to retry immediately (bounded to one retry).
            BeatOutcome::Resync(_) => continue,
        }
    }
    // Two resyncs in a row: the newest corrected id is stored; the next tick will use it.
    BeatReport::ResyncPending
}

// --- the owner handle + the spawned poller ------------------------------------------------------

/// Owner-side handle: stop-aware shutdown of the beat thread, `Drop`-joining — the shared
/// [`StopHandle`] scaffold (`vike_bridge_core::poller`), so a dropped handle never leaks the
/// background thread (the crate's discipline — see `raw_tap.rs` / `auto_redeem.rs`).
pub type HeartbeatHandle = StopHandle;

/// The dead-man beat poller.
pub struct HeartbeatPoller;

impl HeartbeatPoller {
    /// Spawn the beat thread — or NOTHING (returns `None`, makes no network call) unless creds are
    /// present, [`heartbeat_enabled`] is on, AND the L2 trio needed to sign is populated. The caller
    /// passes `creds: Option<PolymarketCreds>` from the normal absent-credentials-is-the-live-gate
    /// load path (`config::load_polymarket_creds_from`), already L2-derived.
    ///
    /// The thread loop, starting IMMEDIATELY (arms the switch at once), then every `interval`: run
    /// one [`beat_once`] pass. A failed/pending beat is `warn!`-logged and retried next tick — the
    /// task never stops itself, because stopping is exactly what wipes the account's resting orders.
    /// The loop is wholly independent of any WS lane's reconnect state (constraint 2, module doc).
    pub fn spawn(
        creds: Option<PolymarketCreds>,
        vars: &HashMap<String, String>,
        interval: Duration,
    ) -> Option<HeartbeatHandle> {
        let creds = creds?;
        if !heartbeat_enabled(vars) {
            return None;
        }
        if creds.api_key.is_empty() || creds.secret.is_empty() {
            tracing::warn!(
                target: "vike_polymarket::heartbeat",
                "polymarket heartbeat unwired: no L2 creds to sign the beat"
            );
            return None;
        }

        Some(spawn_poller("vike-polymarket-heartbeat", move |stop| {
            let deps = ProdTransport::new(creds);
            let mut state = HeartbeatState::new();
            while !stop.load(Ordering::Relaxed) {
                match beat_once(&deps, &mut state) {
                    BeatReport::Beat(_) => {}
                    BeatReport::ResyncPending => tracing::warn!(
                        target: "vike_polymarket::heartbeat",
                        "heartbeat resynced twice this tick — corrected id stored, retrying next beat"
                    ),
                    BeatReport::Failed(e) => tracing::warn!(
                        target: "vike_polymarket::heartbeat",
                        error = %e,
                        "heartbeat beat failed — retrying next beat (resting orders at risk if this persists)"
                    ),
                }
                if sleep_stop_aware(&stop, interval, STOP_POLL_SLICE) {
                    break;
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    // --- request body shape: first call null, then echo the venue id ---------------------------

    #[test]
    fn body_shape_first_call_null_then_echoes_the_id() {
        let mut st = HeartbeatState::new();
        // First beat opens the session with an explicit JSON null.
        let b0 = st.body();
        assert!(b0["heartbeat_id"].is_null(), "first beat sends heartbeat_id: null");
        assert!(st.heartbeat_id().is_none());
        // After a valid beat, subsequent beats echo the id verbatim.
        st.apply(&BeatOutcome::Ok("hb-uuid-1".into()));
        let b1 = st.body();
        assert_eq!(b1["heartbeat_id"], "hb-uuid-1");
        assert_eq!(st.heartbeat_id(), Some("hb-uuid-1"));
    }

    // --- the pure classifier: success / resync / error ------------------------------------------

    #[test]
    fn interpret_classifies_success_resync_and_error() {
        // 200 with an id → Ok (the first-beat success shape).
        assert_eq!(
            interpret_beat(200, &json!({ "heartbeat_id": "u1", "error": null })),
            BeatOutcome::Ok("u1".into())
        );
        // The documented 400 → Resync carrying the CORRECT id.
        assert_eq!(
            interpret_beat(400, &json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "u2" })),
            BeatOutcome::Resync("u2".into())
        );
        // 2xx with no usable id → Err (defensive; leaves state alone).
        assert!(matches!(interpret_beat(200, &json!({ "error": null })), BeatOutcome::Err(_)));
        assert!(matches!(interpret_beat(200, &json!({ "heartbeat_id": "" })), BeatOutcome::Err(_)));
        // A non-2xx with no corrected id (e.g. an auth failure) → Err, not Resync.
        assert!(matches!(
            interpret_beat(401, &json!({ "error": "unauthorized" })),
            BeatOutcome::Err(_)
        ));
    }

    /// A scripted transport: replies are popped in order; every posted body is recorded so a test
    /// can assert exactly what the beat put on the wire.
    struct Scripted {
        calls: RefCell<Vec<Value>>,
        replies: RefCell<VecDeque<Result<(u16, Value), String>>>,
    }
    impl Scripted {
        fn new(replies: Vec<Result<(u16, Value), String>>) -> Self {
            Scripted { calls: RefCell::new(Vec::new()), replies: RefCell::new(replies.into()) }
        }
    }
    impl HeartbeatTransport for Scripted {
        fn post_heartbeat(&self, body: &Value) -> Result<(u16, Value), String> {
            self.calls.borrow_mut().push(body.clone());
            self.replies.borrow_mut().pop_front().expect("a scripted reply for each beat")
        }
    }

    // --- the 400-resync arm: the NEXT beat uses the corrected id ---------------------------------

    #[test]
    fn resync_then_the_retry_uses_the_corrected_id() {
        // First POST → 400 Invalid-Heartbeat-ID(correct-id); the immediate retry → 200 echo.
        let scripted = Scripted::new(vec![
            Ok((400, json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "correct-id" }))),
            Ok((200, json!({ "heartbeat_id": "correct-id", "error": null }))),
        ]);
        // Start with a STALE id so the first beat is the one that gets rejected.
        let mut st = HeartbeatState::new();
        st.apply(&BeatOutcome::Ok("stale-id".into()));

        let report = beat_once(&scripted, &mut st);
        assert_eq!(report, BeatReport::Beat("correct-id".into()));

        let calls = scripted.calls.borrow();
        assert_eq!(calls.len(), 2, "one rejected beat + one in-tick retry");
        assert_eq!(calls[0]["heartbeat_id"], "stale-id", "the first beat sent the stale id");
        assert_eq!(
            calls[1]["heartbeat_id"], "correct-id",
            "the retry sent the venue's corrected id"
        );
        assert_eq!(st.heartbeat_id(), Some("correct-id"), "rolling id resynced to the venue's");
    }

    #[test]
    fn two_resyncs_in_a_row_store_the_newest_id_for_next_tick() {
        let scripted = Scripted::new(vec![
            Ok((400, json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "id-b" }))),
            Ok((400, json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "id-c" }))),
        ]);
        let mut st = HeartbeatState::new();
        st.apply(&BeatOutcome::Ok("id-a".into()));
        let report = beat_once(&scripted, &mut st);
        assert_eq!(report, BeatReport::ResyncPending, "gave up this tick after the bounded retry");
        assert_eq!(scripted.calls.borrow().len(), 2, "bounded to two POSTs, no infinite loop");
        assert_eq!(
            st.heartbeat_id(),
            Some("id-c"),
            "the newest corrected id is stored for next tick"
        );
    }

    #[test]
    fn a_network_error_is_failed_and_keeps_the_rolling_id() {
        // A failed beat must NOT lose the id — the next tick retries with it (fail-soft).
        let scripted = Scripted::new(vec![Err("network: connection refused".into())]);
        let mut st = HeartbeatState::new();
        st.apply(&BeatOutcome::Ok("keep-me".into()));
        let report = beat_once(&scripted, &mut st);
        assert!(matches!(report, BeatReport::Failed(_)));
        assert_eq!(st.heartbeat_id(), Some("keep-me"), "a failed beat leaves the id for the retry");
    }

    #[test]
    fn first_beat_success_arms_from_null() {
        // The whole first-beat happy path: send null → 200 with a fresh id → id adopted.
        let scripted =
            Scripted::new(vec![Ok((200, json!({ "heartbeat_id": "fresh", "error": null })))]);
        let mut st = HeartbeatState::new();
        let report = beat_once(&scripted, &mut st);
        assert_eq!(report, BeatReport::Beat("fresh".into()));
        assert!(scripted.calls.borrow()[0]["heartbeat_id"].is_null(), "opened with null");
        assert_eq!(st.heartbeat_id(), Some("fresh"));
    }

    // --- the OFF gate: unset ⇒ nothing spawned --------------------------------------------------

    #[test]
    fn gate_is_off_by_default_and_exact() {
        assert!(!heartbeat_enabled(&vars(&[])));
        assert!(heartbeat_enabled(&vars(&[(HEARTBEAT_ENV, "1")])));
        // `.env` padding + the trailing-inline-comment style the real workspace `.env` writes.
        assert!(heartbeat_enabled(&vars(&[(HEARTBEAT_ENV, " 1 ")])));
        assert!(heartbeat_enabled(&vars(&[(HEARTBEAT_ENV, "1   # dead-man on")])));
        // Only the exact `1` is on — every fuzzy truthy spelling stays OFF (the VIKE_RECONCILE rule).
        for off in ["0", "true", "yes", "on", "", "11", "1x"] {
            assert!(!heartbeat_enabled(&vars(&[(HEARTBEAT_ENV, off)])), "{off}");
        }
    }

    #[test]
    fn spawn_returns_none_without_creds() {
        // creds are checked BEFORE the env gate, so this is env-independent AND spawns no thread.
        let h = HeartbeatPoller::spawn(None, &vars(&[]), DEFAULT_BEAT_INTERVAL);
        assert!(h.is_none(), "no creds ⇒ nothing spawned");
    }

    #[test]
    fn spawn_returns_none_when_gate_off() {
        // With creds present but the gate absent from the map (and, in CI/dev, the ambient process
        // env), spawn builds nothing and makes no network call — the byte-identical-when-off
        // guarantee. No env MUTATION here on purpose: `heartbeat_enabled` reads the process env, and
        // mutating it would race the getenv in `gate_is_off_by_default_and_exact` (unsetenv/getenv is
        // UB under threads). This relies on an ambient-clean `POLY_HEARTBEAT`, the exact assumption
        // `recon_client`'s `reconcile_gate_is_off_by_default_and_exact` already makes.
        let creds = PolymarketCreds {
            api_key: "k".into(),
            secret: "cG9seW1hcmtldA==".into(),
            ..Default::default()
        };
        let h = HeartbeatPoller::spawn(Some(creds), &vars(&[]), DEFAULT_BEAT_INTERVAL);
        assert!(h.is_none(), "gate off ⇒ nothing spawned");
    }
}
