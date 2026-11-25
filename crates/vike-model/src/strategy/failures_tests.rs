use super::StrategyFailures;

/// A count past the bound keeps the first time and the first distinct messages, each once.
#[test]
fn record_counts_every_failure_and_keeps_the_first_distinct_messages() {
    let mut failures = StrategyFailures::default();
    assert!(failures.is_empty());
    for i in 0..20 {
        failures.record(Some(1_000 + i), &format!("on_bar failed {}", i % 7));
    }
    assert_eq!(failures.count, 20);
    assert_eq!(failures.first_at, Some(1_000));
    let kept: Vec<String> = (0..5).map(|i| format!("on_bar failed {i}")).collect();
    assert_eq!(failures.messages, kept);
    assert!(!failures.disabled && !failures.is_empty());
}

/// Two runs fold into one: counts add, the earlier first time wins, messages stay distinct, and one
/// disabled run makes the fold disabled.
#[test]
fn merge_adds_counts_and_keeps_the_earliest_time() {
    let mut a = StrategyFailures::default();
    a.record(Some(5_000), "a");
    let mut b = StrategyFailures { disabled: true, ..StrategyFailures::default() };
    b.record(Some(2_000), "a");
    b.record(Some(3_000), "b");
    a.merge(&b);
    let expected = StrategyFailures {
        count: 3,
        first_at: Some(2_000),
        messages: vec!["a".to_string(), "b".to_string()],
        disabled: true,
    };
    assert_eq!(a, expected);
    let mut clean = StrategyFailures::default();
    clean.merge(&StrategyFailures::default());
    assert!(clean.is_empty());
}

/// The loud line: the count, the first time in UTC, the messages, and the disabled verdict.
#[test]
fn the_line_names_the_count_the_time_the_messages_and_the_verdict() {
    let failures = StrategyFailures {
        count: 12,
        first_at: Some(86_400_000),
        messages: vec!["on_bar: x".to_string(), "on_stop: y".to_string()],
        disabled: true,
    };
    assert_eq!(
        failures.to_string(),
        "STRATEGY FAILED 12 time(s), first at 1970-01-02T00:00:00Z: on_bar: x | on_stop: y; the \
         strategy was DISABLED and placed no order after it"
    );
}
