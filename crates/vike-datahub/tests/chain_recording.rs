//! **The `flags.record_chains` recorder, end to end, offline**: the options lane's REAL poll pass
//! (`vike_datahub::options::gateway::fetch_options`, what the `dh-options` thread runs every
//! `POLL_INTERVAL`) over the gatewayhub client the lane builds (`OptionsSourceGateway::remote`), carrying
//! the recorder the mount builds (`chain_recorder`, which
//! `crates/vike-datahub/src/datahub_cli/lanes.rs`'s `options_chain_recorder` calls with its
//! arming verdict and the store it serves), writing into a real `HistoryStoreDataFusion` over a temp dir.
//! The one stand-in is the venue, and it stands one hop further out than it used to: a REAL
//! gatewayhub server on loopback (`vike_gatewayhub::testkit::gateway::LoopbackGateway`) answers the
//! lane's options calls from a script (`RestLaneScript`), so nothing leaves the box and no bridge is
//! linked by this file.
//!
//! 1. [`an_armed_recorder_lands_every_polled_chain_in_kind_chain`] — armed: every expiry of every
//!    underlying the pass fetched lands as `kind=chain` rows under `venue=deribit`,
//!    `symbol=<underlying>`, and they survive a reopen;
//! 2. [`the_full_chain_is_recorded_and_the_window_trims_only_what_is_served`] — the datahub asks the
//!    gatewayhub for the FULL chain (`strike_window: None`), records all of it, and serves the
//!    ±`STRIKE_WINDOW` window of it: the recorded rows and the served rows are the ones they were
//!    when the provider did both in one call;
//! 3. [`an_unarmed_lane_polls_but_records_nothing`] — unarmed: no recorder is built, the same pass
//!    still fetches, and the store holds no chain;
//! 4. [`a_dead_gatewayhub_is_a_failed_pass_never_an_empty_chain`] — no gatewayhub, no chain, and a
//!    named error rather than an empty answer;
//! 5. [`a_started_lane_polls_records_folds_and_opens_the_ticker_feed_once`] —
//!    `OptionsSourceGateway::start` end to end over the hop: the listing, the poll thread, the
//!    recorder, the option index, the markprice feed over the listed indexes, and the ticker feed
//!    started ONCE on the first non-empty focus set;
//! 6. [`a_new_listing_reaches_the_served_set_and_the_markprice_feed_without_a_restart`] — the venue,
//!    the underlyings and the indexes are the gatewayhub's listing, and a changed listing is served
//!    on the next pass;
//! 7. [`a_down_gatewayhub_serves_no_underlying_and_says_why`] — no listing, no underlying, in words.
//!
//! What it does NOT cover: the arming VERDICT (row × compaction), which `src/datahub_cli_tests.rs`'s
//! `the_chain_recorder_*` tests hold; the mount's read of the row itself, which
//! `vike_config::CONSUMPTION`'s `flags.record_chains` row pins by its needle; and the gatewayhub's
//! own side of the call (its one persistent provider, its served-underlying refusal), which
//! `crates/vike-gatewayhub/tests/lanes/rest_lane_surfaces.rs` holds.
#![cfg(all(feature = "options-serve", feature = "datafusion-serve"))]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub::options::OptionsSource;
use vike_datahub::options::gateway::{
    OptionsFocus, OptionsSourceGateway, POLL_INTERVAL, STRIKE_WINDOW, chain_cadence_ms,
    chain_recorder, chain_snapshot_rows, fetch_options, poll_interval,
};
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::{Answer, RestLaneScript};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::options::GatewayOptions;
use vike_gatewayhub_client::proto::{
    FetchWhat, OptionIndexKind, OptionsCall, OptionsFeedSpec, OptionsListing, OptionsUnderlying,
};
use vike_hub_protocol::auth::HubKeys;
use vike_log::poll::wait_for;
use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainStrike, OptionKind, OptionQuote, OptionTicker,
    OptionUnderlyingKind,
};
use vike_model::{Contract, ContractOption, ContractOptionExerciseStyle, Instrument, Margining};
use vike_store::HistoryStoreDataFusion;

/// The chain clock the fixtures carry as `asof_ms` (recorded rows keep it as `ts`).
const ASOF: i64 = 1_780_387_200_000;

fn expiry(date: &str, dte: i64) -> OptionChainExpiry {
    OptionChainExpiry { date: date.into(), dte, label: date.into() }
}

