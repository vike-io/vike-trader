//! One shard's membership and stream wrapper, and the per-token REST warmup that seats its tokens.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::market_pump::MarketStream;
use vike_bridge_core::transport::RestTransport;
use vike_bridge_core::user_data::StreamError;

use super::state::TokenSlot;
use crate::instruments::fetch_token_tick_size_while;

/// One shard's live token set, shared between [`Feeds`] (which mutates it on subscribe/unsubscribe)
/// and that shard's feed thread (which reads it at every session boundary). `epoch` is bumped by
/// every mutation; the thread's [`ShardStream`] wrapper compares it against the epoch its session
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
    /// `false` (no mutation) when full or duplicate. A duplicate is refused deliberately: two
    /// subscriptions of the SAME token+mode must stay two independent streams (the pre-batching
    /// behavior — one wire subscription can only be delivered once).
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

/// A [`MarketStream`] decorator that ends the session as soon as its shard's membership changes:
/// `read_frame` compares the live epoch against the one this session subscribed at and reports
/// `Closed` when they differ, which the driver treats like any other session fault — disclose,
/// back off, reconnect, and (in the connect closure) resubscribe the whole new token set. Pickup
/// latency is bounded by the row's read timeout (2 s). `send_text`/`since_last_frame` pass through.
pub(super) struct ShardStream<S: MarketStream> {
    pub(super) inner: S,
    pub(super) membership: Arc<ShardMembership>,
    pub(super) subscribed_at: u64,
}

impl<S: MarketStream> MarketStream for ShardStream<S> {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        if self.membership.epoch() != self.subscribed_at {
            return Err(StreamError::Closed("token set changed — resubscribing".into()));
        }
        self.inner.read_frame()
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
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
/// is put on the tape** ([`crate::taker_hold`]). When — and only when — `properties_rec` is armed
/// (`flags.record_properties`, off by default), a NEWLY-seated token additionally costs two REST
/// requests ([`crate::taker_hold::fetch_token_taker_hold_ms`]: one Gamma point lookup for the
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
/// or the frame router indexes the wrong book — and takes the default tick size, which nothing will
/// read: the driver's next act is a stop check.
pub(super) fn reconcile_slots<T: RestTransport>(
    slots: &mut Vec<TokenSlot>,
    tokens: &[String],
    transport: &T,
    freshness_ms: i64,
    properties_rec: Option<&Arc<vike_data::PropertiesRecorder>>,
    keep_going: &dyn Fn() -> bool,
) {
    let mut old = std::mem::take(slots);
    for token in tokens {
        match old.iter().position(|s| s.token_id == *token) {
            Some(pos) => slots.push(old.remove(pos)),
            None => {
                let tick_size = fetch_token_tick_size_while(transport, token, keep_going);
                record_token_grid(properties_rec, transport, token, tick_size, keep_going);
                slots.push(TokenSlot::new(token.clone(), tick_size, freshness_ms));
            }
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
    token_id: &str,
    tick_size: f64,
    keep_going: &dyn Fn() -> bool,
) {
    let Some(rec) = rec.filter(|r| r.enabled()) else {
        return;
    };
    let taker_hold_ms =
        crate::taker_hold::fetch_token_taker_hold_ms_while(transport, token_id, keep_going);
    tracing::debug!(token_id, tick_size, taker_hold_ms, "recording polymarket token properties");
    crate::filters_rec::record_token_properties(
        &Some(Arc::clone(rec)),
        token_id,
        tick_size,
        taker_hold_ms,
        vike_model::now_ns(),
    );
}
