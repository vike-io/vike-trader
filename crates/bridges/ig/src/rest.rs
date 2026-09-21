//! IG REST session: exchange creds for `CST` / `X-SECURITY-TOKEN` at `POST /session`, then attach
//! those + the API key on every authenticated call. Blocking (ureq, rustls).
//!
//! ## Re-authentication lives HERE, not in the slices
//!
//! ⚠ This doc used to say a long-lived session "must re-login on 401 (handled by the exec/stream
//! slices)". It was not: no call site in this crate inspected a status code for re-authentication,
//! so an expired session turned every submit into an `OrderRejected` carrying an IG error string
//! and left the trade stream re-dialling with dead tokens, forever, with nothing in the log saying
//! why. A restart of the process was the only cure. The recovery is now this type's own job, so a
//! slice cannot forget it and every reader of a session gets it for free.
//!
//! ### The 401 ladder
//!
//! Measured against `demo-api.ig.com` (2026-08-21): a dead `CST` answers **401**
//! `error.security.client-token-invalid`, a dead `X-SECURITY-TOKEN` **401**
//! `error.security.account-token-invalid`, and absent tokens **401**
//! `error.security.client-token-missing`. So a 401 — and only a 401 — is the re-login trigger;
//! every other status is the endpoint's own answer and is returned untouched.
//!
//! The retry is **exactly one, never a loop**. [`reauth_ladder`] snapshots the token GENERATION,
//! runs the call, and on a 401 asks the re-login to advance that generation; it then retries ONCE.
//! A second 401 after a fresh login is a real refusal (revoked key, changed password, a locked
//! account) — it is logged at `error` and returned to the caller with IG's own `errorCode` intact.
//! A permanent rejection stops and says why; it does not spin.
//!
//! The generation counter is what makes a SHARED session safe: two threads that both see a 401
//! produce ONE login, because the second finds the generation already advanced and simply retries
//! with the pair the first fetched.

/// An IG REST error, normalized to (status, message). Message is IG's `errorCode` when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for IgApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IG error {}: {}", self.status, self.message)
    }
}

impl std::error::Error for IgApiError {}

/// The HTTP status IG answers with when the session token pair is expired, wrong or absent — the
/// ONE trigger for a re-login. Measured against `demo-api.ig.com` (2026-08-21); see the module doc
/// for the three `errorCode`s that ride it.
const REAUTH_STATUS: u16 = 401;

/// The mutable half of a session: the pair that EXPIRES, plus the generation counter that makes a
/// concurrent re-login idempotent.
///
/// `account_id` / `lightstreamer_endpoint` deliberately stay OUTSIDE this: they describe the
/// ACCOUNT rather than the credential, a re-login of the same identifier answers with the same two,
/// and leaving them immutable `pub` fields is what lets every existing reader compile unchanged.
struct Tokens {
    cst: String,
    security_token: String,
    /// Bumped on every successful (re-)login. A caller that saw generation `g`, then a 401, knows
    /// its pair is stale iff the generation is STILL `g` — otherwise somebody else already fixed it.
    generation: u64,
}

/// The `POST /session` reply, split out so [`IgSession::login`] and [`IgSession::relogin`] share
/// one parser rather than two copies that could drift about which header carries which token.
struct LoginOutcome {
    cst: String,
    security_token: String,
    account_id: String,
    lightstreamer_endpoint: String,
}

/// An authenticated IG session (holds the session tokens). Created by [`IgSession::login`].
///
/// **Shareable**: the expiring pair sits behind a `Mutex`, so one `Arc<IgSession>` can back the
/// exec command thread and the [`ExecutionClient::confirm`](vike_exec::ExecutionClient::confirm)
/// worker at once, and a 401 either of them meets re-authenticates the session for BOTH.
pub struct IgSession {
    agent: ureq::Agent,
    api_key: String,
    base: String,
    /// The credentials this session re-logs-in with. Held because recovery is this type's job (see
    /// the module doc): without it every caller would need its own 401 ladder — and none had one.
    config: crate::config::IgConfig,
    tokens: std::sync::Mutex<Tokens>,
    /// the account the session is scoped to (from the login response).
    pub account_id: String,
    /// Lightstreamer streaming endpoint from the login response (trade-confirm/position/order stream).
    pub lightstreamer_endpoint: String,
}

