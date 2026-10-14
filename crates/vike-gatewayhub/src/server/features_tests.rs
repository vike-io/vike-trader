use vike_gatewayhub_client::proto::{
    KNOWN_FEATURES, OptionIndexKind, advertised_cd_venues, advertised_md_venues,
};

use super::*;

fn full_facts() -> RestFacts {
    RestFacts {
        klines: vec!["binance".into(), "bybit".into()],
        funding: vec!["binance".into()],
        ticks: vec!["dukascopy".into()],
        catalog: vec!["binance".into(), "dukascopy".into()],
        credentialed: vec!["oanda".into()],
        options: true,
        family: vec!["polymarket".into()],
        presence: true,
    }
}

fn md() -> Vec<String> {
    vec!["binance".into(), "polymarket".into()]
}

/// **The golden (drift register R5).** The exact list, in order, for a full build.
#[test]
fn the_full_build_advertises_this_exact_list() {
    let got = served_features(&md(), &full_facts());
    let want: Vec<String> = [
        "gatewayhub",
        "gw_data",
        "stream_class",
        "gw_fetch",
        "gw_fetch_progress",
        "gw_fetch_cancel",
        "gw_fetch_credentialed",
        "gw_ticks",
        "gw_catalog",
        "gw_options",
        "gw_family",
        "gw_presence",
        "gw_status",
        "md_venue=binance",
        "md_venue=polymarket",
        "cd_venue=oanda",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(got, want);
}

/// A fully armed daemon advertises every string the client vocabulary knows, in its order.
#[test]
fn a_full_build_advertises_the_whole_client_vocabulary() {
    let got = served_features(&md(), &full_facts());
    let fixed: Vec<&str> = got.iter().map(String::as_str).filter(|f| !f.contains('=')).collect();
    assert_eq!(fixed, KNOWN_FEATURES, "the daemon and the client vocabulary disagree");
    assert_eq!(advertised_md_venues(&got), md());
    assert_eq!(advertised_cd_venues(&got), vec!["oanda".to_string()]);
}

/// A bare daemon (no lane at all) still identifies itself and answers the status.
#[test]
fn a_bare_daemon_advertises_the_sentinel_and_the_status_only() {
    let got = served_features(&[], &RestFacts::default());
    assert_eq!(got, vec!["gatewayhub".to_string(), "gw_status".to_string()]);
}

/// Each string is advertised exactly when its lane is served, and no other lane moves it.
#[test]
fn each_string_follows_its_own_lane() {
    let has = |f: &[String], s: &str| f.iter().any(|x| x == s);
    let only = |rest: RestFacts| served_features(&[], &rest);

    let f = only(RestFacts { klines: vec!["binance".into()], ..RestFacts::default() });
    assert!(has(&f, "gw_fetch") && has(&f, "gw_fetch_progress") && has(&f, "gw_fetch_cancel"));
    assert!(!has(&f, "gw_ticks") && !has(&f, "gw_catalog") && !has(&f, "gw_data"));

    let f = only(RestFacts { ticks: vec!["dukascopy".into()], ..RestFacts::default() });
    assert!(has(&f, "gw_ticks") && !has(&f, "gw_fetch"), "ticks alone arm gw_ticks, not gw_fetch");
    assert!(has(&f, "gw_fetch_progress"), "any fetch lane arms the progress contract");

    let f = only(RestFacts { presence: true, ..RestFacts::default() });
    assert!(has(&f, "gw_presence") && !has(&f, "gw_fetch_progress"));

    let f = only(RestFacts { family: vec!["polymarket".into()], ..RestFacts::default() });
    assert!(has(&f, "gw_family") && !has(&f, "gw_fetch_progress"));

    let f = only(RestFacts { options: true, ..RestFacts::default() });
    assert!(has(&f, "gw_options") && has(&f, "gw_fetch_progress"), "the option index is a fetch");

    let f = served_features(&["binance".to_string()], &RestFacts::default());
    assert!(has(&f, "gw_data") && has(&f, "md_venue=binance") && !has(&f, "gw_fetch_progress"));
    assert!(has(&f, "stream_class"), "the class of a subscription rides the stream plane");
    let f = served_features(&[], &RestFacts { presence: true, ..RestFacts::default() });
    assert!(!has(&f, "stream_class"), "no stream plane, no class");
}

/// The text a refusal carries names the verb and the string.
#[test]
fn the_refusal_names_the_string() {
    let text = not_served("CatalogDescribe", "gw_catalog");
    assert!(text.contains("CatalogDescribe") && text.contains("\"gw_catalog\""), "{text}");
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
    assert_eq!(fetch_gate(&klines, false), "gw_fetch");
    assert_eq!(fetch_gate(&klines, true), "gw_fetch_credentialed");
    assert!(fetch_lane_armed(&rest, &klines, false));
    assert!(!fetch_lane_armed(&RestFacts::default(), &klines, false));
    assert_eq!(
        fetch_roster(&rest, &klines, false).map(|(v, r)| (v, r.len())),
        Some(("binance", 2))
    );
    assert_eq!(fetch_roster(&rest, &klines, true).map(|(_, r)| r.len()), Some(1));
    let index = FetchWhat::OptionIndex { kind: OptionIndexKind::Option };
    assert_eq!(fetch_gate(&index, false), "gw_options");
    assert!(fetch_roster(&rest, &index, false).is_none());
    assert!(fetch_lane_armed(&rest, &index, false));
}
