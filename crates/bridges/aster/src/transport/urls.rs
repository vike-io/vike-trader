//! Aster's four REST/WS bases, resolved from the venue's `venue_url` rows
//! (`vike_venues::venue_urls::VenueUrls`, the ONE lookup) — futures = fapi/fstream (product `usdm`),
//! spot = sapi/sstream (product `spot`). No address is written here: the release's defaults are
//! `crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`.
//!
//! Two planes, two asks, each made ONCE where the plane starts:
//!
//! * [`AsterUrls::exec`] — the account's own network (`UrlType::of_tier`): REST at channel `trade`
//!   (orders, recon, the mount's `exchangeInfo` grid, the clock, the listenKey mint), WS at channel
//!   `user` (the listenKey stream). These NEVER fall back from `demo` to `live`, so a `demo` account
//!   signs on the `*.asterdex-testnet.com` rows and nothing else.
//! * [`AsterUrls::data`] — public market data at the tier its caller names (channel `data`; a `demo`
//!   ask falls back to the `live` rows by the lookup rule, the seed holding no aster `demo` data row).
//!
//! History klines and the funding-rate history are asked by `crate::history`'s `KlineSourceAster` /
//! `FundingRateSourceAster` themselves (`live` `history` / `live` `data`); the picker by `crate::catalog`'s
//! `CatalogProviderAster` (`live` `data`).

use vike_model::AccountTier;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The canonical venue id — homed here (the feed-plane URL module) rather than in `spot`, because
/// BOTH planes need it and `spot` is `exec`-gated (split-plane Phase 5): the keyless klines
/// fetcher (`crate::history`) keys its rate buckets by it, and every exec-plane consumer
/// (`spot`/`perp`/`recon_client`) imports it from here — no re-export shim, per the no-move-shims
/// convention (root CLAUDE.md).
pub const VENUE: &str = "aster";

/// The `venue_url` product word of the USDⓈ-M futures book (fapi/fstream).
pub const PERP_PRODUCT: &str = "usdm";

/// The `venue_url` product word of the spot book (sapi/sstream).
pub const SPOT_PRODUCT: &str = "spot";

/// Keyless spot `exchangeInfo` path — homed beside [`VENUE`] for the same reason (the feed-plane
/// `crate::history` composes it onto `sapi_rest`; the exec plane's startup fetch in `crate::execution`
/// spells it `transport::urls::SPOT_PATH_EXCHANGE_INFO` directly).
pub const SPOT_PATH_EXCHANGE_INFO: &str = "/api/v3/exchangeInfo";

/// Keyless perp `exchangeInfo` path — the fapi twin of [`SPOT_PATH_EXCHANGE_INFO`].
pub const PERP_PATH_EXCHANGE_INFO: &str = "/fapi/v3/exchangeInfo";

/// One plane's four bases, already resolved. `&'static str` through
/// `vike_binance::family::static_url` (leaked once per distinct address), so the value stays `Copy`
/// into every thread and closure it rides and fills the family's `UrlTable` as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsterUrls {
    pub fapi_rest: &'static str,
    pub sapi_rest: &'static str,
    pub fapi_websocket: &'static str,
    pub sapi_websocket: &'static str,
}

impl AsterUrls {
    /// **The exec plane's four bases: the account's own network**, `UrlType::of_tier(tier)` (`Paper`
    /// reads as `Demo`), REST at `trade` and WS at `user`, which never fall back to `live`.
    ///
    /// # Errors
    /// The first row the table cannot answer; the account does NOT RUN, with its `Display`.
    pub fn exec(urls: &VenueUrls, tier: AccountTier) -> Result<AsterUrls, UrlRefusal> {
        Self::resolve(urls, UrlType::of_tier(tier), UrlChannel::Trade, UrlChannel::User)
    }

    /// **The public market-data plane's four bases at `tier`** (channel `data`, REST and WS). A
    /// `demo` ask with no `demo` data row reads the `live` rows (the lookup rule's step 3).
    ///
    /// # Errors
    /// The first row the table cannot answer.
    pub fn data(urls: &VenueUrls, tier: AccountTier) -> Result<AsterUrls, UrlRefusal> {
        Self::resolve(urls, UrlType::of_tier(tier), UrlChannel::Data, UrlChannel::Data)
    }

