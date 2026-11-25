//! One shard's membership and stream wrapper, and the per-token REST warmup that seats its tokens.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::transport::RestTransport;
use vike_bridge_core::websocket_session::market::WebsocketSessionMarketStream;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserError;

use super::state::{TokenSlot, TokenState};
use crate::instrument::properties::fetch_token_tick_size_while;

/// One shard's live token set, shared between [`super::DataClientBusSpecPolymarket`] (which mutates it on subscribe/unsubscribe)
/// and that shard's feed thread (which reads it at every session boundary). `epoch` is bumped by
/// every mutation; the thread's [`WebsocketSessionMarketStreamShard`] wrapper compares it against the epoch its session
/// subscribed at and ends the session the moment they differ, so the driver's
/// reconnect-==-resubscribe path re-dials with the WHOLE new set. Membership is therefore never
/// mutated mid-session, and a token is never half-subscribed.
pub(super) struct ShardMembership {
    tokens: Mutex<Vec<String>>,
    epoch: AtomicU64,
}

impl ShardMembership {
    pub(super) fn new(first: String) -> Self {
        ShardMembership { tokens: Mutex::new(vec![first]), epoch: AtomicU64::new(0) }
    }

    pub(super) fn tokens(&self) -> Vec<String> {
        self.tokens.lock().unwrap().clone()
    }

    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Seat `token` here when this shard has room AND does not already carry it, bumping the epoch;
    /// `false` (no mutation) when full or duplicate. A duplicate never reaches here from the source
    /// (one seat per token and mode, owner ruling B-10); refusing it keeps one shard from carrying a
    /// token twice whoever asks.
    pub(super) fn try_admit(&self, token: &str, cap: usize) -> bool {
        let mut tokens = self.tokens.lock().unwrap();
        if tokens.len() >= cap || tokens.iter().any(|t| t == token) {
            return false;
        }
        tokens.push(token.to_string());
        self.epoch.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Release ONE seat held by `token`, bumping the epoch. Returns whether the shard is now empty
    /// (its caller then stops+joins the feed thread instead of resubscribing).
    pub(super) fn release(&self, token: &str) -> bool {
        let mut tokens = self.tokens.lock().unwrap();
        if let Some(pos) = tokens.iter().position(|t| t == token) {
            tokens.remove(pos);
            self.epoch.fetch_add(1, Ordering::Relaxed);
        }
        tokens.is_empty()
    }
}

/// A [`WebsocketSessionMarketStream`] decorator that ends the session as soon as its shard's membership changes:
/// `read_frame` compares the live epoch against the one this session subscribed at and reports
/// `Closed` when they differ, which the driver treats like any other session fault — disclose,
/// back off, reconnect, and (in the connect closure) resubscribe the whole new token set. Pickup
/// latency is bounded by the row's read timeout (2 s). `send_text`/`since_last_frame` pass through.
pub(super) struct WebsocketSessionMarketStreamShard<S: WebsocketSessionMarketStream> {
    pub(super) inner: S,
    pub(super) membership: Arc<ShardMembership>,
    pub(super) subscribed_at: u64,
}

impl<S: WebsocketSessionMarketStream> WebsocketSessionMarketStream
    for WebsocketSessionMarketStreamShard<S>
{
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        if self.membership.epoch() != self.subscribed_at {
            return Err(WebsocketSessionUserError::Closed(
                "token set changed — resubscribing".into(),
            ));
        }
        self.inner.read_frame()
    }

    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        self.inner.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.inner.since_last_frame()
    }
}

/// Human-readable name for a shard's token set — the token itself for a one-seat shard (so a K = 1
/// feed's thread name and status lines read exactly as they did before batching), `"{first}+{n}"`
/// once it carries more.
pub(super) fn shard_label(tokens: &[String]) -> String {
    match tokens.split_first() {
        Some((first, [])) => first.clone(),
        Some((first, rest)) => format!("{first}+{}", rest.len()),
        None => String::new(),
    }
}

