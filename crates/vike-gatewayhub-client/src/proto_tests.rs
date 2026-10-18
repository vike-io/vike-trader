//! Unit tests of the wire's tables and helpers: the scope/surface/reply classification,
//! paging and reassembly, the feature pairs and the client's pure timing functions. The wire's
//! BYTES are pinned elsewhere (`tests/wire_tag_fixtures.rs`, over committed fixtures; the stream half is the tube's `wire_tests.rs`).

use std::time::Duration;

use vike_catalog::{CatalogMode, Tab};
use vike_data::source::{FundingRatePoint, SourceError};
use vike_hub_proto::auth::{self, DATAHUB_DOMAIN, Scope};
use vike_market_stream::opener::stream_deadline;
use vike_marketdata::{Bar, QuoteTick};

use super::*;
use crate::client::{stall_bound, stated_heartbeat};

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// One of every request variant, so a classification test cannot miss a verb: the `match` in
/// `all_requests_are_listed` fails to COMPILE when a variant is added without being listed here.
fn all_requests() -> Vec<Request> {
    let what = FetchWhat::CatalogList { venue: "binance".into() };
    vec![
        Request::Hello { proto_version: 1 },
        Request::Auth { scope: Scope::Read, mac: vec![1] },
        Request::Ping,
        Request::DataOpen { venue: "binance".into(), network: None },
        Request::OptionsFeedOpen { feed: OptionsFeedSpec::MarkPrice { indexes: vec![] } },
        Request::Fetch { rid: 1, what: what.clone() },
        Request::FetchCredentialed { rid: 1, what },
        Request::FetchCancel { rid: 1 },
        Request::CatalogDescribe { venue: "binance".into() },
        Request::OptionsRest { call: OptionsCall::Underlyings },
        Request::ResolveFamily { venue: "polymarket".into(), family: "x-5m".into(), now_ms: 0 },
        Request::CredentialPresence { venue: "oanda".into() },
        Request::GwStatus,
        Request::ArmingQuery,
        Request::PreflightQuery {
            venues: vec!["binance".into()],
            step: crate::proto::PreflightStep::Batch,
        },
        Request::TradeOpen {
            client_id: "tradehub-1".into(),
            venue: "binance".into(),
            account: None,
            tier: vike_model::AccountTier::Demo,
            resume: None,
            takeover: false,
        },
    ]
}

#[test]
fn all_requests_are_listed() {
    // Exhaustive on purpose: the day a variant is added this stops compiling until it is listed in
    // `all_requests`, which every classification test below iterates.
    for r in all_requests() {
        match r {
            Request::Hello { .. }
            | Request::Auth { .. }
            | Request::Ping
            | Request::DataOpen { .. }
            | Request::OptionsFeedOpen { .. }
            | Request::Fetch { .. }
            | Request::FetchCredentialed { .. }
            | Request::FetchCancel { .. }
            | Request::CatalogDescribe { .. }
            | Request::OptionsRest { .. }
            | Request::ResolveFamily { .. }
            | Request::CredentialPresence { .. }
            | Request::GwStatus
            | Request::ArmingQuery
            | Request::PreflightQuery { .. }
            | Request::TradeOpen { .. } => {}
        }
    }
    assert_eq!(all_requests().len(), 16);
}

#[test]
fn request_kinds_are_the_variant_names_and_distinct() {
    let kinds: Vec<&str> = all_requests().iter().map(request_kind).collect();
    let mut sorted = kinds.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), kinds.len(), "two verbs share a name: {kinds:?}");
    for (request, kind) in all_requests().iter().zip(&kinds) {
        let json = serde_json::to_value(request).expect("a request serializes");
        let tag = match &json {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Object(o) => o.keys().next().expect("one tag").clone(),
            other => panic!("unexpected request shape {other}"),
        };
        assert_eq!(&tag, kind, "request_kind must be the WIRE tag");
    }
}

/// The scope table, pinned verb by verb: the cheap verbs are Read, the credentialed fetch is Write (and so
/// is a keyless fetch of a client-named WINDOW: `the_scope_of_a_keyless_fetch_follows_its_lane`).
#[test]
fn the_scope_table_is_pinned_per_verb() {
    for request in all_requests() {
        let expected = match request {
            Request::Hello { .. } | Request::Auth { .. } => VerbScope::Handshake,
            Request::FetchCredentialed { .. } => VerbScope::Write,
            Request::ArmingQuery | Request::PreflightQuery { .. } | Request::TradeOpen { .. } => {
                VerbScope::Trade
            }
            _ => VerbScope::Read,
        };
        assert_eq!(required_scope(&request), expected, "{}", request_kind(&request));
    }
}

