//! The connect + two-stage auth handshake, its bounded retry and self-healing refresh, the spawn.

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use prost::Message;

use vike_data::LiveDataSink;
use vike_exec::EventSender;

use super::actor::actor_loop;
use super::reconcile::seed_positions_at_connect;
use super::session::adopt_refreshed_token;
use super::stream::{EofStream, Stream, wrap_tls};
use super::{
    ActorHandle, CloseState, Command, ConnConfig, ConnError, OrderIdMap, PositionMap, Reader,
    decode_payload, open_and_handshake, read_until, send,
};
use crate::framing::FrameReader;
use crate::oauth;
use crate::positions::{CloseTracker, PositionBook};
use crate::proto::{
    ProtoOaAccountAuthReq, ProtoOaApplicationAuthReq, ProtoOaAssetClassListReq,
    ProtoOaAssetClassListRes, ProtoOaAssetListReq, ProtoOaAssetListRes,
    ProtoOaGetAccountListByAccessTokenReq, ProtoOaGetAccountListByAccessTokenRes,
    ProtoOaSymbolByIdReq, ProtoOaSymbolByIdRes, ProtoOaSymbolCategoryListReq,
    ProtoOaSymbolCategoryListRes, ProtoOaSymbolsListReq, ProtoOaSymbolsListRes, ProtoOaTraderReq,
    ProtoOaTraderRes, pt,
};
use crate::symbols::{SymbolLists, SymbolMap};

#[cfg(doc)]
use super::fetch_symbol_lists;
#[cfg(doc)]
use super::session::reconnect_with_backoff;

/// Read ONE broker reference list (categories, asset classes or assets) during the handshake.
/// A venue REFUSAL (`ERROR_RES`) is logged and read as an empty list: these lists only name what
/// a symbol is (the catalog's class and base/quote), and a broker that declines one must not keep
/// a live mount from trading. Every other failure (I/O, timeout, decode) fails the handshake like
/// any other step.
fn read_reference_list<Req: Message, Res: Message + Default, T>(
    reader: &mut Reader,
    (req_pt, res_pt): (u32, u32),
    req: &Req,
    msg_id: &str,
    take: impl FnOnce(Res) -> Vec<T>,
) -> Result<Vec<T>, ConnError> {
    send(reader, req_pt, req, msg_id)?;
    match read_until(reader, res_pt) {
        Ok(msg) => Ok(take(decode_payload(&msg)?)),
        Err(ConnError::Venue { error_code, description }) => {
            tracing::warn!(
                target: "ctrader",
                error_code = %error_code,
                description = %description,
                payload_type = req_pt,
                "the venue refused a symbol reference list; its symbols read as uncategorised"
            );
            Ok(Vec::new())
        }
        Err(e) => Err(e),
    }
}

