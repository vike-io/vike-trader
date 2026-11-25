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
//! The retry is **exactly one, never a loop**: the shared ladder (`vike_bridge_core::transport::auth`'s
//! `TokenCell::reauth_once`, lifted from this file) snapshots the token GENERATION, runs the call, and on a
//! 401 asks the re-login to advance that generation; it then retries ONCE.
//! A second 401 after a fresh login is a real refusal (revoked key, changed password, a locked
//! account) — it is logged at `error` and returned to the caller with IG's own `errorCode` intact.
//! A permanent rejection stops and says why; it does not spin. A re-login that FAILS answers the
//! login's own error, not the 401 it was trying to cure (as alpaca's mint does): a login host that
//! never answered must not read as refused keys. The exec lane's status line says it by the login
//! error's kind (`crate::account::venue_link`'s `rest_relogin_failed`), and the session is marked
//! (`IgSession::relogin_failed`) so the history cache replaces it, whatever that kind.
//!
//! The generation counter is what makes a SHARED session safe: two threads that both see a 401
//! produce ONE login, because the second finds the generation already advanced and simply retries
//! with the pair the first fetched.
//!
//! ## The account lives HERE too
//!
//! `POST /session` lands on the login's DEFAULT account, which may be a spread-bet or a pound one. Every login
//! ([`IgSession::login`] and [`IgSession::relogin`] alike) therefore ends in `settle_account`: `GET /accounts`, and
//! `PUT /session` only when the rule in `crate::account::pick` says the session must move, so no slice can forget
//! it. A login with no account to trade is refused with [`STATUS_ACCOUNT_REFUSED`].

use vike_bridge_core::transport::auth::{TokenCell, TokenReauthFailedLogin, TokenReauthNote};
use vike_bridge_core::transport::http_error::{VenueApiHttpError, VenueApiHttpSpec, field_or_text};

/// **IG's REST error facts** (`vike_bridge_core::transport::http_error`): every IG call answers a
/// [`VenueApiHttpError`] `{status, message}`, its message IG's `errorCode` when the body names one, its
/// `Display` `IG error {status}: {message}`. A status below 100 is a verdict, not HTTP: `0` provably never
/// sent (`login_with_retry` reads it so), [`VenueApiHttpError::STATUS_OUTCOME_UNKNOWN`] the deal MAY exist
/// (a timeout or reset once the body was written, a reply whose body never arrived), and
/// [`STATUS_ACCOUNT_REFUSED`] IG's own. That `1`, the gateway set, (owner ruling B-3) a 3xx to an ORDER write and
/// (owner ruling B-1) any 5xx to one leave the outcome unknown: no IG agent follows a redirect, so a 3xx is no verdict
/// on the order, and nothing says IG did not act before a 5xx (the bad-epic 500 never reaches it: `non_epic_refusal`
/// refuses a dotless symbol first). So does (owner ruling B-2) a 2xx to an order write whose body does not read: IG
/// answered yes to something, so a submit asks `/confirms`. A read's unreadable 2xx keeps its status.
pub static IG_HTTP: VenueApiHttpSpec = VenueApiHttpSpec {
    prefix: "IG error",
    network_words: "network error: ",
    also_unknown: &[],
    empty_2xx_is_null: false,
    unreadable_2xx_on_write_unknown: true,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
};

/// The `status` of a login this adapter REFUSED for want of an account to trade (`crate::account::pick`'s
/// refusal, in the message). Not an HTTP status, so it cannot collide; like any non-zero status it is a
/// refusal the lanes do not retry (`login_with_retry`), and `login_refused_for_good` remembers it.
pub(crate) const STATUS_ACCOUNT_REFUSED: u16 = 2;

/// The HTTP status IG answers with when the session token pair is expired, wrong or absent — the
/// ONE trigger for a re-login. Measured against `demo-api.ig.com` (2026-08-21); see the module doc
/// for the three `errorCode`s that ride it.
pub(crate) const REAUTH_STATUS: u16 = 401;