    fn resolve(
        urls: &VenueUrls,
        ty: UrlType,
        rest: UrlChannel,
        ws: UrlChannel,
    ) -> Result<AsterUrls, UrlRefusal> {
        let at = |channel, protocol, product| {
            urls.url(ty, channel, protocol, product).map(vike_binance::family::static_url)
        };
        Ok(AsterUrls {
            fapi_rest: at(rest, UrlProtocol::Rest, PERP_PRODUCT)?,
            sapi_rest: at(rest, UrlProtocol::Rest, SPOT_PRODUCT)?,
            fapi_websocket: at(ws, UrlProtocol::Websocket, PERP_PRODUCT)?,
            sapi_websocket: at(ws, UrlProtocol::Websocket, SPOT_PRODUCT)?,
        })
    }

    /// The REST base of the book `perp` names: fapi for the futures book, sapi for spot.
    #[must_use]
    pub fn rest(&self, perp: bool) -> &'static str {
        if perp { self.fapi_rest } else { self.sapi_rest }
    }
}

/// The seed's two tables for this crate's unit tests: what a fresh box's rows resolve to.
#[cfg(test)]
pub(crate) mod seeded {
    use super::*;

    /// The mainnet bases, as the public data plane reads them at `live`.
    pub(crate) fn mainnet() -> AsterUrls {
        AsterUrls::data(&VenueUrls::seed(VENUE), AccountTier::Live).expect("the seed's live rows")
    }

    /// The testnet bases, as a `demo` account's exec plane reads them.
    pub(crate) fn testnet() -> AsterUrls {
        AsterUrls::exec(&VenueUrls::seed(VENUE), AccountTier::Demo).expect("the seed's demo rows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> VenueUrls {
        VenueUrls::seed(VENUE)
    }

    fn all(u: AsterUrls) -> [&'static str; 4] {
        [u.fapi_rest, u.sapi_rest, u.fapi_websocket, u.sapi_websocket]
    }

    /// The seed's rows give each plane exactly the hosts the code dialled before the rows existed.
    #[test]
    fn the_seed_resolves_the_hosts_the_code_dialled() {
        let live = AsterUrls {
            fapi_rest: "https://fapi.asterdex.com",
            sapi_rest: "https://sapi.asterdex.com",
            fapi_websocket: "wss://fstream.asterdex.com",
            sapi_websocket: "wss://sstream.asterdex.com",
        };
        let testnet = AsterUrls {
            fapi_rest: "https://fapi.asterdex-testnet.com",
            sapi_rest: "https://sapi.asterdex-testnet.com",
            fapi_websocket: "wss://fstream.asterdex-testnet.com",
            sapi_websocket: "wss://sstream.asterdex-testnet.com",
        };
        assert_eq!(AsterUrls::exec(&seed(), AccountTier::Live), Ok(live));
        assert_eq!(AsterUrls::exec(&seed(), AccountTier::Demo), Ok(testnet));
        assert_eq!(AsterUrls::exec(&seed(), AccountTier::Paper), Ok(testnet));
        assert_eq!(AsterUrls::data(&seed(), AccountTier::Live), Ok(live));
    }

    /// ⚠ The exec plane of a `demo` (or `paper`) account never yields a LIVE address: none of its
    /// four bases is any of the live rows, so a demo key set can only ever sign on the testnet.
    #[test]
    fn a_demo_execution_lookup_never_yields_a_live_address() {
        let live = all(AsterUrls::exec(&seed(), AccountTier::Live).unwrap());
        for tier in [AccountTier::Demo, AccountTier::Paper] {
            for host in all(AsterUrls::exec(&seed(), tier).unwrap()) {
                assert!(!live.contains(&host), "{tier}: {host} is a live address");
                assert!(host.contains("-testnet."), "{tier}: {host}");
            }
        }
    }

    /// A table without the account's own `demo` rows refuses; it never borrows the live ones.
    #[test]
    fn an_execution_lookup_with_no_demo_rows_refuses_instead_of_reading_live() {
        let live_only = VenueUrls::seed_typed(VENUE, UrlType::Live);
        assert!(AsterUrls::exec(&live_only, AccountTier::Demo).is_err());
        assert!(AsterUrls::exec(&VenueUrls::default(), AccountTier::Live).is_err());
    }

    #[test]
    fn rest_picks_the_book_the_flag_names() {
        let u = AsterUrls::exec(&seed(), AccountTier::Live).unwrap();
        assert_eq!(u.rest(true), u.fapi_rest);
        assert_eq!(u.rest(false), u.sapi_rest);
    }
}