/// Bring `slots` in line with `tokens`: drop the seats no longer subscribed, create one (resolving
/// its tick size over REST — on the FEED thread, off `subscribe_*`'s path, the same discipline as
/// binance's REST warmup) for each newly seated token, and order them like the membership. Slots
/// that survive keep their book, feed-local `seq` and health untouched, so a co-tenant is not
/// disturbed by a neighbour joining or leaving.
///
/// **This is the live token-resolution site, and so the ONE place the venue's declared taker hold
/// is put on the tape** ([`crate::instrument::taker_hold`]). When — and only when — `properties_rec` is armed
/// (no root arms it: `flags.record_properties` arms only the datahub's tape), a NEWLY-seated token
/// additionally costs two REST
/// requests ([`crate::instrument::taker_hold::fetch_token_taker_hold_ms`]: one Gamma point lookup for the
/// `condition_id`, one `/clob-markets/{cid}` for the hold itself) and one best-effort
/// `kind=properties` row carrying the resolved tick size AND hold. The contract that matters:
///
/// * `properties_rec == None` ⇒ **zero** extra requests and zero extra work, byte-identical to
///   before — the recorder is the gate, so an un-armed process never touches those endpoints.
/// * ONCE per newly-seated token, never per tick and never per order. A surviving seat is not
///   re-resolved, so a reconnect or a neighbour joining costs nothing.
/// * Any failure resolves to `0` (= this venue declares no hold), never a guess.
///
/// **`keep_going` is the feed thread's stop flag, and it is what makes this bounded at all.** Every
/// REST call below is a blocking window on a LIVE FEED THREAD, spent from
/// `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS` — and the count is not
/// one: it is per NEWLY-SEATED TOKEN, each of which is up to `1 + TICK_SIZE_LOOKUP_MAX_PAGES`
/// requests, plus two more when the properties recorder is armed. A per-request ceiling alone would
/// therefore bound nothing that matters; the predicate is checked in front of EVERY request (here,
/// per token, and inside `fetch_token_tick_size_while` / `fetch_token_taker_hold_ms_while` per
/// request), so a raised flag is ignored for exactly the ONE request already in flight.
///
/// A token seated after the flag goes up keeps its slot — `slots` must stay aligned with `tokens`
/// or the frame router indexes the wrong book — and waits for its tick ([`TokenSlot::awaiting_tick`]),
/// which nothing will read: the driver's next act is a stop check. So does a token no lookup resolves:
/// its frames are refused until its own `book` snapshot states a tick, and the lookup is asked again at
/// the next reseat. Never a guessed tick (owner, 2026-11-13).
///
/// `clob` and `gamma` are the venue's CLOB and Gamma rows, the feed's own.
pub(super) fn reconcile_slots<T: RestTransport>(
    slots: &mut Vec<TokenSlot>,
    tokens: &[String],
    transport: &T,
    clob: &str,
    gamma: &str,
    freshness_ms: i64,
    properties_rec: Option<&Arc<vike_data::PropertiesRecorder>>,
    keep_going: &dyn Fn() -> bool,
) {
    // The token's tick, recorded with its hold when it resolves. No tick, no guess (owner, 2026-11-13):
    // the token is seated but refused until a lookup or its own book snapshot states one, and nothing is
    // recorded for it.
    let resolve = |token: &str| {
        let tick_size = fetch_token_tick_size_while(transport, clob, token, keep_going)?;
        record_token_grid(properties_rec, transport, clob, gamma, token, tick_size, keep_going);
        Some(tick_size)
    };
    let mut old = std::mem::take(slots);
    for token in tokens {
        match old.iter().position(|s| s.token_id == *token) {
            Some(pos) => {
                let mut slot = old.remove(pos);
                // A seat still waiting for its tick asks again at each reseat (a reconnect is one).
                if slot.tick_pending
                    && let Some(tick_size) = resolve(token)
                {
                    slot.state = TokenState::new(tick_size);
                    slot.tick_pending = false;
                }
                slots.push(slot);
            }
            None => slots.push(match resolve(token) {
                Some(tick_size) => TokenSlot::new(token.clone(), tick_size, freshness_ms),
                None => TokenSlot::awaiting_tick(token.clone(), freshness_ms),
            }),
        }
    }
}

/// The recorder half of [`reconcile_slots`], split out so the gate is one readable early return:
/// no recorder (or a recorder built with recording DISABLED) ⇒ return before any network call.
///
/// `PropertiesRecorder::record` is itself a no-op when disabled, but checking `enabled()` here is
/// load-bearing rather than defensive: the taker-hold resolution is TWO REST requests, and they
/// must not be paid by a process that would then throw the row away.
fn record_token_grid<T: RestTransport>(
    rec: Option<&Arc<vike_data::PropertiesRecorder>>,
    transport: &T,
    clob: &str,
    gamma: &str,
    token_id: &str,
    tick_size: f64,
    keep_going: &dyn Fn() -> bool,
) {
    let Some(rec) = rec.filter(|r| r.enabled()) else {
        return;
    };
    let taker_hold_ms = crate::instrument::taker_hold::fetch_token_taker_hold_ms_while(
        transport, clob, gamma, token_id, keep_going,
    );
    tracing::debug!(token_id, tick_size, taker_hold_ms, "recording polymarket token properties");
    crate::instrument::record::record_token_properties(
        &Some(Arc::clone(rec)),
        token_id,
        tick_size,
        taker_hold_ms,
        vike_model::now_ns(),
    );
}
