use super::*;

/// **The property the crate was split out for, executed rather than asserted in prose.** A
/// build with no vike crate in it holds a real `AlertEngine`, folds a real rule, and delivers a
/// real `FiredAlert` naming the series — the watchdog path, end to end, minus the network.
#[test]
fn a_vike_free_build_folds_a_series_stale_rule_all_the_way_to_a_sink() {
    let inbox = InProcessSink::new();
    let rule =
        AlertRule::new("recorder-series-stale", RuleTrigger::SeriesStale { series_prefix: None });
    let mut engine = AlertEngine::new(vec![rule]).with_sink(Box::new(inbox.clone()));
    assert!(engine.is_active());

    let fired = engine.on_signal(
        &AlertSignal::SeriesStale {
            series: "book/polymarket/0xtok".into(),
            silent_for_ms: Some(19 * 60_000),
            rows: 3_800_000,
        },
        1_000,
    );
    assert_eq!(fired.len(), 1, "one silent series ⇒ one alert");
    assert_eq!(fired[0].rule_id, "recorder-series-stale");

    let delivered = inbox.drain();
    assert_eq!(delivered.len(), 1, "…and it reached the sink, not just the return value");
    assert!(delivered[0].body.contains("book/polymarket/0xtok"), "{}", delivered[0].body);
}

/// The OFF state is unchanged by the engine leaving `core`: no rules ⇒ no evaluation, no sink
/// touched, on the signal path too.
#[test]
fn an_empty_engine_is_still_inert_on_the_signal_path() {
    let inbox = InProcessSink::new();
    let mut engine = AlertEngine::new(Vec::new()).with_sink(Box::new(inbox.clone()));
    assert!(!engine.is_active());
    assert!(
        engine
            .on_signal(
                &AlertSignal::SeriesStale {
                    series: "trade/binance/BTCUSDT".into(),
                    silent_for_ms: None,
                    rows: 0
                },
                1
            )
            .is_empty()
    );
    assert!(inbox.is_empty());
}
