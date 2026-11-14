//! Live Binance market data → the vike-data live seam (R5c): [`DataClientSpecBinance`], binance's half
//! of the one data-client engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`), over the lanes of
//! the shared [`crate::family::feed`].
//!
//! The engine runs one STOPPABLE thread per subscription and owns the lifecycle, the status words and
//! the sink emission: REST kline backfill (warmup seed), then the public kline WS — closed bars via
//! `close_bar` (the lossless lane), intrabar forming updates + last-price ticks via
//! `forming_bar`/`bar_close_tick` (the wait-free conflating lane; a perp bars subscription ALSO opens
//! the `@markPrice@1s` stream feeding the REAL-mark verb `mark_tick`, default ON, the venue's
//! `mark_streams` row via `DataClientWebsocket::with_mark_streams`) — into the core-owned bar cache.
//! `unsubscribe` stops exactly one stream, leaving every other subscription running.
//!
//! Aster's kline/depth grammar is a verbatim fork, so the frame decoders and every lane's plan live
//! once in the family module and both venues' specs call in (F17, dedup rung 2). What stays HERE is
//! binance's delta: [`BinanceTables`], the family's [`UrlTable`] filled from binance's `venue_url` rows
//! at `live` `data` (a demo account's prices come from mainnet by design), never from the environment;
//! the kline seed's hosts (the backfill's own); the depth snapshot fetch; the COIN-M volume column; and
//! the perp naming that pairs a mark stream.
//!
//! Trades (`subscribe_trades`, Task B2): a separate live `@aggTrade` feed with its own
//! WS-buffer→REST-warmup→splice startup dance — implemented in [`crate::family::feed::tick::trades`] (its
//! module doc has the full contract), planned as the engine's `Own` lane.
//!
//! **COIN-M (`BTCUSD_PERP.P`, `BTCUSD_261225.P`) is the third table.** Every lane of a COIN-M name reads
//! [`BinanceTables::coin_m`] (`dstream`, `dapi`) through [`BinanceTables::urls_for`]; the lane bodies are the family's.
//! The kline lane is told the book's volume column ([`kline_volume`]): a COIN-M kline frame states the
//! base-asset amount in `q`, where every other book's states it in `v`. Quote, trade and depth sizes stay
//! the venue's quantity, CONTRACTS, the unit a COIN-M order is in; only a bar's volume is the base asset,
//! as the history path books it.

pub mod tick;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, DataClientSpec};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::VENUE;
use crate::family::feed::{SEED_LIMIT, depth_main, feed_main, mark_main, trades_main};
use crate::family::history::klines::VolumeColumn;
use crate::family::{FamilySpec, PerpTradesLane, UrlTable, static_url};
use crate::history::{KlineSourceBinance, fetch_klines_latest};

/// Binance's public market-data PATHS and lanes — every [`UrlTable`] field that is not a host. The hosts
/// are rows ([`BinanceTables::from_urls`] fills them); the URLs the family builds from the release's rows
/// are byte-identical to the inline `format!`s the table once replaced.
const PATHS: UrlTable = UrlTable {
    spot_websocket: "",
    perp_websocket: "",
    // Binance routes the futures socket by stream class (its March 2026 upgrade, changelog entries
    // 2026-03-05 and 2026-04-02; decision 0251): the legacy path carries the PUBLIC class only, so
    // kline, markPrice and aggTrade need `/market`.
    perp_market_path: "/market",
    perp_public_path: "/public",
    spot_rest: "",
    perp_rest: "",
    spot_agg_trades_path: "/api/v3/aggTrades",
    // Binance serves perp aggTrades at v1 (Aster at v3) — a real venue divergence, carried as data.
    perp_agg_trades_path: "/fapi/v1/aggTrades",
    // Binance's futures `@aggTrade` WS stream is DEAD — it accepts the socket and pushes nothing
    // (measured 0 frames/60s on BTCUSDT AND ETHUSDT, while `@trade` pushed 770). Aster's works, so
    // this is per-venue. See `PerpTradesLane`.
    perp_trades_lane: crate::family::PerpTradesLane::Raw,
    perp_raw_trades_path: "/fapi/v1/trades",
};

