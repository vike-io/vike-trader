//! Session upkeep: auth-error classification, the persisted OAuth refresh, the reconnect loop.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use vike_exec::EventSender;
use vike_venues::venue_timings::{self, Reconnect};

use super::grant::{ExecPathDown, Renewal};
use super::outbound::refuse_order_command;
use super::stream::decode_error;
use super::{
    CloseState, Command, ConnConfig, ConnError, PositionMap, Reader, decode_payload,
    open_and_handshake, send,
};
use crate::oauth;
use crate::proto::{ProtoMessage, ProtoOaAccountAuthReq, ProtoOaOrderErrorEvent, pt};
use crate::symbols::SymbolMap;
use crate::token_store::{REAUTHORIZE_CMD, TokenPersist};

/// The wait before each reconnect attempt after a dropped socket: the exec lane's `reconnect` in
/// `vike_venues::venue_timings::CTRADER`, which carries the reason. `initial` is the wait
/// before the first attempt and it doubles on every failed attempt, capped at `max`.
/// `pub(super)`: `conn_tests.rs` pins its effective value.
pub(super) const RECONNECT_WAIT: Reconnect = venue_timings::CTRADER.exec().reconnect();

/// How often the reconnect loop may spend a refresh token, how often it dials at all once the refresh was refused, and
/// how often a lapse may open a new session: a dead grant is an outage only a person ends, not a reason to hit the
/// token endpoint or the login every few seconds.
pub(super) const RENEW_EVERY: Duration = Duration::from_secs(60);

/// The envelope id of the re-auth on a live socket. The venue echoes it on the refusal, and no order stands behind it.
const REAUTH_MSG_ID: &str = "aa-refresh";

/// What the operator reads while a new session is being opened for an account the venue no longer calls authorized.
pub(super) const SESSION_GONE: &str = "the venue says this trading account is not authorized on its socket; a new session is being opened";

/// What the operator reads when the venue says so again within [`RENEW_EVERY`] of the last new session: the socket stays.
pub(super) const SESSION_GONE_AGAIN: &str = "the venue says this trading account is not authorized on its socket, again, less than a minute after a new session opened";

/// **What a lapse costs the account's orders, in each state the actor can be in** (the status verb's `effect`, read by
/// `trade doctor`). An operator told "refused" re-enters an order by hand, so each must be true of what the actor does
/// while it stands, and it is set where the actor enters that state.
///
/// The socket stays: the actor writes each order and the venue answers it.
pub(super) const ON_THE_SAME_SOCKET: &str = "orders still go out on this socket and the venue answers each, refusing it while it says this; a new session is tried at most once a minute";
/// [`reconnect_with_backoff`] with a grant it can still use. The number is [`HOLD_ORDERS_FOR`]'s (`conn_tests.rs` holds them equal).
pub(super) const WAITING_FOR_A_SESSION: &str = "an order, a close or an amend waits up to 20 s for the new session and goes out when it opens; past that it is refused by name and never sent; a cancel waits for the session however long it takes";
/// [`reconnect_with_backoff`] with a dead grant.
pub(super) const GRANT_IS_DEAD: &str = "every order, close, amend and cancel is refused by name and nothing is sent until the grant works again";

/// **How long an order waits in [`reconnect_with_backoff`] for a session, whatever keeps it from opening** (the network,
/// `CH_CLIENT_AUTH_FAILURE`). Past it the order is refused by name, because the engine's stuck-order ladder
/// (`crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`) does not wait for ever: at the defaults it
/// warns at 30 s (`submit_ack_timeout`; the confirm it issues there is a no-op on this bridge) and closes the order as
/// rejected at the first sweep past 60 s (the timeout plus twice `submit_ack_confirm_grace`; it sweeps every 15 s). An order
/// replayed after that is a live order the node believes dead. An outage on this socket defers none of it: the ladder's
/// deferral is the trade lease's (`Ingest::ExecLink`), and the lease stays up.
///
/// 20 s lets a reconnect's backoff run seven attempts, so a blip still places the order, and an order goes out younger
/// than the ladder's FIRST rung even after the replay's reconcile. A run profile that lowers `guards.submit_ack_timeout_ms`
/// so far that the reject falls inside this is not caught: the bridge is not told the profile.
pub(super) const HOLD_ORDERS_FOR: Duration = Duration::from_secs(20);

