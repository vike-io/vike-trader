//! **binance's READ of an account's leverage** (decision 0182): what the gatewayhub's lane asks the
//! venue before a start (can this account safely be asked to hold N?) and after it (does it now hold
//! N?). Signed GETs through this crate's own signer and transport, and NO write (the account MODES it refuses
//! on are switched by the bridge's own switch on the DEMO host only, `crate::account::modes`, which the lane runs BEFORE its probe through
//! `crate::execution::heal_symbol_modes` (`VenueAdapter::heal_modes`) and the start repeats): the one write is
//! `crate::execution::rest::perp::VenueRestBinancePerp::set_leverage` (`POST /fapi/v1/leverage`, the figure cast `as i64`),
//! fired once by the exec thread at the start and, for a perpetual admitted later, once by
//! `crate::execution::write_symbol_leverage` (`vike_bridge_core::leverage_read`'s module doc has the shape).
//! USDⓈ-M futures only: a spot symbol has no leverage and is refused.
//!
//! | read | endpoint | what is taken from it |
//! |---|---|---|
//! | the account's mode AND what is held | `GET /fapi/v2/positionRisk?symbol=` (the read `recon_client` already makes) | EXACTLY ONE row with `positionSide BOTH` (hedge mode answers LONG and SHORT), `marginType` `cross`, its `leverage`, and whether `positionAmt` is non-zero |
//! | the symbol's maximum (OPTIONAL) | `GET /fapi/v1/leverageBracket?symbol=` (signed) | the highest `initialLeverage` of the brackets. ⚠ Failure-tolerant: an unreadable bracket is "no maximum known", never a refusal |
//!
//! There is no separate account-mode endpoint here, so [`LeverageReader::check_account`] and
//! [`LeverageReader::held`] read the same row (two GETs of weight 5 per probe). A portfolio-margin
//! account answers `/fapi` with an error, which is a refusal ("could not be read"). The bracket is not
//! required for correctness: a figure the venue rejects never becomes held, so the poll ends in an
//! error naming both numbers; the bracket only makes that refusal early and readable.
//!
//! ⚠ **The parsers follow the venue's DOCUMENTED shapes**; `positionRisk` is read by `recon_client`
//! (`tests/binance_smokes/binance_reconcile_smoke.rs`), but `leverageBracket` has been proven only as the
//! unauthenticated `-2014` answer (`crate::adapter`'s `start_with`). A live demo drill is owed. The
//! transport is ungated: a start spends at most three probe reads and seven poll reads, each on the
//! transport's SHORT-timeout agent (`signed_requery`, the one the resync and the confirm use), so a
//! venue that stalls costs a read five seconds, not thirty. A reader is built per call, so its first
//! read also asks the venue's clock (`crate::family::transport`, one public GET, three seconds at most).

use crate::family::signer::BinanceHmacSigner;
use serde_json::Value;
use vike_bridge_core::json::json_number;
use vike_bridge_core::leverage::leverage_figure;
use vike_bridge_core::leverage_read::{HeldLeverage, LeverageReader};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::credentials::api_credentials;

use crate::VENUE;
use crate::execution::BinanceUrls;
use crate::execution::rest::perp::PATH_POSITIONS;
use crate::family::execution::dispatch::split_symbol;
use crate::family::transport::RestTransportOnVenueClock;
use crate::refusals::{BinanceAccountRefusal, BinanceLeverageRefusal};
use vike_bridge_core::signer::VenueClock;

/// `GET /fapi/v1/leverageBracket`.
pub const PATH_LEVERAGE_BRACKET: &str = "/fapi/v1/leverageBracket";

/// What the account holds on `symbol`, and whether its mode is the one the engine models, from the
/// body of `GET /fapi/v2/positionRisk`. PURE.
///
/// # Errors
/// No row, several rows or a non-`BOTH` row (hedge mode), a non-cross `marginType` (or none), or a
/// `leverage` that is not a figure of at least 1.
pub fn parse_position_risk(
    body: &Value,
    symbol: &str,
) -> Result<HeldLeverage, BinanceLeverageRefusal> {
    let rows: Vec<&Value> = body
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| {
                    r.get("symbol")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
                })
                .collect()
        })
        .ok_or(BinanceLeverageRefusal::NoArray)?;
    let row = match rows.as_slice() {
        [] => return Err(BinanceLeverageRefusal::NoRow { symbol: symbol.to_string() }),
        [one] => *one,
        many => {
            return Err(BinanceLeverageRefusal::HedgeRows {
                count: many.len(),
                symbol: symbol.to_string(),
            });
        }
    };
    if row.get("positionSide").and_then(Value::as_str).is_some_and(|side| side != "BOTH") {
        return Err(BinanceLeverageRefusal::HedgeSide);
    }
    match row.get("marginType").and_then(Value::as_str) {
        Some(mode) if mode.eq_ignore_ascii_case("cross") => {}
        Some(mode) => return Err(BinanceLeverageRefusal::NotCross { mode: mode.to_string() }),
        None => return Err(BinanceLeverageRefusal::NoMarginType),
    }
    let held =
        row.get("leverage").and_then(leverage_figure).ok_or(BinanceLeverageRefusal::NoLeverage)?;
    // An unreadable amount must not read as "flat": the open-position guard fails CLOSED.
    let position_open = row
        .get("positionAmt")
        .and_then(json_number)
        .map(|amt| amt != 0.0)
        .ok_or(BinanceLeverageRefusal::NoPositionAmt)?;
    Ok(HeldLeverage { held, position_open })
}

