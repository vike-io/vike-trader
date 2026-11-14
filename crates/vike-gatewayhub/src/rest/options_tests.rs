use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use vike_bridge_core::options::{
    OptionsListedUnderlying, OptionsMarksDeliver, OptionsTickerDeliver,
};
use vike_gatewayhub_client::proto::{
    GATEWAY_OPTIONS_MAX_INDEXES, GATEWAY_OPTIONS_MAX_INSTRUMENTS, GATEWAY_OPTIONS_MAX_NAME_BYTES,
};
use vike_marketdata::{OptionChain, OptionChainExpiry, OptionTicker};

use super::*;

/// A spec that records what it was asked, answers `refuse` when set, starts no thread, and counts the
/// stops of the feeds it opened.
#[derive(Default)]
struct Scripted {
    asked: Mutex<Vec<String>>,
    refuse: Option<String>,
    stops: Arc<AtomicUsize>,
    marks: Mutex<Option<OptionsMarksDeliver>>,
    ticker: Mutex<Option<OptionsTickerDeliver>>,
}

impl Scripted {
    fn ask(&self, what: String) -> Result<(), String> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).push(what);
        self.refuse.clone().map_or(Ok(()), Err)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn stop(&self) -> OptionsFeedStop {
        let stops = Arc::clone(&self.stops);
        Box::new(move || {
            stops.fetch_add(1, Ordering::SeqCst);
        })
    }
}

/// The spec the hub holds: the shared script, so a test reads what it was asked.
struct ScriptedSpec(Arc<Scripted>);

impl std::ops::Deref for ScriptedSpec {
    type Target = Scripted;
    fn deref(&self) -> &Scripted {
        &self.0
    }
}

impl OptionsSpec for ScriptedSpec {
    fn venue(&self) -> &'static str {
        "scripted"
    }

    fn listing(&self, refresh: bool) -> Result<Vec<OptionsListedUnderlying>, String> {
        self.ask(format!("listing refresh={refresh}"))?;
        Ok(vec![
            OptionsListedUnderlying { name: "BTC".into(), index: "btc_usd".into() },
            OptionsListedUnderlying { name: "SOL".into(), index: "sol_usdc".into() },
        ])
    }

    fn expiries(&self, underlying: &str) -> Result<Vec<OptionChainExpiry>, String> {
        self.ask(format!("expiries {underlying}"))?;
        Ok(Vec::new())
    }

    fn chain(
        &self,
        underlying: &str,
        expiry: &str,
        strikes: Option<usize>,
        r: f64,
    ) -> Result<OptionChain, String> {
        self.ask(format!("chain {underlying} {expiry} {strikes:?} {r}"))?;
        Err("no chain in a script".to_string())
    }

    fn instruments(&self, kind: &str) -> Result<Vec<Instrument>, String> {
        self.ask(format!("instruments {kind}"))?;
        Ok(Vec::new())
    }

    fn open_marks(
        &self,
        indexes: Vec<String>,
        deliver: OptionsMarksDeliver,
    ) -> Result<OptionsFeedStop, String> {
        self.ask(format!("marks {indexes:?}"))?;
        *self.marks.lock().unwrap_or_else(PoisonError::into_inner) = Some(deliver);
        Ok(self.stop())
    }

    fn open_ticker(
        &self,
        instruments: Vec<String>,
        deliver: OptionsTickerDeliver,
    ) -> Result<OptionsFeedStop, String> {
        self.ask(format!("ticker {instruments:?}"))?;
        *self.ticker.lock().unwrap_or_else(PoisonError::into_inner) = Some(deliver);
        Ok(self.stop())
    }
}

fn hub(spec: &Arc<Scripted>) -> OptionsHub {
    OptionsHub::new(Box::new(ScriptedSpec(Arc::clone(spec))))
}

/// A sink that records what each feed delivered into it.
#[derive(Default)]
struct RecordingSink {
    marks: Mutex<Vec<usize>>,
    ticker: Mutex<Vec<String>>,
}

impl FeedOptionsSink for RecordingSink {
    fn marks(&self, rows: Vec<OptionTicker>) {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner).push(rows.len());
    }
    fn ticker(&self, row: OptionTicker) {
        self.ticker.lock().unwrap_or_else(PoisonError::into_inner).push(row.symbol);
    }
}

