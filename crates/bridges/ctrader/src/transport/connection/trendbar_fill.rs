//! **A reconnect's trendbar FILL**: the periods an outage hid are asked of the venue and closed from its STORED bars
//! (`crates/bridges/ctrader/src/feed/bar.rs`'s module doc says why, what a fill hands on, and how an answer is
//! told a fill's from a seed's: by what its key asked, never by the envelope id).
//!
//! # A queue beside the read, never inside it
//!
//! The reconnect hands every [`Hole`] `TrendbarCloses::forget_all` named to [`TrendbarFills`] and goes back to reading.
//! The actor gives the queue one TURN between reads ([`TrendbarFills::tick`], which never blocks and never reads): it
//! asks ONE key at a time, `GET_TRENDBARS_REQ` over the seed's own window (`crate::feed::SEED_BAR_COUNT` periods back to
//! now, or back to the hole if that is older) with an EMPTY `clientMsgId` like every non-order request
//! (`super::outbound::write_command`), noted on the sink (`TrendbarCloses::asked_fill`). The next is asked only once this
//! one is answered, refused or has waited [`SEED_TIMEOUT`], and no sooner than [`FILL_EVERY`] after it, so however many
//! keys a socket carries their fills stay under the vendor's five history requests a second per connection. While any
//! is left the actor reads with a [`FILL_EVERY`] timeout at most, so a quiet socket still takes its turns.
//!
//! - **Every frame is dispatched as it is read**: an order's events, the live snapshots the replay's subscribes started,
//!   a seed for another key. Only a key's own live CLOSES wait for its fill, in the sink (`TrendbarCloses`), so the
//!   fill's older periods reach it first. The answer itself is read like any frame (`super::inbound::on_inbound`), and
//!   the sink takes it as the fill's by what was asked; an answer read after the wait is still that fill's.
//! - **A refusal is tied to its key by the same what-was-asked rule** ([`TrendbarFills::refused`]): with one fill
//!   outstanding, an id-less `ERROR_RES` read meanwhile is that fill's (an id names an order; `ALREADY_SUBSCRIBED` names a
//!   stream asked for again). The key's fill is dropped (`TrendbarCloses::refused_fill`), said at WARN as a hole, and its
//!   next answer is a seed. The actor's probe, asked once a minute whatever is outstanding, carries its own id
//!   (`super::session::PROBE_MESSAGE_ID`), so its refusal is the probe's and leaves the fill waiting. The only other
//!   id-less requests that can still be answered then are the subscribes the replay wrote: one of their refusals taken
//!   for the fill's drops a fill whose answer is then a seed, which the seed's window keeps exact.
//! - **A socket that dies** with fills left drops them: the next reconnect's `forget_all` names every hole again.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use vike_model::now_ms;

use super::stream::decode_error;
use super::{ConnectionError, Reader, SEED_TIMEOUT, send};
use crate::feed::SEED_BAR_COUNT;
use crate::feed::bar::{Hole, TrendbarCloses};
use crate::feed::decode::{trendbar_period_for_interval, trendbar_period_ms};
use crate::history::MAX_REQUESTS_PER_SECOND;
use crate::instrument::symbols::SymbolMap;
use crate::transport::proto::{ProtoMessage, ProtoOaGetTrendbarsReq, ProtoOaTrendbarPeriod, pt};

/// The least time between two fills of one connection: the candle history's pace
/// (`crate::history`' `MAX_REQUESTS_PER_SECOND`, four a second), under the vendor's five.
pub(super) const FILL_EVERY: Duration =
    Duration::from_millis(1000 / MAX_REQUESTS_PER_SECOND as u64);

/// The one fill outstanding: its key and when it was asked.
struct Asked {
    symbol: String,
    interval: String,
    at: Instant,
}

/// **A reconnect's fills, asked one at a time beside the actor's read** (module doc).
#[derive(Default)]
pub(super) struct TrendbarFills {
    queue: VecDeque<Hole>,
    asked: Option<Asked>,
    last_sent: Option<Instant>,
}

