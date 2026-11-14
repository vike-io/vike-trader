//! The connect + two-stage auth handshake, its bounded retry and self-healing refresh, the spawn.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use prost::Message;

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_data::DataClientSink;
use vike_execution::EventSender;

use super::actor::actor_loop;
use super::grant::Renewal;
use super::reconcile::{flag_orphaned_exits, seed_positions_at_connect};
use super::session::{RenewError, renew_grant};
use super::stream::{ConnectionSocket, EofStream, wrap_tls};
use super::{
    ActorHandle, CloseState, ConnectionCommand, ConnectionConfig, ConnectionError,
    PositionsByIdShared, Reader, decode_payload, open_and_handshake, read_until_by, send,
};
use crate::execution::positions::{CloseTracker, PositionsById};
use crate::instrument::symbols::{SymbolLists, SymbolMap};
use crate::transport::framing::FrameReader;
use crate::transport::proto::{
    ProtoOaAccountAuthReq, ProtoOaApplicationAuthReq, ProtoOaAsset, ProtoOaAssetClassListReq,
    ProtoOaAssetClassListRes, ProtoOaAssetListReq, ProtoOaAssetListRes,
    ProtoOaGetAccountListByAccessTokenReq, ProtoOaGetAccountListByAccessTokenRes,
    ProtoOaSymbolByIdReq, ProtoOaSymbolByIdRes, ProtoOaSymbolCategoryListReq,
    ProtoOaSymbolCategoryListRes, ProtoOaSymbolsListReq, ProtoOaSymbolsListRes, ProtoOaTrader,
    ProtoOaTraderReq, ProtoOaTraderRes, pt,
};

#[cfg(doc)]
use super::fetch_symbol_lists;
#[cfg(doc)]
use super::session::reconnect_with_backoff;