/// **Each call maps onto one spec method, and the answer onto the wire's reply**: `Underlyings` is the
/// listing's names, `Listing` the listing under the SPEC's venue with the refresh word passed through,
/// and a chain's window becomes the spec's strike count (`None`, the FULL chain, stays `None`).
#[test]
fn each_call_maps_onto_its_spec_method_and_back() {
    let spec = Arc::new(Scripted::default());
    let hub = hub(&spec);
    assert_eq!(
        hub.call(&OptionsCall::Underlyings),
        Ok(OptionsReply::Underlyings(vec!["BTC".into(), "SOL".into()]))
    );
    assert_eq!(
        hub.call(&OptionsCall::Listing { refresh: true }),
        Ok(OptionsReply::Listing(OptionsListing {
            venue: "scripted".into(),
            underlyings: vec![
                OptionsUnderlying { name: "BTC".into(), index: "btc_usd".into() },
                OptionsUnderlying { name: "SOL".into(), index: "sol_usdc".into() },
            ],
        }))
    );
    assert_eq!(
        hub.call(&OptionsCall::Expiries { underlying: "BTC".into() }),
        Ok(OptionsReply::Expiries(Vec::new()))
    );
    for strike_window in [None, Some(2)] {
        let chain = OptionsCall::Chain {
            underlying: "BTC".into(),
            expiry: "2037-06-25".into(),
            strike_window,
            r: 0.03,
        };
        assert_eq!(hub.call(&chain), Err("no chain in a script".to_string()));
    }
    assert_eq!(
        spec.asked(),
        [
            "listing refresh=false",
            "listing refresh=true",
            "expiries BTC",
            "chain BTC 2037-06-25 None 0.03",
            "chain BTC 2037-06-25 Some(2) 0.03",
        ]
    );
}

/// A spec's refusal reaches the peer in its own words, on every entry point.
#[test]
fn a_specs_refusal_is_its_own_words_everywhere() {
    let spec =
        Arc::new(Scripted { refuse: Some("the venue's words".into()), ..Scripted::default() });
    let hub = hub(&spec);
    assert_eq!(hub.call(&OptionsCall::Underlyings), Err("the venue's words".to_string()));
    assert_eq!(
        hub.instruments(OptionIndexKind::Future, &FetchContext::new()),
        Err(SourceError::Fetch("the venue's words".to_string()))
    );
    let marks = OptionsFeedSpec::MarkPrice { indexes: vec!["btc_usd".into()] };
    assert_eq!(
        hub.open_feed(&marks, Arc::new(RecordingSink::default())).err(),
        Some("the venue's words".to_string())
    );
}

/// The index fetch asks the spec for the kind's word, and a cancelled fetch never asks it.
#[test]
fn the_index_fetch_asks_the_kinds_word_and_a_cancelled_one_asks_nothing() {
    let spec = Arc::new(Scripted::default());
    let hub = hub(&spec);
    let ctx = FetchContext::new();
    hub.instruments(OptionIndexKind::Option, &ctx).expect("options");
    hub.instruments(OptionIndexKind::Future, &ctx).expect("futures");
    ctx.cancel();
    assert_eq!(
        hub.instruments(OptionIndexKind::Option, &ctx),
        Err(SourceError::Fetch("gatewayhub fetch cancelled before the venue was asked".into()))
    );
    assert_eq!(
        spec.asked(),
        ["instruments option", "instruments future"],
        "a cancelled fetch asked"
    );
}