/// `guards.submit_ack_timeout_ms`'s default: the ladder's first rung.
const SUBMIT_ACK_TIMEOUT: Duration = Duration::from_secs(30);
const _: () = assert!(
    HOLD_ORDERS_FOR.as_secs() + super::RECONCILE_TIMEOUT.as_secs() < SUBMIT_ACK_TIMEOUT.as_secs(),
    "an order held to the bound and replayed behind the reconcile must still go out under the ladder's first rung"
);

/// **Does this command stop being worth sending once it has waited [`HOLD_ORDERS_FOR`]?**
///
/// - A new order and a close: yes. The engine ends either by itself at the ladder, and the venue would then hold what the
///   node calls dead, or be flattened on a decision minutes old.
/// - An amend: yes. Refused, the order stays working at the terms the venue holds and the engine is told
///   (`OrderModifyRejected`, non-terminal); replayed, it re-prices a resting order on a decision minutes old.
/// - ⚠ **A cancel: no, it waits for the session however long that takes.** A refusal would leave the order resting with
///   nobody asking again, while a late cancel can only remove what the engine already wants gone, and the venue answers the
///   id of an order that ended meanwhile with a non-terminal refusal. With a dead grant it IS refused
///   ([`refuse_order_command`]): no session is coming to carry it, and the engine is told the order still rests.
/// - Everything else carries no order.
pub(super) fn expires_while_held(cmd: &Command) -> bool {
    matches!(cmd, Command::NewOrder(_) | Command::ClosePosition { .. } | Command::AmendOrder { .. })
}

/// The commands that arrived during a reconnect, each with the earliest instant it can have arrived.
struct Held<'a> {
    rx: &'a Receiver<Command>,
    lane: &'a OrderLane<'a>,
    hold_for: Duration,
    /// What a command that waited too long is refused in.
    too_long: String,
    /// When `rx` was last emptied: whatever it holds now arrived after this. A handshake attempt can block for many
    /// response timeouts between two looks, so a command's age is bounded by the look before the one that found it.
    looked_at: Instant,
    commands: Vec<(Instant, Command)>,
}

impl<'a> Held<'a> {
    fn new(rx: &'a Receiver<Command>, lane: &'a OrderLane<'a>, hold_for: Duration) -> Self {
        let too_long = format!(
            "ctrader: no session with the venue for {hold_for:?}, so this was not sent and will not be"
        );
        Self { rx, lane, hold_for, too_long, looked_at: Instant::now(), commands: Vec::new() }
    }