/// One quote with the venue's verbatim instrument name.
fn quote(name: &str, strike: f64, kind: OptionKind, bid: f64, iv: f64) -> OptionQuote {
    OptionQuote {
        bid: Some(bid),
        ask: Some(bid + 1.0),
        mark: Some(bid + 0.5),
        iv: Some(iv),
        open_interest: Some(120.0),
        volume: Some(8.0),
        delta: Some(0.4),
        instrument_name: Some(name.to_string()),
        ..OptionQuote::new(strike, kind)
    }
}

/// A chain of `underlying` / `date` over `strikes` (a call and a put at each), named
/// `{prefix}-{DDMMMYY}-{strike}-{C|P}` with `tag` standing for the `DDMMMYY` spelling.
fn chain(
    underlying: &str,
    prefix: &str,
    date: &str,
    tag: &str,
    dte: i64,
    spot: f64,
    strikes: &[f64],
) -> OptionChain {
    OptionChain {
        underlying: underlying.to_string(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(spot),
        expiry: expiry(date, dte),
        asof_ms: ASOF,
        source: "deribit".to_string(),
        rows: strikes
            .iter()
            .map(|&k| OptionChainStrike {
                strike: k,
                call: Some(quote(
                    &format!("{prefix}-{tag}-{k}-C"),
                    k,
                    OptionKind::Call,
                    50.0,
                    0.62,
                )),
                put: Some(quote(&format!("{prefix}-{tag}-{k}-P"), k, OptionKind::Put, 40.0, 0.61)),
            })
            .collect(),
    }
}

/// The gatewayhub's listing of `underlyings` (name, index), as the venue `deribit`.
fn listing(underlyings: &[(&str, &str)]) -> OptionsListing {
    OptionsListing {
        venue: "deribit".into(),
        underlyings: underlyings
            .iter()
            .map(|(name, index)| OptionsUnderlying { name: (*name).into(), index: (*index).into() })
            .collect(),
    }
}

/// The three underlyings a listing of the venue's captured rows names.
const LISTED: [(&str, &str); 3] = [("BTC", "btc_usd"), ("ETH", "eth_usd"), ("SOL", "sol_usdc")];

/// [`LISTED`]'s names, as a pass takes them.
fn listed_names() -> Vec<String> {
    LISTED.iter().map(|(name, _)| (*name).to_string()).collect()
}

/// BTC's two expiries, ETH's one and SOL's one — what a pass over the shared books fetches. BTC's
/// front expiry has three strikes (spot 104000), its back expiry one.
fn script() -> RestLaneScript {
    let btc_front =
        chain("BTC", "BTC", "2037-06-25", "25JUN37", 3_900, 104_000.0, &[100_000.0, 110_000.0]);
    let btc_back = chain("BTC", "BTC", "2037-12-24", "24DEC37", 4_100, 104_000.0, &[120_000.0]);
    let eth = chain("ETH", "ETH", "2037-06-25", "25JUN37", 3_900, 3_800.0, &[4_000.0]);
    let sol = chain("SOL", "SOL_USDC", "2037-06-25", "25JUN37", 3_900, 190.0, &[200.0]);
    RestLaneScript::new()
        .with_options_listing(Ok(listing(&LISTED)))
        .with_options_expiries(
            "BTC",
            Ok(vec![expiry("2037-06-25", 3_900), expiry("2037-12-24", 4_100)]),
        )
        .with_options_expiries("ETH", Ok(vec![expiry("2037-06-25", 3_900)]))
        .with_options_expiries("SOL", Ok(vec![expiry("2037-06-25", 3_900)]))
        .with_options_chain("BTC", "2037-06-25", Ok(btc_front))
        .with_options_chain("BTC", "2037-12-24", Ok(btc_back))
        .with_options_chain("ETH", "2037-06-25", Ok(eth))
        .with_options_chain("SOL", "2037-06-25", Ok(sol))
}

/// The instruments recorded for `underlying`.
fn recorded(store: &dyn HistoryStore, underlying: &str) -> BTreeSet<String> {
    let rows =
        store.scan_chain("deribit", underlying, TimestampRange::all()).expect("kind=chain scans");
    rows.into_iter().map(|r| r.instrument).collect()
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// A link to nowhere: the tests that never dial still need a value to build a source from.
fn dead_link() -> GatewayLink {
    GatewayLink::new("127.0.0.1:1", HubKeys::new(Vec::new(), Vec::new()))
}

#[test]
fn an_armed_recorder_lands_every_polled_chain_in_kind_chain() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).expect("a store"));

    // The mount's construction when ARMED, over the handle the daemon serves.
    let rec = chain_recorder(
        true,
        Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>,
        chain_cadence_ms(None),
    )
    .expect("the row on builds a recorder");
    assert!(rec.enabled(), "an armed recorder is enabled");
    assert_eq!(rec.cadence_ms(), DEFAULT_CHAIN_CADENCE_MS);

    // The gateway is declared BEFORE the clients, so they drop first.
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), script());

    // The lane's source, and the client its poll thread runs.
    let source = OptionsSourceGateway::new(Some(rec), poll_interval(None), gw.link());
    assert!(source.records(), "a source carrying an enabled recorder records");
    assert!(
        source.chain_recorder().is_some_and(|r| r.enabled()),
        "the recorder reached the source the poll thread is built from"
    );

    // One real pass.
    let fetched = fetch_options(&source.remote(), source.chain_recorder(), &listed_names())
        .expect("the pass fetched");
    assert_eq!(
        fetched.keys().map(String::as_str).collect::<Vec<_>>(),
        ["BTC", "ETH", "SOL"],
        "every underlying the lane serves was polled"
    );

    let btc = names(&[
        "BTC-25JUN37-100000-C",
        "BTC-25JUN37-100000-P",
        "BTC-25JUN37-110000-C",
        "BTC-25JUN37-110000-P",
        "BTC-24DEC37-120000-C",
        "BTC-24DEC37-120000-P",
    ]);
    let eth = names(&["ETH-25JUN37-4000-C", "ETH-25JUN37-4000-P"]);
    let sol = names(&["SOL_USDC-25JUN37-200-C", "SOL_USDC-25JUN37-200-P"]);
    assert_eq!(recorded(store.as_ref(), "BTC"), btc, "both BTC expiries, every quote");
    assert_eq!(recorded(store.as_ref(), "ETH"), eth);
    assert_eq!(recorded(store.as_ref(), "SOL"), sol);

    let btc_rows = store.scan_chain("deribit", "BTC", TimestampRange::all()).expect("scans");
    assert_eq!(btc_rows.len(), btc.len(), "one row per quote, none twice");
    assert!(btc_rows.iter().all(|r| r.underlying == "BTC" && r.iv.is_some() && r.ts == ASOF));

    // Durable, not a cache: a fresh open of the same root reads the same rows.
    drop(source);
    drop(gw);
    drop(store);
    let reopened = HistoryStoreDataFusion::open(dir.path()).expect("reopen the store");
    assert_eq!(recorded(&reopened, "BTC"), btc, "the BTC snapshot survives a reopen");
    assert_eq!(recorded(&reopened, "SOL"), sol);
}

