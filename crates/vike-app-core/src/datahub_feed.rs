//! `DatahubFeed` — one `vike_data::DataClient` per venue slug, over a shared
//! [`MdSession`](crate::md_session::MdSession) (design §9 item 2).
//!
//! This is what puts the datahub's market-data wire behind the seam
//! [`feed_lifecycle`](crate::feed_lifecycle) already drives: `ensure_depth`, `ensure_poly_book`,
//! `ensure_trade_feed_on` and `reap_orphaned_dom_cockpit_streams` call it exactly as they called a
//! venue's own `Feeds`, and nothing on the receiving end changed.
//!
//! # NOTHING BLOCKS
//!
//! Every method here is a desired-set mutation plus a `Condvar` poke: no dial, no `write_frame`, no
//! `read_frame`, no join, no allocation of consequence. It is called from the eframe FRAME THREAD,
//! where a blocking call is a frozen GUI. All I/O lives on `MdSession`'s two background threads.
//!
//! # ⚠ `subscribe_bars` and `subscribe_quotes` are FLAT, UNCONDITIONAL refusals
//!
//! **They must not be routed through `vike_data::require_live_verb`, and the reason is measurable
//! rather than stylistic.** `vike_model::caps_for("binance").live_data.bars` is `true` — so are
//! bybit's, okx's, aster's and hyperliquid's — and hyperliquid and polymarket both declare
//! `quotes: true`. Driving those two verbs through the venue matrix would therefore answer `Ok` and
//! open a wire request for a lane [`MdLane`] has no variant for. The refusal is a property of THIS
//! TRANSPORT, not of the venue:
//!
//! * **no bar lane** (design §10) — lighting `DirectBarStore` from this wire would reopen
//!   [`split_plane::series_render_source`](crate::split_plane::series_render_source)'s decision that
//!   every kline series paints from exactly ONE source, and how a wire bar lane is arbitrated
//!   against `WireSnapshot::bars` per series is an unresolved design question. A chart's HISTORY is
//!   unaffected: it comes from `Request::LoadBars`/`Request::Backfill` today, unchanged.
//! * **no quotes lane** (design §4.4) — `GuiFeedSink::quote` is a spelled no-op whose only consumer
//!   is the core's `PriceBoard`, which does not exist in the desktop, so the lane would deliver into
//!   nothing.
//!
//! `require_live_verb` IS the right authority for the three lanes this wire does serve, and
//! [`MdSession::want`](crate::md_session::MdSession) calls it first — `subscribe_book` on binance
//! (`book: false`) is refused there with no wire traffic, `FeedRetries::note_error` records
//! `RetryState::Refused`, and it is never retried. Which is correct: the five CEX venues serve
//! `depth`+`trades` and refuse `book`, polymarket serves `book`+`trades` and refuses `depth` — a
//! strict partition that matches `ensure_depth` (Depth) and `ensure_poly_book` (Book + Trades)
//! exactly.

use std::collections::HashMap;
use std::sync::Arc;

use vike_data::{DataClient, LiveDataError, SubscriptionId};
use vike_datahub_client::{MdLane, MdSpec};

use crate::md_session::MdSession;

/// One venue's face onto the shared session.
pub struct DatahubFeed {
    /// A `vike_model::VENUES` slug, `&'static str` because `FeedMap`'s key is.
    venue: &'static str,
    session: Arc<MdSession>,
    /// The ids this feed minted, so `unsubscribe(id)` knows which key to release. A plain
    /// `HashMap` and not a lock: the frame thread holds `&mut` through the `FeedMap`.
    ids: HashMap<SubscriptionId, MdSpec>,
}

impl DatahubFeed {
    pub(crate) fn new(venue: &'static str, session: Arc<MdSession>) -> DatahubFeed {
        DatahubFeed { venue, session, ids: HashMap::new() }
    }

    /// ⚠ The symbol is forwarded VERBATIM. `MdSpec::symbol` is the venue's OWN spelling, exactly as
    /// `DataClient` takes it, and [`venue_routing::venue_inst`](crate::venue_routing) has already
    /// produced it at the call site (`okx` → `BTC-USDT-SWAP`, `hyperliquid` → `BTC`, polymarket → a
    /// token id). Re-mapping here would be a bug; symbol MAPPING is vike-catalog's concern.
    fn want(&mut self, symbol: &str, lane: MdLane) -> Result<SubscriptionId, LiveDataError> {
        let id = self.session.want(self.venue, symbol, lane)?;
        self.ids.insert(
            id,
            MdSpec {
                venue: self.venue.to_string(),
                symbol: symbol.to_string(),
                lane,
                depth_levels: None,
            },
        );
        Ok(id)
    }
}

impl DataClient for DatahubFeed {
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.want(symbol, MdLane::Depth)
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.want(symbol, MdLane::Book)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.want(symbol, MdLane::Trades)
    }

    /// DECLARED, not accidental — see this module's doc for why it is not `require_live_verb`.
    fn subscribe_bars(
        &mut self,
        _symbol: &str,
        _interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported(crate::md_session::NO_BAR_LANE))
    }

    /// DECLARED, not accidental — see this module's doc.
    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported(crate::md_session::NO_QUOTE_LANE))
    }

    /// Release the intent. NO I/O: the reconciler notices on its next pass and folds the removal
    /// into one `MdUpdate` alongside whatever else changed in the same frame.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some(spec) = self.ids.remove(&id) {
            self.session.unwant(&spec);
        }
    }

    /// Phase ONE of the two-phase teardown: raise the flags, shut the socket, return.
    ///
    /// ⚠ Implemented even though `App::run_bounded_teardown` currently calls only `shutdown`: the
    /// trait's own doc records that a wrapper delegating `shutdown` and forgetting this silently
    /// pays the wind-down per venue, and a future sequential caller would. Here it is also what
    /// returns the reader from a 45 s blocking read inside a 1500 ms teardown budget.
    fn begin_shutdown(&mut self) {
        self.session.begin_stop();
    }

    /// ⚠ Called ONCE PER VENUE, in PARALLEL, on ONE shared session —
    /// `App::run_bounded_teardown` fans every `App::feeds` entry out at once. Both halves are
    /// idempotent and thread-safe, and exactly one caller joins each thread.
    fn shutdown(&mut self) {
        self.session.stop_and_join();
    }
}
