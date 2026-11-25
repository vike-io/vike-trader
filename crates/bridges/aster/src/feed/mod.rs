//! Live Aster market data (spot + USDⓈ-M perp) → the vike-data live seam: [`DataClientSpecAster`],
//! aster's half of the one data-client engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`), over
//! the lanes of the shared [`vike_binance::family::feed`].
//!
//! The engine runs one STOPPABLE thread per subscription and owns the lifecycle, the status words and
//! the sink emission: REST kline backfill (warmup seed), then the public kline WS — closed bars via
//! `close_bar` (the lossless lane), intrabar forming updates + last-price ticks via
//! `forming_bar`/`bar_close_tick` (the wait-free conflating lane; a perp bars subscription MAY ALSO open
//! the `@markPrice@1s` stream feeding the REAL-mark verb `mark_tick` — but Aster ships this default OFF
//! because its mark grammar, assumed from the family charter, is UNVERIFIED against Aster's own feed:
//! opt in with a `venue.aster.mark_streams = 1` row after its market-data smoke runs) — into the
//! core-owned bar cache. `unsubscribe` stops exactly one stream, leaving every other subscription
//! running.
//!
//! Aster's kline/depth wire grammar is Binance-verbatim (same `@kline_<interval>`/`@depth@100ms`
//! stream names, 12-element klines; ⚠ but the depth SEQUENCE grammar is NOT identical, and saying
//! it was cost this venue a mis-synced depth lane on BOTH planes — aster sends `pu` on spot AND
//! futures where binance sends it on futures only, measured 2026-09-10, and
//! `vike_binance::family::feed::tick`'s `apply_depth_event` reads that off the frame rather than off a
//! venue flag), so the frame decoders and every lane's plan live once in the Binance-wire-grammar
//! core and both venues' specs call in (F17, dedup rung 2). What stays HERE is the ONE real delta:
//! host resolution. Every URL reads the spec's `urls`, the venue's `venue_url` rows resolved ONCE at
//! construction at the tier [`DataClientSpecAster::new`] names ([`crate::transport::urls::AsterUrls::data`]),
//! mapped into the family's `UrlTable` by [`crate::feed::tick::trades::spec`].
//!
//! Trades (`subscribe_trades`): a separate live `@aggTrade` feed with its own WS-buffer→REST-
//! warmup→splice startup dance — implemented in [`vike_binance::family::feed::tick::trades`] (its module doc
//! has the full contract), planned as the engine's `Own` lane.
//!
//! **Depth follows the SYMBOL's instrument class** — a `.P` subscription seeds and streams the
//! USDⓈ-M futures book, a bare one the spot book. It used to pass a hardcoded `perp = false` while
//! the shared lane built its stream name from the RAW `.P` symbol, so a perp subscription asked a
//! spot host for `btcusdt.p@depth@100ms`: a stream nothing resolves, which connects and then
//! streams nothing forever. See the family `depth_main`'s doc for the full failure shape.
//! `fetch_depth_snapshot` takes `(urls, symbol, perp)` and picks the REST host INTERNALLY rather
//! than taking a raw host string, so a caller can't pass a mismatched (host, perp) pair that
//! silently builds a nonexistent URL (review round 1 finding) — the plan forwards the lane's
//! flag instead of pinning it. `feed/tick/mod.rs`'s own HFT tick track is a SEPARATE consumer and
//! still runs its own futures book (`GET /fapi/v3/depth`, per the design spec).

pub mod tick;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, DataClientSpec};
use vike_model::AccountTier;
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

use crate::history::fetch_klines_latest;
use crate::refusals::AsterFeedRefusal;
use crate::transport::urls::{AsterUrls, VENUE};
use vike_binance::family::feed::{SEED_LIMIT, depth_main, feed_main, mark_main, trades_main};
use vike_binance::family::history::klines::VolumeColumn;

/// Aster's mark stream ships default-OFF: its `@markPrice@1s` grammar is assumed from the
/// binance-family charter but has never been observed on Aster's own venue, so pairing is opt-in
/// via a `venue.aster.mark_streams = 1` row. Until then a perp values off the resolver's bar-close
/// rung. Flip to `true` only after a market-data smoke on the prod rigs confirms the wire shape.
///
/// FAIL-SOFT when opted in, two distinct shapes: (1) a frame whose shape does not match decodes to
/// nothing → `WebsocketSessionFrameOutcome::Ignore`, i.e. SILENTLY nothing, and valuation keeps falling back to the
/// bar-close rung — harmless but invisible; (2) a REJECTED WS upgrade (a `@markPrice@1s` path this
/// venue does not serve) re-enters the shared reconnect/backoff loop INDEFINITELY, surfacing only as a
/// status string — there is no operator health signal and no give-up. Neither shape can corrupt a
/// price; both can hide a dead mark stream, so an Aster market-data smoke is the gate before relying
/// on this feed.
const MARK_STREAM_DEFAULT_ON: bool = false;