/// One of every fetch lane. The `match` fails to COMPILE when a lane is added without being listed.
fn every_fetch_what() -> Vec<FetchWhat> {
    let what = vec![
        FetchWhat::Klines {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start_ms: 0,
            end_ms: i64::MAX,
        },
        FetchWhat::Funding {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start_ms: 0,
            end_ms: i64::MAX,
        },
        FetchWhat::Ticks {
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            start_ms: 0,
            end_ms: i64::MAX,
        },
        FetchWhat::CatalogList { venue: "binance".into() },
        FetchWhat::CatalogSearch {
            venue: "binance".into(),
            query: "btc".into(),
            tab: None,
            filter_venue: None,
        },
        FetchWhat::OptionIndex { kind: OptionIndexKind::Option },
    ];
    for w in &what {
        match w {
            FetchWhat::Klines { .. }
            | FetchWhat::Funding { .. }
            | FetchWhat::Ticks { .. }
            | FetchWhat::CatalogList { .. }
            | FetchWhat::CatalogSearch { .. }
            | FetchWhat::OptionIndex { .. } => {}
        }
    }
    what
}

/// **A keyless fetch of a client-named WINDOW needs the Write scope**, as the datahub's `Backfill` did;
/// the catalog and option-index lanes, whose cost is the venue's own universe, stay Read. The table is
/// per lane, not per verb: `Request::Fetch` is one variant.
#[test]
fn the_scope_of_a_keyless_fetch_follows_its_lane() {
    for what in every_fetch_what() {
        let expected = match what {
            FetchWhat::Klines { .. } | FetchWhat::Funding { .. } | FetchWhat::Ticks { .. } => {
                VerbScope::Write
            }
            FetchWhat::CatalogList { .. }
            | FetchWhat::CatalogSearch { .. }
            | FetchWhat::OptionIndex { .. } => VerbScope::Read,
        };
        let lane = what.lane();
        let fetch = Request::Fetch { rid: 1, what: what.clone() };
        assert_eq!(required_scope(&fetch), expected, "Fetch/{lane}");
        // Only the credentialed klines lane exists, and it is Write whatever the lane says.
        assert_eq!(
            required_scope(&Request::FetchCredentialed { rid: 1, what }),
            VerbScope::Write,
            "FetchCredentialed/{lane}"
        );
        // A Read connection is refused the window lanes and admitted the cheap ones, a Write one both.
        assert_eq!(scope_admits(Scope::Read, required_scope(&fetch)), expected == VerbScope::Read);
        assert!(scope_admits(Scope::Write, required_scope(&fetch)), "Fetch/{lane}");
    }
}

#[test]
fn a_read_connection_cannot_send_the_credentialed_fetch_and_account_admits_nothing() {
    assert!(scope_admits(Scope::Read, VerbScope::Read));
    assert!(scope_admits(Scope::Write, VerbScope::Read));
    assert!(!scope_admits(Scope::Read, VerbScope::Write));
    assert!(scope_admits(Scope::Write, VerbScope::Write));
    for needed in [VerbScope::Handshake, VerbScope::Read, VerbScope::Write, VerbScope::Trade] {
        assert!(!scope_admits(Scope::Account, needed), "Account is not a superset of anything");
    }
    assert!(!scope_admits(Scope::Write, VerbScope::Handshake), "a second Hello/Auth is a fault");
}

/// `Scope::Trade` is not a superset, in both directions: it admits the trade surface and nothing
/// else, and no other scope admits the trade surface.
#[test]
fn the_trade_scope_admits_only_the_trade_surface_and_nothing_admits_it_but_trade() {
    assert!(scope_admits(Scope::Trade, VerbScope::Trade));
    for needed in [VerbScope::Handshake, VerbScope::Read, VerbScope::Write] {
        assert!(!scope_admits(Scope::Trade, needed), "Trade is not a superset ({needed:?})");
    }
    for authed in [Scope::Read, Scope::Write, Scope::Account] {
        assert!(
            !scope_admits(authed, VerbScope::Trade),
            "{authed:?} must not admit the trade surface"
        );
    }
}

#[test]
fn the_surface_table_is_pinned_per_verb() {
    for request in all_requests() {
        let expected = match request {
            Request::Hello { .. } | Request::Auth { .. } => Surface::Handshake,
            Request::DataOpen { .. } => Surface::Stream,
            Request::OptionsFeedOpen { .. } => Surface::Feed,
            Request::TradeOpen { .. } => Surface::Trade,
            _ => Surface::Rpc,
        };
        assert_eq!(surface_of(&request), expected, "{}", request_kind(&request));
    }
}