/// **The window is a display concern, never a recording one.** The gatewayhub is asked for the FULL
/// chain (`strike_window: None`: a scripted fetch returns its rows whatever the window, so the
/// REQUEST is what is asserted), the recorder gets every strike, and the served bundle is
/// `limit_strikes(chain, Some(STRIKE_WINDOW))` of the same chain.
#[test]
fn the_full_chain_is_recorded_and_the_window_trims_only_what_is_served() {
    // 100 strikes, 80000..=179000 step 1000, spot 130500: the ±30 window keeps 60 of them.
    let strikes: Vec<f64> = (0..100).map(|i| 80_000.0 + 1_000.0 * f64::from(i)).collect();
    let full = chain("BTC", "BTC", "2037-06-25", "25JUN37", 3_900, 130_500.0, &strikes);
    let rest = RestLaneScript::new()
        .with_options_listing(Ok(listing(&[("BTC", "btc_usd")])))
        .with_options_expiries("BTC", Ok(vec![expiry("2037-06-25", 3_900)]))
        .with_options_chain("BTC", "2037-06-25", Ok(full.clone()));
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());

    let store = Arc::new(vike_data::HistoryStoreMemory::new());
    let rec =
        chain_recorder(true, Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>, 60_000)
            .expect("armed");
    let remote = GatewayOptions::new(gw.link());

    let fetched =
        fetch_options(&remote, Some(rec.as_ref()), &["BTC".into()]).expect("the pass fetched");

    // The request: the full chain, with the risk-free rate the folds use.
    let asked: Vec<_> = rest
        .options_calls()
        .into_iter()
        .filter_map(|c| match c {
            OptionsCall::Chain { underlying, expiry, strike_window, r } => {
                Some((underlying, expiry, strike_window, r.to_bits()))
            }
            _ => None,
        })
        .collect();
    assert!(
        asked.contains(&("BTC".into(), "2037-06-25".into(), None, 0.0f64.to_bits())),
        "the lane asks for the full chain at r = 0, never a window: {asked:?}"
    );
    assert!(asked.iter().all(|(_, _, window, _)| window.is_none()), "no call carries a window");

    // The record: EVERY strike of the pre-window chain, byte for byte what the flattening gives.
    let got = store.scan_chain("deribit", "BTC", TimestampRange::all()).expect("scans");
    assert_eq!(got.len(), 2 * strikes.len(), "a call and a put at every one of the 100 strikes");
    assert_eq!(got, chain_snapshot_rows(&full), "the recorded rows are the full chain's rows");
    assert!(got.iter().all(|r| r.ts == full.asof_ms), "stamped with the chain's own asof");

    // The served rows: the same chain through the same window `fetch_chain(.., Some(30), ..)` applied.
    let served = &fetched["BTC"].chains["2037-06-25"];
    let windowed = vike_options::limit_strikes(full.clone(), Some(STRIKE_WINDOW));
    assert_eq!(served.rows.len(), 2 * STRIKE_WINDOW, "±{STRIKE_WINDOW} strikes around spot");
    assert_eq!(served, &windowed, "the served chain is exactly the windowed chain");
    assert!(served.rows.len() < full.rows.len(), "the window really trimmed something");
}

