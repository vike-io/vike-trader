//! The per-event DELIVERY CLASS: what a tube may do with one [`crate::live::DataClientSink`] verb's
//! frames when its reader falls behind. One table, beside the seam it classifies, that every tube
//! reads (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`,
//! verdict 3).
//!
//! Before this table the class lived in the gatewayhub's per-session queue (its `Verb`), drawn from
//! the sink's documented contracts ([`crate::live::DataClientSink`]'s method docs and `live.rs`'s
//! module doc: a missed bar close is a series hole, an intrabar snapshot is latest-wins). Moving it
//! here changes WHICH verb is which in no case; it changes who owns the answer.
//!
//! # The three classes ([`DataClientSinkDelivery`])
//!
//! | class | verbs | on overflow |
//! |---|---|---|
//! | [`DataClientSinkDelivery::Lossless`] | `seed_bars`, `close_bar`, `quote`, `trade`, `book`, `book_update` | evict the oldest, and OWE a disclosed gap |
//! | [`DataClientSinkDelivery::Conflatable`] | `forming_bar`, `mark_tick`, `bar_close_tick`, `top_of_book_tick`, `l2_snapshot`, `funding` | supersede the unsent frame of the slot: latest wins, which is not a loss for a chart |
//! | [`DataClientSinkDelivery::Control`] | `stream_status`, `data_delay` | never evicted (the wire's own `SubscribeOk`, `SubscribeError` and `Key` frames are control too but are not sink verbs) |
//!
//! The derived `Ord` is the strength order, `Conflatable < Lossless < Control`: a stronger class loses
//! less. [`stronger`] is the maximum.
//!
//! # The per-subscriber override, and why it exists
//!
//! The base class is a property of the VERB, but conflation is not a loss for a chart and is one for a
//! tape: a recorder that writes `l2_snapshot` rows into `kind=depth` has a missing row, with no marker,
//! whenever a conflatable frame is superseded under backlog. A subscriber therefore declares a FLOOR
//! and the class it gets is [`stronger`] of the verb's base and that floor ([`DataClientSinkVerb::under`]); a
//! recorder's floor is [`RECORDER_FLOOR`] and applies to the verbs it records
//! ([`DataClientSinkVerb::for_recorder`]), so a recorded kind is never superseded for it.
//!
//! One real subscription can serve two holders (the datahub's market-data hub and its recorder share
//! a venue session), so the class a subscription runs at is the strongest of its holders' needs
//! ([`lane_delivery`] per holder, then [`stronger`]), and it can CHANGE while the subscription is open
//! (the recorder joins a depth key the hub already holds, or leaves it): that is
//! [`crate::live::DataClient::set_delivery`].
//!
//! ⚠ **Raising a class is not "stop superseding".** A tube that lets a depth frame survive but keeps
//! it conflatable-by-position makes room by evicting LOSSLESS quotes and trades instead: the raised
//! class has to ride the lossless rule, gap and all.

use serde::{Deserialize, Serialize};
use vike_model::StreamLabel;

/// What a tube may do with a frame when its reader lags. See this module's doc for the table.
///
/// ⚠ **The serde tag is the stream wire's** (the variant name, `"Lossless"`): a subscriber's floor rides
/// `vike_market_stream::wire::StreamUp::Subscribe`'s `class` and `StreamUp::Delivery`, and the sender's answer
/// `StreamDown::Delivery`, as this type itself. `fixtures/stream_wire/` freezes the tags
/// (`crates/vike-market-stream/tests/stream_tag_fixtures.rs` reads this file for the variants), so a
/// renamed or reordered variant is a protocol break even though nothing here fails to compile.
///
/// What a subscriber sends is a FLOOR: the class a verb takes is the stronger of its own and the floor
/// ([`DataClientSinkVerb::under`]), so `Conflatable` (the weakest) changes nothing and a floor can only raise. A
/// data verb at [`DataClientSinkDelivery::Control`] is queued like a `Lossless` one: the control lane is for the
/// wire's own frames, and a data frame under it would be unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum DataClientSinkDelivery {
    /// Latest-wins: an unsent frame of the same slot is superseded, which is not a loss.
    Conflatable,
    /// Every frame counts: the oldest is evicted under pressure and the hole is disclosed as a gap.
    Lossless,
    /// Never evicted.
    Control,
}

/// The stronger of two classes (the one that loses less): their maximum under the derived order.
#[must_use]
pub const fn stronger(
    a: DataClientSinkDelivery,
    b: DataClientSinkDelivery,
) -> DataClientSinkDelivery {
    if (a as u8) >= (b as u8) { a } else { b }
}

/// The floor a RECORDER holder declares: every verb it records arrives without supersession.
pub const RECORDER_FLOOR: DataClientSinkDelivery = DataClientSinkDelivery::Lossless;

