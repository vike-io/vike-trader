//! `recording` — the RECORDING plane's venue table: per recordable venue, how a profile
//! subscription's family resolves to symbols, and whether its book already carries quotes.
//!
//! **Clients come from the broker; this table constructs none** (decision record 0092, one venue
//! client per process: `docs/decisions/0092-one-venue-client-per-process.md`). Each arm pairs a
//! resolver with a `crate::feeds::FeedHandle` over the process's ONE `crate::feeds::FeedBroker`, so
//! a key the market-data plane also holds is subscribed once. The REAL client behind the handle is
//! built by `crates/vike-datahub/src/feeds/venues.rs`'s `build_client` on the handle's first
//! subscribe, with the sink `crate::recorder::record` attached for `Holder::Rec` before any feed
//! here exists.
//!
//! It replaced `vike_recorder::venues` on 2026-09-28. That module built the recorder its own venue
//! clients from its own table, beside the md plane's, which is why a `record` + `live-feeds` daemon
//! used to subscribe a shared key twice; with the table here, `vike-recorder` names no bridge at
//! all.
//!
//! # The shape: one `#[cfg]` arm AND one `#[cfg(not)]` arm per venue
//!
//! Kept from the table it replaced, including its two refusals: a venue this table knows but this
//! build did not compile is refused naming the `record-<venue>` feature to rebuild with; a venue
//! it does not know at all is refused naming what IS supported. Two different messages, because
//! "unknown venue" sends somebody hunting for a typo when the answer is a rebuild.
//!
//! # Why `record-<venue>` implies `venue-<venue>`
//!
//! A PLANE feature (`record-`, `live-`) says what the process SERVES; a VENUE feature says what it
//! LINKS (`crates/vike-datahub/Cargo.toml`). The recording plane links a venue through exactly the
//! broker arm the md plane uses, so `record-binance` is `record` plus `venue-binance` — and binance
//! therefore arrives FEEDS-ONLY (`default-features = false`), where the recorder's own manifest used
//! to take it with the exec plane on.
//!
//! # Where "the book already carries quotes" lives
//!
//! The second argument of each arm's `AssembledFeed::new` — decision D1 of the plan behind 0092
//! chose a column of THIS table over a `vike_model::venues::venue_caps::LiveDataCaps` field, because this table is its
//! only consumer.
use std::sync::Arc;

use vike_recorder::Subscription;
use vike_recorder::assembled::AssembledFeed;

use crate::feeds::FeedBroker;

/// The venues this build can actually RECORD — and, since the `rec_venue=` advertisement, the
/// handshake's source of truth, so a client learns at CONNECT which venues it may name in a
/// recorder subscription.
///
/// `crates/vike-datahub/src/server.rs`'s `served_features` turns each slug into a
/// `vike_datahub_client::proto::rec_venue_feature` entry. That is the same arrangement
/// `crates/vike-datahub/src/md/venues.rs`'s own `supported` has with `md_venue=` — and the two are
/// deliberately SEPARATE advertisements, because the sets differ: the shipped image serves six
/// venues live and records two.
pub fn supported() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "record-binance")]
        "binance",
        #[cfg(feature = "record-polymarket")]
        "polymarket",
    ]
}

/// One profile subscription as a recorder feed over the broker.
///
/// `Err` when this build cannot record that venue — the message names the Cargo feature to rebuild
/// with. A recorder that silently skipped an unsupported venue would look healthy while recording
/// nothing, which is the failure `vike_recorder::config::RecorderProfile`'s validation already
/// exists to prevent at the profile level; the same rule applies to the build.
///
/// Building a feed opens nothing: the handle reaches the broker only on its first subscribe.
// `broker` is consumed by whichever venue arm this build compiled; a build with NO venue feature
// (`record` alone, the mount with no venue in it) has no such arm, and the signature stays the same
// either way.
#[cfg_attr(
    not(any(feature = "record-binance", feature = "record-polymarket")),
    allow(unused_variables)
)]
pub fn build_recording_feed(
    sub: &Subscription,
    broker: &Arc<FeedBroker>,
) -> Result<AssembledFeed, String> {
    match sub.venue.as_str() {
        #[cfg(feature = "record-binance")]
        "binance" => binance_feed(sub, broker),
        #[cfg(not(feature = "record-binance"))]
        "binance" => Err(missing_feature("binance", "record-binance")),
        #[cfg(feature = "record-polymarket")]
        "polymarket" => polymarket_feed(sub, broker),
        #[cfg(not(feature = "record-polymarket"))]
        "polymarket" => Err(missing_feature("polymarket", "record-polymarket")),
        other => Err(format!(
            "recorder: venue `{other}` has no feed in this build. Supported: [{}]",
            supported().join(", ")
        )),
    }
}