#[test]
fn an_unarmed_lane_polls_but_records_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).expect("a store"));

    let rec = chain_recorder(
        false,
        Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>,
        chain_cadence_ms(None),
    );
    assert!(rec.is_none(), "unarmed (the row off, or refused) builds no recorder");

    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), script());
    let source = OptionsSourceGateway::new(rec, poll_interval(None), gw.link());
    assert!(!source.records());
    assert!(source.chain_recorder().is_none(), "no recorder reaches the poll pass");

    let fetched = fetch_options(&source.remote(), source.chain_recorder(), &listed_names())
        .expect("the pass still fetches");
    assert_eq!(fetched.len(), 3, "the lane serves chains whether or not it records");
    for u in ["BTC", "ETH", "SOL"] {
        assert!(recorded(store.as_ref(), u).is_empty(), "{u}: nothing recorded with the row off");
    }
}

/// **A gatewayhub that is not there is a failed pass, never an empty chain**: `fetch_options` errors
/// (so the poll thread keeps every last-good bundle and the verb keeps answering `NotReady` with the
/// reason), it does not return an `Ok` with nothing in it that would REPLACE a bundle.
#[test]
fn a_dead_gatewayhub_is_a_failed_pass_never_an_empty_chain() {
    let source = OptionsSourceGateway::new(None, poll_interval(None), dead_link());
    let err =
        fetch_options(&source.remote(), None, &listed_names()).expect_err("nothing to fetch from");
    assert_eq!(err, "no option chain fetched for any listed underlying");
}

/// **No listing, no underlying, and the words say why**: over a gatewayhub that is not there the
/// source serves no underlying and knows no venue — before its first pass (the listing was not asked
/// yet) and after it (the ask failed) — never the BTC/ETH/SOL table it once held.
#[test]
fn a_down_gatewayhub_serves_no_underlying_and_says_why() {
    let source = OptionsSourceGateway::new(None, Duration::from_millis(100), dead_link());
    let first = source.underlyings().expect_err("no listing before the first pass");
    assert!(first.contains("not been asked") || first.contains("could not be had"), "{first}");
    wait_for(WAIT_FOR, "the first pass to fail the ask", || {
        source.underlyings().is_err_and(|why| why.contains("could not be had from the gatewayhub"))
    });
    assert_eq!(source.venue(), None, "no venue is known until a listing was had");
}

/// **The on-demand refresh asks the gatewayhub to read the venue NOW, and its answer is served at
/// once**; a refresh over a gatewayhub that is not there is the error, and the source then serves no
/// underlying.
#[test]
fn a_refresh_asks_the_gatewayhub_to_read_now_and_its_answer_is_served() {
    let rest = script();
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let source = OptionsSourceGateway::new(None, poll_interval(None), gw.link());
    assert_eq!(source.refresh().expect("refreshed"), listed_names());
    assert!(
        rest.options_calls().contains(&OptionsCall::Listing { refresh: true }),
        "the refresh asks the gatewayhub to read now: {:?}",
        rest.options_calls()
    );
    assert_eq!(source.venue().as_deref(), Some("deribit"));

    let dead = OptionsSourceGateway::new(None, poll_interval(None), dead_link());
    let why = dead.refresh().expect_err("no gatewayhub");
    assert!(why.contains("could not be had from the gatewayhub"), "{why}");
    assert!(dead.underlyings().is_err(), "a failed refresh leaves no underlying served");
}