/// **The text every kline, mark and trade lane sends at its pump row's `keepalive_every`** (60 s,
/// `vike_bridge_core::websocket_session::spec`'s `row`, the `"aster"` arm): the venue's documented subscription-list
/// request, which it answers on a URL-subscribed `/ws/<stream>` socket with `{"id":1,"result":[<stream>]}`, a frame
/// every decoder here ignores. Aster's perp host cuts a socket its client has sent nothing for 240 s, and its own
/// 300 s ping comes too late, so a quiet symbol's lane was dropped every few minutes; the answer is also what the
/// row's idle watchdog hears (measured 2026-11-22: the row's comment).
pub const KEEPALIVE: &str = r#"{"method":"LIST_SUBSCRIPTIONS","id":1}"#;

/// **Aster's half of the one data-client engine**: `DataClientWebsocket::new(DataClientSpecAster::new(&urls,
/// tier)?, sink, wake)` is aster's live `vike_data::DataClient`.
pub struct DataClientSpecAster {
    /// The public market-data bases, resolved once at construction from the venue's rows at the
    /// tier [`DataClientSpecAster::new`] names. Every host a lane dials reads this.
    urls: AsterUrls,
    /// Earliest live aggTrade id ever observed per symbol: every trades lane of this client records
    /// `min(existing, its startup splice's first id)` for its own symbol right after that splice (see
    /// `family::trades::run_trades_feed`'s module doc, "Earliest-live-id reporting"). A backfill would
    /// page strictly below it. ⚠ NOTHING reads it today; the public getter it was reachable through
    /// was deleted on 2026-10-10.
    earliest_live_ids: Arc<Mutex<HashMap<String, u64>>>,
}

impl DataClientSpecAster {
    /// The spec over the venue's public market-data rows ([`AsterUrls::data`]) at `tier`, resolved
    /// once, here. The gatewayhub builds it at `Live`. ⚠ At `Demo` a `data` ask with no `demo` data
    /// row reads the `live` rows (the lookup rule), and the seed holds no aster `demo` data row, so a
    /// `Demo` spec reads the MAINNET public hosts on a fresh box.
    ///
    /// # Errors
    /// The row the table cannot answer: no feed is built.
    pub fn new(urls: &VenueUrls, tier: AccountTier) -> Result<Self, UrlRefusal> {
        Ok(DataClientSpecAster {
            urls: AsterUrls::data(urls, tier)?,
            earliest_live_ids: Arc::default(),
        })
    }

    /// The family row every lane reads: aster's hosts, lanes and keepalive. Public so the link harness runs the
    /// trade tape (`vike_binance::family::feed::tick::trades::run_trades_feed_on`) on the row its plan reads
    /// (`crates/vike-bridge-core/tests/link_conformance/market.rs`).
    #[must_use]
    pub fn family(&self) -> vike_binance::family::FamilySpec {
        crate::feed::tick::trades::spec(self.urls)
    }
}

impl DataClientSpec for DataClientSpecAster {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = "Aster";
    const MARK_STREAMS_DEFAULT_ON: bool = MARK_STREAM_DEFAULT_ON;

    /// A `.P` perp pairs its `@markPrice@1s` stream (once the venue's row opts in); spot has no mark
    /// price at all. The catalog's name rule (`split_perp_at` on a `Naming::PerpSuffix` venue is
    /// `strip_suffix(".P").is_some()`), never a bare `ends_with(".P")`.
    fn pairs_mark(&self, symbol: &str) -> bool {
        vike_catalog::split_perp_at(VENUE, symbol).1
    }

    /// A trailing `.P` marks a USDⓈ-M perp (the catalog's distinct-symbol tag, matching
    /// `ASTER:BTCUSDT.P`): it is stripped to the exchange symbol for the fapi REST seeds and WS URLs,
    /// while the engine keys every sink call on the ORIGINAL `.P`-suffixed symbol. Every aster book
    /// states its base-asset amount in the kline frame's `v` (it has no COIN-M book). Quotes and the
    /// lossless book are refused by aster's caps row before any plan is asked; a plan asked for one
    /// anyway says so.
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        let urls = self.urls;
        let family = self.family();
        let (api_symbol, is_perp) = vike_catalog::split_perp_at(VENUE, symbol);
        match lane {
            DataClientLane::Bars => Ok(feed_main(
                api_symbol,
                interval,
                is_perp,
                VolumeColumn::Index5,
                &family.urls,
                move |api, iv, perp| fetch_klines_latest(urls, api, iv, SEED_LIMIT, perp),
                None, // aster's streams document no announced close (binance spot's `serverShutdown` is not theirs)
                family.market_keepalive,
            )),
            DataClientLane::Mark => {
                Ok(mark_main(symbol, api_symbol, &family.urls, family.market_keepalive))
            }
            DataClientLane::Trades => {
                Ok(trades_main(symbol, family, Arc::clone(&self.earliest_live_ids), None))
            }
            DataClientLane::Depth => Ok(depth_main(symbol, &family, move |api, perp| {
                crate::feed::tick::fetch_depth_snapshot(urls, api, perp)
            })),
            DataClientLane::Quotes | DataClientLane::Book => {
                Err(AsterFeedRefusal::NoLane { lane: lane.word(interval).to_string() }.to_string())
            }
        }
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