/// A family resolves through the venue's own resolver; an explicit symbol list is itself.
#[cfg(any(feature = "record-binance", feature = "record-polymarket"))]
fn resolver(
    sub: &Subscription,
    family: impl FnOnce(&str) -> Result<Box<dyn vike_data::live::SymbolResolver>, String>,
) -> Result<Box<dyn vike_data::live::SymbolResolver>, String> {
    match &sub.family {
        Some(f) => family(f),
        None => Ok(Box::new(vike_data::live::FixedSymbols::new(sub.symbols.iter().cloned()))),
    }
}

/// The Binance row — the STATIC-family case, and the proof that "families are general" (spec §8.2
/// of `docs/superpowers/specs/2026-08-02-live-recorder-design.md`) is not a Polymarket-shaped
/// claim.
///
/// Polymarket resolves a family by asking Gamma what is live right now, because its token ids rotate
/// every 5 minutes. Binance resolves the same key as a **filter over an instrument list that does not
/// change from tick to tick** — a glob (`*USDT`, `BTC*`, `*USDT.P`) matched against
/// `vike_binance::catalog::BinanceCatalog`'s `list_instruments`, through the venue-free
/// `vike_recorder::resolve::CatalogGlob`. One trait, rotation as the general case, static as the
/// degenerate one.
///
/// ## ⚠ What this can actually record: TRADES and conflating DEPTH. Not quotes, not book.
///
/// Binance's declared capabilities are `LiveDataCaps { bars, trades, depth }` — **`quotes: false`,
/// `book: false`** (`vike_model::venues::venue_caps`), and its `DataClient` refuses both verbs accordingly.
/// What it does serve is `subscribe_depth`, the DOM's **conflating** L2 lane, which emits through
/// `vike_data::LiveDataSink::l2_snapshot` — and `RecorderSink` PERSISTS that verb, as its own
/// `kind=depth` series.
///
/// ⚠ **That last clause used to read "…and `RecorderSink` does not implement that verb, so the
/// trait's default no-op swallows it", and it stopped being true in #995** — the commit that added
/// the impl (`crates/vike-data/src/rec/live_rec.rs`'s `RecorderSink`, gated by
/// `crates/vike-data/tests/hist_datafusion.rs`) edited this very paragraph without editing that
/// sentence. It is called out rather than quietly replaced because the stale sentence went on to be
/// used as EVIDENCE: a later change reasoned "so a depth socket is never opened by this daemon" from
/// it, and left `crates/vike-bridge-core/src/depth.rs`'s `connect_depth` dialing unbounded inside the
/// recorder's own teardown budget. A wrong doc is load-bearing right up until somebody trusts it.
/// (This paragraph was the module doc of `vike-recorder`'s binance venue module until decision 0092
/// moved the recording table here; it moved with the row it describes.)
///
/// So a Binance recording is the **trade tape plus a conflated depth series**, and that is worth
/// stating plainly because it interacts with `vike_backfill::caps`: no venue-direct backfill serves
/// `trade` or `book` either. Combined:
///
/// | binance | backfill | record live |
/// |---|---|---|
/// | `bar` | ✓ (klines) | — (derived; the recorder does not store bars) |
/// | `trade` | ✗ | **✓ — this row is the only path** |
/// | `quote` | ✗ | ✗ (venue serves none) |
/// | `depth` | ✗ | **✓ — the conflating lane, under its own kind** |
/// | `book` | ✗ (archive rights-blocked) | ✗ (venue serves no lossless lane) |
///
/// ⚠ **And for forty days the depth column above was true in NAME only.** From 2026-08-01 to
/// 2026-09-10 the `kind=depth/venue=binance/symbol=BTCUSDT.P` series recorded **0.41–0.43
/// updates/s** against the ~10/s a `@depth@100ms` stream carries — the shared decoder
/// (`crates/bridges/binance/src/family/depth.rs`'s `apply_depth_event`) enforced binance's SPOT
/// contiguity rule on a USDⓈ-M FUTURES stream, so the second diff of every session read as a gap
/// and the driver re-seeded on its 3 s backoff forever. Two rows per ~4.3 s cycle, both full
/// snapshots, nothing incremental. The rows that ARE there are honest snapshots; roughly 32 million
/// book states between them are not recoverable — `crates/vike-backfill/src/caps.rs`'s
/// `PLANNABLE_KINDS` omits depth and no vendor sells binance L2, so this kind can only ever be
/// re-recorded LIVE. **The catalog surfaces still advertise that range as complete and will keep
/// doing so**: coverage is computed from rows present, and rows were present.
///
/// **A LOSSLESS Binance L2 book is still not obtainable in this workspace by any means**, and
/// `kind=depth` is deliberately not a rename of one: a full snapshot every 100 ms with every
/// intermediate state discarded is not a lossless book, and writing it as `kind=book` would let a
/// maker-fill backtest run on it and report fills it could never have got. THE PATH IS THE
/// DISCLOSURE.
///
/// No special-casing was needed for any of this. `vike_recorder::session::SubscriptionSet` asks for
/// each stream once, learns `Unsupported` as a permanent capability answer, and never asks again —
/// so a Binance feed subscribes trades and depth, is told twice that quotes and book do not exist,
/// and records the two lanes it got.
///
/// `book_carries_quotes = false`: binance serves no book lane, so its book never carries quotes.
#[cfg(feature = "record-binance")]
fn binance_feed(sub: &Subscription, broker: &Arc<FeedBroker>) -> Result<AssembledFeed, String> {
    let symbols = resolver(sub, |family| {
        Ok(Box::new(vike_recorder::resolve::CatalogGlob::new(
            family,
            Box::new(vike_binance::catalog::BinanceCatalog),
        )))
    })?;
    let client = Box::new(broker.handle(crate::feeds::Holder::Rec, "binance"));
    Ok(AssembledFeed::new("binance", false, symbols, client))
}