    /// Empty `rx` into the held set (refusing what arrives in `dead_words`, when the grant is dead), then refuse what has
    /// waited too long. `false` when the actor is to stop.
    fn look(&mut self, dead_words: Option<&str>) -> bool {
        let arrived_after = std::mem::replace(&mut self.looked_at, Instant::now());
        loop {
            match self.rx.try_recv() {
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return false,
                Ok(cmd) => {
                    let kept = match dead_words {
                        Some(words) => refuse_order_command(cmd, words, self.lane),
                        None => Some(cmd),
                    };
                    self.commands.extend(kept.map(|cmd| (arrived_after, cmd)));
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        for (arrived, cmd) in std::mem::take(&mut self.commands) {
            if expires_while_held(&cmd) && arrived.elapsed() >= self.hold_for {
                refuse_order_command(cmd, &self.too_long, self.lane);
            } else {
                self.commands.push((arrived, cmd));
            }
        }
        true
    }

    /// The grant is dead: every order command held is refused by name.
    fn refuse_all(&mut self, words: &str) {
        for (arrived, cmd) in std::mem::take(&mut self.commands) {
            self.commands
                .extend(refuse_order_command(cmd, words, self.lane).map(|cmd| (arrived, cmd)));
        }
    }
}

/// What a frame says about this socket's account session ([`session_word`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionWord {
    /// The access token is over (invalidated, expired, refused): get a pair the venue honours and authorize again
    /// on THIS socket ([`try_refresh_and_reauth`]).
    TokenRefused,
    /// The venue holds no authorized session on this socket, and says nothing against the token: a NEW session is
    /// what its proto asks for, and the reconnect path also re-learns what the gap may have hidden from this socket
    /// (execution events are delivered only to an authorized session).
    SessionGone,
}

/// Does the venue say, in these words, that the account (or the application) is not authorized on this connection?
///
/// ⚠ **The description is read, not only the code.** MEASURED on the CI box 2026-11-06 02:04 UTC, two days after the
/// access token lapsed: a far limit order was answered `INVALID_REQUEST`, "Trading account is not authorized". The
/// proto calls `INVALID_REQUEST` a generic error, so [`is_auth_error`]'s code rule read it as a plain venue refusal
/// and the socket stayed as it was.
fn says_not_authorized(error_code: &str, description: &str) -> bool {
    matches!(
        error_code.to_ascii_uppercase().as_str(),
        "ACCOUNT_NOT_AUTHORIZED" | "CH_CLIENT_NOT_AUTHENTICATED"
    ) || description.to_ascii_lowercase().contains("not authorized")
}

/// **What this frame says about the account session, when it says anything.** The ONE place the actor decides it.
/// `ProtoOAAccountDisconnectEvent` ("a new session must be authorized for the account") used to be a debug line.
pub(super) fn session_word(msg: &ProtoMessage) -> Option<SessionWord> {
    match msg.payload_type {
        pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT => Some(SessionWord::TokenRefused),
        pt::ACCOUNT_DISCONNECT_EVENT => Some(SessionWord::SessionGone),
        pt::ERROR_RES => match decode_error(msg) {
            ConnError::Venue { error_code, description }
                if says_not_authorized(&error_code, &description) =>
            {
                Some(SessionWord::SessionGone)
            }
            _ => is_auth_error(msg).then_some(SessionWord::TokenRefused),
        },
        pt::ORDER_ERROR_EVENT => {
            let ev = decode_payload::<ProtoOaOrderErrorEvent>(msg).ok()?;
            says_not_authorized(&ev.error_code, ev.description.as_deref().unwrap_or_default())
                .then_some(SessionWord::SessionGone)
        }
        _ => None,
    }
}

/// Is this frame the venue's answer to the re-auth itself? Every other refusal may name an order, and must reach it.
pub(super) fn answers_the_reauth(msg: &ProtoMessage) -> bool {
    msg.client_msg_id.as_deref() == Some(REAUTH_MSG_ID)
}

/// **The actor's record of an account session the venue ended**, from the moment it is found until a session is
/// authorized again: said once each way at `warn` (the daemon's file log keeps nothing lower), and, on the exec
/// socket, stated to the grant's cell so the status verb can tell the node's doctor that an engine it calls RUNNING
/// is not authorized (`SharedGrant::exec_path_down`).
pub(super) struct SessionLapse {
    exec: bool,
    ctid: i64,
    since_ms: Option<i64>,
}

impl SessionLapse {
    pub(super) fn new(exec: bool, ctid: i64) -> Self {
        Self { exec, ctid, since_ms: None }
    }

    /// The session is gone, or a new one is refused, for the reason `why`, and this is what it costs the orders.
    pub(super) fn found(&mut self, cfg: &ConnConfig, why: &str, effect: &'static str) {
        if self.since_ms.is_none() {
            self.since_ms = Some(vike_model::now_ms());
            tracing::warn!(target: "ctrader", exec = self.exec, why, "cTrader account session lost");
        }
        self.state(cfg, why, effect, 0, 0);
    }

    fn state(
        &self,
        cfg: &ConnConfig,
        why: &str,
        effect: &'static str,
        failed_attempts: u32,
        retry_in: u128,
    ) {
        if self.exec
            && let Some(cell) = &cfg.grant
        {
            cell.set_exec_path(
                self.ctid,
                Some(ExecPathDown {
                    why: why.to_string(),
                    effect,
                    failed_attempts,
                    retry_in_ms: u64::try_from(retry_in).unwrap_or(u64::MAX),
                }),
            );
        }
    }

    /// A session is authorized (a handshake, or the answer to a re-auth).
    pub(super) fn healed(&mut self, cfg: &ConnConfig) {
        if let Some(since) = self.since_ms.take() {
            tracing::warn!(
                target: "ctrader",
                exec = self.exec,
                down_ms = vike_model::now_ms() - since,
                "cTrader account session authorized again"
            );
        }
        if self.exec
            && let Some(cell) = &cfg.grant
        {
            cell.set_exec_path(self.ctid, None);
        }
    }
}

/// Where an order command refused during a lapse goes ([`refuse_order_command`]).
pub(super) struct OrderLane<'a> {
    pub(super) events: Option<&'a EventSender>,
    pub(super) positions: &'a PositionMap,
    pub(super) closes: &'a CloseState,
}

/// Heuristic: does this `ERROR_RES` frame represent an auth/token failure (vs. some other venue
/// error, e.g. a rejected order) that should trigger a token refresh + re-auth rather than just
/// being logged? cTrader's Open API does not publish a closed enum of `errorCode` strings for
/// this; matching `AUTH`/`TOKEN` substrings (case-insensitive) covers the documented codes (e.g.
/// `OA_AUTH_TOKEN_EXPIRED`, `ACCESS_TOKEN_INVALID`) without hardcoding an exact list that could
/// drift from the venue's actual wire behavior.
pub(super) fn is_auth_error(msg: &ProtoMessage) -> bool {
    if msg.payload_type != pt::ERROR_RES {
        return false;
    }
    match decode_error(msg) {
        ConnError::Venue { error_code, .. } => {
            let code = error_code.to_ascii_uppercase();
            code.contains("AUTH") || code.contains("TOKEN")
        }
        _ => false,
    }
}

/// Adopt a freshly-refreshed grant into `cfg` and PERSIST it to the credential store.
///
/// Three things move together, which is why this is one function rather than three lines at two
/// call sites (the reactive refresh and the proactive one):
///
/// 1. the in-memory access token, so the re-auth below and any LATER reconnect carry it;
/// 2. the refresh token — cTrader ROTATES it, so keeping the old one would spend a dead token on
///    the next refresh;
/// 3. the expiry, so the proactive tick knows when to go again.
///
/// The store write is BEST-EFFORT and never fails the refresh: the running session already holds a
/// working token, and refusing to continue because the store could not be written would turn a
/// successful self-heal into an outage. A failure is `warn!`ed with the PATH and the ACCOUNT ROW —
/// never a token value — and says what the consequence is (a restart falls back to the stale pair).
pub(super) fn adopt_refreshed_token(cfg: &mut ConnConfig, token: &oauth::Token) {
    // ONE clock read for both consumers: the expiry arithmetic and the change-journal stamp
    // (`vike_change_journal` reads no clock — the instant is a parameter all the way down).
    // The only clock read in this file, and off the fold: the actor thread, after an OAuth round trip.
    let now_ms = vike_model::now_ms();
    cfg.access_token = token.access_token.clone();
    cfg.refresh_token = Some(token.refresh_token.clone());
    cfg.token_expires_at_ms = crate::token_store::expires_at_ms(now_ms, token.expires_in);

    let Some(persist) = cfg.token_persist.clone() else {
        tracing::debug!(
            target: "ctrader",
            "token refreshed; no credential store was resolved for this mount, so the new pair              lives only as long as this process"
        );
        return;
    };
    save_rotation(&persist, token, now_ms);
}

/// Write a rotated pair to its rotation home through `crate::token_store::persist` — the ONE
/// writer — and say what happened. Best-effort: see [`adopt_refreshed_token`]. Shared by that
/// function (a config with no shared cell) and [`super::grant::SharedGrant::renew`] (every config
/// built by `CtraderConfig::to_conn_config`), so the two cannot word one fact two ways.
pub(super) fn save_rotation(persist: &TokenPersist, token: &oauth::Token, now_ms: i64) {
    match crate::token_store::persist(persist, token, now_ms) {
        // The PATH and the ACCOUNT ROW, never a value — and the row is what makes it verifiable.
        Ok(()) => tracing::info!(
            target: "ctrader",
            store = %persist.store().display(),
            account_row = ?persist.account_row,
            "refreshed cTrader grant persisted to the credential store (other rows untouched)"
        ),
        Err(e) => {
            tracing::warn!(target: "ctrader", error = %e, "could not persist the refreshed grant")
        }
    }
}

/// Why [`renew_grant`] produced no usable pair. Carries no cause on purpose: the OAuth error's
/// `Display` can echo the token endpoint's URL, which embeds the client secret and the refresh
/// token, so no caller can interpolate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenewError {
    /// The config holds no refresh token to spend.
    NoRefreshToken,
    /// The token endpoint refused the refresh (or was unreachable).
    Refused,
}