/// Declare the sink-verb table ONCE. The enum, [`DataClientSinkVerb::ALL`], [`DataClientSinkVerb::base`] and
/// [`DataClientSinkVerb::recorded`] are all expanded from this one table, so there is no second list to drift:
/// a verb added here carries a class and a recorded flag or the crate does not build. That the table
/// names every method of `DataClientSink` is `delivery_tests.rs`'s test over `live.rs`.
macro_rules! sink_verbs {
    ($($(#[$doc:meta])* $variant:ident => ($class:ident, $recorded:literal),)+) => {
        /// One verb of [`crate::live::DataClientSink`], by role. The thirteen methods of the trait, one
        /// variant each; the tube's own frames (`SubscribeOk`, `Key`, `Gap`, `Heartbeat`) are not sink verbs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum DataClientSinkVerb {
            $($(#[$doc])* $variant,)+
        }

        impl DataClientSinkVerb {
            /// Every verb, in declaration order. Complete BY CONSTRUCTION: the enum is declared
            /// nowhere but this macro invocation.
            pub const ALL: &'static [DataClientSinkVerb] = &[$(DataClientSinkVerb::$variant,)+];

            /// The verb's own class, before any subscriber's floor. Exhaustive and `const`, with no
            /// `_` arm: this is the table the gatewayhub queue's `Verb` carried.
            #[must_use]
            pub const fn base(self) -> DataClientSinkDelivery {
                match self {
                    $(DataClientSinkVerb::$variant => DataClientSinkDelivery::$class,)+
                }
            }

            /// Does the recorder write this verb into a `kind=` series? `book` is not recorded (the
            /// recorder folds `book_update` itself) and neither is a bar, a mark or a delay label.
            #[must_use]
            pub const fn recorded(self) -> bool {
                match self {
                    $(DataClientSinkVerb::$variant => $recorded,)+
                }
            }
        }
    };
}

sink_verbs! {
    /// `seed_bars`.
    SeedBars => (Lossless, false),
    /// `close_bar`.
    CloseBar => (Lossless, false),
    /// `forming_bar`. Never recorded: the recorder records no bars.
    FormingBar => (Conflatable, false),
    /// `mark_tick`. Not recorded today (`kind=mark` is deferred); it joins the recorded set the day
    /// that series is written.
    MarkTick => (Conflatable, false),
    /// `bar_close_tick`. Not recorded today, for the reason `TickMark` gives.
    BarCloseTick => (Conflatable, false),
    /// `top_of_book_tick`. Latest-wins like `mark_tick`. Never on a tube: only the tradehub's price tap sends
    /// it, in process, to its own core (decision 0208's amendment of 2026-11-15); not recorded.
    TopOfBookTick => (Conflatable, false),
    /// `l2_snapshot`. The ONE conflatable verb the recorder writes (`kind=depth`).
    L2Snapshot => (Conflatable, true),
    /// `quote`.
    Quote => (Lossless, true),
    /// `trade`.
    Trade => (Lossless, true),
    /// `book`. Lossless, and not recorded: the recorder folds the deltas itself.
    Book => (Lossless, false),
    /// `book_update`.
    BookUpdate => (Lossless, true),
    /// `stream_status`. Rides the control lane; the recorder writes it as a gap marker.
    StreamStatus => (Control, true),
    /// `data_delay`. Rides the control lane; the recorder stores no delay label.
    DataDelay => (Control, false),
    /// `funding`. Latest-wins like `mark_tick`: the newest rate and settlement time supersede an
    /// unsent older one. Not recorded (the `interval=funding` history comes from the venue's REST lane).
    Funding => (Conflatable, false),
}

impl DataClientSinkVerb {
    /// The class this verb takes for a subscriber that declared `floor`: never below the verb's own
    /// base, so a floor can only STRENGTHEN. Both a conflatable and a control verb keep what they
    /// are when the floor is weaker than their base.
    #[must_use]
    pub const fn under(self, floor: DataClientSinkDelivery) -> DataClientSinkDelivery {
        stronger(self.base(), floor)
    }

    /// The class this verb takes for a RECORDER holder: a recorded verb is at least
    /// [`RECORDER_FLOOR`] (so `l2_snapshot` is never superseded), an unrecorded one keeps its base.
    #[must_use]
    pub const fn for_recorder(self) -> DataClientSinkDelivery {
        if self.recorded() { self.under(RECORDER_FLOOR) } else { self.base() }
    }
}

/// The data verbs a lane's subscription delivers. Exhaustive over [`StreamLabel`]: a new lane must
/// decide its verbs here before the crate builds. `book` also lists `quote`, because a book pump may
/// derive L1 quotes (the broker's routing sink admits a `Book` holder to quotes for that reason).
#[must_use]
pub const fn lane_verbs(lane: StreamLabel) -> &'static [DataClientSinkVerb] {
    match lane {
        StreamLabel::Quotes => &[DataClientSinkVerb::Quote],
        StreamLabel::Trades => &[DataClientSinkVerb::Trade],
        StreamLabel::Book => {
            &[DataClientSinkVerb::Book, DataClientSinkVerb::BookUpdate, DataClientSinkVerb::Quote]
        }
        StreamLabel::Depth => &[DataClientSinkVerb::L2Snapshot],
    }
}

/// The class a subscription on `lane` must run at for ONE holder: the strongest class over the lane's
/// verbs, each at its base (`recorder == false`, the market-data hub) or at the recorder's floor
/// (`recorder == true`). Only `Depth` differs between the two today: it is conflatable for the hub and
/// lossless for the recorder, and every other lane is lossless for both.
#[must_use]
pub const fn lane_delivery(lane: StreamLabel, recorder: bool) -> DataClientSinkDelivery {
    let verbs = lane_verbs(lane);
    let mut class = DataClientSinkDelivery::Conflatable;
    let mut i = 0;
    while i < verbs.len() {
        let v = verbs[i];
        class = stronger(class, if recorder { v.for_recorder() } else { v.base() });
        i += 1;
    }
    class
}

#[path = "delivery_tests.rs"]
#[cfg(test)]
mod delivery_tests;