/// The Polymarket row — the rotating-family case, and the reason families exist at all.
///
/// A Polymarket up/down market lives **5 minutes**. Each window is a different market with a
/// different `condition_id` and different outcome token ids, so a customer cannot subscribe by token
/// id: the ids they picked would be dead before the next flush. They name the FAMILY, and
/// `vike_polymarket::rolling_family::RollingFamily` resolves it to live tokens on every tick — over
/// the real Gamma directory, or an explicit token list. The broker's handle opens no socket, so this
/// cannot fail on the network; it fails only on a family name with no `-<N>m`.
///
/// **`Quotes` is dropped when `Book` is requested** (`crates/vike-recorder/src/assembled.rs`'s
/// `narrow`, armed by the `true` this row passes as `book_carries_quotes`). On this venue
/// `subscribe_book` already emits derived L1 (`PumpMode::Book` → `sink.book()` **and**
/// `sink.quote()`, per `crates/bridges/polymarket/src/market_feed.rs`'s module doc), so
/// subscribing both opens a second socket deriving the same L1 from its own copy of the book and
/// writes every quote row twice.
///
/// ⚠ Polymarket is `crate::feeds::Sharing::PerHolder` (`crates/vike-datahub/src/feeds/venues.rs`'s
/// `EXPLICIT_SHARING`): its `unsubscribe` resubscribes a shard's co-tenant tokens, so the recorder
/// gets its own client, built with the recorder's own sink, and an md release can never punch a
/// gap into a recorded book.
#[cfg(feature = "record-polymarket")]
fn polymarket_feed(sub: &Subscription, broker: &Arc<FeedBroker>) -> Result<AssembledFeed, String> {
    let symbols = resolver(sub, |family| {
        Ok(Box::new(vike_polymarket::rolling_family::RollingFamily::new(family)?))
    })?;
    let client = Box::new(broker.handle(crate::feeds::Holder::Rec, "polymarket"));
    Ok(AssembledFeed::new("polymarket", true, symbols, client))
}