/// The handshake itself ([`open_and_handshake`]'s body), returning the raw [`SymbolLists`] instead
/// of the built map so [`fetch_symbol_lists`] can hand them out verbatim.
pub(super) fn open_and_read_lists(
    cfg: &ConnConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolLists, u32), ConnError> {
    let tcp = TcpStream::connect((cfg.host.as_str(), cfg.port))?;
    // Short read timeout BEFORE any TLS wrap so both the handshake loop and the actor loop see
    // `Ok(None)` on idle rather than blocking.
    tcp.set_read_timeout(Some(Duration::from_secs(1)))?;
    let stream = if cfg.no_tls { Stream::Plain(tcp) } else { wrap_tls(&cfg.host, tcp)? };

    let (eof_stream, _eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    // 1. ApplicationAuth.
    let app = ProtoOaApplicationAuthReq {
        client_id: cfg.client_id.clone(),
        client_secret: cfg.client_secret.clone(),
        ..Default::default()
    };
    send(&mut reader, pt::APPLICATION_AUTH_REQ, &app, "app")?;
    read_until(&mut reader, pt::APPLICATION_AUTH_RES)?;

    // 2. Resolve the ctid: the caller's fixed override (reconnect) -> the configured one -> else
    //    discover via GetAccountListByAccessToken and pick the first demo (non-live) account
    //    (matches ctrader_catcher.py::prove). NEVER fall back to a live account here — an explicit
    //    ctid is the only sanctioned way to target a live/specific account; silently authorizing
    //    whatever account happens to be first (which may be live) is a safety hazard.
    let ctid = match fixed_ctid.or(cfg.account_id) {
        Some(id) => id,
        None => {
            let req = ProtoOaGetAccountListByAccessTokenReq {
                access_token: cfg.access_token.clone(),
                ..Default::default()
            };
            send(&mut reader, pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ, &req, "accts")?;
            let msg = read_until(&mut reader, pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES)?;
            let res: ProtoOaGetAccountListByAccessTokenRes = decode_payload(&msg)?;
            let acct = res
                .ctid_trader_account
                .iter()
                .find(|a| !a.is_live.unwrap_or(false))
                .ok_or(ConnError::NoAccounts)?;
            acct.ctid_trader_account_id as i64
        }
    };

    // 3. AccountAuth.
    let account_auth = ProtoOaAccountAuthReq {
        ctid_trader_account_id: ctid,
        access_token: cfg.access_token.clone(),
        ..Default::default()
    };
    send(&mut reader, pt::ACCOUNT_AUTH_REQ, &account_auth, "aa")?;
    read_until(&mut reader, pt::ACCOUNT_AUTH_RES)?;

    // 4. Trader (account info) — the account-level `money_digits`, the exponent balance
    //    integers are scaled by; also the FALLBACK exponent for a deal's commission when that
    //    deal omits its own `ProtoOADeal.moneyDigits` (tag 17 — present per-deal in practice; its
    //    wire doc says "Affects commission." directly, so `event_mapper::deal_fill` prefers it and
    //    only falls back to this account-level value). Live-verified 2026-07-14: a demo account's
    //    `money_digits` is 2 (commission=-3 raw -> -0.03 after the vike-model sign flip), but this
    //    is fetched per-account rather than hardcoded — a different broker/account could scale
    //    differently.
    let trader_req = ProtoOaTraderReq { ctid_trader_account_id: ctid, ..Default::default() };
    send(&mut reader, pt::TRADER_REQ, &trader_req, "tr")?;
    let msg = read_until(&mut reader, pt::TRADER_RES)?;
    let trader_res: ProtoOaTraderRes = decode_payload(&msg)?;
    let money_digits = trader_res.trader.money_digits.unwrap_or(0);

    // 5. The three broker reference lists that NAME what a symbol is: its category (and through
    //    the category its asset class) and its base/quote assets. The catalog's class and
    //    base/quote are read from these (`crate::catalog::kind_of`), never from a symbol name.
    //    Sent BEFORE the symbol lists so `SymbolById` stays the handshake's last step.
    let categories = read_reference_list(
        &mut reader,
        (pt::SYMBOL_CATEGORY_REQ, pt::SYMBOL_CATEGORY_RES),
        &ProtoOaSymbolCategoryListReq { ctid_trader_account_id: ctid, ..Default::default() },
        "cat",
        |res: ProtoOaSymbolCategoryListRes| res.symbol_category,
    )?;
    let asset_classes = read_reference_list(
        &mut reader,
        (pt::ASSET_CLASS_LIST_REQ, pt::ASSET_CLASS_LIST_RES),
        &ProtoOaAssetClassListReq { ctid_trader_account_id: ctid, ..Default::default() },
        "acl",
        |res: ProtoOaAssetClassListRes| res.asset_class,
    )?;
    let assets = read_reference_list(
        &mut reader,
        (pt::ASSET_LIST_REQ, pt::ASSET_LIST_RES),
        &ProtoOaAssetListReq { ctid_trader_account_id: ctid, ..Default::default() },
        "ast",
        |res: ProtoOaAssetListRes| res.asset,
    )?;

    // 6. SymbolsList (light symbols: name<->id, NO digits).
    let symbols_req = ProtoOaSymbolsListReq {
        ctid_trader_account_id: ctid,
        include_archived_symbols: Some(false),
        ..Default::default()
    };
    send(&mut reader, pt::SYMBOLS_LIST_REQ, &symbols_req, "sy")?;
    let msg = read_until(&mut reader, pt::SYMBOLS_LIST_RES)?;
    let list: ProtoOaSymbolsListRes = decode_payload(&msg)?;

    // 7. SymbolById (full symbols: digits -> scale). ONE batch request for every id (resolution
    //    1). If a broker's list is enormous this is the place to switch to lazy per-subscribe
    //    fetches; the batch keeps `scale()` populated eagerly.
    let ids: Vec<i64> = list.symbol.iter().map(|s| s.symbol_id).collect();
    let by_id_req =
        ProtoOaSymbolByIdReq { ctid_trader_account_id: ctid, symbol_id: ids, ..Default::default() };
    send(&mut reader, pt::SYMBOL_BY_ID_REQ, &by_id_req, "sbid")?;
    let msg = read_until(&mut reader, pt::SYMBOL_BY_ID_RES)?;
    let by_id: ProtoOaSymbolByIdRes = decode_payload(&msg)?;

    let lists =
        SymbolLists { light: list.symbol, full: by_id.symbol, categories, asset_classes, assets };
    Ok((reader, ctid, lists, money_digits))
}

/// [`open_and_handshake_retry`] that SELF-HEALS an expired grant: on an auth/token refusal it
/// spends the configured refresh token once, adopts + PERSISTS the new pair, and retries the
/// handshake a single time.
///
/// # Why the initial connect needed this, and not just the actor loop
///
/// The reactive refresh (`try_refresh_and_reauth`) only ever ran on an ALREADY-AUTHENTICATED
/// socket — an `ACCOUNTS_TOKEN_INVALIDATED_EVENT` mid-session. A grant that had already lapsed
/// before startup therefore never reached it: `open_and_handshake_retry` classifies an auth
/// rejection as PERMANENT and returns immediately, so the daemon refused to start while a perfectly
/// good refresh token sat in the same credential store. That is exactly what the live rehearsal hit
/// (the alpaca+ctrader live rehearsal (PR #1407), Evidence 2 — `CH_ACCESS_TOKEN_INVALID`,
/// "Access token expired", daemon down).
///
/// # Bounded, and it stays bounded
///
/// EXACTLY ONE refresh + ONE retry. A refresh that succeeds and a handshake that then fails again
/// returns that second error unchanged — retrying further would spin an OAuth endpoint on a
/// credential that is not the problem. A non-auth failure is returned untouched and costs nothing:
/// this wrapper is inert on every healthy startup, which is the byte-identical claim.
///
/// # The refusal names the remedy
///
/// When there is no refresh token, or the refresh itself is refused, the operator's grant is dead
/// and only re-authorizing fixes it — so the error log names
/// [`crate::token_store::REAUTHORIZE_CMD`] verbatim. The rehearsal's daemon printed the venue's
/// `CH_ACCESS_TOKEN_INVALID` and stopped there, which says what broke and not what to do.
fn open_and_handshake_self_healing(
    cfg: &mut ConnConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnError> {
    let first = match open_and_handshake_retry(cfg, fixed_ctid) {
        Ok(ok) => return Ok(ok),
        Err(e) => e,
    };
    if !first.is_auth_failure() {
        return Err(first);
    }

    let Some(refresh_token) = cfg.refresh_token.clone() else {
        tracing::error!(
            target: "ctrader",
            error = %first,
            "the access token was refused and NO refresh token is configured — re-issue the grant              with `{}` (the daemon cannot self-heal this)",
            crate::token_store::REAUTHORIZE_CMD
        );
        return Err(first);
    };

    tracing::warn!(
        target: "ctrader",
        error = %first,
        "the access token was refused at connect; spending the refresh token once before giving up"
    );
    // ⚠ The error is NEVER interpolated: `oauth`'s token endpoint embeds the client_secret and the
    // refresh token as query params, and a transport error's `Display` can echo that URL back.
    let token = match oauth::refresh(&cfg.client_id, &cfg.client_secret, &refresh_token) {
        Ok(t) => t,
        Err(_) => {
            tracing::error!(
                target: "ctrader",
                "the refresh token was ALSO refused (no creds/token in this message) — the grant is                  dead; re-issue it with `{}`",
                crate::token_store::REAUTHORIZE_CMD
            );
            return Err(first);
        }
    };
    // Adopts the pair into `cfg` AND writes it back to the credential store, so the very next
    // start does not repeat this round trip.
    adopt_refreshed_token(cfg, &token);
    tracing::info!(target: "ctrader", "grant refreshed at connect; retrying the handshake once");
    open_and_handshake_retry(cfg, fixed_ctid)
}

/// Run [`open_and_handshake`] under the bounded exponential-backoff [`ConnConfig::connect_retry`]
/// policy — the hardening that keeps a transient network/TLS blip during startup from permanently
/// dropping the venue to paper.
///
/// Retries ONLY a TRANSIENT fault ([`ConnError::is_transient`] — a failed TCP/TLS connect, a read
/// timeout, a connection reset, a peer that closed mid-handshake, or no response), sleeping the
/// doubling, capped backoff between attempts. A PERMANENT failure (auth REJECTED, no accounts,
/// decode/tls-config) returns IMMEDIATELY — retrying a bad credential only delays the paper
/// fallback. On exhausting `max_attempts` the LAST error is returned UNCHANGED, so the ultimate
/// failure contract (terminal error → paper fallback) is preserved verbatim; the retry only gives a
/// transient blip more chances first.
///
/// INERT on success: the first attempt succeeding neither sleeps nor re-sends a frame, so a healthy
/// startup is byte-identical to calling [`open_and_handshake`] directly. Deliberately SEPARATE from
/// [`reconnect_with_backoff`] (the unbounded mid-session reconnect for an already-authed socket that
/// drops) — this bounds the FIRST connect so a dead venue eventually yields to paper rather than
/// looping forever. `fetch_symbols` (catalog enumeration) and [`crate::recon_client`] (lazy
/// revive-on-next-fetch) keep their own single-shot paths and do NOT route through here.
pub(super) fn open_and_handshake_retry(
    cfg: &ConnConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnError> {
    let policy = cfg.connect_retry;
    let max_attempts = policy.max_attempts.max(1);
    let mut backoff = policy.initial_backoff;
    let mut attempt = 1u32;
    loop {
        match open_and_handshake(cfg, fixed_ctid) {
            Ok(ready) => return Ok(ready),
            // Give up on the last attempt, or immediately on a permanent (non-transient) fault.
            Err(e) if attempt >= max_attempts || !e.is_transient() => return Err(e),
            Err(e) => {
                tracing::warn!(
                    target: "ctrader",
                    error = %e,
                    attempt,
                    max_attempts,
                    backoff_ms = backoff.as_millis() as u64,
                    "initial connect attempt failed transiently; retrying after backoff"
                );
                thread::sleep(backoff);
                backoff = (backoff * 2).min(policy.max_backoff);
                attempt += 1;
            }
        }
    }
}

pub(super) fn connect_and_auth_inner(
    cfg: ConnConfig,
    sink: Arc<dyn LiveDataSink>,
    events: Option<EventSender>,
) -> Result<ActorHandle, ConnError> {
    let mut cfg = cfg;
    let (mut reader, ctid, symbols, money_digits) =
        open_and_handshake_self_healing(&mut cfg, None)?;
    let symbols = Arc::new(symbols);

    // Spawn the read/heartbeat/command loop. The actor gets its OWN clone of `cfg` (it needs
    // `client_id`/`client_secret`/`access_token`/`refresh_token`/`host`/`port` to reconnect and,
    // separately, to refresh an invalidated token — Task 6).
    let (tx, rx) = mpsc::channel::<Command>();
    let actor_symbols = symbols.clone();
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::new()));
    let actor_orders = orders.clone();
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let actor_positions = positions.clone();
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let actor_closes = closes.clone();

    // Seed the OPEN-position book BEFORE the actor thread starts — see `seed_positions_at_connect`
    // for why a fresh mount used to start (and stay) blind. EXEC MOUNTS ONLY: a data-only
    // connection has no reduce routing and no halt boundary to feed, so it keeps its byte-identical
    // handshake with no extra round trip.
    if events.is_some() {
        seed_positions_at_connect(
            &mut reader,
            ctid,
            symbols.as_ref(),
            money_digits,
            sink.as_ref(),
            events.as_ref(),
            &orders,
            &positions,
            &closes,
        );
    }

    let actor_cfg = cfg.clone();
    let join = thread::Builder::new()
        .name("ctrader-actor".into())
        .spawn(move || {
            actor_loop(
                reader,
                rx,
                actor_cfg,
                ctid,
                actor_symbols,
                money_digits,
                sink,
                events,
                actor_orders,
                actor_positions,
                actor_closes,
            )
        })
        .map_err(ConnError::Io)?;

    Ok(ActorHandle { tx, symbols, ctid, orders, positions, closes, join: Some(join) })
}
