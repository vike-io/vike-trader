use vike_gatewayhub_client::proto::{
    KNOWN_FEATURES, OptionIndexKind, advertised_credentialed_data_venues,
};
use vike_hub_protocol::features::advertised_market_data_venues;

use super::*;
use crate::testkit::fixtures::full_facts;

fn market_data() -> Vec<String> {
    vec!["binance".into(), "polymarket".into()]
}

/// **The golden (drift register R5).** The exact list, in order, for a full build.
#[test]
fn the_full_build_advertises_this_exact_list() {
    let got = served_features(&market_data(), &full_facts());
    let want: Vec<String> = [
        "gatewayhub",
        "gateway_data",
        "stream_class",
        "stream_funding",
        "stream_binary_ticks",
        "gateway_fetch",
        "gateway_fetch_progress",
        "gateway_fetch_cancel",
        "gateway_fetch_credentialed",
        "gateway_ticks",
        "gateway_catalog",
        "gateway_options",
        "gateway_options_listing",
        "gateway_option_trades",
        "gateway_family",
        "gateway_market_rules",
        "gateway_market_rules_series",
        "gateway_presence",
        "gateway_status",
        "wire_msgpack",
        "market_data_venue=binance",
        "market_data_venue=polymarket",
        "credentialed_data_venue=oanda",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(got, want);
}

/// A data lane that hosts every strategy-lane addition (the strings come from its build facts).
struct FullStrategyLane;

impl DataLane for FullStrategyLane {
    fn venues(&self) -> Vec<String> {
        market_data()
    }
    fn open(
        &self,
        _venue: &str,
        _holder: &crate::data::DataLaneHolder,
        _sink: std::sync::Arc<dyn vike_data::DataClientSink>,
    ) -> Result<Box<dyn vike_data::DataClient + Send>, String> {
        Err("not opened in this test".into())
    }
    fn network_venues(&self) -> Vec<String> {
        vec!["hyperliquid".into()]
    }
    fn feed_tick_venues(&self) -> Vec<String> {
        vec!["binance".into()]
    }
    fn credentialed_venues(&self) -> Vec<String> {
        vec!["alpaca".into(), "oanda".into()]
    }
}

/// The strategy lane's strings follow what the lane declares, appended after the wave-1 list, and a lane
/// that declares nothing adds nothing.
#[test]
fn the_strategy_lane_strings_follow_the_lanes_build_facts() {
    let got = strategy_lane_features(&FullStrategyLane);
    assert_eq!(got, ["stream_network", "gateway_feed_tick", "cs_venue=alpaca", "cs_venue=oanda"]);
    assert_eq!(
        vike_gatewayhub_client::proto::advertised_cs_venues(&got),
        vec!["alpaca".to_string(), "oanda".to_string()]
    );
    struct Bare;
    impl DataLane for Bare {
        fn venues(&self) -> Vec<String> {
            Vec::new()
        }
        fn open(
            &self,
            _venue: &str,
            _holder: &crate::data::DataLaneHolder,
            _sink: std::sync::Arc<dyn vike_data::DataClientSink>,
        ) -> Result<Box<dyn vike_data::DataClient + Send>, String> {
            Err("none".into())
        }
    }
    assert!(strategy_lane_features(&Bare).is_empty());
}

/// A fully armed daemon advertises every string the client vocabulary knows, in its order.
#[test]
fn a_full_build_advertises_the_whole_client_vocabulary() {
    let mut got = served_features(&market_data(), &full_facts());
    got.extend(strategy_lane_features(&FullStrategyLane));
    let fixed: Vec<&str> = got.iter().map(String::as_str).filter(|f| !f.contains('=')).collect();
    assert_eq!(fixed, KNOWN_FEATURES, "the daemon and the client vocabulary disagree");
    assert_eq!(advertised_market_data_venues(&got), market_data());
    assert_eq!(advertised_credentialed_data_venues(&got), vec!["oanda".to_string()]);
}

/// A bare daemon (no lane at all) still identifies itself, answers the status and speaks the session codec.
#[test]
fn a_bare_daemon_advertises_the_sentinel_the_status_and_the_codec_only() {
    let got = served_features(&[], &RestRosters::default());
    assert_eq!(got, ["gatewayhub", "gateway_status", "wire_msgpack"].map(String::from));
}

/// Each string is advertised exactly when its lane is served, and no other lane moves it.
#[test]
fn each_string_follows_its_own_lane() {
    let has = |f: &[String], s: &str| f.iter().any(|x| x == s);
    let only = |rest: RestRosters| served_features(&[], &rest);

    let f = only(RestRosters { klines: vec!["binance".into()], ..RestRosters::default() });
    assert!(
        has(&f, "gateway_fetch")
            && has(&f, "gateway_fetch_progress")
            && has(&f, "gateway_fetch_cancel")
    );
    assert!(!has(&f, "gateway_ticks") && !has(&f, "gateway_catalog") && !has(&f, "gateway_data"));

    let f = only(RestRosters { ticks: vec!["dukascopy".into()], ..RestRosters::default() });
    assert!(
        has(&f, "gateway_ticks") && !has(&f, "gateway_fetch"),
        "ticks alone arm gateway_ticks, not gateway_fetch"
    );
    assert!(has(&f, "gateway_fetch_progress"), "any fetch lane arms the progress contract");

    let f = only(RestRosters { presence: true, ..RestRosters::default() });
    assert!(has(&f, "gateway_presence") && !has(&f, "gateway_fetch_progress"));

    let f = only(RestRosters { family: vec!["polymarket".into()], ..RestRosters::default() });
    assert!(has(&f, "gateway_family") && !has(&f, "gateway_fetch_progress"));

    let f = only(RestRosters { market_rules: vec!["polymarket".into()], ..RestRosters::default() });
    assert!(
        has(&f, "gateway_market_rules") && has(&f, "gateway_fetch_progress"),
        "the market rules are a fetch"
    );
    assert!(has(&f, "gateway_market_rules_series"), "the series lane rides the rules lane");
    assert!(!has(&f, "gateway_family"), "the rules lane does not arm the family resolver");

    let f = only(RestRosters { options: true, ..RestRosters::default() });
    assert!(
        has(&f, "gateway_options") && has(&f, "gateway_fetch_progress"),
        "the option index is a fetch"
    );
    assert!(has(&f, "gateway_options_listing"), "the listing rides the options lane");

    let f = served_features(&["binance".to_string()], &RestRosters::default());
    assert!(
        has(&f, "gateway_data")
            && has(&f, "market_data_venue=binance")
            && !has(&f, "gateway_fetch_progress")
    );
    assert!(has(&f, "stream_class"), "the class of a subscription rides the stream plane");
    let f = served_features(&[], &RestRosters { presence: true, ..RestRosters::default() });
    assert!(!has(&f, "stream_class"), "no stream plane, no class");
}

/// The text a refusal carries names the verb and the string.
#[test]
fn the_refusal_names_the_string() {
    let text = not_served("CatalogDescribe", "gateway_catalog");
    assert!(text.contains("CatalogDescribe") && text.contains("\"gateway_catalog\""), "{text}");
}

#[test]
fn the_fetch_gates_follow_the_lane_a_request_names() {
    let rest = full_facts();
    let klines = FetchWhat::Klines {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        start_ms: 0,
        end_ms: 1,
    };
    assert_eq!(fetch_gate(&klines, false), "gateway_fetch");
    assert_eq!(fetch_gate(&klines, true), "gateway_fetch_credentialed");
    assert!(fetch_lane_armed(&rest, &klines, false));
    assert!(!fetch_lane_armed(&RestRosters::default(), &klines, false));
    assert_eq!(
        fetch_roster(&rest, &klines, false).map(|(v, r)| (v, r.len())),
        Some(("binance", 2))
    );
    assert_eq!(fetch_roster(&rest, &klines, true).map(|(_, r)| r.len()), Some(1));
    let index = FetchWhat::OptionIndex { kind: OptionIndexKind::Option };
    assert_eq!(fetch_gate(&index, false), "gateway_options");
    assert!(fetch_roster(&rest, &index, false).is_none());
    assert!(fetch_lane_armed(&rest, &index, false));
}

/// The trade strings are the client's `KNOWN_TRADE_FEATURES` in order, then one `trade_venue=` each, and
/// they never join the data vocabulary: `served_features` is unchanged by them.
#[test]
fn the_trade_class_is_advertised_as_its_own_list_after_the_data_one() {
    use vike_gatewayhub_client::proto::{KNOWN_TRADE_FEATURES, advertised_trade_venues};
    let venues = vec!["binance".to_string(), "okx".to_string()];
    let got = trade_features(&venues);
    let fixed: Vec<&str> = got.iter().map(String::as_str).filter(|f| !f.contains('=')).collect();
    assert_eq!(fixed, KNOWN_TRADE_FEATURES);
    assert_eq!(advertised_trade_venues(&got), venues);
    // No trade string leaks into the data list, and the data list is untouched by a trade lane.
    let data = served_features(&market_data(), &full_facts());
    assert!(data.iter().all(|f| !f.starts_with("gateway_trade") && !f.starts_with("trade_venue=")));
    assert_eq!(trade_features(&[]).len(), KNOWN_TRADE_FEATURES.len());
}