impl TrendbarFills {
    /// A reconnect's holes, in the order `forget_all` named them. A queue left from a dead socket goes: its asks died
    /// with it, and `forget_all` named its holes again.
    pub(super) fn start(&mut self, holes: Vec<Hole>) {
        self.queue = holes.into();
        self.asked = None;
    }

    /// Whether no fill is left to ask or to wait for.
    pub(super) fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.asked.is_none()
    }

    /// **The queue's turn between two reads** (module doc): end the outstanding fill once the sink took its answer or
    /// it waited [`SEED_TIMEOUT`], then ask the next key still subscribed once [`FILL_EVERY`] passed since the last ask.
    /// Never blocks and reads nothing. A write that fails empties the queue: the socket is gone, and the next read
    /// reconnects.
    pub(super) fn tick(
        &mut self,
        reader: &mut Reader,
        ctid: i64,
        symbols: &SymbolMap,
        bars: &TrendbarCloses,
        trendbar_subs: &HashSet<(i64, ProtoOaTrendbarPeriod)>,
    ) {
        if let Some(a) = &self.asked {
            if bars.awaits_fill(&a.symbol, &a.interval) {
                if a.at.elapsed() < SEED_TIMEOUT {
                    return;
                }
                tracing::warn!(
                    target: "ctrader",
                    symbol = %a.symbol,
                    interval = %a.interval,
                    "a reconnect's trendbar fill was not answered in its wait: a later answer is still that fill's, \
                     and what it holds before the stream's next close stays a hole"
                );
                bars.gave_up_fill(&a.symbol, &a.interval);
            } else {
                tracing::info!(
                    target: "ctrader",
                    symbol = %a.symbol,
                    interval = %a.interval,
                    "a reconnect's trendbar fill answered"
                );
            }
            self.asked = None;
        }
        if self.last_sent.is_some_and(|at| at.elapsed() < FILL_EVERY) {
            return;
        }
        while let Some((symbol, interval, from)) = self.queue.pop_front() {
            let (Some(symbol_id), Some(period)) =
                (symbols.id_of(&symbol), trendbar_period_for_interval(&interval))
            else {
                bars.gave_up_fill(&symbol, &interval);
                continue;
            };
            if !trendbar_subs.contains(&(symbol_id, period)) {
                bars.gave_up_fill(&symbol, &interval); // released during the outage
                continue;
            }
            let now = now_ms();
            let seed_from = now - trendbar_period_ms(period) * i64::from(SEED_BAR_COUNT);
            let req = ProtoOaGetTrendbarsReq {
                ctid_trader_account_id: ctid,
                // One millisecond below: whether the venue's bound is inclusive is NOT MEASURED (`crate::history`).
                from_timestamp: Some(seed_from.min(from - 1)),
                to_timestamp: Some(now),
                period: period as i32,
                symbol_id,
                ..Default::default()
            };
            if send(reader, pt::GET_TRENDBARS_REQ, &req, "").is_err() {
                self.queue.clear(); // the socket is gone, and the next read reconnects
                return;
            }
            self.last_sent = Some(Instant::now());
            bars.asked_fill(&symbol, &interval, from);
            self.asked = Some(Asked { symbol, interval, at: Instant::now() });
            return;
        }
    }

    /// A frame the actor read, while a fill is outstanding: an id-less refusal that does not name a stream asked for
    /// again is that fill's (module doc). It drops the fill; the frame is still dispatched as any other.
    pub(super) fn refused(&mut self, msg: &ProtoMessage, bars: &TrendbarCloses) {
        let Some(a) = &self.asked else { return };
        if msg.payload_type != pt::ERROR_RES
            || !msg.client_msg_id.as_deref().is_none_or(str::is_empty)
        {
            return;
        }
        let why = decode_error(msg);
        if matches!(&why, ConnectionError::Venue { error_code, .. } if error_code == "ALREADY_SUBSCRIBED")
        {
            return;
        }
        bars.refused_fill(&a.symbol, &a.interval);
        tracing::warn!(
            target: "ctrader",
            symbol = %a.symbol,
            interval = %a.interval,
            %why,
            "the venue refused a reconnect's trendbar fill: the periods it was asked for stay a hole"
        );
        self.asked = None;
    }
}