/// The symbol's highest allowed leverage (the first bracket's `initialLeverage`), from the body of
/// `GET /fapi/v1/leverageBracket`, which is an array of `{symbol, brackets}` or one such object.
/// `None` for any shape this does not understand: the bracket is optional, so it never refuses.
#[must_use]
pub fn parse_bracket_max(body: &Value, symbol: &str) -> Option<f64> {
    let entry = match body {
        Value::Array(rows) => rows.iter().find(|r| {
            r.get("symbol").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case(symbol))
        })?,
        Value::Object(_) => body,
        _ => return None,
    };
    entry
        .get("brackets")?
        .as_array()?
        .iter()
        .filter_map(|b| b.get("initialLeverage").and_then(leverage_figure))
        .reduce(f64::max)
}

/// The reader for one account's mounted USDⓈ-M symbol over this crate's signed transport.
pub struct LeverageReaderBinance<S: Signer, T: RestTransport> {
    signer: S,
    transport: T,
    base_url: String,
    /// The EXCHANGE symbol (`BTCUSDT`, the `.P` suffix removed), the one `set_leverage` posts.
    symbol: String,
}

impl<S: Signer, T: RestTransport> LeverageReaderBinance<S, T> {
    /// A reader over `transport`, signing with `signer`, against the fapi `base_url`, for `symbol`.
    pub fn new(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        LeverageReaderBinance {
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
        }
    }

    /// One signed GET of `path?symbol=`. The `Err` is the venue's own words and never a credential.
    fn get(&self, what: &'static str, path: &str) -> Result<Value, BinanceLeverageRefusal> {
        self.transport
            .signed_requery(
                &self.base_url,
                path,
                "GET",
                &[("symbol", self.symbol.clone())],
                &self.signer,
            )
            .map_err(|why| BinanceLeverageRefusal::Unread { what, why })
    }

    /// The account's one row of `positionRisk`, read and judged ([`parse_position_risk`]).
    fn position_risk(&self) -> Result<HeldLeverage, BinanceLeverageRefusal> {
        parse_position_risk(&self.get("positionRisk", PATH_POSITIONS)?, &self.symbol)
    }
}

impl<S: Signer, T: RestTransport> LeverageReader for LeverageReaderBinance<S, T> {
    fn check_account(&self) -> Result<(), String> {
        self.position_risk().map(|_| ()).map_err(|refusal| refusal.to_string())
    }

    fn venue_max(&self) -> Result<Option<f64>, String> {
        // Optional, so any failure is "no maximum known": the poll is the judge of a rejected figure.
        Ok(self
            .get("leverageBracket", PATH_LEVERAGE_BRACKET)
            .ok()
            .and_then(|body| parse_bracket_max(&body, &self.symbol)))
    }

    fn held(&self) -> Result<HeldLeverage, String> {
        self.position_risk().map_err(|refusal| refusal.to_string())
    }
}

/// The reader for `symbol` (the engine's spelling, `BTCUSDT.P`) on the account whose credentials are
/// `creds`, on the futures host its `venue_url` rows `urls` name at `tier` (`live` is mainnet;
/// `crate::execution`'s `BinanceUrls::resolve`, the order clients' own lookup). The caller (the gatewayhub's
/// lane) has already checked that the table (`vike_venues::LEVERAGE_VENUES`) opens this tier.
///
/// # Errors
/// The account holds no binance key pair, `symbol` is a spot symbol (no `.P`), or the rows name no
/// address for the tier (the refusal's words).
pub fn leverage_reader(
    creds: &AccountCredentials<'_>,
    urls: &vike_venues::venue_urls::VenueUrls,
    tier: AccountTier,
    symbol: &str,
) -> Result<
    LeverageReaderBinance<
        BinanceHmacSigner,
        RestTransportOnVenueClock<
            crate::execution::rest::coin_m::RestTransportDapiPaths<UreqTransport>,
        >,
    >,
    String,
> {
    // a USER name (decision 0290), read by its kept row's lane name
    let lane =
        crate::instrument::names::lane_name_of(symbol).map_err(|refusal| refusal.to_string())?;
    let (api_symbol, is_perp) = split_symbol(&lane);
    if !is_perp {
        return Err(BinanceLeverageRefusal::SpotSymbol { symbol: symbol.to_string() }.to_string());
    }
    let keys = api_credentials(creds, VENUE)
        .ok_or_else(|| BinanceAccountRefusal::NoKeyPair.to_string())?;
    // The order clients' own host for `tier` (`Live` is MAINNET) and the symbol's book (COIN-M's `dapi` for a
    // COIN-M name): the read-back must see the network and the book the start's write went to.
    let hosts = BinanceUrls::resolve(urls, tier).map_err(|refusal| refusal.to_string())?;
    let clock = VenueClock::default();
    Ok(LeverageReaderBinance::new(
        BinanceHmacSigner::new(&keys, clock.ms()),
        RestTransportOnVenueClock::new(
            crate::execution::rest::coin_m::RestTransportDapiPaths(
                UreqTransport::new("binance"),
                crate::execution::rest::coin_m::margining_of(&api_symbol),
            ),
            VENUE,
            &clock,
            crate::execution::rest::coin_m::time_path_for(&api_symbol),
        ),
        hosts.futures_rest(&api_symbol),
        api_symbol,
    ))
}

#[path = "leverage_tests.rs"]
#[cfg(test)]
mod leverage_tests;