/// **Get `cfg` a pair the venue should honour again** — the ONE entry the three refresh sites go
/// through (the connect-time self-heal, the reactive refresh on an invalidation or auth error, the
/// proactive one on the heartbeat tick).
///
/// With a shared cell ([`ConnConfig::grant`]) this is [`super::grant::SharedGrant::renew`]: a pair a
/// sibling socket already rotated is ADOPTED without touching the venue, and only a current socket
/// spends the refresh token — under the cell's rotation gate, with the new pair saved before any
/// sibling that needs to spend can go again. Without one it is the historical single-config refresh
/// ([`adopt_refreshed_token`]), byte-identical to before the cell existed.
pub(super) fn renew_grant(cfg: &mut ConnConfig) -> Result<Renewal, RenewError> {
    let Some(refresh_token) = cfg.refresh_token.clone() else {
        return Err(RenewError::NoRefreshToken);
    };
    let (client_id, client_secret) = (cfg.client_id.clone(), cfg.client_secret.clone());
    let refresher = cfg.refresher.clone();
    let spend = move |token: &str| match &refresher {
        Some(refresher) => refresher(token),
        None => oauth::refresh(&client_id, &client_secret, token),
    };
    if let Some(cell) = cfg.grant.clone() {
        // The clock is read by the cell, once, AFTER the OAuth round trip (as `adopt_refreshed_token`
        // always did): off the fold, on the actor thread.
        let (how, grant) = cell
            .renew(cfg.grant_seen, vike_model::now_ms, spend)
            .map_err(|_| RenewError::Refused)?;
        cfg.adopt_grant(grant);
        return Ok(how);
    }
    match spend(&refresh_token) {
        Ok(token) => {
            adopt_refreshed_token(cfg, &token);
            Ok(Renewal::Refreshed)
        }
        Err(_) => Err(RenewError::Refused),
    }
}