impl IgSession {
    /// The IG Lightstreamer password: `CST-<cst>|XST-<security_token>` (paired with `account_id` as
    /// the LS user). Used by the trade-update stream driver.
    ///
    /// ⚠ **Read it per DIAL, never once per thread.** It renders the CURRENT pair, so a driver that
    /// caches the string across reconnects keeps re-presenting dead tokens after a re-login — which
    /// is exactly what `crates/bridges/ig/src/stream.rs` used to do through its snapshot `LsParams`.
    pub fn ls_password(&self) -> String {
        let t = self.tokens.lock().unwrap();
        format!("CST-{}|XST-{}", t.cst, t.security_token)
    }

    /// The current token generation — `1` after the first login, `+1` per successful re-login.
    /// Exposed so a caller that wants to force recovery can pass it to [`Self::relogin`] and learn
    /// whether its own view was the stale one.
    pub fn token_generation(&self) -> u64 {
        self.tokens.lock().unwrap().generation
    }

    /// Re-authenticate this session in place, returning the generation now in force.
    ///
    /// `seen` is the generation the caller was using when it decided the pair was dead. If the
    /// current generation has already moved past it, ANOTHER caller re-logged-in in the meantime
    /// and this is a no-op returning the new generation — that guard is the whole reason a shared
    /// session cannot stampede the login endpoint.
    ///
    /// A failure is logged at `error` (an operator watching a venue that rejects everything needs
    /// to be told the session died, which is precisely what nothing used to say) and returned; the
    /// old pair is left in place, so a transient network failure does not also destroy the tokens.
    pub fn relogin(&self, seen: u64) -> Result<u64, IgApiError> {
        // The lock is held ACROSS the login round trip on purpose: that is what collapses a
        // concurrent stampede into one login. There is no reentrancy here (`login_once` takes only
        // the agent and the config), so it cannot deadlock.
        let mut guard = self.tokens.lock().unwrap();
        if guard.generation != seen {
            return Ok(guard.generation);
        }
        match Self::login_once(&self.agent, &self.config) {
            Ok(out) => {
                guard.cst = out.cst;
                guard.security_token = out.security_token;
                guard.generation += 1;
                tracing::info!(
                    target: "vike_ig::rest",
                    account = %self.account_id,
                    generation = guard.generation,
                    "IG session re-authenticated after token expiry"
                );
                Ok(guard.generation)
            }
            Err(e) => {
                tracing::error!(
                    target: "vike_ig::rest",
                    account = %self.account_id,
                    status = e.status,
                    error = %e.message,
                    "IG re-login FAILED — this session cannot recover on its own; authenticated \
                     calls will keep failing until the credentials or the network are fixed"
                );
                Err(e)
            }
        }
    }

    /// Run one authenticated call through the 401 ladder and parse its body.
    ///
    /// `run(cst, xst)` performs the HTTP with the pair it is handed and returns the raw
    /// `(status, body)` — deliberately NOT a parsed `Result`, because the ladder must see the
    /// status before [`Self::finish`] turns a non-2xx into an `Err` and loses it.
    fn call_with_reauth<F>(&self, run: F) -> Result<serde_json::Value, IgApiError>
    where
        F: Fn(&str, &str) -> Result<(u16, String), IgApiError>,
    {
        let (status, text) =
            reauth_ladder(&self.tokens, &self.account_id, run, |seen| self.relogin(seen))?;
        Self::finish(status, &text)
    }
}