/// The ceiling on [`IgSession::logout`]'s whole call: three seconds. It runs at a clean shutdown, where
/// the gatewayhub's unit allows its stop 35 seconds (`deploy/vike-gatewayhub.service`'s
/// `TimeoutStopSec`), and a session that does not answer is left to IG's own expiry.
const LOGOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The ceiling on ONE `GET /confirms/{dealReference}` ([`IgSession::confirm`]): five seconds, the re-query bound the
/// shared exec loop gives a lookup after a lost answer (`vike_bridge_core::transport::REQUERY_TIMEOUT`). A confirm is
/// a small read IG answers in well under a second, and a submit makes up to seven of them behind its POST, so on the
/// shared 30 s agent one hung confirm alone could carry the submit past the core's last-resort reject. Pinned to the
/// `vike_venues::venue_timings::IG` row's `submit` chain in `crate::execution`.
pub(crate) const CONFIRM_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The `POST /session` reply, split out so [`IgSession::login`] and [`IgSession::relogin`] share
/// one parser rather than two copies that could drift about which header carries which token.
struct LoginOutcome {
    cst: String,
    security_token: String,
    account_id: String,
    lightstreamer_endpoint: String,
    account_currency: Option<String>,
    /// The `GET /accounts` body the login's account choice read, when it could.
    accounts: Option<serde_json::Value>,
}

/// An authenticated IG session (holds the session tokens). Created by [`IgSession::login`].
///
/// **Shareable**: the expiring pair sits behind a `Mutex`, so one `Arc<IgSession>` can back the
/// exec command thread and the [`ExecutionClient::confirm`](vike_execution::ExecutionClient::confirm)
/// worker at once, and a 401 either of them meets re-authenticates the session for BOTH.
pub struct IgSession {
    agent: ureq::Agent,
    /// The agent of [`Self::confirm`], on [`CONFIRM_CALL_TIMEOUT`].
    confirm_agent: ureq::Agent,
    api_key: String,
    base: String,
    /// The credentials this session re-logs-in with. Held because recovery is this type's job (see
    /// the module doc): without it every caller would need its own 401 ladder — and none had one.
    pub(crate) config: crate::config::IgConfig,
    /// The pair that EXPIRES, `(CST, X-SECURITY-TOKEN)`, with the generation that makes a concurrent re-login
    /// idempotent. `account_id` / `lightstreamer_endpoint` stay OUTSIDE it: they describe the ACCOUNT rather than
    /// the credential, and a re-login of the same identifier answers with the same two.
    tokens: TokenCell<(String, String)>,
    /// The account the session is scoped to: the login response's `currentAccountId`, or the account
    /// `crate::account::pick`'s rule moved the session to (`PUT /session`) when that one was not a USD CFD
    /// account. A re-login must land on the same one ([`Self::relogin`]).
    pub account_id: String,
    /// Lightstreamer streaming endpoint from the login response (trade-confirm/position/order stream).
    pub lightstreamer_endpoint: String,
    /// The account's currency: `GET /accounts`' word for [`Self::account_id`] when it states one, else the
    /// login response's `currencyIsoCode` (documented, not yet captured from the demo gateway), `None` when
    /// neither does. Only `crate::execution::order_currency`'s tie-break between several listed dealing currencies
    /// reads it.
    pub account_currency: Option<String>,
    /// The `GET /accounts` body the login read to choose the account, so the facts source does not ask IG for
    /// the same list again (`IgPropertiesSource::account`). `None` when that read failed.
    pub(crate) accounts: Option<serde_json::Value>,
    /// Where every authenticated call states IG's answer (`crate::account::venue_link`): set for the exec lane's
    /// session alone ([`Self::report_to`]), so every other session (reconcile, facts, history, catalog) writes nothing.
    link: std::sync::OnceLock<crate::account::venue_link::SessionLink>,
    /// A re-login of this session after a 401 has FAILED (any kind: refused, unreachable, another account): the
    /// pair is dead and the session cannot recover on its own. Never cleared; only the history cache reads it
    /// ([`Self::relogin_failed`]), to replace the session.
    relogin_failed: std::sync::atomic::AtomicBool,
}