/// **The venue, the underlyings and the indexes are the gatewayhub's listing, and a new one is served
/// without a restart**: a listing that gains XRP (and its index) is served on the next pass, and the
/// markprice feed is opened again over the new indexes, the old one shut down.
#[test]
fn a_new_listing_reaches_the_served_set_and_the_markprice_feed_without_a_restart() {
    let rest = script()
        .with_option_index(OptionIndexKind::Option, Answer::rows(Vec::new()))
        .with_option_index(OptionIndexKind::Future, Answer::rows(Vec::new()));
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let source = OptionsSourceGateway::new(None, Duration::from_millis(100), gw.link());
    source.start();
    wait_for(WAIT_FOR, "the first listing", || source.underlyings().is_ok());
    assert_eq!(source.underlyings().expect("listed"), listed_names());
    assert_eq!(source.venue().as_deref(), Some("deribit"), "the venue is the listing's");
    let markprice_specs = |rest: &RestLaneScript| -> Vec<Vec<String>> {
        (0..rest.feeds_opened())
            .filter_map(|n| match rest.feed(n).spec.clone() {
                OptionsFeedSpec::MarkPrice { indexes } => Some(indexes),
                OptionsFeedSpec::Ticker { .. } => None,
            })
            .collect()
    };
    wait_for(WAIT_FOR, "the markprice feed", || !markprice_specs(&rest).is_empty());
    assert_eq!(markprice_specs(&rest), [vec!["btc_usd", "eth_usd", "sol_usdc"]]);

    // The venue lists XRP now.
    let mut grown = LISTED.to_vec();
    grown.push(("XRP", "xrp_usdc"));
    let _ = rest.with_options_listing(Ok(listing(&grown)));
    wait_for(WAIT_FOR, "the grown listing to be served", || {
        source.underlyings().is_ok_and(|u| u.contains(&"XRP".to_string()))
    });
    wait_for(WAIT_FOR, "the markprice feed over the grown indexes", || {
        markprice_specs(&rest).len() == 2
    });
    assert_eq!(
        markprice_specs(&rest)[1],
        ["btc_usd", "eth_usd", "sol_usdc", "xrp_usdc"],
        "the new feed names every listed index"
    );
    let first_feed = (0..rest.feeds_opened())
        .map(|n| rest.feed(n))
        .find(|f| matches!(f.spec, OptionsFeedSpec::MarkPrice { .. }))
        .expect("the first markprice feed");
    wait_for(WAIT_FOR, "the old markprice feed to be shut down", || first_feed.is_shut_down());
}

/// How long a wait in this file allows before `vike_log::poll::wait_for` fails it.
const WAIT_FOR: Duration = Duration::from_secs(10);