/// Each feed spec reaches ITS spec method with its own focus set, unchanged, delivers into the sink, and
/// the handle stops it.
#[test]
fn each_feed_spec_opens_its_own_feed_and_delivers_into_the_sink() {
    let spec = Arc::new(Scripted::default());
    let hub = hub(&spec);
    let sink = Arc::new(RecordingSink::default());
    let mut marks = hub
        .open_feed(
            &OptionsFeedSpec::MarkPrice { indexes: vec!["btc_usd".into(), "eth_usd".into()] },
            Arc::clone(&sink) as Arc<dyn FeedOptionsSink>,
        )
        .expect("marks");
    let mut ticker = hub
        .open_feed(
            &OptionsFeedSpec::Ticker { instruments: vec!["BTC-25JUN37-100000-C".into()] },
            Arc::clone(&sink) as Arc<dyn FeedOptionsSink>,
        )
        .expect("ticker");
    assert_eq!(
        spec.asked(),
        [r#"marks ["btc_usd", "eth_usd"]"#, r#"ticker ["BTC-25JUN37-100000-C"]"#]
    );
    let deliver_marks = spec.marks.lock().unwrap_or_else(PoisonError::into_inner).take();
    deliver_marks.expect("a marks feed")(vec![OptionTicker::new(1, "BTC-25JUN37-100000-C")]);
    let deliver_ticker = spec.ticker.lock().unwrap_or_else(PoisonError::into_inner).take();
    deliver_ticker.expect("a ticker feed")(OptionTicker::new(2, "BTC-25JUN37-100000-C"));
    assert_eq!(*sink.marks.lock().unwrap_or_else(PoisonError::into_inner), [1]);
    assert_eq!(
        *sink.ticker.lock().unwrap_or_else(PoisonError::into_inner),
        ["BTC-25JUN37-100000-C"]
    );
    marks.shutdown();
    ticker.shutdown();
    assert_eq!(spec.stops.load(Ordering::SeqCst), 2);
}

/// The focus set is capped by the SERVER's constants even when the server did not run the check, and
/// BEFORE the spec is asked: an empty set, a blank name and a set over its cap are refused, and no feed
/// is started. At the cap exactly, the feed opens.
#[test]
fn the_focus_set_is_capped_by_the_servers_constants_before_the_spec() {
    let spec = Arc::new(Scripted::default());
    let hub = hub(&spec);
    let sink: Arc<dyn FeedOptionsSink> = Arc::new(RecordingSink::default());
    let too_many_indexes = (0..=GATEWAY_OPTIONS_MAX_INDEXES).map(|i| format!("idx_{i}")).collect();
    let too_many_instruments =
        (0..=GATEWAY_OPTIONS_MAX_INSTRUMENTS).map(|i| format!("BTC-25JUN37-{i}-C")).collect();
    for feed in [
        OptionsFeedSpec::MarkPrice { indexes: Vec::new() },
        OptionsFeedSpec::MarkPrice { indexes: too_many_indexes },
        OptionsFeedSpec::Ticker { instruments: vec!["  ".to_string()] },
        OptionsFeedSpec::Ticker { instruments: too_many_instruments },
    ] {
        assert!(hub.open_feed(&feed, Arc::clone(&sink)).is_err(), "{feed:?}");
    }
    assert!(spec.asked().is_empty(), "the spec was asked for a refused set");
    let at_cap =
        (0..GATEWAY_OPTIONS_MAX_INSTRUMENTS).map(|i| format!("BTC-25JUN37-{i}-C")).collect();
    assert!(hub.open_feed(&OptionsFeedSpec::Ticker { instruments: at_cap }, sink).is_ok());
}

/// The CHARSET and LENGTH of a name are `OptionsFeedSpec::refusal`'s, and the engine re-runs it: a dot, a
/// space, a control byte, a slash and a paragraph never reach the spec, let alone a channel string.
#[test]
fn a_name_that_could_write_the_channel_grammar_never_reaches_the_spec() {
    let spec = Arc::new(Scripted::default());
    let hub = hub(&spec);
    let sink: Arc<dyn FeedOptionsSink> = Arc::new(RecordingSink::default());
    let long = "b".repeat(GATEWAY_OPTIONS_MAX_NAME_BYTES + 1);
    for name in
        ["btc_usd.100ms", "btc usd", "btc_usd\n", "btc/usd", "btc_usd,eth_usd", long.as_str()]
    {
        let marks = OptionsFeedSpec::MarkPrice { indexes: vec![name.to_string()] };
        assert!(hub.open_feed(&marks, Arc::clone(&sink)).is_err(), "{name:?}");
        let ticker = OptionsFeedSpec::Ticker { instruments: vec![name.to_string()] };
        assert!(hub.open_feed(&ticker, Arc::clone(&sink)).is_err(), "{name:?}");
    }
    assert!(spec.asked().is_empty());
}

/// The one handle every feed is held by stops it ONCE: at an explicit `shutdown` (a second one and the
/// drop then stop nothing), or at the drop when nobody called it.
#[test]
fn a_feed_handle_stops_its_feed_once_at_shutdown_or_at_the_drop() {
    let stops = Arc::new(AtomicUsize::new(0));
    let counted = |stops: &Arc<AtomicUsize>| {
        let stops = Arc::clone(stops);
        FeedHandle(Some(Box::new(move || {
            stops.fetch_add(1, Ordering::SeqCst);
        })))
    };
    let mut handle = counted(&stops);
    handle.shutdown();
    handle.shutdown();
    drop(handle);
    assert_eq!(stops.load(Ordering::SeqCst), 1, "shutdown stops it once; the drop adds nothing");
    drop(counted(&stops));
    assert_eq!(
        stops.load(Ordering::SeqCst),
        2,
        "a handle dropped without a shutdown stops its feed"
    );
}
