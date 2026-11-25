//! The one-shot QUOTE session behind the multi-currency rate (decision 0183, phase 1): connect, run the
//! handshake ([`open_and_read_lists_by`]: the account's deposit currency and symbol list come off it), subscribe
//! to the conversion pair's spots, read ONE two-sided, timestamped quote (for a cross through USD: each of its two
//! pairs in turn, on the same session, under the same ceiling), drop the socket. Nothing is spawned and
//! nothing is kept (closing the socket ends the subscription), so it leaves nothing running behind it, as
//! [`super::fetch_symbol_lists`] does not.
//!
//! The choice of the pair, the orientation and the quote checks are `crate::account`'s (pure, with their
//! goldens); this file is the wire. Subscribing at runtime to any symbol the account lists is what the Open
//! API allows (`ProtoOASubscribeSpotsReq`, <https://help.ctrader.com/open-api/messages/>), and the first
//! `ProtoOASpotEvent` after it carries the latest price.
//!
//! # One deadline for the whole ask
//!
//! A gatewayhub lookup thread waits on an ask, and the hub runs four of them at a time for every venue, so an
//! ask that hangs starves the others. Every ask here therefore runs under ONE ceiling ([`RATE_ASK_DEADLINE`],
//! [`ACCOUNT_CURRENCY_DEADLINE`]) across the connect (each address `CONNECT_TIMEOUT`, four seconds, at most)
//! and every handshake step's wait, where the steps alone could take ten times the 15 s a step is allowed. The
//! ceiling is checked between reads and a read blocks for the socket's 1 s timeout at most, so an ask overshoots
//! by one second at worst. The retry policy (`ConnectRetry`) is NOT used: a failed ask is the caller's to
//! remember, not to repeat here.
//!
//! ⚠ A full authenticated session per ask (about ten request/response pairs, not one): the gatewayhub keeps
//! the answer for a short time and the node's rate timer asks about once a minute per held foreign-quoted
//! symbol. shortcut: a kept data session (`DataClientBusSpecCtrader` already subscribes any listed symbol) would make an ask
//! one round trip; upgrade when the per-minute session cost is measured to matter.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use vike_model::{
    AccountPerQuote, ConversionLeg, ConversionRoute, QuoteRate, currency_code, now_ms,
};

use super::{
    ConnectionConfig, ConnectionError, Reader, decode_error, decode_payload,
    open_and_read_deposit_currency_by, open_and_read_lists_by, send,
};
use crate::account::{
    Conversion, MarketState, QuoteSides, conversion_route, market_state, par, rate_of_quote,
    schedule_zone, spot_age_refusal, trading_refusal,
};
use crate::instrument::symbols::{SymbolLists, SymbolMap};
use crate::refusals::CtraderRateRefusal;
use crate::transport::proto::{ProtoOaSpotEvent, ProtoOaSubscribeSpotsReq, pt};

/// How long the session waits for a two-sided, timestamped quote after subscribing. The venue says the latest
/// price follows the subscription "shortly" (its own word); the socket's 1 s read timeout quantizes the wait.
/// Never more than what is left of [`RATE_ASK_DEADLINE`].
pub const SPOT_WAIT: Duration = Duration::from_secs(5);

/// **The whole of one rate ask**, connect to quote: a lookup thread of the gatewayhub is never held longer
/// (plus one read of up to a second). A judgement call, UNMEASURED: the handshake against the real demo host
/// is about ten round trips, a few hundred milliseconds when healthy, so this is a bound on a sick host and
/// not a budget for a slow one.
pub const RATE_ASK_DEADLINE: Duration = Duration::from_secs(12);

/// **The whole of one account-currency ask** (`fetch_account_currency`). It runs inside an adapter start, with
/// the session's adapter lock held, so it is the tighter of the two: a start that waits on it waits this long
/// and no longer, and the currency is then unknown (no conversion), as for any failure.
pub const ACCOUNT_CURRENCY_DEADLINE: Duration = Duration::from_secs(5);

/// The rate of `quote_ccy` in the account's deposit currency, asked of the venue NOW and under
/// [`RATE_ASK_DEADLINE`] as a whole, and beside it that deposit currency AS THE SESSION THAT PRICED IT READ IT
/// (upper-case): the currency `account_per_quote` is in. The caller holds a listing of its own (kept minutes) with
/// a deposit currency and refuses a rate whose currency is another one: credentials swapped under one label would
/// otherwise book a rate for one account on another's ledger. The rate's `as_of_ms` is when it was ASKED: the
/// moment the subscription was SENT, not when the quote came back, so the rate is no older than it says (the
/// conservative side; OANDA's rate is stamped alike); a cross through USD is the product of its two pairs' rates,
/// stamped when the FIRST pair's subscription was sent.
pub fn fetch_conversion_quote(
    config: ConnectionConfig,
    quote_ccy: &str,
) -> Result<(QuoteRate, String), CtraderRateRefusal> {
    fetch_conversion_quote_within(config, quote_ccy, RATE_ASK_DEADLINE)
}

