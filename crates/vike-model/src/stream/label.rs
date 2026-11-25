//! `StreamLabel` — the CLOSED vocabulary of tick-lane words a feed passes as the `stream` argument of
//! `vike_data::DataClientSink::stream_status` and `vike_data::DataClientSink::data_delay`:
//! `"quotes"`, `"trades"`, `"book"` and `"depth"`. A bar lane's label is its INTERVAL (`"1m"`), so
//! the argument itself stays a `&str` and [`StreamLabel::from_word`] answers `None` for an interval
//! or any other word: the seam is open for bars by design and closed for ticks by this type.
//!
//! **Why one type.** Before it, the four words were respelled by hand at every producer
//! (`crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main` and its bybit, okx and deribit
//! twins, oanda's quotes pump, ibkr's quote and cpapi pumps, ig, polymarket's
//! `crates/bridges/polymarket/src/feed/state.rs`'s `FeedLane::as_str`) and at every consumer
//! (the datahub broker's routing sink, the market-data hub's sink, the recorder's `Stream`, the
//! store recorder's `crates/vike-store/src/live_rec.rs`'s `stream_status`), with only
//! literal-against-literal tests between them. A producer that spelled one word differently dropped
//! its gap and stale disclosures SILENTLY: that is the forty-day `depth` blind spot recorded in
//! `crates/vike-store/src/live_rec.rs`'s `stream_status` doc. Now a producer names the word through
//! [`StreamLabel::word`] and a consumer decides each word through an exhaustive `match`, so a new
//! label does not compile until every consumer has decided what it does with it.
//!
//! **Two spellings, two wires, never mixed.**
//! - The WORD ([`StreamLabel::word`]) is a wire and stored string: it rides `crate::StreamStatus`
//!   disclosures as the `stream` argument and lands in logs. `crates/vike-model/src/stream/label_tests.rs`
//!   pins it byte for byte. ⚠ It is NOT the store's `kind=` spelling: a label is the SUBSCRIPTION verb
//!   (`quotes`, `trades`, plural), [`crate::StoreKindId::word`] is the persisted series kind
//!   (`quote`, `trade`, singular); `book` and `depth` happen to coincide, which is how the near-miss
//!   hides. The recorder's `Stream::store_kind` is the map between the two.
//! - The serde TAG is the variant name (`"Quotes"`, `"Trades"`, `"Book"`, `"Depth"`, no `rename_all`):
//!   the tube carries the type itself (`vike_market_stream::wire::StreamWordWire::Lane`,
//!   `StreamDown::Delivery`'s `lane`), and the datahub market-data wire's lane carries the same tags.
//!   Those tags are frozen by `fixtures/stream_wire/` (whose suite,
//!   `crates/vike-market-stream/tests/stream_tag_fixtures.rs`, reads this file for the variants) and
//!   `fixtures/datahub_wire/`, so a renamed variant is a protocol break even where the word did not move.
//!
//! **The derived `Ord` is the declaration order** (`Quotes < Trades < Book < Depth`), the order the
//! datahub broker's lanes and the recorder's `Stream` have always had. A map keyed by a label iterates
//! in it.
//!
//! **What this type is not.**
//! - Not a bar vocabulary: an interval never parses here.
//! - Not the `"ticks"` link word of the HFT tick-sender path (`StreamStatusUpdate::stream`, written
//!   by the bridges' `disclose_link`): that rides a `vike_execution::TickSender`, not a `DataClientSink`. The
//!   core reads it only to derive the dead-man's SOURCE (`vike_venues::link_deadman::DeadManLinkSource::of`):
//!   every word but the two reserved hub-link words (`gateway-trade`, `gateway-strat`) is the venue's
//!   own socket, so a misspelling here still loses nothing, but a venue lane must NEVER spell a reserved word.
//! - Not [`TAPE_HOLE_WORD`], which says something about the RECORDED tape and is no lane of a feed.
//! - Not `vike_venues::DataClientVerb`, which is the SUPERSET (it also names `Bars`) and lives above this
//!   crate. `vike_venues::DataClientVerb`'s `From<StreamLabel>` and `DataClientVerb::stream_label` are the two
//!   exhaustive maps between them, so a new label or verb fails to compile there until it is decided.
//!
//! **The parallel enums that stay, and why.** `crates/vike-recorder/src/session.rs`'s `Stream` (the
//! recorder's subscription set, keyed by it, with a text gate over
//! `crates/vike-recorder/src/assembled.rs`'s `narrow`) derives its string conversions FROM this type
//! through an exhaustive `match`, never from a second list of words. The datahub broker's old `Lane`,
//! the tube's `WireLane` and the datahub wire's `MdLane` (whose variant order, and so derived `Ord`,
//! was the reverse) were the same four lanes and are gone: `crates/vike-datahub/src/feeds/mod.rs`,
//! `crates/vike-market-stream/src/wire.rs` and `crates/vike-datahub-client/src/market.rs` name this
//! type directly.

use serde::{Deserialize, Serialize};

/// The `stream` word of a disclosure addressed to the recorder alone: the QUOTE TAPE of this symbol has a
/// hole, because the venue's socket is down. `GapStart` opens it and `Live` closes it.
///
/// It is not [`StreamLabel::Quotes`], because `quotes` reaches a strategy: the strategy lane forwards it to
/// the tradehub, and a `GapStart` there is the venue's own socket to `vike_core::LinkDeadMan`, which can
/// cancel the venue's resting orders and HALT it. This word is no label and no bar interval, so no holder
/// but the recorder is admitted to it (`crates/vike-datahub/src/feeds/routing.rs`'s `stream_status`), and
/// it crosses the gatewayhub's wire as any other word does (`vike_market_stream::wire::StreamWordWire::Bar`).
///
/// A venue that emits it adds nothing else: `crates/vike-store/src/live_rec.rs`'s `stream_status` stores it in
/// `vike_data::store::store_kind::QUOTE_HOLES_LANE`.
pub const TAPE_HOLE_WORD: &str = "tape-hole";

// The label vocabulary, declared ONCE through `word_enum!`'s DEFINING form: the enum, `ALL`, `WORDS`,
// `word`, `from_word` and `parse` all expand from this one table, so a variant added here joins every one
// of them or none. The invocation spells the enum's header and four-space variant lines AT THE LEFT MARGIN
// on purpose: the tube's tag suite (`crates/vike-market-stream/tests/stream_tag_fixtures.rs`) reads the
// variants of this wire enum off the source exactly as it reads `wire.rs`'s, and stops at a `}` in column 0.
vike_utility::word_enum! {
/// A tick-lane `stream` label — see this module's doc for the contract (the word, the serde tag and
/// the derived `Ord`, which is declaration order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StreamLabel {
    /// The L1 quote lane — `DataClient::subscribe_quotes`, delivered through `DataClientSink::quote`.
    Quotes => "quotes",
    /// The trade tape — `DataClient::subscribe_trades`, delivered through `DataClientSink::trade`.
    Trades => "trades",
    /// The LOSSLESS L2 lane — `DataClient::subscribe_book`, delivered through `DataClientSink::book`
    /// and `DataClientSink::book_update`. Recorded as `kind=book`.
    Book => "book",
    /// The CONFLATING L2 lane — `DataClient::subscribe_depth`, delivered through
    /// `DataClientSink::l2_snapshot`. Recorded as `kind=depth`, deliberately NOT `kind=book`.
    Depth => "depth",
}
}

#[path = "label_tests.rs"]
#[cfg(test)]
mod label_tests;