#[test]
fn the_reply_class_table_is_pinned_per_verb() {
    for request in all_requests() {
        let expected = match request {
            Request::Hello { .. } | Request::Auth { .. } => ReplyWait::Handshake,
            Request::Fetch { .. } | Request::FetchCredentialed { .. } => ReplyWait::Progress,
            // The two that call a venue's REST endpoint keep the long bound…
            Request::OptionsRest { .. }
            | Request::ResolveFamily { .. }
            | Request::PreflightQuery { .. } => ReplyWait::Bounded,
            // …every pure lookup gets the short one.
            _ => ReplyWait::Lookup,
        };
        assert_eq!(reply_wait(&request), expected, "{}", request_kind(&request));
    }
}

/// A mac minted for the gateway must not verify at the datahub, even with the same key bytes.
#[test]
fn the_domain_separator_is_disjoint_from_the_datahubs() {
    let key = b"one key reused by mistake";
    let nonce = [7u8; 32];
    let mac = auth::sign(GATEWAYHUB_DOMAIN, key, &nonce, GATEWAYHUB_PROTO_VERSION, Scope::Write);
    assert!(auth::verify(
        GATEWAYHUB_DOMAIN,
        key,
        &nonce,
        GATEWAYHUB_PROTO_VERSION,
        Scope::Write,
        &mac
    ));
    assert!(
        !auth::verify(DATAHUB_DOMAIN, key, &nonce, GATEWAYHUB_PROTO_VERSION, Scope::Write, &mac),
        "a gateway mac replayed at the datahub verified"
    );
    assert!(
        GATEWAYHUB_DOMAIN.as_bytes().ends_with(&[0]),
        "NUL-terminated, so no domain is a prefix"
    );
    assert_ne!(GATEWAYHUB_DOMAIN, DATAHUB_DOMAIN);
}

#[test]
fn the_tab_and_mode_maps_are_inverse() {
    for tab in [
        Tab::Stocks,
        Tab::Forex,
        Tab::Crypto,
        Tab::Perps,
        Tab::Options,
        Tab::Futures,
        Tab::Indices,
        Tab::Prediction,
    ] {
        assert_eq!(WireTab::from_tab(tab).tab(), tab);
    }
    for mode in [CatalogMode::Enumerable, CatalogMode::QueryBacked] {
        assert_eq!(WireCatalogMode::from_mode(mode).mode(), mode);
    }
}

#[test]
fn a_funding_point_crosses_without_losing_a_bit() {
    let p = FundingRatePoint {
        ts_ms: 1_700_000_000_000,
        rate: -0.000_123_456_789_012_3,
        premium: Some(0.1 + 0.2),
    };
    let json = serde_json::to_string(&WireFundingPoint::from(p)).unwrap();
    let back: FundingRatePoint = serde_json::from_str::<WireFundingPoint>(&json).unwrap().into();
    assert_eq!(back.ts_ms, p.ts_ms);
    assert_eq!(back.rate.to_bits(), p.rate.to_bits());
    assert_eq!(back.premium.map(f64::to_bits), p.premium.map(f64::to_bits));
    let none = FundingRatePoint { premium: None, ..p };
    assert_eq!(FundingRatePoint::from(WireFundingPoint::from(none)).premium, None);
}

#[test]
fn a_fetch_failure_keeps_its_variant_and_text() {
    for error in [SourceError::Fetch("venue fetch: 429".into()), SourceError::Refused("no".into())]
    {
        let back = FetchFailure::from_source_error(error.clone()).into_source_error();
        assert_eq!(back, error);
    }
    assert_eq!(FetchFailure::Fetch("a".into()).text(), "a");
    assert_eq!(FetchFailure::Refused("b".into()).text(), "b");
}

#[test]
fn the_venue_feature_pairs_read_back_what_they_write_and_do_not_shadow_each_other() {
    let features = vec![
        md_venue_feature("binance"),
        cd_venue_feature("oanda"),
        md_venue_feature("polymarket"),
        cs_venue_feature("alpaca"),
        "cs_venue=".to_string(),
        "md_venue=".to_string(),
        "md_venue=  ".to_string(),
        FEATURE_GW_DATA.to_string(),
    ];
    assert_eq!(advertised_md_venues(&features), ["binance", "polymarket"]);
    assert_eq!(advertised_cd_venues(&features), ["oanda"]);
    assert_eq!(advertised_cs_venues(&features), ["alpaca"], "cs_venue= reads its own prefix only");
    assert!(welcome_is_gatewayhub(&[GATEWAYHUB_PLANE_SENTINEL.to_string()]));
    assert!(!welcome_is_gatewayhub(&features));
    let msg = plane_mismatch_message("127.0.0.1:1", &features);
    assert!(msg.contains("127.0.0.1:1") && msg.contains(GATEWAYHUB_PLANE_SENTINEL), "{msg}");
}