/// [`fetch_conversion_quote`] under `deadline` for the WHOLE ask (connect, handshake, subscription and quote; the
/// quote itself is waited for at most `SPOT_WAIT`, five seconds, of what is left). `Ok` is always a rate that passed
/// `crate::account`'s checks, stamped with the moment the subscription was sent, with the account currency this
/// session read beside it.
pub fn fetch_conversion_quote_within(
    config: ConnectionConfig,
    quote_ccy: &str,
    deadline: Duration,
) -> Result<(QuoteRate, String), CtraderRateRefusal> {
    fetch_conversion_quote_timed(config, quote_ccy, deadline, SPOT_WAIT)
}

/// [`fetch_conversion_quote_within`] with the wait for the quote itself (`spot_wait`, at most what is left of
/// `deadline`) given too, where the production ask uses [`SPOT_WAIT`]. The two are separate knobs because they
/// bound different things: `deadline` is the thread's ceiling for the whole ask, `spot_wait` how long a venue
/// that answered the handshake is given to quote. A test of the quote's refusals (one side only, no stamp)
/// gives the ask a generous `deadline` and a short `spot_wait`, so a loaded machine slowing the handshake
/// cannot turn the asserted refusal into a timeout.
pub fn fetch_conversion_quote_timed(
    config: ConnectionConfig,
    quote_ccy: &str,
    deadline: Duration,
    spot_wait: Duration,
) -> Result<(QuoteRate, String), CtraderRateRefusal> {
    fetch_conversion_quote_assuming(config, quote_ccy, deadline, spot_wait, None)
}

/// [`fetch_conversion_quote_timed`] for an account whose deposit currency the broker's asset list may NOT name
/// (owner ruling 2026-10-19: "the account currency is what the venue API states; if the API does not state it, or
/// the read fails: USD"): `assumed_deposit` is the currency to convert into when the list names none (the
/// gatewayhub passes `USD`), and `None` keeps the refusal. A deposit the list DOES name is used as stated,
/// whatever `assumed_deposit` says: only silence is filled in. The currency beside the rate is the one actually
/// used, so the caller's check against its own listing's currency still holds.
pub fn fetch_conversion_quote_assuming(
    config: ConnectionConfig,
    quote_ccy: &str,
    deadline: Duration,
    spot_wait: Duration,
    assumed_deposit: Option<&str>,
) -> Result<(QuoteRate, String), CtraderRateRefusal> {
    let ceiling = Instant::now() + deadline;
    // A one-shot fetch never refreshes the grant, it only follows the process's current pair.
    let mut config = config;
    config.sync_grant();
    let (mut reader, ctid, lists, _money_digits) =
        open_and_read_lists_by(&config, None, Some(ceiling))
            .map_err(|e| CtraderRateRefusal::SymbolsUnread { why: e.to_string() })?;
    let map = SymbolMap::from_lists(&lists);
    let Some(account) = map.deposit_currency().or(assumed_deposit) else {
        return Err(CtraderRateRefusal::DepositUnnamed);
    };
    let (Some(quote), Some(account)) = (currency_code(quote_ccy), currency_code(account)) else {
        return Err(CtraderRateRefusal::EmptyCurrency);
    };
    if quote == account {
        return par(&account, now_ms()).ok_or(CtraderRateRefusal::EmptyCurrency);
    }
    let route = conversion_route(&map, &account, &quote)?;
    let mut session = LegSession { reader: &mut reader, ctid, lists: &lists, ceiling, spot_wait };
    let (account_per_quote, asked_ms) = match &route {
        ConversionRoute::Direct(leg) => session.leg_rate(leg)?,
        ConversionRoute::ThroughHub([first, second]) => {
            // Stamped at the FIRST leg's ask, the older of the two: the product is no older than it says.
            let named = |leg: &ConversionLeg<Conversion>, refusal| CtraderRateRefusal::ThroughHub {
                quote: quote.as_str().into(),
                account: account.as_str().into(),
                from: leg.from.as_str().into(),
                to: leg.to.as_str().into(),
                pair: leg.pair.name.as_str().into(),
                leg: Box::new(refusal),
            };
            let (r1, asked_ms) = session.leg_rate(first).map_err(|e| named(first, e))?;
            let (r2, _) = session.leg_rate(second).map_err(|e| named(second, e))?;
            let rate = AccountPerQuote::new(r1.get() * r2.get()).ok_or_else(|| {
                CtraderRateRefusal::ComposedNotFinite {
                    quote: quote.clone(),
                    account: account.clone(),
                }
            })?;
            (rate, asked_ms)
        }
    };
    let rate = QuoteRate::new(&quote, account_per_quote, asked_ms)
        .ok_or(CtraderRateRefusal::EmptyCurrency)?;
    Ok((rate, account))
}

/// The one-shot session a rate ask reads its legs on, one after the other: the socket, the account, the symbol
/// lists the handshake read, and the ask's ceiling and wait for a quote.
struct LegSession<'a> {
    reader: &'a mut Reader,
    ctid: i64,
    lists: &'a SymbolLists,
    ceiling: Instant,
    spot_wait: Duration,
}