/// **Binance's public market-data hosts**: binance's `venue_url` rows at `live` `data` (a feed has no
/// account, so no tier; a demo account's prices come from mainnet by design), joined to [`PATHS`]. The
/// whole per-venue delta the shared market-data stack takes as a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinanceTables {
    /// The spot and USDⓈ-M book: `stream.binance.com:9443`, `fstream`, `api`, `fapi` on the release's rows.
    pub linear: UrlTable,
    /// Binance's COIN-M (coin-margined, `dapi`) public endpoints: [`Self::linear`] with its futures half on
    /// the COIN-M hosts (rows of product `coinm`); the spot half is the same, so a spot symbol reads the same
    /// hosts from either table.
    ///
    /// MEASURED 2026-11-08 from the dev box, 30 s windows on `btcusd_perp`: `dstream` serves kline (4 frames),
    /// aggTrade (4), trade (6), bookTicker (~1060) and depth@100ms (~260) on `/ws`, `/market` AND `/public`
    /// alike (kline and aggTrade push per trade on this book), so it is not split by stream class today. The
    /// table takes fapi's class paths anyway (decision 0251), so a split like fapi's does not silence it.
    /// `@aggTrade` streams here (fapi's was silent on its legacy path), so the trade lane is the aggregate
    /// one: its warmup is dapi's `aggTrades`, in the same id space, and its first id is a backfill boundary.
    pub coin_m: UrlTable,
}

impl BinanceTables {
    /// The tables over `urls`, binance's `venue_url` rows.
    ///
    /// # Errors
    /// A row is missing: binance's market data is NOT RUNNING, the refusal names the row.
    pub fn from_urls(urls: &VenueUrls) -> Result<BinanceTables, UrlRefusal> {
        let at = |protocol, product| {
            urls.url(UrlType::Live, UrlChannel::Data, protocol, product).map(static_url)
        };
        use UrlProtocol::{Rest, Websocket};
        let linear = UrlTable {
            spot_websocket: at(Websocket, "spot")?,
            perp_websocket: at(Websocket, "usdm")?,
            spot_rest: at(Rest, "spot")?,
            perp_rest: at(Rest, "usdm")?,
            ..PATHS
        };
        let coin_m = UrlTable {
            perp_websocket: at(Websocket, "coinm")?,
            perp_rest: at(Rest, "coinm")?,
            perp_agg_trades_path: "/dapi/v1/aggTrades",
            perp_trades_lane: PerpTradesLane::Aggregated,
            perp_raw_trades_path: "/dapi/v1/trades",
            ..linear
        };
        Ok(BinanceTables { linear, coin_m })
    }

    /// The table every lane of `symbol` reads: [`Self::coin_m`] for a COIN-M name (the catalog's name
    /// rule, `vike_catalog::is_inverse_contract`, the one the exec plane mounts it by), else
    /// [`Self::linear`]. No spot or USDⓈ-M symbol is a COIN-M name, so theirs is unchanged.
    #[must_use]
    pub fn urls_for(&self, symbol: &str) -> UrlTable {
        if vike_catalog::is_inverse_contract(VENUE, symbol) { self.coin_m } else { self.linear }
    }
}

/// The release's tables: the hosts a fresh box's rows name (this crate's unit tests).
#[cfg(test)]
pub(crate) fn seeded_tables() -> BinanceTables {
    BinanceTables::from_urls(&VenueUrls::seed(VENUE)).expect("the seed holds every binance row")
}

/// The kline frame field `symbol`'s bars take their volume from: [`VolumeColumn::Index7`] (`q`) for a
/// COIN-M perp, where `v` is a CONTRACT count (MEASURED 2026-11-08 on `btcusd_perp@kline_1m`: `v` "865",
/// `q` "1.14357557"; 865 contracts of 100 USD at 75,640 is 1.1436 BTC), else [`VolumeColumn::Index5`]
/// (`v`). The seed reads the same column ([`fetch_klines_latest`]), so a live bar and the history path's
/// bar of the same minute agree; read from `v`, a COIN-M bar would be ~760 times the history's.
pub fn kline_volume(symbol: &str) -> VolumeColumn {
    let perp = vike_catalog::split_perp_at(VENUE, symbol).1;
    if perp && vike_catalog::is_inverse_contract(VENUE, symbol) {
        VolumeColumn::Index7
    } else {
        VolumeColumn::Index5
    }
}

/// This venue's mark wire is documented, so its mark stream ships ON; `venue.binance.mark_streams
/// = 0` turns it off (decision 0095).
const MARK_STREAM_DEFAULT_ON: bool = true;

