//! **The `flags.record_chains` recorder, end to end, offline**: the options lane's REAL poll pass
//! (`vike_datahub::options::deribit::fetch_options`, what the `dh-options` thread runs every
//! `POLL_INTERVAL`) over the gatewayhub client the lane builds (`DeribitOptions::remote`), carrying
//! the recorder the mount builds (`chain_recorder`, which
//! `crates/vike-datahub/src/datahub_cli/lanes.rs`'s `options_chain_recorder` calls with its
//! arming verdict and the store it serves), writing into a real `DataFusionHist` over a temp dir.
//! The one stand-in is the venue, and it stands one hop further out than it used to: a REAL
//! gatewayhub server on loopback (`vike_gatewayhub::testkit::gateway::LoopbackGateway`) answers the
//! lane's options calls from a script (`ScriptedRest`), so nothing leaves the box and no bridge is
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
//! 5. [`a_started_lane_polls_records_folds_and_opens_the_ticker_feed_once`] — `DeribitOptions::start`
//!    end to end over the hop: the poll thread, the recorder, the option index, the markprice feed,
//!    and the ticker feed started ONCE on the first non-empty focus set.
//!
//! What it does NOT cover: the arming VERDICT (row × compaction), which `src/datahub_cli_tests.rs`'s
//! `the_chain_recorder_*` tests hold; the mount's read of the row itself, which
//! `vike_config::CONSUMPTION`'s `flags.record_chains` row pins by its needle; and the gatewayhub's
//! own side of the call (its one persistent provider, its served-underlying refusal), which
//! `crates/vike-gatewayhub/tests/rest_lane_surfaces.rs` holds.
#![cfg(all(feature = "options-serve", feature = "datafusion-serve"))]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS;
use vike_data::{HistStore, TsRange};
use vike_datahub::options::OptionsSource;
use vike_datahub::options::deribit::{
    DeribitOptions, FOCUS_STRIKES, MARKPRICE_INDEXES, POLL_INTERVAL, STRIKE_WINDOW,
    chain_cadence_ms, chain_recorder, chain_snapshot_rows, fetch_options, poll_interval,
};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::{Answer, ScriptedRest};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::options::RemoteOptions;
use vike_gatewayhub_client::proto::{FetchWhat, OptionIndexKind, OptionsCall, OptionsFeedSpec};
use vike_hub_proto::auth::HubKeys;
use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, OptionTicker, StrikeRow, UnderlyingKind,
};
use vike_model::{Contract, ExerciseStyle, Instrument, Margining, OptionSpec};
use vike_store::DataFusionHist;

/// The chain clock the fixtures carry as `asof_ms` (recorded rows keep it as `ts`).
const ASOF: i64 = 1_780_387_200_000;

fn expiry(date: &str, dte: i64) -> Expiry {
    Expiry { date: date.into(), dte, label: date.into() }
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
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(spot),
        expiry: expiry(date, dte),
        asof_ms: ASOF,
        source: "deribit".to_string(),
        rows: strikes
            .iter()
            .map(|&k| StrikeRow {
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

/// BTC's two expiries, ETH's one and SOL's one — what a pass over the shared books fetches. BTC's
/// front expiry has three strikes (spot 104000), its back expiry one.
fn script() -> ScriptedRest {
    let btc_front =
        chain("BTC", "BTC", "2037-06-25", "25JUN37", 3_900, 104_000.0, &[100_000.0, 110_000.0]);
    let btc_back = chain("BTC", "BTC", "2037-12-24", "24DEC37", 4_100, 104_000.0, &[120_000.0]);
    let eth = chain("ETH", "ETH", "2037-06-25", "25JUN37", 3_900, 3_800.0, &[4_000.0]);
    let sol = chain("SOL", "SOL_USDC", "2037-06-25", "25JUN37", 3_900, 190.0, &[200.0]);
    ScriptedRest::new()
        .with_options_underlyings(Ok(vec!["BTC".into(), "ETH".into(), "SOL".into()]))
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
fn recorded(store: &dyn HistStore, underlying: &str) -> BTreeSet<String> {
    let rows = store.scan_chain("deribit", underlying, TsRange::all()).expect("kind=chain scans");
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
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));

    // The mount's construction when ARMED, over the handle the daemon serves.
    let rec = chain_recorder(
        true,
        Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>,
        chain_cadence_ms(None),
    )
    .expect("the row on builds a recorder");
    assert!(rec.enabled(), "an armed recorder is enabled");
    assert_eq!(rec.cadence_ms(), DEFAULT_CHAIN_CADENCE_MS);

    // The gateway is declared BEFORE the clients, so they drop first.
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), script());

    // The lane's source, and the client its poll thread runs.
    let source = DeribitOptions::new(Some(rec), poll_interval(None), gw.link());
    assert!(source.records(), "a source carrying an enabled recorder records");
    assert!(
        source.chain_recorder().is_some_and(|r| r.enabled()),
        "the recorder reached the source the poll thread is built from"
    );

    // One real pass.
    let fetched =
        fetch_options(&source.remote(), source.chain_recorder()).expect("the pass fetched");
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

    let btc_rows = store.scan_chain("deribit", "BTC", TsRange::all()).expect("scans");
    assert_eq!(btc_rows.len(), btc.len(), "one row per quote, none twice");
    assert!(btc_rows.iter().all(|r| r.underlying == "BTC" && r.iv.is_some() && r.ts == ASOF));

    // Durable, not a cache: a fresh open of the same root reads the same rows.
    drop(source);
    drop(gw);
    drop(store);
    let reopened = DataFusionHist::open(dir.path()).expect("reopen the store");
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
    let rest = ScriptedRest::new()
        .with_options_underlyings(Ok(vec!["BTC".into()]))
        .with_options_expiries("BTC", Ok(vec![expiry("2037-06-25", 3_900)]))
        .with_options_chain("BTC", "2037-06-25", Ok(full.clone()));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    let store = Arc::new(vike_data::MemHistStore::new());
    let rec = chain_recorder(true, Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>, 60_000)
        .expect("armed");
    let remote = RemoteOptions::new(gw.link());

    let fetched = fetch_options(&remote, Some(rec.as_ref())).expect("the pass fetched");

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
    let got = store.scan_chain("deribit", "BTC", TsRange::all()).expect("scans");
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
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));

    let rec = chain_recorder(
        false,
        Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>,
        chain_cadence_ms(None),
    );
    assert!(rec.is_none(), "unarmed (the row off, or refused) builds no recorder");

    let gw = LoopbackGateway::start(ScriptedData::new(&[]), script());
    let source = DeribitOptions::new(rec, poll_interval(None), gw.link());
    assert!(!source.records());
    assert!(source.chain_recorder().is_none(), "no recorder reaches the poll pass");

    let fetched =
        fetch_options(&source.remote(), source.chain_recorder()).expect("the pass still fetches");
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
    let source = DeribitOptions::new(None, poll_interval(None), dead_link());
    let err = fetch_options(&source.remote(), None).expect_err("nothing to fetch from");
    assert_eq!(err, "no Deribit option chain fetched for any underlying");
}