impl IgSession {
    /// The IG Lightstreamer password: `CST-<cst>|XST-<security_token>` (paired with `account_id` as
    /// the LS user). Used by the trade-update stream driver.
    ///
    /// ⚠ **Read it per DIAL, never once per thread.** It renders the CURRENT pair, so a driver that
    /// caches the string across reconnects keeps re-presenting dead tokens after a re-login — which
    /// is exactly what `crates/bridges/ig/src/execution/user_data.rs` used to do through its snapshot `LightstreamerDial`.
    pub fn ls_password(&self) -> String {
        let ((cst, security_token), _) = self.tokens.current();
        format!("CST-{cst}|XST-{security_token}")
    }

    /// The current token generation — `1` after the first login, `+1` per successful re-login.
    /// Exposed so a caller that wants to force recovery can pass it to [`Self::relogin`] and learn
    /// whether its own view was the stale one.
    pub fn token_generation(&self) -> u64 {
        self.tokens.current().1
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
    pub fn relogin(&self, seen: u64) -> Result<u64, VenueApiHttpError> {
        // The cell holds its lock ACROSS the login round trip on purpose: that is what collapses a
        // concurrent stampede into one login. There is no reentrancy here (`login_once` takes only
        // the agent and the config), so it cannot deadlock.
        self.tokens.relogin(seen, || match Self::login_once(&self.agent, &self.config) {
            // `account_id` is fixed for the session's life (the trade stream's item and every reader name
            // it), so a re-login that lands on ANOTHER account (the operator changed the login's default
            // or its accounts meanwhile) is not this session: it is refused, loudly, and the old pair stays.
            Ok(out) if out.account_id != self.account_id => {
                let e = IG_HTTP.error(
                    STATUS_ACCOUNT_REFUSED,
                    format!(
                        "ig: a re-login landed on the account `{}`, not `{}` this session trades;                          the session cannot continue on another account",
                        out.account_id, self.account_id
                    ),
                );
                tracing::error!(
                    target: "vike_ig::transport",
                    account = %self.account_id,
                    error = %e.message,
                    "IG re-login REFUSED — the account changed under a live session; restart the account"
                );
                Err(e)
            }
            Ok(out) => {
                tracing::info!(
                    target: "vike_ig::transport",
                    account = %self.account_id,
                    // The cell moves from `seen` (it called this login only because it still held it) to the next.
                    generation = seen + 1,
                    "IG session re-authenticated after token expiry"
                );
                Ok((out.cst, out.security_token))
            }
            Err(e) => {
                tracing::error!(
                    target: "vike_ig::transport",
                    account = %self.account_id,
                    status = e.status,
                    error = %e.message,
                    "IG re-login FAILED — this session cannot recover on its own; authenticated                      calls will keep failing until the credentials or the network are fixed"
                );
                Err(e)
            }
        })
    }

    /// Run one authenticated call through the 401 ladder and parse its body.
    ///
    /// `run(cst, xst)` performs the HTTP with the pair it is handed and returns the raw
    /// `(status, body)` — deliberately NOT a parsed `Result`, because the ladder must see the
    /// status before [`Self::finish`] turns a non-2xx into an `Err` and loses it.
    fn call_with_reauth<F>(
        &self,
        write: bool,
        run: F,
    ) -> Result<serde_json::Value, VenueApiHttpError>
    where
        F: Fn(&str, &str) -> Result<(u16, String), VenueApiHttpError>,
    {
        let account = &self.account_id;
        let relogin_failed = std::cell::Cell::new(false);
        let answer = self.tokens.reauth_once(
            REAUTH_STATUS,
            TokenReauthFailedLogin::AnswerLoginError,
            |(cst, xst)| run(cst, xst),
            |seen| {
                let relogin = self.relogin(seen);
                relogin_failed.set(relogin.is_err());
                relogin
            },
            |note| match note {
                TokenReauthNote::Reauthenticating => tracing::warn!(
                    target: "vike_ig::transport",
                    %account,
                    "IG answered 401 — re-authenticating, then retrying this call ONCE"
                ),
                TokenReauthNote::RefusedAgain => tracing::error!(
                    target: "vike_ig::transport",
                    %account,
                    "IG answered 401 AGAIN on a FRESH session — the credentials are being refused, not                      expiring. NOT retrying further"
                ),
            },
        );
        // Under `AnswerLoginError` a failed re-login answers the LOGIN's error: said as the login's, by its kind.
        match &answer {
            Err(login) if relogin_failed.get() => {
                self.relogin_failed.store(true, std::sync::atomic::Ordering::SeqCst);
                if let Some(link) = self.link.get() {
                    link.rest_relogin_failed(login);
                }
            }
            _ => {
                if let Some(link) = self.link.get() {
                    link.rest_heard(&answer);
                }
            }
        }
        let (status, text) = answer?;
        let ((cst, security_token), _) = self.tokens.current();
        let carried = [self.api_key.as_str(), cst.as_str(), security_token.as_str()];
        Self::finish_call(status, &text, write, &carried)
    }

    /// Has a re-login of this session after a 401 failed? Then it cannot recover on its own, and the history cache
    /// replaces it (`crate::history::klines::SessionCache`).
    pub(crate) fn relogin_failed(&self) -> bool {
        self.relogin_failed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// State every authenticated call's answer on `link` from now on: the exec lane's session, so the status verb can
    /// say whether IG answers it. A second call is ignored. It changes no call.
    pub(crate) fn report_to(&self, link: crate::account::venue_link::SessionLink) {
        let _ = self.link.set(link);
    }
}

impl IgSession {
    fn net_err(e: impl std::fmt::Display) -> VenueApiHttpError {
        IG_HTTP.network(e)
    }

    /// A send failure of an ORDER POST: ambiguous ([`VenueApiHttpError::STATUS_OUTCOME_UNKNOWN`]) unless it
    /// provably preceded the write (`vike_transport::send_outcome::send_error_is_definite` is the one rule).
    fn order_send_err(e: ureq::Error) -> VenueApiHttpError {
        IG_HTTP.write_send_error(&e)
    }

    /// A body read failure on an ORDER reply: the headers arrived, so IG received the request.
    fn order_read_err(e: ureq::Error) -> VenueApiHttpError {
        IG_HTTP.write_read_error(e)
    }

    /// A status and its body: the JSON on a 2xx, else IG's `errorCode` (the body's summary when it names none,
    /// never the body itself). `carried`: the request's API key, session tokens or login, redacted from the words.
    fn finish(
        status: u16,
        text: &str,
        carried: &[&str],
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        Self::finish_call(status, text, false, carried)
    }

    /// [`Self::finish`] for an authenticated call: `write` when it may change the account (an order's POST, PUT or
    /// DELETE), so the spec's write rules apply ([`IG_HTTP`]).
    fn finish_call(
        status: u16,
        text: &str,
        write: bool,
        carried: &[&str],
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        IG_HTTP.finish(status, text, write, carried, |body| field_or_text(body, "errorCode"))
    }

    fn agent() -> ureq::Agent {
        vike_transport::http::blocking_agent()
    }

    fn confirm_agent() -> ureq::Agent {
        vike_transport::http::blocking_agent_with_timeout(CONFIRM_CALL_TIMEOUT)
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
    /// (`crates/vike-gatewayhub/src/trade/adapters/server_time.rs`'s `CLOCK_READ_TIMEOUT`), and a
    /// clock canary that can park a start
    /// for half a minute is worse than the warning it produces.
    ///
    /// ⚠ Without the `X-IG-API-KEY` header this endpoint is HTTP **400**, not 200 — it is
    /// credential-gated, not public. It is still safe for a preflight because IG's live gate
    /// (`load_ig_config_from`) has already resolved that key by the time anything starts.
    ///
    /// ⚠ A drifted clock cannot get an IG order rejected: IG authenticates with the session token
    /// pair above and stamps no timestamp on a request, so this is a HOST-HEALTH canary rather than
    /// a rejection guard (see `vike_model::ClockRisk`).
    ///
    /// The RSA key in the same body is deliberately ignored, and no secret is read or returned.
    pub fn server_time_ms(
        config: &crate::config::IgConfig,
        timeout: std::time::Duration,
    ) -> Result<i64, VenueApiHttpError> {
        let agent = vike_transport::http::blocking_agent_with_timeout(timeout);
        let url = format!("{}/session/encryptionKey", config.rest_base);
        let mut response = agent
            .get(&url)
            .header("X-IG-API-KEY", config.api_key.expose())
            .header("Version", "1")
            .header("Accept", "application/json")
            .call()
            .map_err(Self::net_err)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(Self::net_err)?;
        let carried = [config.api_key.expose().as_str()];
        parse_server_time_ms(&Self::finish(status, &text, &carried)?, status)
    }

    /// Log in (v2 `/session`) and capture the `CST` / `X-SECURITY-TOKEN` response headers.
    pub fn login(config: &crate::config::IgConfig) -> Result<Self, VenueApiHttpError> {
        let agent = Self::agent();
        let out = Self::login_once(&agent, config)?;
        Ok(Self {
            agent,
            confirm_agent: Self::confirm_agent(),
            api_key: config.api_key.expose().clone(),
            base: config.rest_base.clone(),
            config: config.clone(),
            tokens: TokenCell::new((out.cst, out.security_token)),
            account_id: out.account_id,
            lightstreamer_endpoint: out.lightstreamer_endpoint,
            account_currency: out.account_currency,
            accounts: out.accounts,
            link: std::sync::OnceLock::new(),
            relogin_failed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// A session over `base` holding a made-up token pair, with no login: a unit test's logged-in session.
    #[cfg(test)]
    pub(crate) fn offline(base: &str) -> Self {
        use vike_model::secret::Secret;
        let config = crate::config::IgConfig {
            api_key: Secret::new("k".to_string()),
            identifier: Secret::new("i".to_string()),
            password: Secret::new("p".to_string()),
            rest_base: base.to_string(),
            account_pin: None,
            fx_clock: None,
        };
        Self {
            agent: Self::agent(),
            confirm_agent: Self::confirm_agent(),
            api_key: config.api_key.expose().clone(),
            base: config.rest_base.clone(),
            config,
            tokens: TokenCell::new(("c".to_string(), "x".to_string())),
            account_id: "ACC1".to_string(),
            lightstreamer_endpoint: String::new(),
            account_currency: None,
            accounts: None,
            link: std::sync::OnceLock::new(),
            relogin_failed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// One `POST /session` round trip → the four things a session is made of. Shared by
    /// [`Self::login`] and [`Self::relogin`] so the header names cannot drift between them.
    fn login_once(
        agent: &ureq::Agent,
        config: &crate::config::IgConfig,
    ) -> Result<LoginOutcome, VenueApiHttpError> {
        // No IG login while the FX market is closed on the config's clock (owner ruling 2026-11-13): refused in its
        // words, nothing is sent.
        if let Some(words) = config.fx_closed_words() {
            return Err(Self::net_err(words));
        }
        let url = format!("{}/session", config.rest_base);
        let body = serde_json::json!({
            "identifier": config.identifier.expose(),
            "password": config.password.expose(),
        });
        let payload = serde_json::to_string(&body).map_err(Self::net_err)?;

        let mut response = agent
            .post(&url)
            .header("X-IG-API-KEY", config.api_key.expose())
            .header("Version", "2")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(payload.as_bytes())
            .map_err(Self::net_err)?;

        let status = response.status().as_u16();
        // capture tokens from headers before reading the body
        let hdr = |name: &str| {
            response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
        };
        let cst = hdr("CST");
        let security_token = hdr("X-SECURITY-TOKEN");
        let text = response.body_mut().read_to_string().map_err(Self::net_err)?;

        if !(200..300).contains(&status) {
            let carried = [
                config.api_key.expose().as_str(),
                config.identifier.expose().as_str(),
                config.password.expose().as_str(),
            ];
            return Err(Self::finish(status, &text, &carried)
                .expect_err("finish is Err for a non-2xx status"));
        }
        let (Some(cst), Some(security_token)) = (cst, security_token) else {
            return Err(IG_HTTP
                .error(status, "login ok but CST / X-SECURITY-TOKEN header missing".to_string()));
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

        let account_currency = body
            .get("currencyIsoCode")
            .and_then(|c| c.as_str())
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty());

        let mut out = LoginOutcome {
            cst,
            security_token,
            account_id,
            lightstreamer_endpoint,
            account_currency,
            accounts: None,
        };
        Self::settle_account(agent, config, &mut out)?;
        Ok(out)
    }

    /// **Put this session on the account the rule picks** ([`crate::account::pick`]), right after `POST /session`
    /// landed on whichever account IG made the login's default: `GET /accounts`, then, only when the session's
    /// account is not the one to trade, `PUT /session` (version 1) with `defaultAccount: false`, which moves THIS
    /// session and leaves the login's default (and every other session of it) where it was. Shared by
    /// [`Self::login`] and [`Self::relogin`], so a re-login settles the same way.
    ///
    /// ⚠ **A list that cannot be read changes nothing**: the session stays on the default (what every login did
    /// before this existed) and the `warn` says which account that is, UNLESS the account row named a book,
    /// which then cannot be checked and fails the login (a transport failure retries, like any login's).
    ///
    /// The switch's answer may carry a fresh `X-SECURITY-TOKEN` (the token is the account's): when it does the
    /// session adopts it. IG's own reference does not say, and nothing has captured it, so both are handled.
    fn settle_account(
        agent: &ureq::Agent,
        config: &crate::config::IgConfig,
        out: &mut LoginOutcome,
    ) -> Result<(), VenueApiHttpError> {
        let pin = config.account_pin.as_deref();
        let accounts = match Self::list_accounts(agent, config, &out.cst, &out.security_token) {
            Ok(body) => body,
            Err(e) if pin.is_none() => {
                tracing::warn!(
                    target: "vike_ig::transport",
                    account = %out.account_id,
                    error = %e,
                    "IG GET /accounts failed after the login: the session stays on the account IG made \
                     the default, and whether that is a USD CFD account is NOT checked"
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let rows = crate::account::pick::listed_accounts(&accounts);
        match crate::account::pick::pick(&rows, &out.account_id, pin) {
            Ok(crate::account::pick::Pick::Stay) => {}
            Ok(crate::account::pick::Pick::Switch(target)) => {
                let (cst, xst) =
                    Self::switch_account(agent, config, &out.cst, &out.security_token, &target)
                        .map_err(|e| {
                            IG_HTTP.error(
                    e.status,
                    format!(
                        "ig: moving this session from the account `{}` to `{target}` was not \
                                 done: {}",
                        out.account_id, e.message
                    ),
                )
                        })?;
                tracing::info!(
                    target: "vike_ig::transport",
                    from = %out.account_id,
                    to = %target,
                    new_security_token = xst.is_some(),
                    "IG session moved to the account the rule picks (the login's default is unchanged)"
                );
                if let Some(cst) = cst {
                    out.cst = cst;
                }
                if let Some(xst) = xst {
                    out.security_token = xst;
                }
                out.account_id = target;
            }
            Err(why) => {
                return Err(IG_HTTP.error(STATUS_ACCOUNT_REFUSED, why.to_string()));
            }
        }
        if let Some(row) = rows.iter().find(|r| r.id.eq_ignore_ascii_case(&out.account_id))
            && row.currency.is_some()
        {
            out.account_currency.clone_from(&row.currency);
        }
        out.accounts = Some(accounts);
        Ok(())
    }

    /// `GET /accounts` (version 1) with a token pair in hand, outside the 401 ladder (it runs INSIDE a login).
    fn list_accounts(
        agent: &ureq::Agent,
        config: &crate::config::IgConfig,
        cst: &str,
        security_token: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let mut response = agent
            .get(&format!("{}/accounts", config.rest_base))
            .header("X-IG-API-KEY", config.api_key.expose())
            .header("CST", cst)
            .header("X-SECURITY-TOKEN", security_token)
            .header("Version", "1")
            .header("Accept", "application/json")
            .call()
            .map_err(Self::net_err)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(Self::net_err)?;
        Self::finish(status, &text, &[config.api_key.expose().as_str(), cst, security_token])
    }

    /// `PUT /session` (version 1) `{accountId, defaultAccount: false}`: this session's account. Returns the
    /// `CST` and `X-SECURITY-TOKEN` the answer carried, when it carried one.
    fn switch_account(
        agent: &ureq::Agent,
        config: &crate::config::IgConfig,
        cst: &str,
        security_token: &str,
        account_id: &str,
    ) -> Result<(Option<String>, Option<String>), VenueApiHttpError> {
        let payload = serde_json::json!({ "accountId": account_id, "defaultAccount": false });
        let mut response = agent
            .put(&format!("{}/session", config.rest_base))
            .header("X-IG-API-KEY", config.api_key.expose())
            .header("CST", cst)
            .header("X-SECURITY-TOKEN", security_token)
            .header("Version", "1")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(payload.to_string().as_bytes())
            .map_err(Self::net_err)?;
        let status = response.status().as_u16();
        let hdr = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let (new_cst, new_security_token) = (hdr("CST"), hdr("X-SECURITY-TOKEN"));
        let text = response.body_mut().read_to_string().map_err(Self::net_err)?;
        Self::finish(status, &text, &[config.api_key.expose().as_str(), cst, security_token])?;
        Ok((new_cst, new_security_token))
    }

    /// Authenticated GET. `version` is the IG endpoint version header (e.g. "3" for prices);
    /// `query` is a pre-formatted `"k=v&k=v"` string (may be empty).
    pub fn get(
        &self,
        path: &str,
        version: &str,
        query: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        self.get_on(&self.agent, path, version, query)
    }

    /// `GET /confirms/{deal_ref}` (version 1), IG's answer to one deal, on its own agent bounded by
    /// [`CONFIRM_CALL_TIMEOUT`]: every confirm of a submit sits in its chain to the core
    /// (`crate::execution`'s `confirm_with_retry`).
    pub fn confirm(&self, deal_ref: &str) -> Result<serde_json::Value, VenueApiHttpError> {
        self.get_on(&self.confirm_agent, &format!("/confirms/{deal_ref}"), "1", "")
    }

    /// [`Self::get`] on `agent`.
    fn get_on(
        &self,
        agent: &ureq::Agent,
        path: &str,
        version: &str,
        query: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = if query.is_empty() {
            format!("{}{}", self.base, path)
        } else {
            format!("{}{}?{}", self.base, path, query)
        };
        self.call_with_reauth(false, |cst, xst| {
            let mut response = agent
                .get(&url)
                .header("X-IG-API-KEY", &self.api_key)
                .header("CST", cst)
                .header("X-SECURITY-TOKEN", xst)
                .header("Version", version)
                .header("Accept", "application/json")
                .call()
                .map_err(Self::net_err)?;
            let status = response.status().as_u16();
            Ok((status, response.body_mut().read_to_string().map_err(Self::net_err)?))
        })
    }

    /// Authenticated POST with a JSON body (open position / place working order).
    pub fn post(
        &self,
        path: &str,
        version: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
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
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        self.post_with(path, version, body, Some("DELETE"))
    }

    fn post_with(
        &self,
        path: &str,
        version: &str,
        body: &serde_json::Value,
        method_override: Option<&str>,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{}{}", self.base, path);
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        self.call_with_reauth(true, |cst, xst| {
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
            let mut response = req.send(payload.as_bytes()).map_err(Self::order_send_err)?;
            let status = response.status().as_u16();
            Ok((status, response.body_mut().read_to_string().map_err(Self::order_read_err)?))
        })
    }

    /// Authenticated PUT with a JSON body (amend a working order by dealId). A real `PUT` carries
    /// its body fine — the entity-discarding trap is `DELETE`'s alone (see
    /// [`Self::post_method_delete`]). A failure after the write reads as an order POST's does: IG may have moved the
    /// level ([`VenueApiHttpError::outcome_unknown`]), and only a failure provably before it says the order keeps its terms.
    pub fn put(
        &self,
        path: &str,
        version: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{}{}", self.base, path);
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        self.call_with_reauth(true, |cst, xst| {
            let mut response = self
                .agent
                .put(&url)
                .header("X-IG-API-KEY", &self.api_key)
                .header("CST", cst)
                .header("X-SECURITY-TOKEN", xst)
                .header("Version", version)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .send(payload.as_bytes())
                .map_err(Self::order_send_err)?;
            let status = response.status().as_u16();
            Ok((status, response.body_mut().read_to_string().map_err(Self::order_read_err)?))
        })
    }

    /// **End THIS session** (`DELETE /session`, version 1) with its current token pair: once, with no
    /// 401 ladder (logging out an expired session must not log in again to do it), and best effort —
    /// every failure is ignored, because the caller is replacing or dropping the session anyway. It ends
    /// only the session it is called on; the history lane (decision 0194) is its one caller, for the
    /// session it opened itself.
    pub fn logout(&self) {
        let ((cst, xst), _) = self.tokens.current();
        let url = format!("{}/session", self.base);
        // A short agent of its own: this runs at a clean shutdown, and the gatewayhub's unit gives its
        // stop 35 seconds, of which the shared agent's 30 second ceiling would eat nearly all.
        let agent = vike_transport::http::blocking_agent_with_timeout(LOGOUT_TIMEOUT);
        let sent = agent
            .delete(&url)
            .header("X-IG-API-KEY", &self.api_key)
            .header("CST", cst.as_str())
            .header("X-SECURITY-TOKEN", xst.as_str())
            .header("Version", "1")
            .header("Accept", "application/json")
            .call();
        match sent {
            Ok(_) => {
                tracing::debug!(target: "vike_ig::transport", account = %self.account_id, "IG session logged out")
            }
            Err(e) => tracing::debug!(
                target: "vike_ig::transport",
                account = %self.account_id,
                error = %e,
                "IG session logout failed (ignored: the session expires by itself)"
            ),
        }
    }

    /// Authenticated DELETE (cancel a working order by dealId). Body-less, so a real DELETE is
    /// correct here — see [`Self::post_method_delete`] for the endpoints where it is not. A failure after the write reads
    /// as an order POST's does: IG may have deleted the order ([`VenueApiHttpError::outcome_unknown`]); it read "never
    /// sent" (status 0), so a cancel IG had carried out was answered refused (S4b IG-1).
    pub fn delete(
        &self,
        path: &str,
        version: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{}{}", self.base, path);
        self.call_with_reauth(true, |cst, xst| {
            let mut response = self
                .agent
                .delete(&url)
                .header("X-IG-API-KEY", &self.api_key)
                .header("CST", cst)
                .header("X-SECURITY-TOKEN", xst)
                .header("Version", version)
                .header("Accept", "application/json")
                .call()
                .map_err(Self::order_send_err)?;
            let status = response.status().as_u16();
            Ok((status, response.body_mut().read_to_string().map_err(Self::order_read_err)?))
        })
    }
}

/// The `/session/encryptionKey` body → IG's clock in epoch ms. Split out of
/// [`IgSession::server_time_ms`] so the FIELD NAME and its UNIT are gated by a fixture test in CI
/// rather than only by an `#[ignore]`d live call: a renamed field or a seconds-valued stamp read as
/// ms would otherwise reach a live start before anything noticed.
///
/// `status` is carried only so the error names the HTTP status the body arrived with; a 2xx body
/// without `timeStamp` is an error, never a zero.
pub(crate) fn parse_server_time_ms(
    body: &serde_json::Value,
    status: u16,
) -> Result<i64, VenueApiHttpError> {
    body.get("timeStamp").and_then(serde_json::Value::as_i64).ok_or(
        IG_HTTP.error(status, "timeStamp missing from the encryptionKey response".to_string()),
    )
}

#[path = "transport_tests.rs"]
#[cfg(test)]
mod transport_tests;
