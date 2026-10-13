//! `StreamLabel` — the CLOSED vocabulary of tick-lane words a feed passes as the `stream` argument of
//! [`crate::live::LiveDataSink::stream_status`] and [`crate::live::LiveDataSink::data_delay`]:
//! `"quotes"`, `"trades"`, `"book"` and `"depth"`. A bar lane's label is its INTERVAL (`"1m"`), so
//! the argument itself stays a `&str` and [`StreamLabel::from_label`] answers `None` for an interval
//! or any other word: the seam is open for bars by design and closed for ticks by this type.
//!
//! **Why one type.** Before it, the four words were respelled by hand at every producer
//! (`crates/bridges/binance/src/family/market_feed.rs`'s `depth_main` and its bybit, okx and deribit
//! twins, oanda's quotes pump, ibkr's quote and cpapi pumps, ig, polymarket's
//! `crates/bridges/polymarket/src/market_feed/state.rs`'s `PumpMode::as_str`) and at every consumer
//! (the datahub broker's routing sink, the market-data hub's sink, the recorder's `Stream`, the
//! store recorder's `crates/vike-store/src/live_rec.rs`'s `stream_status`), with only
//! literal-against-literal tests between them. A producer that spelled one word differently dropped
//! its gap and stale disclosures SILENTLY: that is the forty-day `depth` blind spot recorded in
//! `crates/vike-store/src/live_rec.rs`'s `stream_status` doc. Now a producer names the word through
//! [`StreamLabel::as_str`] and a consumer decides each word through an exhaustive `match`, so a new
//! label does not compile until every consumer has decided what it does with it.
//!
//! **The spellings are wire and stored strings** (they ride `vike_model::StreamStatus` disclosures
//! onto the datahub wire and into logs): `crates/vike-data/src/stream_label_tests.rs` pins them byte
//! for byte. ⚠ They are NOT the store's `kind=` spellings: a label is the SUBSCRIPTION verb
//! (`quotes`, `trades`, plural), `vike_data::StoreKindId::as_str` is the persisted series kind
//! (`quote`, `trade`, singular); `book` and `depth` happen to coincide, which is how the near-miss
//! hides. The recorder's `Stream::store_kind` is the map between the two.
//!
//! **What this type is not.**
//! - Not a bar vocabulary: an interval never parses here.
//! - Not the `"ticks"` link label of the HFT tick-sender path (`StreamStatusUpdate::stream`, written
//!   by the bridges' `disclose_link`): that rides a `vike_exec::TickSender`, not a `LiveDataSink`, is
//!   diagnostics only and is destructured away by the core, so a misspelling there loses nothing.
//! - Not `vike_venues::LiveVerb`, which is the SUPERSET (it also names `Bars`) and lives below this
//!   crate. [`StreamLabel::live_verb`] and [`StreamLabel::from_live_verb`] are the two exhaustive
//!   maps between them, so a new `LiveVerb` fails to compile here until it is decided.
//!
//! **The parallel enums that stay, and why.** Each derives its string conversions FROM this type
//! through an exhaustive `match`, never from a second list of words:
//! `crates/vike-recorder/src/session.rs`'s `Stream` (the recorder's subscription set, keyed by it,
//! with a text gate over `crates/vike-recorder/src/assembled.rs`'s `narrow`) and
//! `crates/vike-datahub-client/src/market.rs`'s `MdLane` (a serde WIRE type with no `Quotes` lane
//! and its own variant order). The datahub broker's old `Lane` was verifiably identical and is gone:
//! `crates/vike-datahub/src/feeds/mod.rs` names this type directly.

use vike_venues::LiveVerb;

/// Declare the label vocabulary ONCE. The enum, [`StreamLabel::ALL`] and [`StreamLabel::as_str`]
/// are all expanded from this one table (and [`StreamLabel::from_label`] searches it through
/// `ALL`), so there is no second list of words or of variants to drift: a variant added here joins
/// every one of them or none.
macro_rules! stream_labels {
    ($($(#[$doc:meta])* $variant:ident => $word:literal,)+) => {
        /// A tick-lane `stream` label — see this module's doc for the contract.
        ///
        /// The derived `Ord` is declaration order (`Quotes < Trades < Book < Depth`), the order the
        /// datahub broker's and the recorder's lanes have always had.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum StreamLabel {
            $($(#[$doc])* $variant,)+
        }

        impl StreamLabel {
            /// Every label, in declaration order. Complete BY CONSTRUCTION: the enum is declared
            /// nowhere but this macro invocation.
            pub const ALL: &'static [StreamLabel] = &[$(StreamLabel::$variant,)+];

            /// The one spelling of this label, as a feed passes it and a sink receives it. Exhaustive
            /// and `const`, so a producer can name it in a `const` and a new variant must be spelled
            /// before the crate builds.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(StreamLabel::$variant => $word,)+
                }
            }
        }
    };
}

stream_labels! {
    /// The L1 quote lane — `DataClient::subscribe_quotes`, delivered through `LiveDataSink::quote`.
    Quotes => "quotes",
    /// The trade tape — `DataClient::subscribe_trades`, delivered through `LiveDataSink::trade`.
    Trades => "trades",
    /// The LOSSLESS L2 lane — `DataClient::subscribe_book`, delivered through `LiveDataSink::book`
    /// and `LiveDataSink::book_update`. Recorded as `kind=book`.
    Book => "book",
    /// The CONFLATING L2 lane — `DataClient::subscribe_depth`, delivered through
    /// `LiveDataSink::l2_snapshot`. Recorded as `kind=depth`, deliberately NOT `kind=book`.
    Depth => "depth",
}

impl StreamLabel {
    /// Parse a feed's `stream` argument back to its label. `None` for a bar interval and for any
    /// other word — never a wrong label, so a consumer that matches the result exhaustively decides
    /// the unknown case in one place.
    #[must_use]
    pub fn from_label(label: &str) -> Option<StreamLabel> {
        StreamLabel::ALL.iter().copied().find(|l| l.as_str() == label)
    }

    /// The [`LiveVerb`] that serves this lane — the seam through which
    /// [`crate::live::require_live_verb`] decides whether a venue may be asked for it. Total.
    #[must_use]
    pub const fn live_verb(self) -> LiveVerb {
        match self {
            StreamLabel::Quotes => LiveVerb::Quotes,
            StreamLabel::Trades => LiveVerb::Trades,
            StreamLabel::Book => LiveVerb::Book,
            StreamLabel::Depth => LiveVerb::Depth,
        }
    }

    /// The label of a [`LiveVerb`]; `None` for `Bars`, whose stream label is an interval. Exhaustive
    /// over `LiveVerb`, so a new verb must be decided here before this crate builds.
    #[must_use]
    pub const fn from_live_verb(verb: LiveVerb) -> Option<StreamLabel> {
        match verb {
            LiveVerb::Bars => None,
            LiveVerb::Quotes => Some(StreamLabel::Quotes),
            LiveVerb::Trades => Some(StreamLabel::Trades),
            LiveVerb::Book => Some(StreamLabel::Book),
            LiveVerb::Depth => Some(StreamLabel::Depth),
        }
    }
}

#[path = "stream_label_tests.rs"]
#[cfg(test)]
mod stream_label_tests;
