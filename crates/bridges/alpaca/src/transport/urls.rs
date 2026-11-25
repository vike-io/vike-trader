//! Alpaca host table — sandbox vs live endpoints for the three planes this bridge dials (authx /
//! broker / data WS), out of alpaca's `venue_url` rows (decision 0265). Verified live 2026-07-14: the
//! OAuth Bearer authorizes both the broker host and the data host; data is NOT reachable via the
//! broker host. (The data REST host has rows in the seed and no reader in this bridge.)

use vike_model::AccountTier;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The Alpaca base URLs for one tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlpacaUrls {
    /// The OAuth2 token host (the `login` row).
    pub authx: String,
    /// The Broker API host (the `trade` row): orders, positions, the account, the events stream.
    pub broker: String,
    /// The market-data websocket host (the `data` `ws` row).
    pub data_websocket: String,
}

/// The sandbox's rows for `Paper`/`Demo`, the live ones for `Live` (`UrlType::of_tier`), out of
/// alpaca's address rows `urls`.
///
/// # Errors
/// The first row refused, whose words are the account's NOT RUNNING reason.
pub fn urls_for(env: AccountTier, urls: &VenueUrls) -> Result<AlpacaUrls, UrlRefusal> {
    let ty = UrlType::of_tier(env);
    let row = |channel, protocol| urls.url(ty, channel, protocol, "").map(str::to_string);
    Ok(AlpacaUrls {
        authx: row(UrlChannel::Login, UrlProtocol::Rest)?,
        broker: row(UrlChannel::Trade, UrlProtocol::Rest)?,
        data_websocket: row(UrlChannel::Data, UrlProtocol::Websocket)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the release's seed each tier's hosts are, byte for byte, the ones the table held before
    /// the addresses moved into rows; a sandbox tier never reads a live row.
    #[test]
    fn sandbox_vs_live() {
        let seed = VenueUrls::seed("alpaca");
        let demo = urls_for(AccountTier::Demo, &seed).unwrap();
        assert_eq!(demo.authx, "https://authx.sandbox.alpaca.markets");
        assert_eq!(demo.broker, "https://broker-api.sandbox.alpaca.markets");
        assert_eq!(demo.data_websocket, "wss://stream.data.sandbox.alpaca.markets");
        assert_eq!(urls_for(AccountTier::Paper, &seed).unwrap(), demo);
        let live = urls_for(AccountTier::Live, &seed).unwrap();
        assert_eq!(live.authx, "https://authx.alpaca.markets");
        assert_eq!(live.broker, "https://broker-api.alpaca.markets");
        assert_eq!(live.data_websocket, "wss://stream.data.alpaca.markets");
        let none = VenueUrls::from_rows("alpaca", std::iter::empty());
        assert!(urls_for(AccountTier::Demo, &none).is_err(), "no row, no host");
    }
}