/// **`OptionsSourceGateway::start` over the hop, end to end.** Armed with a recorder, the source starts at
/// mount: its poll thread fetches through the gatewayhub, records the full chains, windows and folds
/// them; the option index comes through the gatewayhub's instrument fetch; the markprice feed opens
/// over the LISTED indexes; and the ticker feed opens EXACTLY ONCE, on the first non-empty focus set,
/// however many passes follow. (The poll thread is detached for the process lifetime, as it always
/// was: it ends with this test binary.)
#[test]
fn a_started_lane_polls_records_folds_and_opens_the_ticker_feed_once() {
    // The index Deribit's `public/get_instruments` would give for BTC's front-expiry front call.
    let call_name = "BTC-25JUN37-100000-C";
    let call = Instrument::new(
        "deribit",
        call_name,
        "BTC",
        "BTC",
        Contract::Option(ContractOption {
            right: OptionKind::Call,
            strike: 100_000.0,
            expiry_ms: vike_options::expiry_ms("2037-06-25"),
            style: ContractOptionExerciseStyle::European,
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_currency: Some("BTC".into()),
            underlying: Some("BTC".into()),
        }),
    );
    let rest = script()
        .with_option_index(OptionIndexKind::Option, Answer::rows(vec![call]))
        .with_option_index(OptionIndexKind::Future, Answer::rows(Vec::new()));
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());

    let store = Arc::new(vike_data::HistoryStoreMemory::new());
    let rec =
        chain_recorder(true, Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>, 60_000)
            .expect("armed");
    // A short poll, so several passes land inside the test; the focus is the list the deploy writes
    // (the row has no default, so a lane without it opens no ticker feed).
    let deploy_focus = OptionsFocus::from_rows(Some("BTC,ETH"), None).expect("a legal row");
    let source = OptionsSourceGateway::new(Some(rec), Duration::from_millis(300), gw.link())
        .with_focus(deploy_focus);
    assert!(source.records());
    source.start();
    source.start(); // once: a second call changes nothing

    // The first pass lands: the book answers, and the recorder wrote the full pre-window chains.
    wait_for(WAIT_FOR, "the first pass to land in the book", || {
        source.snapshot("BTC", &[]).is_ok()
    });
    let btc = store.scan_chain("deribit", "BTC", TimestampRange::all()).expect("scans");
    assert_eq!(btc.len(), 6, "BTC's two expiries, a call and a put at each of three strikes");

    // Both feeds open: the markprice one over the listed indexes, the ticker one over the focus set.
    wait_for(WAIT_FOR, "both feeds to open", || rest.feeds_opened() >= 2);
    let specs: Vec<OptionsFeedSpec> =
        (0..rest.feeds_opened()).map(|n| rest.feed(n).spec.clone()).collect();
    let indexes: Vec<String> = LISTED.iter().map(|(_, index)| (*index).to_string()).collect();
    assert!(
        specs.contains(&OptionsFeedSpec::MarkPrice { indexes }),
        "the markprice feed asks for the three listed indexes: {specs:?}"
    );
    let focus = specs
        .iter()
        .find_map(|s| match s {
            OptionsFeedSpec::Ticker { instruments } => Some(instruments.clone()),
            OptionsFeedSpec::MarkPrice { .. } => None,
        })
        .expect("a ticker feed opened");
    assert!(
        focus.contains(&call_name.to_string()),
        "the focus set names the front call: {focus:?}"
    );
    assert!(
        focus.len() <= 2 * 2 * source.focus().strikes,
        "two focus underlyings, a call and a put per strike: {focus:?}"
    );

    // More passes land; the ticker feed is NOT opened again.
    let chain_calls = |rest: &RestLaneScript| {
        rest.options_calls().iter().filter(|c| matches!(c, OptionsCall::Chain { .. })).count()
    };
    let first = chain_calls(&rest);
    wait_for(WAIT_FOR, "two more passes", || chain_calls(&rest) >= first + 2 * 4);
    assert_eq!(
        rest.feeds_opened(),
        2,
        "one markprice feed and ONE ticker feed, however many passes"
    );

    // The option index came through the gatewayhub: both kinds, options first (the order the
    // in-process `fetch_option_index` asked in).
    let index_kinds: Vec<OptionIndexKind> = rest
        .fetches()
        .into_iter()
        .filter_map(|f| match f.what {
            FetchWhat::OptionIndex { kind } => Some(kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        index_kinds.get(..2),
        Some([OptionIndexKind::Option, OptionIndexKind::Future].as_slice()),
        "the index asks for options and futures, in that order: {index_kinds:?}"
    );

    // A markprice row pushed through the gatewayhub folds onto the served book via that index.
    let marks = (0..rest.feeds_opened())
        .map(|n| rest.feed(n))
        .find(|f| matches!(f.spec, OptionsFeedSpec::MarkPrice { .. }))
        .expect("the markprice feed");
    // The row is re-sent on every look: the next REST pass replaces the bundle with the scripted
    // chain (as a real pass replaces it with the venue's), and a single fold could land between two
    // looks and be overwritten unseen.
    wait_for(WAIT_FOR, "the streamed mark to reach the served book", || {
        let mut row = OptionTicker::new(vike_model::now_ms(), call_name);
        row.mark = Some(0.01);
        row.mark_iv = Some(0.7);
        marks.sink.marks(vec![row]);
        source.snapshot("BTC", &[]).is_ok_and(|bundle| {
            bundle.chains.get("2037-06-25").is_some_and(|c| {
                c.rows[0].call.as_ref().is_some_and(|q| q.mark == Some(0.01 * 104_000.0))
            })
        })
    });
}

/// **No `config.datahub_options_poll_secs` row is the lane as it always was (12 s), and a row is
/// exactly the seconds it says** — carried to the source the mount builds.
#[test]
fn the_poll_row_is_the_interval_and_no_row_is_the_default() {
    assert_eq!(POLL_INTERVAL, Duration::from_secs(12), "the default is twelve seconds");
    assert_eq!(poll_interval(None), POLL_INTERVAL);
    assert_eq!(poll_interval(Some(12)), POLL_INTERVAL, "writing the default is the default");
    assert_eq!(poll_interval(Some(5)), Duration::from_secs(5));
    assert_eq!(poll_interval(Some(300)), Duration::from_secs(300));

    assert_eq!(
        OptionsSourceGateway::new(None, poll_interval(None), dead_link()).poll(),
        POLL_INTERVAL
    );
    assert_eq!(
        OptionsSourceGateway::new(None, poll_interval(Some(30)), dead_link()).poll(),
        Duration::from_secs(30)
    );
}

/// **The `config.datahub_chain_cadence_secs` row is SECONDS and the recorder counts milliseconds**,
/// no row is the minute it always recorded at, and the value reaches the recorder the mount builds.
#[test]
fn the_chain_cadence_row_is_seconds_and_no_row_is_the_default() {
    assert_eq!(DEFAULT_CHAIN_CADENCE_MS, 60_000, "the default is one snapshot a minute");
    assert_eq!(chain_cadence_ms(None), DEFAULT_CHAIN_CADENCE_MS);
    assert_eq!(chain_cadence_ms(Some(60)), DEFAULT_CHAIN_CADENCE_MS, "the default, written");
    assert_eq!(chain_cadence_ms(Some(12)), 12_000);
    assert_eq!(chain_cadence_ms(Some(3_600)), 3_600_000);

    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).expect("a store"));
    let rec = chain_recorder(
        true,
        Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>,
        chain_cadence_ms(Some(120)),
    )
    .expect("armed");
    assert_eq!(rec.cadence_ms(), 120_000, "the row's seconds reach the recorder as milliseconds");
}

