//! **The options lane's ONE contract: the venue's half.** The gatewayhub serves the options surface
//! (`OptionsRest`'s calls, the `OptionIndex` fetch and the two feeds) through ONE engine,
//! `crates/vike-gatewayhub/src/rest/options.rs`'s `OptionsHub`, and the venue's whole difference is an
//! [`OptionsSpec`] in that venue's bridge: its provider and the listing it serves, the names it admits to
//! a channel, its index fetch and its two feeds, with every word they refuse in. The engine + spec split
//! of `crate::credentialed_klines`, for the options lane.
//!
//! # What the engine does, so a spec does not
//!
//! It maps each wire call onto one spec method and the answer back onto the wire's reply (the listing's
//! venue is [`OptionsSpec::venue`]), checks the request's cancel flag before the index fetch, runs the
//! wire's own focus-set check (empty, over its cap, a name that is not channel-safe) before a feed is
//! asked for, and holds a started feed's [`OptionsFeedStop`] in a handle that calls it ONCE, at an
//! explicit shutdown or at the drop.
//!
//! # What a spec promises
//!
//! * **A failure is `Err` in the venue's words, never an empty answer**: an empty listing or chain is a
//!   statement the datahub records.
//! * **A name the client sends is checked against the venue before the venue is asked for it**: an
//!   underlying it does not list, and a focus-set name that is no real index or instrument of one (a junk
//!   channel would be retried against the venue for the life of the socket), are refused in words.
//! * **A feed delivers on its own thread and stops when its stop is called**, joining that thread.

use vike_marketdata::{OptionChain, OptionChainExpiry, OptionTicker};
use vike_model::Instrument;

/// One underlying a venue lists options on, as the options lane serves it: its name as the venue spells
/// it (`BTC`) and the index its options' marks stream under (`btc_usd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsListedUnderlying {
    /// The underlying as the venue spells it.
    pub name: String,
    /// The venue index its marks feed subscribes.
    pub index: String,
}

/// Where a marks feed delivers each decoded batch (mark and implied vol only). Cheap, never blocking.
pub type OptionsMarksDeliver = Box<dyn Fn(Vec<OptionTicker>) + Send + Sync>;

/// Where a ticker feed delivers each decoded row. Cheap, never blocking.
pub type OptionsTickerDeliver = Box<dyn Fn(OptionTicker) + Send + Sync>;

/// Stops a started feed and joins its thread. The engine calls it at most once.
pub type OptionsFeedStop = Box<dyn FnOnce() + Send>;

/// **A venue's options lane**, as the gatewayhub's engine asks it. See the module doc.
pub trait OptionsSpec: Send + Sync {
    /// The venue whose options this lane serves: a `vike_model::VENUES` slug.
    fn venue(&self) -> &'static str;

    /// Every underlying the venue lists options on, in its listing's order: the listing held, or read
    /// again first when `refresh` asks (the venue's own pace decides whether it is).
    fn listing(&self, refresh: bool) -> Result<Vec<OptionsListedUnderlying>, String>;

    /// The expiries of a listed `underlying`, ascending.
    fn expiries(&self, underlying: &str) -> Result<Vec<OptionChainExpiry>, String>;

    /// One expiry's chain of a listed `underlying`, greeks enriched at the risk-free rate `r`: the FULL
    /// chain for `strikes: None`, ±`strikes` around spot otherwise.
    fn chain(
        &self,
        underlying: &str,
        expiry: &str,
        strikes: Option<usize>,
        r: f64,
    ) -> Result<OptionChain, String>;

    /// The contract index of one `kind` (`"option"` or `"future"`): the whole of it, or the reason in
    /// words.
    fn instruments(&self, kind: &str) -> Result<Vec<Instrument>, String>;

    /// Start the marks feed over `indexes`, delivering into `deliver`, once every name is a real index.
    fn open_marks(
        &self,
        indexes: Vec<String>,
        deliver: OptionsMarksDeliver,
    ) -> Result<OptionsFeedStop, String>;

    /// Start the ticker feed over `instruments`, delivering into `deliver`, once every name is a real
    /// option instrument.
    fn open_ticker(
        &self,
        instruments: Vec<String>,
        deliver: OptionsTickerDeliver,
    ) -> Result<OptionsFeedStop, String>;
}