impl LegSession<'_> {
    /// The rate of ONE leg's pair, from a two-sided, timestamped quote of it read after a subscription on this
    /// session, with the moment the subscription was sent: the pair's full record (`SymbolById`) refuses a
    /// suspended pair and dates the market, and the quote's wait is `spot_wait`, at most what is left of the
    /// ceiling.
    fn leg_rate(
        &mut self,
        leg: &ConversionLeg<Conversion>,
    ) -> Result<(AccountPerQuote, i64), CtraderRateRefusal> {
        let conversion = &leg.pair;
        // The pair's own full record (`SymbolById`): its trading mode and its schedule.
        let record = self.lists.full.iter().find(|s| s.symbol_id == conversion.symbol_id);
        if let Some(suspended) = record.and_then(|s| trading_refusal(s, &conversion.name)) {
            return Err(suspended);
        }
        // Stamped BEFORE the subscription is sent: the quote is no older than this says.
        let asked_ms = now_ms();
        let market = record.map_or(MarketState::Unknown, |s| market_state(s, asked_ms));
        let req = ProtoOaSubscribeSpotsReq {
            ctid_trader_account_id: self.ctid,
            symbol_id: vec![conversion.symbol_id],
            // Without this the venue need not state a timestamp at all ("If TRUE you will also receive the
            // timestamp in ProtoOASpotEvent"), and an untimed quote is refused.
            subscribe_to_spot_timestamp: Some(true),
            ..Default::default()
        };
        let name = || conversion.name.clone();
        send(self.reader, pt::SUBSCRIBE_SPOTS_REQ, &req, "rate")
            .map_err(|e| CtraderRateRefusal::Subscribe { name: name(), why: e.to_string() })?;
        let wait = self.spot_wait.min(self.ceiling.saturating_duration_since(Instant::now()));
        let sides = read_spots(self.reader, conversion.symbol_id, wait)
            .map_err(|e| CtraderRateRefusal::ReadSpots { name: name(), why: e.to_string() })?;
        let Some((bid, ask)) = sides.two_sided() else {
            return Err(CtraderRateRefusal::NoTwoSidedQuote { name: name(), secs: wait.as_secs() });
        };
        // Two sides and no stamp for the whole wait (the first snapshot may omit it, a later event may not, and
        // none came) is refused here, in words: its age is unknown, so it is no rate.
        let zone = record.and_then(schedule_zone);
        if let Some(old) = spot_age_refusal(sides.timestamp_ms(), asked_ms, market, zone) {
            return Err(old);
        }
        Ok((rate_of_quote(leg.orientation, bid, ask)?, asked_ms))
    }
}

/// **The account's deposit currency alone**, read by one handshake under [`ACCOUNT_CURRENCY_DEADLINE`] as a
/// whole (the grant is followed, never spent). `Ok(None)`: the session ran and the broker's asset list does not
/// name the deposit asset.
pub fn fetch_account_currency(config: ConnectionConfig) -> Result<Option<String>, ConnectionError> {
    fetch_account_currency_within(config, ACCOUNT_CURRENCY_DEADLINE)
}

/// [`fetch_account_currency`] under `deadline` for the whole ask.
pub fn fetch_account_currency_within(
    config: ConnectionConfig,
    deadline: Duration,
) -> Result<Option<String>, ConnectionError> {
    let mut config = config;
    config.sync_grant();
    let ceiling = Instant::now() + deadline;
    // The handshake stops after the asset list, the last step that names the deposit asset: the symbol list and
    // the full symbols, which the deposit currency does not need, are never asked. The socket drops on return.
    open_and_read_deposit_currency_by(&config, Some(ceiling))
}

/// Read frames for up to `wait`, folding every spot event of `symbol_id` into its quote's sides, until both
/// sides AND a timestamp are in. An `ERROR_RES` is the venue's refusal; a closed socket is an error; other frames
/// (the subscription's own response, heartbeats) are skipped. Returns whatever was seen when the wait ends (the
/// caller judges).
fn read_spots(
    reader: &mut Reader,
    symbol_id: i64,
    wait: Duration,
) -> Result<QuoteSides, ConnectionError> {
    let deadline = Instant::now() + wait;
    let mut sides = QuoteSides::default();
    while Instant::now() < deadline && sides.timed_two_sided().is_none() {
        match reader.next_frame()? {
            Some(msg) if msg.payload_type == pt::SPOT_EVENT => {
                sides.on_event(&decode_payload::<ProtoOaSpotEvent>(&msg)?, symbol_id);
            }
            Some(msg) if msg.payload_type == pt::ERROR_RES => return Err(decode_error(&msg)),
            Some(_) => {}
            None if reader.get_mut().eof.load(Ordering::SeqCst) => {
                return Err(ConnectionError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before a quote",
                )));
            }
            None => {} // a read timeout: loop to the deadline
        }
    }
    Ok(sides)
}