/// Handle an `ACCOUNTS_TOKEN_INVALIDATED_EVENT` or an auth-flavored `ERROR_RES`: get `cfg` a
/// working pair ([`renew_grant`] — adopt a sibling socket's rotation, or spend the refresh token
/// once) and re-send `AccountAuth` on the SAME socket (a token invalidation does not imply the
/// transport died — no reconnect needed). Updates `cfg` in place so a LATER reconnect attempt also
/// carries the fresh token/refresh_token pair.
///
/// Returns `false` when there is no configured `refresh_token`, or the refresh/re-auth itself
/// fails: the caller goes to [`reconnect_with_backoff`], which keeps trying and refuses the account's
/// orders by name meanwhile. It used to stop the actor, which left the data keys of the socket silent
/// for the life of the process with nothing to say so. Exactly ONE `tracing::error!` is emitted in
/// that case, and it is a static message: the underlying `OAuthError`/`io::Error` is deliberately
/// NEVER interpolated into it, because `oauth`'s token-endpoint URL embeds the client_secret and
/// refresh_token as query params — a transport error's `Display` can otherwise echo that URL back.
pub(super) fn try_refresh_and_reauth(cfg: &mut ConnConfig, reader: &mut Reader, ctid: i64) -> bool {
    match renew_grant(cfg) {
        Ok(Renewal::Refreshed) => {}
        Ok(Renewal::Adopted) => tracing::info!(
            target: "ctrader",
            "another socket of this process already rotated the grant; adopting its pair \
             (the refresh token is not spent twice)"
        ),
        Err(RenewError::NoRefreshToken) => {
            tracing::error!(
                target: "ctrader",
                "access token invalidated and no refresh_token configured; opening a new session"
            );
            return false;
        }
        Err(RenewError::Refused) => {
            tracing::error!(
                target: "ctrader",
                "OAuth token refresh failed; opening a new session (no creds/token in this message)"
            );
            return false;
        }
    }

    let account_auth = ProtoOaAccountAuthReq {
        ctid_trader_account_id: ctid,
        access_token: cfg.access_token.clone(),
        ..Default::default()
    };
    if send(reader, pt::ACCOUNT_AUTH_REQ, &account_auth, REAUTH_MSG_ID).is_err() {
        tracing::error!(
            target: "ctrader",
            "failed to re-send AccountAuth after token refresh; opening a new session"
        );
        return false;
    }
    tracing::info!(target: "ctrader", "access token refreshed; re-authenticated");
    true
}