/// The 401 ladder itself, as a free function over the token cell so its semantics — *exactly one*
/// retry, and only after a re-login that actually advanced the generation — are testable with no
/// session, no agent and no network. Returns the raw `(status, body)` of whichever attempt was
/// last, for the caller to parse.
///
/// A transport error (`Err` from `run`) is returned immediately and is NEVER a re-login trigger: a
/// dead socket says nothing about whether the token is alive, and re-logging-in on one would turn
/// an outage into a login storm.
fn reauth_ladder<R, L>(
    tokens: &std::sync::Mutex<Tokens>,
    account: &str,
    run: R,
    relogin: L,
) -> Result<(u16, String), IgApiError>
where
    R: Fn(&str, &str) -> Result<(u16, String), IgApiError>,
    L: Fn(u64) -> Result<u64, IgApiError>,
{
    let (cst, xst, generation) = {
        let t = tokens.lock().unwrap();
        (t.cst.clone(), t.security_token.clone(), t.generation)
    };
    let first = run(&cst, &xst)?;
    if first.0 != REAUTH_STATUS {
        return Ok(first);
    }
    tracing::warn!(
        target: "vike_ig::rest",
        %account,
        "IG answered 401 — re-authenticating, then retrying this call ONCE"
    );
    if relogin(generation).is_err() {
        // The re-login already logged WHY at `error`. Hand the caller IG's original 401 body so the
        // error it surfaces still carries the venue's own `errorCode`.
        return Ok(first);
    }
    let (cst, xst) = {
        let t = tokens.lock().unwrap();
        (t.cst.clone(), t.security_token.clone())
    };
    let second = run(&cst, &xst)?;
    if second.0 == REAUTH_STATUS {
        tracing::error!(
            target: "vike_ig::rest",
            %account,
            "IG answered 401 AGAIN on a FRESH session — the credentials are being refused, not \
             expiring. NOT retrying further"
        );
    }
    Ok(second)
}

impl IgSession {
    fn net_err(e: impl std::fmt::Display) -> IgApiError {
        IgApiError { status: 0, message: format!("network error: {e}") }
    }