/// `DataOpen`'s `network` is absent from the frame when none is named, so every old peer's bytes are
/// unchanged and an old frame reads as "no network"; a named network crosses as itself.
#[test]
fn a_data_open_without_a_network_has_the_bytes_it_always_had() {
    let none = serde_json::to_string(&Request::DataOpen { venue: "binance".into(), network: None });
    assert_eq!(none.unwrap(), r#"{"DataOpen":{"venue":"binance"}}"#);
    let named = Request::DataOpen {
        venue: "hyperliquid".into(),
        network: Some(DATA_NETWORK_TESTNET.into()),
    };
    let json = serde_json::to_string(&named).unwrap();
    assert_eq!(json, r#"{"DataOpen":{"venue":"hyperliquid","network":"testnet"}}"#);
    let old: Request = serde_json::from_str(r#"{"DataOpen":{"venue":"okx"}}"#).unwrap();
    assert!(matches!(old, Request::DataOpen { network: None, .. }));
    assert_eq!((DATA_NETWORK_MAINNET, DATA_NETWORK_TESTNET), ("mainnet", "testnet"));
}

/// The `trade_venue=` pair is a third set beside `md_venue=` and `cd_venue=`, read through ONE prefix.
#[test]
fn the_trade_venue_pair_reads_back_what_it_writes_and_shadows_nothing() {
    let features = vec![
        trade_venue_feature("binance"),
        md_venue_feature("binance"),
        cd_venue_feature("oanda"),
        trade_venue_feature("oanda"),
        "trade_venue=".to_string(),
        FEATURE_GW_TRADE.to_string(),
    ];
    assert_eq!(advertised_trade_venues(&features), ["binance", "oanda"]);
    assert_eq!(advertised_md_venues(&features), ["binance"]);
    assert_eq!(advertised_cd_venues(&features), ["oanda"]);
}

/// The trade strings are disjoint from the data vocabulary: a data build must never advertise one by
/// accident, and a trade daemon appends exactly these.
#[test]
fn the_trade_vocabulary_is_disjoint_from_the_data_vocabulary() {
    for f in KNOWN_TRADE_FEATURES {
        assert!(!KNOWN_FEATURES.contains(&f), "{f} is in both vocabularies");
    }
    assert_eq!(KNOWN_TRADE_FEATURES.len(), 10);
}

#[test]
fn every_fetch_lane_names_its_feature() {
    let (v, s) = (String::from("v"), String::from("s"));
    let lanes = [
        (
            FetchWhat::Klines {
                venue: v.clone(),
                symbol: s.clone(),
                interval: "1m".into(),
                start_ms: 0,
                end_ms: 1,
            },
            FEATURE_GW_FETCH,
            "klines",
        ),
        (
            FetchWhat::Funding { venue: v.clone(), symbol: s.clone(), start_ms: 0, end_ms: 1 },
            FEATURE_GW_FETCH,
            "funding",
        ),
        (
            FetchWhat::Ticks { venue: "dukascopy".into(), symbol: s, start_ms: 0, end_ms: 1 },
            FEATURE_GW_TICKS,
            "ticks",
        ),
        (FetchWhat::CatalogList { venue: v.clone() }, FEATURE_GW_CATALOG, "catalog_list"),
        (
            FetchWhat::CatalogSearch { venue: v, query: "q".into(), tab: None, filter_venue: None },
            FEATURE_GW_CATALOG,
            "catalog_search",
        ),
        (
            FetchWhat::OptionIndex { kind: OptionIndexKind::Option },
            FEATURE_GW_OPTIONS,
            "option_index",
        ),
    ];
    for (what, feature, lane) in lanes {
        assert_eq!(what.feature(), feature);
        assert_eq!(what.lane(), lane);
    }
}

// ---- Paging and reassembly -----------------------------------------------------------------------

#[test]
fn pages_of_splits_by_payload_and_loses_no_row() {
    let rows: Vec<Bar> = (0..500).map(|i| bar(i, 100.0 + i as f64)).collect();
    let one = serde_json::to_vec(&rows[0]).unwrap().len();
    // A ceiling that fits about 40 rows beside the envelope allowance.
    let pages: Vec<Vec<Bar>> = pages_of(rows.clone(), 256 + 40 * (one + 1)).collect();
    assert!(pages.len() > 10, "500 rows at ~40 a page is a dozen pages, got {}", pages.len());
    assert!(pages.iter().all(|p| !p.is_empty()));
    let flat: Vec<Bar> = pages.into_iter().flatten().collect();
    assert_eq!(flat, rows, "order and content survive the split");
}

#[test]
fn a_row_larger_than_the_ceiling_is_its_own_page_and_nothing_is_no_pages() {
    let big = vec![bar(1, 1.0), bar(2, 2.0), bar(3, 3.0)];
    let pages: Vec<Vec<Bar>> = pages_of(big, 1).collect();
    assert_eq!(pages.len(), 3, "one row per page when the ceiling is smaller than a row");
    assert_eq!(pages_of(Vec::<Bar>::new(), FETCH_PAGE_CEILING_BYTES).count(), 0);
}

/// The splitter this crate shipped before the pages were made lazily, copied verbatim as the REFERENCE:
/// the wire's page boundaries are the daemon's contract with every client reassembling them, and the
/// lazy pager must draw exactly the lines this one drew.
fn eager_pages<T: serde::Serialize>(rows: Vec<T>, ceiling_bytes: usize) -> Vec<Vec<T>> {
    const PAGE_ENVELOPE_BYTES: usize = 256;
    let mut pages: Vec<Vec<T>> = Vec::new();
    let mut page: Vec<T> = Vec::new();
    let mut bytes = PAGE_ENVELOPE_BYTES;
    for row in rows {
        let n = serde_json::to_vec(&row).unwrap().len() + 1;
        if !page.is_empty() && bytes + n > ceiling_bytes {
            pages.push(std::mem::take(&mut page));
            bytes = PAGE_ENVELOPE_BYTES;
        }
        bytes += n;
        page.push(row);
    }
    if !page.is_empty() {
        pages.push(page);
    }
    pages
}

/// **The lazy pager draws the eager splitter's boundaries**, row for row, over rows of every size
/// (a symbol of 0 to 40 bytes makes the JSON size of a quote vary) and ceilings from smaller than one
/// row to larger than the whole answer.
#[test]
fn the_lazy_pager_draws_the_eager_splits_boundaries() {
    let quotes: Vec<QuoteTick> = (0..700)
        .map(|i: i64| QuoteTick {
            ts: i,
            local_ts: i * 3,
            bid: 1.0 + (i as f64) * 0.013,
            ask: 1.5 + (i as f64) * 0.017,
            bid_size: 3.0,
            ask_size: 4.0,
            symbol: "S".repeat(usize::try_from(i * 7 % 41).unwrap()),
        })
        .collect();
    for ceiling in [0, 1, 255, 256, 300, 400, 1_000, 4_096, 20_000, 1_000_000] {
        let lazy: Vec<Vec<QuoteTick>> = pages_of(quotes.clone(), ceiling).collect();
        let eager = eager_pages(quotes.clone(), ceiling);
        assert_eq!(lazy.len(), eager.len(), "ceiling {ceiling}: the page COUNT differs");
        for (n, (l, e)) in lazy.iter().zip(&eager).enumerate() {
            assert_eq!(l, e, "ceiling {ceiling}: page {n} differs");
        }
    }
    // The same through the typed wrapper the daemon pulls: one `FetchRows` per page, same boundaries.
    let eager = eager_pages(quotes.clone(), 4_096);
    let wrapped: Vec<FetchRows> = FetchRows::Quotes(quotes).into_pages(4_096).collect();
    assert_eq!(wrapped.len(), eager.len());
    for (w, e) in wrapped.into_iter().zip(eager) {
        assert_eq!(w, FetchRows::Quotes(e));
    }
}

thread_local! {
    /// How many times a [`Counted`] row has been serialized on this thread.
    static SERIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A row whose serialization counts itself, so a test can see how much of an answer the pager has
/// looked at.
#[derive(Debug, PartialEq)]
struct Counted(u32);

impl serde::Serialize for Counted {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        SERIALIZED.with(|c| c.set(c.get() + 1));
        s.serialize_u32(self.0)
    }
}

/// **A page is made when it is pulled, not before.** The eager split looked at (and moved) every row of
/// the answer before the first page was written, which is what doubled the daemon's peak for a large
/// answer. Here pulling the first page measures that page's rows plus the one row that did not fit
/// (it opens the next page), and nothing else.
#[test]
fn pages_are_made_one_at_a_time_not_up_front() {
    // Each row is four JSON digits plus a comma: 5 bytes. The envelope is 256, so a ceiling of 306
    // holds exactly ten rows, and the eleventh is the one that does not fit.
    let rows: Vec<Counted> = (1_000..1_100).map(Counted).collect();
    SERIALIZED.with(|c| c.set(0));
    let mut pages = pages_of(rows, 256 + 50);
    assert_eq!(SERIALIZED.with(std::cell::Cell::get), 0, "building the pager looks at no row");

    let first = pages.next().expect("a first page");
    assert_eq!(first.len(), 10);
    assert_eq!(
        SERIALIZED.with(std::cell::Cell::get),
        11,
        "the first page costs its ten rows and the one that did not fit, not the whole answer"
    );
    let second = pages.next().expect("a second page");
    assert_eq!((second[0].0, second.len()), (1_010, 10), "the carried row opens the next page");
    assert_eq!(
        SERIALIZED.with(std::cell::Cell::get),
        21,
        "the carried row is not measured twice: the second page adds ten rows"
    );
    assert_eq!(pages.count(), 8, "the other eighty rows are eight more pages");
}

/// The memory arithmetic of `deploy/vike-gatewayhub.service`'s `MemoryMax=` comment is made of these
/// sizes: a fetch answer is held whole, so a row's size times the rows of the largest window is the
/// buffer. Measured here (the test writes all four to its output) and pinned for the ones the unit's
/// figures are built on.
#[cfg(target_pointer_width = "64")]
#[test]
fn the_row_sizes_the_unit_memory_arithmetic_uses() {
    println!(
        "size_of: Bar {} B, QuoteTick {} B, WireFundingPoint {} B, Instrument {} B",
        size_of::<Bar>(),
        size_of::<QuoteTick>(),
        size_of::<WireFundingPoint>(),
        size_of::<vike_model::Instrument>()
    );
    assert_eq!(
        size_of::<Bar>(),
        120,
        "deploy/vike-gatewayhub.service sizes MemoryMax from 120 B per bar: re-derive it if a Bar changes"
    );
    assert_eq!(size_of::<WireFundingPoint>(), 32);
    // A quote is its struct plus the heap bytes of its symbol (a venue code is a handful of bytes).
    assert_eq!(size_of::<QuoteTick>(), 72);
}

/// A payload above the page ceiling splits and reassembles identically (drift register R9), through the
/// real `FetchRows` page type and the assembler the client uses.
#[test]
fn a_payload_above_the_ceiling_reassembles_identically() {
    let rows: Vec<QuoteTick> = (0..2_000)
        .map(|i| QuoteTick {
            ts: i,
            local_ts: i + 1,
            bid: 1.0 + (i as f64) * 0.1,
            ask: 1.5 + (i as f64) * 0.1,
            bid_size: 3.0,
            ask_size: 4.0,
            symbol: "EURUSD".into(),
        })
        .collect();
    let pages: Vec<FetchRows> = FetchRows::Quotes(rows.clone()).into_pages(8 * 1024).collect();
    assert!(pages.len() > 4);
    let mut asm = FetchAssembler::new(9);
    for (n, page) in pages.into_iter().enumerate() {
        asm.push(9, u32::try_from(n).unwrap(), page).unwrap();
    }
    let (p, r) = (asm.pages(), asm.rows() as u64);
    assert_eq!(asm.finish_quotes(p, r).unwrap(), rows);
}

#[test]
fn the_assembler_refuses_a_lost_page_a_foreign_page_a_kind_change_and_a_short_total() {
    let page = |n: i64| FetchRows::Bars(vec![bar(n, 1.0)]);
    let mut asm = FetchAssembler::new(1);
    asm.push(1, 0, page(0)).unwrap();
    assert!(asm.push(1, 2, page(2)).unwrap_err().contains("lost or repeated"));
    assert!(asm.push(1, 0, page(0)).unwrap_err().contains("lost or repeated"));
    assert!(asm.push(2, 1, page(1)).unwrap_err().contains("one fetch at a time"));
    assert!(asm.push(1, 1, FetchRows::Quotes(vec![])).unwrap_err().contains("Quotes"));
    asm.push(1, 1, page(1)).unwrap();
    assert!(
        FetchAssembler::new(1).finish_bars(1, 1).is_err(),
        "no page arrived but the server said one did"
    );
    let (p, r) = (asm.pages(), asm.rows() as u64);
    assert_eq!((p, r), (2, 2));
    assert!(
        asm.finish_bars(2, 3).unwrap_err().contains("lost on the way"),
        "a short total is an error"
    );
}

#[test]
fn zero_pages_is_a_legitimate_empty_result_and_only_when_the_server_said_so() {
    assert_eq!(FetchAssembler::new(5).finish_bars(0, 0).unwrap(), Vec::<Bar>::new());
    assert!(FetchAssembler::new(5).finish_instruments(0, 1).is_err());
}

// ---- Client timing functions ---------------------------------------------------------------------

#[test]
fn the_stall_bound_is_the_floor_or_three_stated_periods() {
    let floor = Duration::from_secs(30);
    assert_eq!(stall_bound(floor, 5_000), floor, "three 5 s periods is under the floor");
    assert_eq!(stall_bound(floor, 20_000), Duration::from_secs(60), "a slower daemon is believed");
    assert_eq!(
        stall_bound(floor, u64::MAX),
        3 * FETCH_NEXT_IN_CEILING,
        "an absurd stated cadence is believed only up to the ceiling, and never panics"
    );
    assert_eq!(
        stall_bound(floor, FETCH_NEXT_IN_CEILING.as_millis() as u64),
        3 * FETCH_NEXT_IN_CEILING,
        "the ceiling itself is believed"
    );
}

#[test]
fn a_stated_heartbeat_is_clamped_into_the_protocol_range() {
    assert_eq!(stated_heartbeat(0), GW_STREAM_HEARTBEAT_MIN);
    assert_eq!(stated_heartbeat(15_000), GW_STREAM_HEARTBEAT);
    assert_eq!(stated_heartbeat(86_400_000), GW_STREAM_HEARTBEAT_MAX);
}

#[test]
fn the_stream_deadline_follows_the_stated_period_over_the_floor() {
    assert_eq!(stream_deadline(GW_STREAM_READ_FLOOR, GW_STREAM_HEARTBEAT), GW_STREAM_READ_FLOOR);
    assert_eq!(
        stream_deadline(GW_STREAM_READ_FLOOR, Duration::from_secs(60)),
        Duration::from_secs(180)
    );
    assert_eq!(stream_deadline(GW_STREAM_READ_FLOOR, Duration::from_secs(1)), GW_STREAM_READ_FLOOR);
}

// ---- Features and the options focus cap ------------------------------------------------------------

/// `KNOWN_FEATURES` is complete against `proto/features.rs`: every `pub const FEATURE_GW_*` and the
/// plane sentinel is in it (the prefixes `md_venue=` / `cd_venue=` are not capabilities).
#[test]
fn every_fixed_feature_is_known() {
    let src = include_str!("proto/features.rs");
    let mut declared = vec!["gatewayhub".to_string()];
    for line in src.lines() {
        let Some(rest) = line.strip_prefix("pub const FEATURE_GW_") else { continue };
        let Some(i) = rest.find("&str = \"") else { continue };
        let value = &rest[i + "&str = \"".len()..];
        declared.push(value[..value.find('"').unwrap()].to_string());
    }
    // The DATA vocabulary and the TRADE vocabulary are two lists (the daemon's own test holds a full
    // data build equal to the first); together they are every `FEATURE_GW_*` constant.
    let mut known: Vec<String> = KNOWN_FEATURES
        .iter()
        .chain(KNOWN_TRADE_FEATURES.iter())
        .map(|s| (*s).to_string())
        .collect();
    known.sort();
    declared.sort();
    assert_eq!(
        known, declared,
        "KNOWN_FEATURES + KNOWN_TRADE_FEATURES and the FEATURE_GW_* constants disagree"
    );
    let mut unique = known.clone();
    unique.dedup();
    assert_eq!(unique.len(), known.len(), "a feature string is listed twice");
}

#[test]
fn an_options_feed_spec_names_something_within_its_cap() {
    let names = |n: usize| (0..n).map(|i| format!("n{i}")).collect::<Vec<_>>();
    assert!(OptionsFeedSpec::MarkPrice { indexes: names(3) }.refusal().is_none());
    assert!(
        OptionsFeedSpec::Ticker { instruments: names(GW_OPTIONS_MAX_INSTRUMENTS) }
            .refusal()
            .is_none()
    );
    let empty = OptionsFeedSpec::MarkPrice { indexes: vec![] }.refusal().unwrap();
    assert!(empty.contains("at least one"), "{empty}");
    let big = OptionsFeedSpec::MarkPrice { indexes: names(GW_OPTIONS_MAX_INDEXES + 1) }
        .refusal()
        .unwrap();
    assert!(big.contains("at most"), "{big}");
    let big = OptionsFeedSpec::Ticker { instruments: names(GW_OPTIONS_MAX_INSTRUMENTS + 1) }
        .refusal()
        .unwrap();
    assert!(big.contains(&GW_OPTIONS_MAX_INSTRUMENTS.to_string()), "{big}");
    let blank = OptionsFeedSpec::Ticker { instruments: vec!["BTC-1".into(), "  ".into()] }
        .refusal()
        .unwrap();
    assert!(blank.contains("BLANK"), "{blank}");
}

/// A name is spliced into a venue channel string (`markprice.options.<index>`,
/// `ticker.<instrument>.100ms`), so `refusal` bounds its length and its bytes: a name that is a
/// paragraph, or carries a dot, a space, a slash, a comma or a control byte, cannot reach a channel.
#[test]
fn an_options_feed_name_is_a_short_channel_safe_word() {
    for ok in ["btc_usd", "sol_usdc", "BTC-27JUL26-58000-C", "BTC_USDC-27JUL26-58000-C", "a", "9"] {
        assert!(
            OptionsFeedSpec::MarkPrice { indexes: vec![ok.to_string()] }.refusal().is_none(),
            "{ok}"
        );
        assert!(
            OptionsFeedSpec::Ticker { instruments: vec![ok.to_string()] }.refusal().is_none(),
            "{ok}"
        );
    }
    let at_bound = "x".repeat(GW_OPTIONS_MAX_NAME_BYTES);
    assert!(OptionsFeedSpec::Ticker { instruments: vec![at_bound] }.refusal().is_none());
    let over = "x".repeat(GW_OPTIONS_MAX_NAME_BYTES + 1);
    let text = OptionsFeedSpec::Ticker { instruments: vec!["BTC-1".into(), over] }
        .refusal()
        .expect("a name over the bound is refused");
    assert!(text.contains(&GW_OPTIONS_MAX_NAME_BYTES.to_string()), "{text}");
    for bad in [
        "btc_usd.raw",
        "btc usd",
        "btc/usd",
        "btc_usd,eth_usd",
        "btc_usd\n",
        "btc\u{0}usd",
        "btc_usd*",
        "ürün",
    ] {
        let text = OptionsFeedSpec::MarkPrice { indexes: vec![bad.to_string()] }
            .refusal()
            .unwrap_or_else(|| panic!("{bad:?} was accepted"));
        assert!(text.contains("[A-Za-z0-9_-]"), "{text}");
        assert!(
            OptionsFeedSpec::Ticker { instruments: vec!["BTC-1".into(), bad.to_string()] }
                .refusal()
                .is_some(),
            "{bad:?}"
        );
    }
}

/// Decision 0170's wire leg: a `Live` reply from a daemon that predates `contract_value` (it sent
/// the old scalar `contract_size`) still decodes, with the value ABSENT, and a reply that carries
/// the value round-trips it.
/// **The asked leverage is ADDITIVE on the wire** (decision 0182): a start from a tradehub that predates
/// `account_leverage` decodes with `None` (a newer daemon then does exactly what it always did), a start
/// carrying it round-trips, and a frame with a key this build does not know still decodes (an older
/// daemon ignores `account_leverage`, answers no `leverage_cap`, and the tradehub stays at 1x).
#[test]
fn an_adapter_start_without_an_asked_leverage_decodes_as_none_and_a_carried_one_round_trips() {
    let old = r#"{"rid":1,"symbol":"BTCUSDT","recon_enabled":true,"route_key":"bybit"}"#;
    let start: AdapterStart = serde_json::from_str(old).expect("an old frame still decodes");
    assert_eq!(start.account_leverage, None);

    let newer = r#"{"rid":1,"symbol":"BTCUSDT","recon_enabled":true,"route_key":"bybit",
                    "account_leverage":10.0,"a_field_from_the_future":[1,2]}"#;
    let start: AdapterStart = serde_json::from_str(newer).expect("an unknown key is ignored");
    assert_eq!(start.account_leverage, Some(10.0));

    let text = serde_json::to_string(&start).unwrap();
    assert!(text.contains(r#""account_leverage":10.0"#), "{text}");
    assert_eq!(serde_json::from_str::<AdapterStart>(&text).unwrap(), start);

    // It is part of the start's identity: the daemon compares whole starts to decide "the same one".
    let mut other = start.clone();
    other.account_leverage = Some(4.0);
    assert_ne!(other, start);
    other.account_leverage = None;
    assert_ne!(other, start);
}

#[test]
fn a_live_reply_without_a_contract_value_decodes_as_none_and_a_carried_one_round_trips() {
    let old = r#"{"Live":{"bound_tier":"demo","contract_size":10.0,"recon":true}}"#;
    match serde_json::from_str::<AdapterReply>(old).expect("an old frame still decodes") {
        AdapterReply::Live { contract_value, recon, .. } => {
            assert_eq!(contract_value, None, "the old scalar is ignored, not read as a value");
            assert!(recon);
        }
        other => panic!("{other:?}"),
    }
    let reply = AdapterReply::Live {
        bound_tier: vike_model::AccountTier::Demo,
        grid: None,
        contract_value: Some(vike_model::ContractValue::inverse(1.0)),
        margin_mode: None,
        leg_grids: Vec::new(),
        recon: false,
        leverage_cap: None,
    };
    let text = serde_json::to_string(&reply).unwrap();
    assert!(text.contains(r#""contract_value":{"kind":"inverse","per_unit":1.0}"#), "{text}");
    assert_eq!(serde_json::from_str::<AdapterReply>(&text).unwrap(), reply);
}