/// **Binance's half of the one data-client engine**: `DataClientWebsocket::new(DataClientSpecBinance::new(&urls)?,
/// sink, wake)` is binance's live `vike_data::DataClient`. It holds what every lane reads: the hosts,
/// the kline seed's hosts and the trade tape's earliest-live-id map.
pub struct DataClientSpecBinance {
    /// The streams' and REST warmups' hosts ([`BinanceTables::from_urls`]).
    tables: BinanceTables,
    /// The kline seed's hosts, the backfill's own ([`KlineSourceBinance::from_urls`]).
    klines: KlineSourceBinance,
    /// Earliest live aggTrade id ever observed per symbol (SP3 T2): every trades lane of this client
    /// records `min(existing, its startup splice's first id)` for its own symbol right after that
    /// splice (see `family::feed::tick::trades::run_trades_feed`'s module doc, "Earliest-live-id reporting"). A
    /// backfill pages strictly below it (`global-constraints.md`'s no-double-count invariant). ⚠
    /// NOTHING reads it today: the GUI's backfill, its only reader, went with the shell's local feed
    /// plane (#1727), and the public getter it read through was deleted on 2026-10-10.
    earliest_live_ids: Arc<Mutex<HashMap<String, u64>>>,
}

impl DataClientSpecBinance {
    /// The spec over `urls`, binance's `venue_url` rows: every host a lane dials is looked up here,
    /// once.
    ///
    /// # Errors
    /// A row is missing: binance's feed is NOT RUNNING, the refusal names the row.
    pub fn new(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(DataClientSpecBinance {
            tables: BinanceTables::from_urls(urls)?,
            klines: KlineSourceBinance::from_urls(urls)?,
            earliest_live_ids: Arc::default(),
        })
    }

    /// The family row every lane of `symbol` reads: its book's hosts (a COIN-M contract's lanes dial
    /// `dstream` and seed from `dapi`).
    fn family(&self, symbol: &str) -> FamilySpec {
        crate::feed::tick::trades::spec(self.tables.urls_for(symbol))
    }
}

impl DataClientSpec for DataClientSpecBinance {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = "Binance";
    const MARK_STREAMS_DEFAULT_ON: bool = MARK_STREAM_DEFAULT_ON;

    /// A `.P` perp pairs its `@markPrice@1s` stream; spot has no mark price at all. The catalog's
    /// name rule (`split_perp_at` on a `Naming::PerpSuffix` venue is `strip_suffix(".P").is_some()`),
    /// never a ninth bare `ends_with(".P")`.
    fn pairs_mark(&self, symbol: &str) -> bool {
        vike_catalog::split_perp_at(VENUE, symbol).1
    }

    /// A trailing `.P` marks a USDⓈ-M (or COIN-M) perp, the catalog's distinct-symbol tag: it is
    /// stripped to the exchange symbol for the REST seeds and the WS URLs, while the engine keys every
    /// sink call on the ORIGINAL `.P`-suffixed symbol (so a perp's series never collides with its spot
    /// twin). Quotes and the lossless book are refused by binance's caps row before any plan is asked;
    /// a plan asked for one anyway says so.
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        let family = self.family(symbol);
        let (api_symbol, is_perp) = vike_catalog::split_perp_at(VENUE, symbol);
        match lane {
            DataClientLane::Bars => {
                let klines = self.klines.clone();
                Ok(feed_main(
                    api_symbol,
                    interval,
                    is_perp,
                    kline_volume(symbol),
                    &family.urls,
                    move |api, iv, perp| fetch_klines_latest(&klines, api, iv, SEED_LIMIT, perp),
                ))
            }
            DataClientLane::Mark => Ok(mark_main(symbol, api_symbol, &family.urls)),
            DataClientLane::Trades => {
                Ok(trades_main(symbol, family, Arc::clone(&self.earliest_live_ids)))
            }
            DataClientLane::Depth => {
                let tables = self.tables;
                Ok(depth_main(symbol, &family, move |api, perp| {
                    crate::feed::tick::fetch_depth_snapshot_for(&tables, perp, api)
                }))
            }
            DataClientLane::Quotes | DataClientLane::Book => {
                Err(format!("binance serves no {} lane", lane.word(interval)))
            }
        }
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