/// **The two focus rows: the cap `vike-config` restates is the gatewayhub's, its per-strike cost is
/// what the focus selection really takes, and the defaults (stated in `vike-config` alone) are NO
/// underlying (owner, 2026-11-13) at ten strikes; the list the deploy writes, BTC and ETH, fits the
/// cap at them.** `vike-config` cannot name the gatewayhub client or `vike-options`, so this crate,
/// which links both, holds the three numbers.
#[test]
fn the_focus_rows_cap_is_the_gatewayhubs_and_the_defaults_fit_it() {
    use vike_config::config::{
        DATAHUB_OPTIONS_FOCUS_INSTRUMENTS_PER_STRIKE, DATAHUB_OPTIONS_FOCUS_MAX_INSTRUMENTS,
    };
    use vike_gatewayhub_client::proto::GATEWAY_OPTIONS_MAX_INSTRUMENTS;

    assert_eq!(DATAHUB_OPTIONS_FOCUS_MAX_INSTRUMENTS as usize, GATEWAY_OPTIONS_MAX_INSTRUMENTS);
    // A front chain of 40 strikes around spot: `n` strikes each side cost `4 × n` instruments.
    let strikes: Vec<f64> = (0..40).map(|i| 80_000.0 + 1_000.0 * f64::from(i)).collect();
    let btc = chain("BTC", "BTC", "2037-06-25", "25JUN37", 3_900, 100_500.0, &strikes);
    let bundle = vike_marketdata::OptionChainSet {
        default_expiry: "2037-06-25".into(),
        expiries: vec![btc.expiry.clone()],
        chains: [("2037-06-25".to_string(), btc)].into_iter().collect(),
    };
    let grid = [("BTC".to_string(), bundle)].into_iter().collect();
    for n in [1, 5, 10] {
        let focus = vike_options::front_expiry_focus_instruments(&grid, &["BTC"], n);
        assert_eq!(
            focus.len(),
            DATAHUB_OPTIONS_FOCUS_INSTRUMENTS_PER_STRIKE as usize * n,
            "n = {n}"
        );
    }
    assert_eq!(OptionsFocus::default(), OptionsFocus { underlyings: Vec::new(), strikes: 10 });
    let deployed = OptionsFocus::from_rows(Some("BTC,ETH"), None).expect("the deploy's row");
    assert!(
        deployed.underlyings.len()
            * DATAHUB_OPTIONS_FOCUS_INSTRUMENTS_PER_STRIKE as usize
            * deployed.strikes
            <= GATEWAY_OPTIONS_MAX_INSTRUMENTS,
        "the deploy's focus fits one ticker feed"
    );
    assert_eq!(
        OptionsFocus::from_rows(Some("SOL, XRP"), Some(3)),
        Ok(OptionsFocus { underlyings: vec!["SOL".into(), "XRP".into()], strikes: 3 }),
        "a set row is read by the loader's grammar"
    );
}

