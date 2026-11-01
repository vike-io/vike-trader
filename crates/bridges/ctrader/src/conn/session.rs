//! Session upkeep: auth-error classification, the persisted OAuth refresh, the reconnect loop.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;

use vike_venues::venue_timings::{self, Reconnect};

use super::grant::Renewal;
use super::stream::decode_error;
use super::{Command, ConnConfig, ConnError, Reader, open_and_handshake, send};
use crate::oauth;
use crate::proto::{ProtoMessage, ProtoOaAccountAuthReq, pt};
use crate::symbols::SymbolMap;
use crate::token_store::TokenPersist;

/// The wait before each reconnect attempt after a dropped socket: the exec lane's `reconnect` in
/// `vike_venues::venue_timings::CTRADER`, which carries the reason. `initial` is the wait
/// before the first attempt and it doubles on every failed attempt, capped at `max`.
/// `pub(super)`: `conn_tests.rs` pins its effective value.
pub(super) const RECONNECT_WAIT: Reconnect = venue_timings::CTRADER.exec().reconnect();

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
    if let Some(cell) = cfg.grant.clone() {
        let (client_id, client_secret) = (cfg.client_id.clone(), cfg.client_secret.clone());
        // The clock is read by the cell, once, AFTER the OAuth round trip (as `adopt_refreshed_token`
        // always did): off the fold, on the actor thread.
        let (how, grant) = cell
            .renew(cfg.grant_seen, vike_model::now_ms, |spend| {
                oauth::refresh(&client_id, &client_secret, spend)
            })
            .map_err(|_| RenewError::Refused)?;
        cfg.adopt_grant(grant);
        return Ok(how);
    }
    match oauth::refresh(&cfg.client_id, &cfg.client_secret, &refresh_token) {
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
/// fails — the caller degrades (stops the actor). Exactly ONE `tracing::error!` is emitted in
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
                "access token invalidated and no refresh_token configured; actor stopping"
            );
            return false;
        }
        Err(RenewError::Refused) => {
            tracing::error!(
                target: "ctrader",
                "OAuth token refresh failed; actor stopping (no creds/token in this message)"
            );
            return false;
        }
    }

    let account_auth = ProtoOaAccountAuthReq {
        ctid_trader_account_id: ctid,
        access_token: cfg.access_token.clone(),
        ..Default::default()
    };
    if send(reader, pt::ACCOUNT_AUTH_REQ, &account_auth, "aa-refresh").is_err() {
        tracing::error!(
            target: "ctrader",
            "failed to re-send AccountAuth after token refresh; actor stopping"
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
/// caller should stop the actor without retrying further.
///
/// `cfg` is `&mut` for one reason: each attempt first takes the process's CURRENT grant
/// ([`ConnConfig::sync_grant`]), so a socket that reconnects after a sibling rotated the grant
/// authenticates with the new pair and not with the one the venue ended — without it the loop
/// would retry the superseded access token forever. It does not refresh: a lapsed grant that no
/// socket has renewed is still the unbounded retry it always was.
pub(super) fn reconnect_with_backoff(
    cfg: &mut ConnConfig,
    ctid: i64,
    rx: &Receiver<Command>,
    pending: &mut Vec<Command>,
) -> Option<(Reader, SymbolMap, u32)> {
    let mut backoff = RECONNECT_WAIT.initial;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Command::Shutdown) => return None,
                Ok(cmd) => pending.push(cmd),
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => break,
            }
        }
        thread::sleep(backoff);
        cfg.sync_grant();
        match open_and_handshake(cfg, Some(ctid)) {
            Ok((reader, _ctid, symbols, money_digits)) => {
                tracing::info!(target: "ctrader", "reconnected and re-authenticated");
                return Some((reader, symbols, money_digits));
            }
            Err(e) => {
                tracing::warn!(
                    target: "ctrader",
                    error = %e,
                    backoff_ms = backoff.as_millis() as u64,
                    "reconnect attempt failed; retrying"
                );
                backoff = (backoff * 2).min(RECONNECT_WAIT.max);
            }
        }
    }
}