#[cfg(not(all(feature = "record-binance", feature = "record-polymarket")))]
fn missing_feature(venue: &str, feature: &str) -> String {
    format!(
        "recorder: venue `{venue}` is not compiled into this build — rebuild with \
         `--features {feature}`. (It is a Cargo feature because the bridge is heavy: it pulls the \
         venue's transport tree into this binary.)"
    )
}

// ⚠ INLINE, deliberately, where the plan drew a `recording_tests.rs` beside this file — the
// `crates/vike-datahub/src/feeds/venues.rs` precedent: the code-layout rule moves a test block out
// only for a file >= 400 lines carrying >= 150 test lines, and this one is neither. A new
// `#[cfg(test)] mod NAME;` file would also widen the set
// `crates/vike-ops/tests/compile_time_path_gate.rs` skips wholesale.
//
// These are `vike-recorder`'s former `venues` tests, moved with the table. They run only where
// this module compiles — a `record*` build, i.e. the `recorder-venues` lane's
// `-p vike-datahub --features record-polymarket,record-binance`.
#[cfg(test)]
mod tests {
    use super::*;

    use vike_recorder::config::Backfill;

    fn sub(venue: &str, family: Option<&str>) -> Subscription {
        Subscription {
            venue: venue.into(),
            family: family.map(str::to_string),
            symbols: Vec::new(),
            backfill: Backfill::Off,
        }
    }

    /// The broker on the real client table — the production wiring, minus the process.
    fn broker() -> Arc<FeedBroker> {
        FeedBroker::new(
            crate::feeds::venues::real_client_table(std::collections::BTreeMap::new()),
            crate::feeds::venues::sharing_for,
        )
    }