/// **A set focus reaches the ticker feed**: a lane whose rows name ETH at one strike streams ETH's
/// front-expiry instruments around spot and no BTC one.
#[test]
fn a_set_focus_is_the_ticker_feeds_focus_set() {
    let rest = script()
        .with_option_index(OptionIndexKind::Option, Answer::rows(Vec::new()))
        .with_option_index(OptionIndexKind::Future, Answer::rows(Vec::new()));
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let focus = OptionsFocus::from_rows(Some("ETH"), Some(1)).expect("a legal pair");
    let source = OptionsSourceGateway::new(None, Duration::from_millis(100), gw.link())
        .with_focus(focus.clone());
    assert_eq!(source.focus(), &focus);
    source.start();
    let ticker = |rest: &RestLaneScript| {
        (0..rest.feeds_opened()).find_map(|n| match rest.feed(n).spec.clone() {
            OptionsFeedSpec::Ticker { instruments } => Some(instruments),
            OptionsFeedSpec::MarkPrice { .. } => None,
        })
    };
    wait_for(WAIT_FOR, "the ticker feed", || ticker(&rest).is_some());
    let instruments = ticker(&rest).expect("opened");
    assert!(
        !instruments.is_empty() && instruments.iter().all(|i| i.starts_with("ETH-")),
        "only ETH's front instruments: {instruments:?}"
    );
    assert!(instruments.len() <= 4, "one strike each side, a call and a put: {instruments:?}");
}

/// **An unset focus opens NO ticker feed** (the list has no default: owner, 2026-11-13): the lane
/// still polls every listed chain and opens the markprice feed, and never a ticker one.
#[test]
fn an_unset_focus_opens_no_ticker_feed() {
    let rest = script()
        .with_option_index(OptionIndexKind::Option, Answer::rows(Vec::new()))
        .with_option_index(OptionIndexKind::Future, Answer::rows(Vec::new()));
    let gw = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let source = OptionsSourceGateway::new(None, Duration::from_millis(100), gw.link());
    assert!(source.focus().underlyings.is_empty(), "no compiled focus");
    source.start();
    let chain_calls = |rest: &RestLaneScript| {
        rest.options_calls().iter().filter(|c| matches!(c, OptionsCall::Chain { .. })).count()
    };
    // Four chain calls a pass (BTC's two expiries, ETH's, SOL's): three whole passes.
    wait_for(WAIT_FOR, "three passes", || chain_calls(&rest) >= 3 * 4);
    let specs: Vec<OptionsFeedSpec> =
        (0..rest.feeds_opened()).map(|n| rest.feed(n).spec.clone()).collect();
    assert!(specs.iter().any(|s| matches!(s, OptionsFeedSpec::MarkPrice { .. })), "{specs:?}");
    assert!(!specs.iter().any(|s| matches!(s, OptionsFeedSpec::Ticker { .. })), "{specs:?}");
}

/// **`vike-config` restates both rows' ranges, and this holds them to this lane.** That crate is
/// below this one, so the numbers are typed there: the chain cadence's floor is the options lane's
/// own REST poll (a bucket narrower than a poll records every poll), and the shipped default of
/// each row is a value its own range takes, so writing the default back is legal.
#[test]
fn the_options_rows_ranges_hold_their_defaults_and_the_cadence_floor_is_the_poll() {
    use vike_config::config::{DATAHUB_CHAIN_CADENCE_SECS, DATAHUB_OPTIONS_POLL_SECS};

    let poll_secs = u32::try_from(POLL_INTERVAL.as_secs()).expect("seconds fit");
    assert!(DATAHUB_OPTIONS_POLL_SECS.contains(&poll_secs), "the default poll is a legal row");
    let cadence_secs = u32::try_from(DEFAULT_CHAIN_CADENCE_MS / 1_000).expect("seconds fit");
    assert!(
        DATAHUB_CHAIN_CADENCE_SECS.contains(&cadence_secs),
        "the default cadence is a legal row"
    );
    assert_eq!(
        *DATAHUB_CHAIN_CADENCE_SECS.start(),
        poll_secs,
        "vike-config refuses a cadence under the default poll: a narrower bucket records every poll"
    );
}
