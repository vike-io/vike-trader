//! `DataClientDatahub` — one `vike_data::DataClient` per venue slug, over a shared
//! [`MarketDataSession`](crate::data::market_data_session::MarketDataSession) (design §9 item 2).
//!
//! This is what puts the datahub's market-data wire behind the seam
//! [`feed_lifecycle`](crate::ui::feed_lifecycle) already drives: `ensure_depth`, `ensure_polymarket_book`,
//! `ensure_trade_feed_on` and `reap_orphaned_trade_cockpit_streams` call it exactly as they called a
//! venue's own `Feeds`, and nothing on the receiving end changed.
//!
//! # NOTHING BLOCKS
//!
//! Every method here is a desired-set mutation plus a `Condvar` poke: no dial, no `write_frame`, no
//! `read_frame`, no join, no allocation of consequence. It is called from the eframe FRAME THREAD,
//! where a blocking call is a frozen GUI. All I/O lives on `MarketDataSession`'s two background threads.
//!
//! # ⚠ `subscribe_bars` is a FLAT, UNCONDITIONAL refusal
//!
//! **It must not be routed through `vike_data::require_live_verb`, and the reason is measurable
//! rather than stylistic.** `vike_venues::capabilities_for("binance").live_data.bars` is `true` — so are
//! bybit's, okx's, aster's and hyperliquid's. Driving the verb through the venue matrix would
//! therefore answer `Ok` and open a wire request for a bar lane, which [`StreamLabel`] has no variant for. The refusal
//! is a property of THIS TRANSPORT, not of the venue: there is **no bar lane** (design §10) — lighting
//! `DirectBarStore` from this wire would reopen
//! [`split_plane::series_render_source`](crate::backend::split_plane::series_render_source)'s decision that
//! every kline series paints from exactly ONE source, and how a wire bar lane is arbitrated
//! against `CoreSnapshotWire::bars` per series is an unresolved design question. A chart's HISTORY is
//! unaffected: it comes from `Request::LoadBars`/`Request::Backfill` today, unchanged.
//!
//! `require_live_verb` IS the right authority for the book and tape lanes this wire serves, and
//! [`MarketDataSession::want`](crate::data::market_data_session::MarketDataSession) calls it first — `subscribe_book` on binance
//! (`book: false`) is refused there with no wire traffic, `FeedRetries::note_error` records
//! `RetryState::Refused`, and it is never retried. Which is correct: the five CEX venues serve
//! `depth`+`trades` and refuse `book`, polymarket serves `book`+`trades` and refuses `depth` — a
//! strict partition that matches `ensure_depth` (Depth) and `ensure_polymarket_book` (Book + Trades)
//! exactly.
//!
//! The QUOTE lane is the server's word and not the matrix's: `want` asks the handshake's
//! `market_data_lane_quotes` and `market_data_venue=` instead, because FXCM's and Dukascopy's rows declare no live data
//! although the gatewayhub streams each a price (`crates/vike-datahub/src/market_data/hub/subscribe.rs`'s
//! `quote_lane_of_a_held_stream` is the server's half).

use std::collections::HashMap;
use std::sync::Arc;

use vike_data::{DataClient, DataClientError, DataClientSubscriptionId};
use vike_datahub_client::market::MarketDataSpec;

use crate::data::market_data_session::MarketDataSession;
use vike_model::StreamLabel;

/// One venue's face onto the shared session.
pub struct DataClientDatahub {
    /// A `vike_model::VENUES` slug, `&'static str` because `FeedMap`'s key is.
    venue: &'static str,
    session: Arc<MarketDataSession>,
    /// The ids this feed minted, so `unsubscribe(id)` knows which key to release. A plain
    /// `HashMap` and not a lock: the frame thread holds `&mut` through the `FeedMap`.
    ids: HashMap<DataClientSubscriptionId, MarketDataSpec>,
}

impl DataClientDatahub {
    pub(crate) fn new(venue: &'static str, session: Arc<MarketDataSession>) -> DataClientDatahub {
        DataClientDatahub { venue, session, ids: HashMap::new() }
    }

    /// ⚠ The symbol is forwarded VERBATIM. `MarketDataSpec::symbol` is the venue's OWN spelling, exactly as
    /// `DataClient` takes it, and the caller already holds it: a Trade window's address names the
    /// venue's own symbol as the catalog lists it (`okx` → `BTC-USDT-SWAP`, `hyperliquid` → `BTC`),
    /// and the Polymarket cockpit a token id. Re-mapping here would be a bug; symbol MAPPING is
    /// vike-catalog's concern.
    fn want(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        let id = self.session.want(self.venue, symbol, lane)?;
        self.ids.insert(
            id,
            MarketDataSpec {
                venue: self.venue.to_string(),
                symbol: symbol.to_string(),
                lane,
                depth_levels: None,
            },
        );
        Ok(id)
    }
}

impl DataClient for DataClientDatahub {
    fn subscribe_depth(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.want(symbol, StreamLabel::Depth)
    }

    fn subscribe_book(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.want(symbol, StreamLabel::Book)
    }

    fn subscribe_trades(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.want(symbol, StreamLabel::Trades)
    }

    /// DECLARED, not accidental — see this module's doc for why it is not `require_live_verb`.
    fn subscribe_bars(
        &mut self,
        _symbol: &str,
        _interval: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        Err(DataClientError::Unsupported(crate::data::market_data_session::NO_BAR_LANE))
    }

    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.want(symbol, StreamLabel::Quotes)
    }

    /// Release the intent. NO I/O: the reconciler notices on its next pass and folds the removal
    /// into one `MarketDataUpdate` alongside whatever else changed in the same frame.
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
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