/// Wait up to ten seconds for `ready`, polling every 20 ms; panic with `what` otherwise.
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// **`DeribitOptions::start` over the hop, end to end.** Armed with a recorder, the source starts at
/// mount: its poll thread fetches through the gatewayhub, records the full chains, windows and folds
/// them; the option index comes through the gatewayhub's instrument fetch; the markprice feed opens
/// over the fixed indexes; and the ticker feed opens EXACTLY ONCE, on the first non-empty focus set,
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
        Contract::Option(OptionSpec {
            right: OptionKind::Call,
            strike: 100_000.0,
            expiry_ms: vike_options::expiry_ms("2037-06-25"),
            style: ExerciseStyle::European,
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("BTC".into()),
            underlying: Some("BTC".into()),
        }),
    );
    let rest = script()
        .with_option_index(OptionIndexKind::Option, Answer::rows(vec![call]))
        .with_option_index(OptionIndexKind::Future, Answer::rows(Vec::new()));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    let store = Arc::new(vike_data::MemHistStore::new());
    let rec = chain_recorder(true, Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>, 60_000)
        .expect("armed");
    // A short poll, so several passes land inside the test.
    let source = DeribitOptions::new(Some(rec), Duration::from_millis(300), gw.link());
    assert!(source.records());
    source.start();
    source.start(); // once: a second call changes nothing

    // The first pass lands: the book answers, and the recorder wrote the full pre-window chains.
    wait_for("the first pass to land in the book", || source.snapshot("BTC", &[]).is_ok());
    let btc = store.scan_chain("deribit", "BTC", TsRange::all()).expect("scans");
    assert_eq!(btc.len(), 6, "BTC's two expiries, a call and a put at each of three strikes");

    // Both feeds open: the markprice one over the fixed indexes, the ticker one over the focus set.
    wait_for("both feeds to open", || rest.feeds_opened() >= 2);
    let specs: Vec<OptionsFeedSpec> =
        (0..rest.feeds_opened()).map(|n| rest.feed(n).spec.clone()).collect();
    let indexes: Vec<String> = MARKPRICE_INDEXES.iter().map(|i| (*i).to_string()).collect();
    assert!(
        specs.contains(&OptionsFeedSpec::MarkPrice { indexes }),
        "the markprice feed asks for the three fixed indexes: {specs:?}"
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
        focus.len() <= 2 * 2 * FOCUS_STRIKES,
        "two focus underlyings, a call and a put per strike: {focus:?}"
    );

    // More passes land; the ticker feed is NOT opened again.
    let chain_calls = |rest: &ScriptedRest| {
        rest.options_calls().iter().filter(|c| matches!(c, OptionsCall::Chain { .. })).count()
    };
    let first = chain_calls(&rest);
    wait_for("two more passes", || chain_calls(&rest) >= first + 2 * 4);
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
    wait_for("the streamed mark to reach the served book", || {
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

    assert_eq!(DeribitOptions::new(None, poll_interval(None), dead_link()).poll(), POLL_INTERVAL);
    assert_eq!(
        DeribitOptions::new(None, poll_interval(Some(30)), dead_link()).poll(),
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
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));
    let rec = chain_recorder(
        true,
        Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>,
        chain_cadence_ms(Some(120)),
    )
    .expect("armed");
    assert_eq!(rec.cadence_ms(), 120_000, "the row's seconds reach the recorder as milliseconds");
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