/// Read ONE broker reference list (categories, asset classes or assets) during the handshake.
/// A venue REFUSAL (`ERROR_RES`) is logged and read as an empty list: these lists only name what
/// a symbol is (the catalog's class and base/quote), and a broker that declines one must not keep
/// a live start from trading. Every other failure (I/O, timeout, decode) fails the handshake like
/// any other step.
fn read_reference_list<SeenRequest: Message, Res: Message + Default, T>(
    reader: &mut Reader,
    (req_pt, res_pt): (u32, u32),
    req: &SeenRequest,
    message_id: &str,
    ceiling: Option<Instant>,
    take: impl FnOnce(Res) -> Vec<T>,
) -> Result<Vec<T>, ConnectionError> {
    send(reader, req_pt, req, message_id)?;
    match read_until_by(reader, res_pt, ceiling) {
        Ok(msg) => Ok(take(decode_payload(&msg)?)),
        Err(ConnectionError::Venue { error_code, description }) => {
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

/// How long ONE connect attempt of a bounded ask ([`open_and_read_lists_by`] with a ceiling) may take.
/// The actor's own connect takes the exec row's dial window instead ([`DIAL_WINDOW`]): it is the path that
/// starts a venue, and this bound is for the one-shot asks a gatewayhub lookup thread waits on (a
/// dropped-packet host held such a thread for the OS connect timeout, about two minutes).
///
/// shortcut: `ToSocketAddrs` is the OS resolver and has no bound of its own here; a resolver that stalls
/// still holds the thread for its own timeout. Upgrade by resolving on a helper thread if DNS stalls are
/// ever seen in the lookup threads' log.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// The window ONE address's dial of an ask with no ceiling (the actor's session, its reconnects, the listing) may
/// take: the exec row's `dial_timeout` (owner ruling B-13). It used to be the OS connect timeout, so a host that
/// answers no SYN held the actor, and the daemon's stop behind it, for about two minutes.
const DIAL_WINDOW: Duration = vike_venues::venue_timings::CTRADER.exec().dial_timeout();

/// Connect to `config`'s endpoint as `TcpStream::connect` does, one address after another, each within `window`.
fn connect_within(config: &ConnectionConfig, window: Duration) -> io::Result<TcpStream> {
    let mut last =
        io::Error::new(io::ErrorKind::InvalidInput, "could not resolve to any addresses");
    for addr in (config.host.as_str(), config.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, window) {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// What ONE connect attempt may take when `left` of the ask's ceiling is all that remains for `remaining`
/// addresses (this one included): an equal share, never more than [`CONNECT_TIMEOUT`]. A dual-stack host
/// resolves to several addresses, and the first, blackholed one used to burn four seconds of a five-second ask
/// (`ACCOUNT_CURRENCY_DEADLINE`) before the working one was tried.
fn attempt_share(left: Duration, remaining: usize) -> Duration {
    let remaining = u32::try_from(remaining.max(1)).unwrap_or(u32::MAX);
    (left / remaining).min(CONNECT_TIMEOUT)
}

/// Connect to `config`'s endpoint, each address for at most [`CONNECT_TIMEOUT`] (and an equal share of what is
/// left, [`attempt_share`]) and all of them within `ceiling`. A ceiling already passed connects to nothing.
fn connect_by(config: &ConnectionConfig, ceiling: Instant) -> io::Result<TcpStream> {
    let mut last = io::Error::new(
        io::ErrorKind::TimedOut,
        "the ask's deadline passed before a connection was made",
    );
    let addrs: Vec<_> = (config.host.as_str(), config.port).to_socket_addrs()?.collect();
    for (tried, addr) in addrs.iter().enumerate() {
        let left = ceiling.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(addr, attempt_share(left, addrs.len() - tried)) {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The handshake itself ([`open_and_handshake`]'s body), returning the raw [`SymbolLists`] instead
/// of the built map so [`fetch_symbol_lists`] can hand them out verbatim. The last element is the
/// trader's `moneyDigits` AS THE VENUE STATED IT (`None`: it stated none), see [`open_through_trader`].
pub(super) fn open_and_read_lists(
    config: &ConnectionConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolLists, Option<u32>), ConnectionError> {
    open_and_read_lists_by(config, fixed_ctid, None)
}

/// [`open_and_read_lists`] under ONE overall `ceiling` (the end of a bounded ask), enforced across the
/// connect ([`connect_by`]) and every step's wait ([`read_until_by`]); `None` is the handshake the actor
/// and the listing run, with no ceiling over it and only the dial bounded ([`DIAL_WINDOW`]).
pub(super) fn open_and_read_lists_by(
    config: &ConnectionConfig,
    fixed_ctid: Option<i64>,
    ceiling: Option<Instant>,
) -> Result<(Reader, i64, SymbolLists, Option<u32>), ConnectionError> {
    let (mut reader, ctid, trader) = open_through_trader(config, fixed_ctid, ceiling)?;

    // 5. The three broker reference lists that NAME what a symbol is: its category (and through
    //    the category its asset class) and its base/quote assets. The catalog's class and
    //    base/quote are read from these (`crate::catalog::kind_of`), never from a symbol name.
    //    Sent BEFORE the symbol lists so `SymbolById` stays the handshake's last step.
    let categories = read_reference_list(
        &mut reader,
        (pt::SYMBOL_CATEGORY_REQ, pt::SYMBOL_CATEGORY_RES),
        &ProtoOaSymbolCategoryListReq { ctid_trader_account_id: ctid, ..Default::default() },
        "cat",
        ceiling,
        |res: ProtoOaSymbolCategoryListRes| res.symbol_category,
    )?;
    let asset_classes = read_reference_list(
        &mut reader,
        (pt::ASSET_CLASS_LIST_REQ, pt::ASSET_CLASS_LIST_RES),
        &ProtoOaAssetClassListReq { ctid_trader_account_id: ctid, ..Default::default() },
        "acl",
        ceiling,
        |res: ProtoOaAssetClassListRes| res.asset_class,
    )?;
    let assets = read_asset_list(&mut reader, ctid, ceiling)?;

    // 6. SymbolsList (light symbols: name<->id, NO digits).
    let symbols_req = ProtoOaSymbolsListReq {
        ctid_trader_account_id: ctid,
        include_archived_symbols: Some(false),
        ..Default::default()
    };
    send(&mut reader, pt::SYMBOLS_LIST_REQ, &symbols_req, "sy")?;
    let msg = read_until_by(&mut reader, pt::SYMBOLS_LIST_RES, ceiling)?;
    let list: ProtoOaSymbolsListRes = decode_payload(&msg)?;

    // 7. SymbolById (full symbols: digits -> scale). ONE batch request for every id (resolution
    //    1). If a broker's list is enormous this is the place to switch to lazy per-subscribe
    //    fetches; the batch keeps `scale()` populated eagerly.
    let ids: Vec<i64> = list.symbol.iter().map(|s| s.symbol_id).collect();
    let by_id_req =
        ProtoOaSymbolByIdReq { ctid_trader_account_id: ctid, symbol_id: ids, ..Default::default() };
    send(&mut reader, pt::SYMBOL_BY_ID_REQ, &by_id_req, "sbid")?;
    let msg = read_until_by(&mut reader, pt::SYMBOL_BY_ID_RES, ceiling)?;
    let by_id: ProtoOaSymbolByIdRes = decode_payload(&msg)?;

    let lists = SymbolLists {
        light: list.symbol,
        full: by_id.symbol,
        categories,
        asset_classes,
        assets,
        deposit_asset_id: Some(trader.deposit_asset_id),
        account_type: trader.account_type,
    };
    Ok((reader, ctid, lists, trader.money_digits))
}

/// Read the broker's ASSET list (step 5's third list), the one that names an asset id: the deposit asset
/// and every base and quote asset of a symbol. A venue refusal reads as an empty list, as for the other
/// reference lists ([`read_reference_list`]).
fn read_asset_list(
    reader: &mut Reader,
    ctid: i64,
    ceiling: Option<Instant>,
) -> Result<Vec<ProtoOaAsset>, ConnectionError> {
    read_reference_list(
        reader,
        (pt::ASSET_LIST_REQ, pt::ASSET_LIST_RES),
        &ProtoOaAssetListReq { ctid_trader_account_id: ctid, ..Default::default() },
        "ast",
        ceiling,
        |res: ProtoOaAssetListRes| res.asset,
    )
}

/// **The account's deposit currency alone**: the handshake STOPPED after the asset list, which is the last step
/// that names it (`ProtoOATrader.depositAssetId` through `ProtoOAAssetListRes`). It does not ask for the
/// categories, the asset classes, the symbol list or the full symbols, which a broker with a large list answers
/// slowest and which this answer does not need. `None`: the asset list does not name the deposit asset. The
/// socket closes when this returns.
pub(super) fn open_and_read_deposit_currency_by(
    config: &ConnectionConfig,
    ceiling: Option<Instant>,
) -> Result<Option<String>, ConnectionError> {
    let (mut reader, ctid, trader) = open_through_trader(config, None, ceiling)?;
    let assets = read_asset_list(&mut reader, ctid, ceiling)?;
    let lists = SymbolLists {
        assets,
        deposit_asset_id: Some(trader.deposit_asset_id),
        ..Default::default()
    };
    Ok(SymbolMap::from_lists(&lists).deposit_currency().map(str::to_string))
}

/// **The scale of every money integer of a session, or a refusal in words.** `stated` is
/// `ProtoOATrader.moneyDigits` as the venue sent it: an optional field with NO documented default, so an absent
/// one is not `0` (which would read a true 2 a hundred times too large in every commission, conversion fee and
/// balance). Permanent ([`ConnectionError::Decode`] is not transient), so the connect retry does not spin on it and
/// the start ends NOT RUNNING with this reason.
pub(crate) fn require_money_digits(stated: Option<u32>) -> Result<u32, ConnectionError> {
    stated.ok_or_else(|| {
        ConnectionError::Decode(
            "the venue's trader record states no moneyDigits, the scale of every money integer \
             (commission, conversion fee, balance), and the vendor documents no default: refusing \
             the session rather than reading the money at a scale of 0"
                .to_string(),
        )
    })
}

/// Steps 1 to 4 of the handshake: connect (within `ceiling`, when there is one), application auth, the ctid,
/// account auth and the trader record. Returns the reader, the ctid and the trader record: its
/// `moneyDigits`, its deposit asset id and its account type are read by the callers.
///
/// ⚠ `moneyDigits` is `ProtoOATrader.moneyDigits`, an OPTIONAL field whose default the vendor never states, so
/// an absent one is `None` and NEVER `0`: a venue that omits it while the true scale is 2 would otherwise have
/// every commission, conversion fee and balance integer read 100 times too large. The sessions that scale a
/// money field ([`open_and_handshake`]) refuse a `None`; the asks that scale nothing (the symbol list, the
/// deposit currency, a rate) do not need it.
///
/// A refusal of the access token is told to the grant's cell, whoever met it: every session of the process comes
/// through here, and only the actors can spend ([`super::grant::SharedGrant::note_token_refused`]). A handshake the
/// venue authorized ends that note ([`super::grant::SharedGrant::note_token_honoured`]).
fn open_through_trader(
    config: &ConnectionConfig,
    fixed_ctid: Option<i64>,
    ceiling: Option<Instant>,
) -> Result<(Reader, i64, ProtoOaTrader), ConnectionError> {
    // Every session of the process logs in here, and none does while the FX market is closed (owner ruling
    // 2026-11-13): refused in its words before anything is dialled. The one exception is the config's own
    // (`ConnectionConfig::fx_week_holds_login`: the data feed's first, synchronous handshake).
    if let Some(words) = vike_bridge_core::fx_login::closed_words(super::VENUE)
        .filter(|_| config.fx_week_holds_login)
    {
        return Err(ConnectionError::FxMarketClosed(words));
    }
    let opened = dial_through_trader(config, fixed_ctid, ceiling);
    match (&opened, &config.grant) {
        (Err(e), Some(cell)) if e.is_token_refusal() => cell.note_token_refused(config.grant_seen),
        (Ok(_), Some(cell)) => cell.note_token_honoured(config.grant_seen),
        _ => {}
    }
    opened
}

/// [`open_through_trader`]'s body.
fn dial_through_trader(
    config: &ConnectionConfig,
    fixed_ctid: Option<i64>,
    ceiling: Option<Instant>,
) -> Result<(Reader, i64, ProtoOaTrader), ConnectionError> {
    let tcp = match ceiling {
        Some(ceiling) => connect_by(config, ceiling)?,
        None => connect_within(config, DIAL_WINDOW)?,
    };
    // Short read timeout BEFORE any TLS wrap so the handshake loop sees `Ok(None)` on idle rather than
    // blocking. ⚠ Not the actor's poll, which it sets for itself ([`super::READ_TIMEOUT`] says why).
    tcp.set_read_timeout(Some(config.read_timeout))?;
    let stream =
        if config.no_tls { ConnectionSocket::Plain(tcp) } else { wrap_tls(&config.host, tcp)? };

    let (eof_stream, _eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    // 1. ApplicationAuth.
    let app = ProtoOaApplicationAuthReq {
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.expose().clone(),
        ..Default::default()
    };
    send(&mut reader, pt::APPLICATION_AUTH_REQ, &app, "app")?;
    read_until_by(&mut reader, pt::APPLICATION_AUTH_RES, ceiling)?;

    // 2. Resolve the ctid: the caller's fixed override (reconnect) -> the configured one -> else
    //    discover via GetAccountListByAccessToken and pick the first demo (non-live) account
    //    (matches ctrader_catcher.py::prove). NEVER fall back to a live account here — an explicit
    //    ctid is the only sanctioned way to target a live/specific account; silently authorizing
    //    whatever account happens to be first (which may be live) is a safety hazard.
    let ctid = match fixed_ctid.or(config.account_id) {
        Some(id) => id,
        None => {
            let req = ProtoOaGetAccountListByAccessTokenReq {
                access_token: config.access_token.expose().clone(),
                ..Default::default()
            };
            send(&mut reader, pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ, &req, "accts")?;
            let msg = read_until_by(&mut reader, pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES, ceiling)?;
            let res: ProtoOaGetAccountListByAccessTokenRes = decode_payload(&msg)?;
            let account = res
                .ctid_trader_account
                .iter()
                .find(|a| !a.is_live.unwrap_or(false))
                .ok_or(ConnectionError::NoAccounts)?;
            account.ctid_trader_account_id as i64
        }
    };

    // 3. AccountAuth.
    let account_auth = ProtoOaAccountAuthReq {
        ctid_trader_account_id: ctid,
        access_token: config.access_token.expose().clone(),
        ..Default::default()
    };
    send(&mut reader, pt::ACCOUNT_AUTH_REQ, &account_auth, "aa")?;
    read_until_by(&mut reader, pt::ACCOUNT_AUTH_RES, ceiling)?;

    // 4. Trader (account info) — the account-level `money_digits`, the exponent balance
    //    integers are scaled by; also the FALLBACK exponent for a deal's commission when that
    //    deal omits its own `ProtoOADeal.moneyDigits` (tag 17 — present per-deal in practice; its
    //    wire doc says "Affects commission." directly, so `feed::decode::deal_fill` prefers it and
    //    only falls back to this account-level value). Live-verified 2026-07-14: a demo account's
    //    `money_digits` is 2 (commission=-3 raw -> -0.03 after the vike-model sign flip), but this
    //    is fetched per-account rather than hardcoded — a different broker/account could scale
    //    differently.
    let trader_req = ProtoOaTraderReq { ctid_trader_account_id: ctid, ..Default::default() };
    send(&mut reader, pt::TRADER_REQ, &trader_req, "tr")?;
    let msg = read_until_by(&mut reader, pt::TRADER_RES, ceiling)?;
    let trader_res: ProtoOaTraderRes = decode_payload(&msg)?;
    // NO default for `trader.money_digits`: see this function's doc ("moneyDigits ... NEVER 0").
    // The deposit asset id rides the same response (the any-symbol fence reads it,
    // `SymbolMap::deposit_currency`), and so does the account type (`SymbolMap::account_may_hold_a_hedge`).
    Ok((reader, ctid, trader_res.trader))
}

/// [`open_and_handshake_retry`] that SELF-HEALS an expired grant: on an auth/token refusal it
/// spends the configured refresh token once, adopts + PERSISTS the new pair, and retries the
/// handshake a single time.
///
/// # Why the initial connect needed this, and not just the actor loop
///
/// The actor's own refreshes (`try_refresh_and_reauth`, [`reconnect_with_backoff`]) only ever run on an
/// actor that has started. A grant that had already lapsed
/// before startup therefore never reached them: `open_and_handshake_retry` classifies an auth
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
/// [`crate::transport::token_store::REAUTHORIZE_COMMAND`] verbatim. The rehearsal's daemon printed the venue's
/// `CH_ACCESS_TOKEN_INVALID` and stopped there, which says what broke and not what to do.
fn open_and_handshake_self_healing(
    config: &mut ConnectionConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnectionError> {
    let first = match open_and_handshake_retry(config, fixed_ctid) {
        Ok(ok) => return Ok(ok),
        Err(e) => e,
    };
    if !first.is_auth_failure() {
        return Err(first);
    }

    if config.refresh_token.is_none() {
        tracing::error!(
            target: "ctrader",
            error = %first,
            "the access token was refused and NO refresh token is configured — re-issue the grant              with `{}` (the daemon cannot self-heal this)",
            crate::transport::token_store::REAUTHORIZE_COMMAND
        );
        return Err(first);
    }

    tracing::warn!(
        target: "ctrader",
        error = %first,
        "the access token was refused at connect; spending the refresh token once before giving up"
    );
    // ⚠ The error is NEVER interpolated: `oauth`'s token endpoint embeds the client_secret and the
    // refresh token as query params, and a transport error's `Display` can echo that URL back
    // (`renew_grant` drops it for exactly that reason).
    //
    // Adopts the pair into `config` AND writes it back to the credential store, so the very next
    // start does not repeat this round trip — or, when a sibling socket of this process rotated the
    // grant first, adopts ITS pair and spends nothing (the refresh token is never spent twice).
    match renew_grant(config) {
        Ok(Renewal::Refreshed) => {
            tracing::info!(target: "ctrader", "grant refreshed at connect; retrying the handshake once")
        }
        Ok(Renewal::Adopted) => tracing::info!(
            target: "ctrader",
            "another socket of this process already rotated the grant; retrying the handshake \
             once with its pair"
        ),
        Err(RenewError::Refused | RenewError::NoRefreshToken) => {
            tracing::error!(
                target: "ctrader",
                "the refresh token was ALSO refused (no creds/token in this message) — the grant is                  dead; re-issue it with `{}`",
                crate::transport::token_store::REAUTHORIZE_COMMAND
            );
            return Err(first);
        }
    }
    open_and_handshake_retry(config, fixed_ctid)
}

/// Run [`open_and_handshake`] under the bounded exponential-backoff [`ConnectionConfig::connect_retry`]
/// policy — the hardening that keeps a transient network/TLS blip during startup from permanently
/// dropping the venue to paper.
///
/// Retries ONLY a TRANSIENT fault ([`ConnectionError::is_transient`] — a failed TCP/TLS connect, a read
/// timeout, a connection reset, a peer that closed mid-handshake, or no response), sleeping the
/// doubling, capped backoff between attempts. A PERMANENT failure (auth REJECTED, no accounts,
/// decode/tls-config) returns IMMEDIATELY — retrying a bad credential only delays the failed
/// start. On exhausting `max_attempts` the LAST error is returned UNCHANGED, so the ultimate
/// failure contract (terminal error → the account does NOT RUN) is preserved verbatim; the retry only gives a
/// transient blip more chances first.
///
/// INERT on success: the first attempt succeeding neither sleeps nor re-sends a frame, so a healthy
/// startup is byte-identical to calling [`open_and_handshake`] directly. Deliberately SEPARATE from
/// [`reconnect_with_backoff`] (the unbounded mid-session reconnect for an already-authed socket that
/// drops) — this bounds the FIRST connect so a dead venue eventually yields to paper rather than
/// looping forever. `fetch_symbols` (catalog enumeration) and [`crate::reconcile`] (lazy
/// revive-on-next-fetch) keep their own single-shot paths and do NOT route through here.
pub(super) fn open_and_handshake_retry(
    config: &ConnectionConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnectionError> {
    let policy = config.connect_retry;
    let max_attempts = policy.max_attempts.max(1);
    let mut backoff = policy.initial_backoff;
    let mut attempt = 1u32;
    loop {
        match open_and_handshake(config, fixed_ctid) {
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
    config: ConnectionConfig,
    sink: Arc<dyn DataClientSink>,
    events: Option<EventSender>,
) -> Result<ActorHandle, ConnectionError> {
    let mut config = config;
    // Before the FIRST handshake, take the process's current grant: a config built from the boot
    // snapshot of the account (the data feed's is) may be one rotation behind a sibling socket
    // that healed a lapsed grant moments ago — presenting the pair that sibling already spent is
    // exactly the failure `grant::SharedGrant` exists to end.
    config.sync_grant();
    let (mut reader, ctid, symbols, money_digits) =
        open_and_handshake_self_healing(&mut config, None)?;
    let symbols = Arc::new(symbols);

    // Spawn the read/heartbeat/command loop. The actor gets its OWN clone of `config` (it needs
    // `client_id`/`client_secret`/`access_token`/`refresh_token`/`host`/`port` to reconnect and,
    // separately, to refresh an invalidated token — Task 6).
    let (tx, rx) = mpsc::channel::<ConnectionCommand>();
    let actor_symbols = symbols.clone();
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let actor_orders = orders.clone();
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let actor_positions = positions.clone();
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let actor_closes = closes.clone();

    // Seed the OPEN-position book BEFORE the actor thread starts — see `seed_positions_at_connect`
    // for why a fresh start used to start (and stay) blind. EXEC STARTS ONLY: a data-only
    // connection has no reduce routing and no halt boundary to feed, so it keeps its byte-identical
    // handshake with no extra round trip.
    if events.is_some() {
        let answer = seed_positions_at_connect(
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
        // The resting exits whose position closed while no bridge was watching: said once, loudly, never
        // cancelled (`flag_orphaned_exits`). Only against an answer the seed could read.
        if let Some(answer) = answer {
            flag_orphaned_exits(
                &mut reader,
                ctid,
                symbols.as_ref(),
                money_digits,
                sink.as_ref(),
                events.as_ref(),
                &orders,
                &positions,
                &closes,
                &answer,
            );
        }
    }

    // Only a FIRST handshake may be exempt from the FX week's hold: every later one of this socket is held.
    let actor_config = ConnectionConfig { fx_week_holds_login: true, ..config.clone() };
    let join = thread::Builder::new()
        .name("ctrader-actor".into())
        .spawn(move || {
            actor_loop(
                reader,
                rx,
                actor_config,
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
        .map_err(ConnectionError::Io)?;

    Ok(ActorHandle { tx, symbols, ctid, orders, positions, closes, join: Some(join) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blackholed first address of a dual-stack host cannot burn the whole ask: each attempt gets an equal
    /// share of what is left, capped at [`CONNECT_TIMEOUT`], and the last address gets all that remains.
    #[test]
    fn a_connect_attempt_gets_an_equal_share_of_what_is_left() {
        let secs = Duration::from_secs;
        assert_eq!(
            attempt_share(secs(5), 2),
            Duration::from_millis(2500),
            "the account-currency ask"
        );
        assert_eq!(
            attempt_share(secs(5), 1),
            CONNECT_TIMEOUT,
            "the last address: the cap, not 5 s"
        );
        assert_eq!(attempt_share(secs(12), 3), CONNECT_TIMEOUT, "a roomy ask: the cap");
        assert_eq!(attempt_share(secs(1), 2), Duration::from_millis(500));
        assert_eq!(attempt_share(Duration::ZERO, 2), Duration::ZERO);
        assert_eq!(attempt_share(secs(5), 0), CONNECT_TIMEOUT, "no addresses left reads as one");
    }

    /// An absent `moneyDigits` is a refusal in words and never a scale of 0; a stated one (0 included: a
    /// broker whose money has no decimals states it) passes through unchanged, and the refusal is permanent.
    #[test]
    fn an_absent_money_digits_is_refused_not_defaulted_to_zero() {
        assert_eq!(require_money_digits(Some(2)).ok(), Some(2));
        assert_eq!(require_money_digits(Some(0)).ok(), Some(0), "a stated zero is a scale");
        let why = require_money_digits(None).expect_err("no default");
        let text = why.to_string();
        assert!(text.contains("states no moneyDigits") && text.contains("scale of 0"), "{text}");
        assert!(
            !why.is_transient() && !why.is_auth_failure(),
            "the connect retry must not spin on it"
        );
    }
}