    /// `build_recording_feed` returns an `AssembledFeed`, which has no `Debug`, so `unwrap_err`
    /// cannot be used on it.
    fn err_of(r: Result<AssembledFeed, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    /// An unknown venue must FAIL at startup, not be skipped. A recorder that skipped it would
    /// report itself healthy while recording nothing for that venue — the exact silent-nothing
    /// failure `RecorderProfile::from_toml`'s validation exists to prevent one layer up.
    #[test]
    fn an_unsupported_venue_is_a_startup_error_naming_what_is_supported() {
        let err = err_of(build_recording_feed(&sub("kalshi", Some("x-5m")), &broker()));
        assert!(err.contains("kalshi"), "{err}");
        assert!(err.contains("Supported"), "{err}");
    }

    /// A venue that EXISTS but was compiled out gets a different, actionable message: the feature to
    /// rebuild with, rather than "unknown venue", which would send someone hunting for a typo.
    #[cfg(not(feature = "record-polymarket"))]
    #[test]
    fn a_compiled_out_venue_names_the_feature() {
        let err =
            err_of(build_recording_feed(&sub("polymarket", Some("btc-updown-5m")), &broker()));
        assert!(err.contains("--features record-polymarket"), "{err}");
    }

    /// The polymarket row: a rolling family, and a book that carries quotes.
    #[cfg(feature = "record-polymarket")]
    #[test]
    fn the_polymarket_row_rolls_and_its_book_carries_quotes() {
        use vike_recorder::{Stream, VenueFeed};

        let f = build_recording_feed(&sub("polymarket", Some("btc-updown-5m")), &broker()).unwrap();
        assert_eq!(f.venue(), "polymarket");
        assert_eq!(f.family(), Some("btc-updown-5m"));
        assert!(
            !f.narrow(&Stream::ALL).contains(&Stream::Quotes),
            "the book pump derives L1, so Quotes beside Book would write every quote row twice"
        );
        // CONTAINS, not equals: `supported()` grows with each venue feature, and this build may have
        // several on. An equality assertion here made adding the second venue fail a test about the
        // first one.
        assert!(supported().contains(&"polymarket"), "{:?}", supported());
    }

    /// The binance row — the STATIC-family venue, built through the same dispatch.
    ///
    /// ⚠ **The `family()` assertion INVERTED on 2026-09-19 and that is the point of the test now.**
    /// It read `Some("*USDT.P")` — the operator's glob, verbatim, straight out to the store as a
    /// `group=*USDT.P` directory. `*` may not be in a directory name on Windows, so that series
    /// could be written on Linux and never opened. The glob stays where the MATCHING happens
    /// (`vike_recorder::resolve::CatalogGlob`'s `pattern`, which this test cannot reach and
    /// `crates/vike-recorder/src/resolve_tests.rs` covers); what `family()` answers is the GROUP
    /// NAME, and a group name has to be a directory.
    #[cfg(feature = "record-binance")]
    #[test]
    fn the_binance_row_is_a_static_family_with_a_path_safe_group() {
        use vike_recorder::{Stream, VenueFeed};

        let f = build_recording_feed(&sub("binance", Some("*USDT.P")), &broker()).unwrap();
        assert_eq!(f.venue(), "binance");
        assert_eq!(
            f.family(),
            Some("USDT.P"),
            "the group NAME is rendered path-safe; the glob is kept on the match pattern"
        );
        assert_eq!(
            f.narrow(&Stream::ALL),
            Stream::ALL.to_vec(),
            "binance's book carries no quotes"
        );
        assert!(supported().contains(&"binance"), "{:?}", supported());
    }

    /// Every venue this build claims it can RECORD must be a real `vike_model::VENUES` slug —
    /// otherwise the `rec_venue=` advertisement names something no operator could ever write into a
    /// subscription row, and a CLI refusing an unadvertised venue would refuse the correct spelling
    /// while accepting nothing.
    ///
    /// The exact twin of `crates/vike-datahub/src/md/venues.rs`'s
    /// `every_supported_venue_is_a_roster_slug`, which holds the same property for the LIVE plane's
    /// `md_venue=` entries. Two advertisements, two rosters, one rule — and the rule has to be
    /// stated on each side, because neither `supported()` can see the other.
    ///
    /// ⚠ It is VACUOUS on a `record` build with no venue feature, exactly as the md twin is on a
    /// build with no `live-<venue>`. The `recorder-venues` lane's
    /// `-p vike-datahub --features record-polymarket,record-binance` is where it has something to
    /// assert.
    #[test]
    fn every_supported_venue_is_a_roster_slug() {
        for v in supported() {
            assert!(
                vike_model::VENUES.contains(&v),
                "`{v}` is not in vike_model::VENUES, so the `rec_venue={v}` this build advertises \
                 names a venue no subscription row could legally carry"
            );
        }
    }

    /// A profile naming BOTH venues is the real multi-venue case, and the one whose `#[cfg]` shape
    /// differs from either single-venue build.
    #[cfg(all(feature = "record-binance", feature = "record-polymarket"))]
    #[test]
    fn both_venues_dispatch_in_one_build() {
        use vike_recorder::VenueFeed;

        assert_eq!(supported(), vec!["binance", "polymarket"]);
        let b = broker();
        assert_eq!(
            build_recording_feed(&sub("polymarket", Some("btc-updown-5m")), &b).unwrap().venue(),
            "polymarket"
        );
        assert_eq!(
            build_recording_feed(&sub("binance", Some("*USDT")), &b).unwrap().venue(),
            "binance"
        );
    }
}