    fn finish(status: u16, text: &str) -> Result<serde_json::Value, IgApiError> {
        if (200..300).contains(&status) {
            return serde_json::from_str(text)
                .map_err(|e| IgApiError { status, message: format!("bad json: {e}") });
        }
        let message = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|b| b.get("errorCode").and_then(|c| c.as_str()).map(str::to_string))
            .unwrap_or_else(|| text.to_string());
        Err(IgApiError { status, message })
    }

    fn agent() -> ureq::Agent {
        vike_bridge_core::http::blocking_agent()
    }

    /// IG's server clock, in epoch ms — **without logging in**.
    ///
    /// `GET /session/encryptionKey` (`Version: 1`) needs the per-app API KEY ONLY: no `POST
    /// /session`, no `CST`, no `X-SECURITY-TOKEN`, so it costs one round trip and creates no
    /// session to expire. Its body carries `timeStamp`, IG's own clock as a plain epoch-ms integer
    /// alongside the (unused) RSA key. Measured against `demo-api.ig.com`: +23..25 ms of skew over
    /// an 82-94 ms round trip from the CI box (2026-08-08), and +160 ms over a 444 ms round trip from
    /// the Windows dev box (2026-08-09 — the capture that produced this crate's fixture).
    ///
    /// `timeout` is the caller's ceiling on the whole call, on a dedicated agent rather than the
    /// shared 30 s one: the only caller is a pre-mount preflight nobody is waiting on
    /// (`vike_mount::server_time`'s `CLOCK_READ_TIMEOUT`), and a clock canary that can park a mount
    /// for half a minute is worse than the warning it produces.
    ///
    /// ⚠ Without the `X-IG-API-KEY` header this endpoint is HTTP **400**, not 200 — it is
    /// credential-gated, not public. It is still safe for a preflight because IG's live gate
    /// (`load_ig_config_from`) has already resolved that key by the time anything mounts.
    ///
    /// ⚠ A drifted clock cannot get an IG order rejected: IG authenticates with the session token
    /// pair above and stamps no timestamp on a request, so this is a HOST-HEALTH canary rather than
    /// a rejection guard (see `vike_mount::server_time`'s `ClockRisk`).
    ///
    /// The RSA key in the same body is deliberately ignored, and no secret is read or returned.
    pub fn server_time_ms(
        config: &crate::config::IgConfig,
        timeout: std::time::Duration,
    ) -> Result<i64, IgApiError> {
        let agent = vike_bridge_core::http::blocking_agent_with_timeout(timeout);
        let url = format!("{}/session/encryptionKey", config.rest_base);
        let mut resp = agent
            .get(&url)
            .header("X-IG-API-KEY", &config.api_key)
            .header("Version", "1")
            .header("Accept", "application/json")
            .call()
            .map_err(Self::net_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        parse_server_time_ms(&Self::finish(status, &text)?, status)
    }

    /// Log in (v2 `/session`) and capture the `CST` / `X-SECURITY-TOKEN` response headers.
    pub fn login(config: &crate::config::IgConfig) -> Result<Self, IgApiError> {
        let agent = Self::agent();
        let out = Self::login_once(&agent, config)?;
        Ok(Self {
            agent,
            api_key: config.api_key.clone(),
            base: config.rest_base.clone(),
            config: config.clone(),
            tokens: std::sync::Mutex::new(Tokens {
                cst: out.cst,
                security_token: out.security_token,
                generation: 1,
            }),
            account_id: out.account_id,
            lightstreamer_endpoint: out.lightstreamer_endpoint,
        })
    }

    /// One `POST /session` round trip → the four things a session is made of. Shared by
    /// [`Self::login`] and [`Self::relogin`] so the header names cannot drift between them.
    fn login_once(
        agent: &ureq::Agent,
        config: &crate::config::IgConfig,
    ) -> Result<LoginOutcome, IgApiError> {
        let url = format!("{}/session", config.rest_base);
        let body = serde_json::json!({
            "identifier": config.identifier,
            "password": config.password,
        });
        let payload = serde_json::to_string(&body).map_err(Self::net_err)?;

        let mut resp = agent
            .post(&url)
            .header("X-IG-API-KEY", &config.api_key)
            .header("Version", "2")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(payload.as_bytes())
            .map_err(Self::net_err)?;

        let status = resp.status().as_u16();
        // capture tokens from headers before reading the body
        let hdr =
            |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let cst = hdr("CST");
        let security_token = hdr("X-SECURITY-TOKEN");
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;

        if !(200..300).contains(&status) {
            return Err(Self::finish(status, &text).unwrap_err());
        }
        let (Some(cst), Some(security_token)) = (cst, security_token) else {
            return Err(IgApiError {
                status,
                message: "login ok but CST / X-SECURITY-TOKEN header missing".to_string(),
            });
        };
        let body: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let account_id =
            body.get("currentAccountId").and_then(|a| a.as_str()).unwrap_or_default().to_string();
        let lightstreamer_endpoint = body
            .get("lightstreamerEndpoint")
            .and_then(|a| a.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(LoginOutcome { cst, security_token, account_id, lightstreamer_endpoint })
    }

    /// Authenticated GET. `version` is the IG endpoint version header (e.g. "3" for prices);
    /// `query` is a pre-formatted `"k=v&k=v"` string (may be empty).
    pub fn get(
        &self,
        path: &str,
        version: &str,
        query: &str,
    ) -> Result<serde_json::Value, IgApiError> {
        let url = if query.is_empty() {
            format!("{}{}", self.base, path)
        } else {
            format!("{}{}?{}", self.base, path, query)
        };
        self.call_with_reauth(|cst, xst| {
            let mut resp = self
                .agent
                .get(&url)
                .header("X-IG-API-KEY", &self.api_key)
                .header("CST", cst)
                .header("X-SECURITY-TOKEN", xst)
                .header("Version", version)
                .header("Accept", "application/json")
                .call()
                .map_err(Self::net_err)?;
            let status = resp.status().as_u16();
            Ok((status, resp.body_mut().read_to_string().map_err(Self::net_err)?))
        })
    }

    /// Authenticated POST with a JSON body (open position / place working order).
    pub fn post(
        &self,
        path: &str,
        version: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, IgApiError> {
        self.post_with(path, version, body, None)
    }

    /// Authenticated POST carrying IG's `_method` verb override — the ONLY way to reach an IG
    /// endpoint that is documented as `DELETE` but takes a request BODY.
    ///
    /// ⚠ **A real HTTP `DELETE` with a body does not work here, and fails in a way that reads like
    /// a validation bug in our own payload.** Measured against `demo-api.ig.com` (2026-08-21):
    /// `DELETE /positions/otc` with a well-formed close body answers **400**
    /// `validation.null-not-allowed.request` — IG's gateway discards the entity on a DELETE, so the
    /// handler sees no request at all. The same body sent as `POST` with `_method: DELETE` answers
    /// **200** `{"dealReference": …}`. This is why [`Self::delete`] (which sends no body) stays a
    /// real DELETE and the position-close path does not.
    pub fn post_method_delete(
        &self,
        path: &str,
        version: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, IgApiError> {
        self.post_with(path, version, body, Some("DELETE"))
    }

    fn post_with(
        &self,
        path: &str,
        version: &str,
        body: &serde_json::Value,
        method_override: Option<&str>,
    ) -> Result<serde_json::Value, IgApiError> {
        let url = format!("{}{}", self.base, path);
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        self.call_with_reauth(|cst, xst| {
            let mut req = self
                .agent
                .post(&url)
                .header("X-IG-API-KEY", &self.api_key)
                .header("CST", cst)
                .header("X-SECURITY-TOKEN", xst)
                .header("Version", version)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json");
            if let Some(verb) = method_override {
                req = req.header("_method", verb);
            }
            let mut resp = req.send(payload.as_bytes()).map_err(Self::net_err)?;
            let status = resp.status().as_u16();
            Ok((status, resp.body_mut().read_to_string().map_err(Self::net_err)?))
        })
    }

    /// Authenticated DELETE (cancel a working order by dealId). Body-less, so a real DELETE is
    /// correct here — see [`Self::post_method_delete`] for the endpoints where it is not.
    pub fn delete(&self, path: &str, version: &str) -> Result<serde_json::Value, IgApiError> {
        let url = format!("{}{}", self.base, path);
        self.call_with_reauth(|cst, xst| {
            let mut resp = self
                .agent
                .delete(&url)
                .header("X-IG-API-KEY", &self.api_key)
                .header("CST", cst)
                .header("X-SECURITY-TOKEN", xst)
                .header("Version", version)
                .header("Accept", "application/json")
                .call()
                .map_err(Self::net_err)?;
            let status = resp.status().as_u16();
            Ok((status, resp.body_mut().read_to_string().map_err(Self::net_err)?))
        })
    }
}

/// The `/session/encryptionKey` body → IG's clock in epoch ms. Split out of
/// [`IgSession::server_time_ms`] so the FIELD NAME and its UNIT are gated by a fixture test in CI
/// rather than only by an `#[ignore]`d live call: a renamed field or a seconds-valued stamp read as
/// ms would otherwise reach a live mount before anything noticed.
///
/// `status` is carried only so the error names the HTTP status the body arrived with; a 2xx body
/// without `timeStamp` is an error, never a zero.
pub fn parse_server_time_ms(body: &serde_json::Value, status: u16) -> Result<i64, IgApiError> {
    body.get("timeStamp").and_then(serde_json::Value::as_i64).ok_or(IgApiError {
        status,
        message: "timeStamp missing from the encryptionKey response".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A REAL `GET /session/encryptionKey` body, captured from `demo-api.ig.com` on 2026-08-09.
    /// The `encryptionKey` value — the RSA public key IG returns for password encryption, which
    /// this parser never reads — is replaced by a placeholder, the same redact-the-leaf-keep-the-
    /// shape idiom as `vike_bridge_core::capture`'s `capture_frame`.
    const CAPTURED: &str = include_str!("../tests/fixtures/session_encryption_key.json");

    #[test]
    fn the_captured_encryption_key_body_yields_igs_clock() {
        let body: serde_json::Value =
            serde_json::from_str(CAPTURED).expect("the capture is valid JSON");
        assert_eq!(parse_server_time_ms(&body, 200), Ok(1_786_242_762_192));
    }

    /// The UNIT trap, at the one venue whose stamp is a bare integer with no sibling to compare
    /// against: a seconds-valued reading is off by a factor of a thousand and still looks like a
    /// plausible epoch. The captured stamp is asserted to be MILLISECONDS by its magnitude.
    #[test]
    fn the_captured_stamp_is_milliseconds_not_seconds() {
        let body: serde_json::Value = serde_json::from_str(CAPTURED).expect("valid JSON");
        let ms = parse_server_time_ms(&body, 200).expect("the capture carries timeStamp");
        // 2001-09-09T01:46:40Z in ms; any seconds-valued stamp for this century is far below it.
        assert!(ms > 1_000_000_000_000, "a seconds-valued stamp would be a thousandfold out: {ms}");
        assert!(ms < 10_000_000_000_000, "a µs/ns-valued stamp would be far above this: {ms}");
    }

    // ── The 401 ladder (offline: no agent, no network) ────────────────────────────────────────
    //
    // These drive [`reauth_ladder`] directly, which is why it is a free function over the token
    // cell rather than a method: the properties that matter here are COUNTS (how many HTTP
    // attempts, how many logins), and a count is only assertable when the two closures are the
    // test's own.

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn cell(generation: u64) -> Mutex<Tokens> {
        Mutex::new(Tokens {
            cst: format!("cst-{generation}"),
            security_token: format!("xst-{generation}"),
            generation,
        })
    }

    /// A call that succeeds first time never touches the login endpoint — the ordinary path must
    /// cost exactly one round trip.
    #[test]
    fn a_non_401_answer_never_re_logs_in() {
        let tokens = cell(1);
        let (runs, logins) = (AtomicU32::new(0), AtomicU32::new(0));
        let out = reauth_ladder(
            &tokens,
            "ACC",
            |_, _| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok((200, "{}".to_string()))
            },
            |_| {
                logins.fetch_add(1, Ordering::Relaxed);
                Ok(2)
            },
        )
        .expect("no transport error");
        assert_eq!(out.0, 200);
        assert_eq!(runs.load(Ordering::Relaxed), 1);
        assert_eq!(logins.load(Ordering::Relaxed), 0, "a 200 is not a re-login trigger");
    }

    /// The recovery itself: a 401 re-logs-in ONCE and retries with the NEW pair — asserted by the
    /// tokens the second attempt was handed, not merely by the attempt count.
    #[test]
    fn a_401_re_logs_in_once_and_retries_with_the_fresh_pair() {
        let tokens = cell(1);
        let seen: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let logins = AtomicU32::new(0);
        let out = reauth_ladder(
            &tokens,
            "ACC",
            |cst, _| {
                seen.lock().unwrap().push(cst.to_string());
                if cst == "cst-1" {
                    Ok((401, r#"{"errorCode":"error.security.client-token-invalid"}"#.to_string()))
                } else {
                    Ok((200, r#"{"ok":true}"#.to_string()))
                }
            },
            |gen_seen| {
                logins.fetch_add(1, Ordering::Relaxed);
                assert_eq!(gen_seen, 1, "the ladder reports the generation it actually used");
                let mut t = tokens.lock().unwrap();
                t.cst = "cst-2".into();
                t.security_token = "xst-2".into();
                t.generation = 2;
                Ok(2)
            },
        )
        .expect("no transport error");
        assert_eq!(out.0, 200);
        assert_eq!(logins.load(Ordering::Relaxed), 1);
        assert_eq!(*seen.lock().unwrap(), vec!["cst-1".to_string(), "cst-2".to_string()]);
    }

    /// ⚠ The anti-spin property, and the reason the ladder is a ladder rather than a loop: a
    /// second 401 on a FRESH session is a REFUSAL (revoked key, changed password), not an expiry.
    /// It stops after exactly two attempts and one login, and hands the caller IG's own body.
    #[test]
    fn a_second_401_after_a_fresh_login_stops_instead_of_spinning() {
        let tokens = cell(1);
        let (runs, logins) = (AtomicU32::new(0), AtomicU32::new(0));
        let out = reauth_ladder(
            &tokens,
            "ACC",
            |_, _| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok((401, r#"{"errorCode":"error.security.client-token-invalid"}"#.to_string()))
            },
            |_| {
                logins.fetch_add(1, Ordering::Relaxed);
                tokens.lock().unwrap().generation = 2;
                Ok(2)
            },
        )
        .expect("no transport error");
        assert_eq!(out.0, 401);
        assert!(out.1.contains("client-token-invalid"), "IG's own errorCode survives: {}", out.1);
        assert_eq!(runs.load(Ordering::Relaxed), 2, "exactly one retry, never a loop");
        assert_eq!(logins.load(Ordering::Relaxed), 1, "exactly one re-login attempt");
    }

    /// A re-login that itself fails must not be retried either, and must surface IG's ORIGINAL 401
    /// body — the caller's error should name what the venue said, not what our recovery said.
    #[test]
    fn a_failed_re_login_surfaces_the_original_401_and_does_not_retry() {
        let tokens = cell(1);
        let runs = AtomicU32::new(0);
        let out = reauth_ladder(
            &tokens,
            "ACC",
            |_, _| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok((401, r#"{"errorCode":"error.security.account-token-invalid"}"#.to_string()))
            },
            |_| Err(IgApiError { status: 0, message: "network error: down".into() }),
        )
        .expect("a failed re-login is not itself a transport error");
        assert_eq!(out.0, 401);
        assert!(out.1.contains("account-token-invalid"), "{}", out.1);
        assert_eq!(
            runs.load(Ordering::Relaxed),
            1,
            "no retry once recovery is known to have failed"
        );
    }

    /// The shared-session guard, in the shape that actually happens: a SIBLING's login lands while
    /// this caller's request is still in flight. The ladder snapshotted generation 1, so by the time
    /// its 401 comes back the cell already holds 9 — and `IgSession::relogin`'s guard must then log
    /// in NOTHING and just report the generation, so the two threads produce one login between them
    /// rather than stampeding IG's rate-limited login endpoint.
    #[test]
    fn a_sibling_login_landing_mid_flight_is_not_re_logged_in_again() {
        let tokens = cell(1);
        let (seen, logins) = (Mutex::new(Vec::new()), AtomicU32::new(0));
        let out = reauth_ladder(
            &tokens,
            "ACC",
            |cst, _| {
                let mut s = seen.lock().unwrap();
                s.push(cst.to_string());
                if s.len() == 1 {
                    // ...the sibling's re-login lands right here, while we were on the wire.
                    let mut t = tokens.lock().unwrap();
                    t.cst = "cst-9".into();
                    t.security_token = "xst-9".into();
                    t.generation = 9;
                    Ok((401, "{}".to_string()))
                } else {
                    Ok((200, "{}".to_string()))
                }
            },
            // Exactly what `IgSession::relogin` does when the generation already moved.
            |seen_gen| {
                let current = tokens.lock().unwrap().generation;
                assert_eq!(seen_gen, 1, "the ladder reports the generation IT used");
                assert_ne!(seen_gen, current, "which the sibling has already moved past");
                logins.fetch_add(1, Ordering::Relaxed);
                Ok(current)
            },
        )
        .expect("no transport error");
        assert_eq!(out.0, 200);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["cst-1".to_string(), "cst-9".to_string()],
            "the retry uses the SIBLING's fresh pair"
        );
        assert_eq!(logins.load(Ordering::Relaxed), 1, "asked once; it performed no login");
    }

    /// ⚠ A dead socket says NOTHING about whether the token is alive. Re-logging-in on a transport
    /// error would turn an outage into a login storm against the one endpoint that is rate-limited
    /// per account, so it propagates untouched and the ladder never runs.
    #[test]
    fn a_transport_error_is_never_a_re_login_trigger() {
        let tokens = cell(1);
        let logins = AtomicU32::new(0);
        let err = reauth_ladder(
            &tokens,
            "ACC",
            |_, _| Err(IgApiError { status: 0, message: "network error: reset".into() }),
            |_| {
                logins.fetch_add(1, Ordering::Relaxed);
                Ok(2)
            },
        )
        .expect_err("the transport error propagates");
        assert_eq!(err.status, 0);
        assert_eq!(logins.load(Ordering::Relaxed), 0);
    }

    /// A 2xx body that carries no stamp is an ERROR naming the field, never a zero.
    #[test]
    fn a_body_without_the_stamp_is_an_error_naming_the_field() {
        let e = parse_server_time_ms(&serde_json::json!({"encryptionKey": "x"}), 200)
            .expect_err("no timeStamp");
        assert_eq!(e.status, 200);
        assert!(e.message.contains("timeStamp"), "{}", e.message);
    }
}