/// Reconnect loop for a dropped socket: waits out an exponential backoff, then replays the full
/// connect + auth + symbol-discovery handshake ([`open_and_handshake`]) pinned to the SAME `ctid`.
/// Checks `rx` for a pending `Shutdown` before every attempt, buffering any OTHER queued command
/// into `pending` (in arrival order) so the caller can replay it once reconnected instead of
/// silently dropping it. Returns `None` when `Shutdown` was observed (or the channel died) — the
/// caller should stop the actor without retrying further. ⚠ An order held past [`HOLD_ORDERS_FOR`] is refused by name
/// and never replayed, whatever keeps the session from opening ([`expires_while_held`] decides per kind).
///
/// `cfg` is `&mut` because each attempt first takes the process's CURRENT grant
/// ([`ConnConfig::sync_grant`]), so a socket that reconnects after a sibling rotated the grant
/// authenticates with the new pair and not with the one the venue ended.
///
/// ⚠ **A refusal of the access token spends the refresh token** ([`renew_grant`], at most once per
/// [`RENEW_EVERY`]). A store carries the pair and not its issue time, so a process that has not rotated
/// has no expiry to refresh ahead of, and this loop used to retry a lapsed token for as long as nothing
/// else renewed it. When the refresh is refused too the grant is dead: the loop keeps going so a later
/// rotation or a working endpoint heals the socket, every order command held or arriving is refused
/// by name instead of waiting to be placed hours later ([`refuse_order_command`]), and `lapse` tells the operator.
pub(super) fn reconnect_with_backoff(
    cfg: &mut ConnConfig,
    ctid: i64,
    rx: &Receiver<Command>,
    pending: &mut Vec<Command>,
    lapse: &mut SessionLapse,
    orders: &OrderLane<'_>,
) -> Option<(Reader, SymbolMap, u32)> {
    let dead_words = format!(
        "ctrader: the trading account is NOT AUTHORIZED: the venue refused its access token and the refresh \
         was refused too, so nothing was sent; re-issue the grant with `{REAUTHORIZE_CMD}`"
    );
    let mut backoff = RECONNECT_WAIT.initial;
    let mut spent_at: Option<Instant> = None;
    let mut grant_dead = false;
    let mut failed = 0_u32;
    let mut held = Held::new(rx, orders, cfg.hold_orders_for);
    loop {
        if !held.look(grant_dead.then_some(dead_words.as_str())) {
            return None;
        }
        thread::sleep(backoff);
        let moved = cfg.sync_grant();
        if grant_dead && !moved && spent_at.is_some_and(|at| at.elapsed() < RENEW_EVERY) {
            continue;
        }
        match open_and_handshake(cfg, Some(ctid)) {
            Ok((reader, _ctid, symbols, money_digits)) => {
                tracing::info!(target: "ctrader", "reconnected and re-authenticated");
                lapse.healed(cfg);
                // What arrived during the handshake, which can have blocked for a long time: it must not go out older
                // than the bound either. The session is open, so nothing is refused for the grant.
                if !held.look(None) {
                    return None;
                }
                pending.extend(held.commands.into_iter().map(|(_, cmd)| cmd));
                return Some((reader, symbols, money_digits));
            }
            Err(e) => {
                tracing::warn!(
                    target: "ctrader",
                    error = %e,
                    backoff_ms = backoff.as_millis() as u64,
                    "reconnect attempt failed; retrying"
                );
                failed = failed.saturating_add(1);
                backoff = (backoff * 2).min(RECONNECT_WAIT.max);
                if !e.is_token_refusal() {
                    continue;
                }
                let may_spend = spent_at.is_none_or(|at| at.elapsed() >= RENEW_EVERY);
                if may_spend {
                    spent_at = Some(Instant::now());
                }
                if may_spend && renew_grant(cfg).is_ok() {
                    grant_dead = false;
                    backoff = RECONNECT_WAIT.initial;
                    lapse.found(cfg, SESSION_GONE, WAITING_FOR_A_SESSION);
                    continue;
                }
                lapse.found(cfg, &dead_words, GRANT_IS_DEAD);
                lapse.state(cfg, &dead_words, GRANT_IS_DEAD, failed, RENEW_EVERY.as_millis());
                if !grant_dead {
                    grant_dead = true;
                    tracing::error!(target: "ctrader", "{dead_words}");
                    held.refuse_all(&dead_words);
                }
            }
        }
    }
}
