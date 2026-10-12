use super::*;

#[test]
fn empty_vars_all_absent() {
    let vars = HashMap::new();
    let statuses = credential_status(&vars);
    assert_eq!(statuses.len(), VENUES.len());
    for s in &statuses {
        assert!(!s.sim, "{} sim should be false", s.venue);
        assert!(!s.demo, "{} demo should be false", s.venue);
        assert!(!s.live, "{} live should be false", s.venue);
    }
}

#[test]
fn binance_live_configured_detected() {
    let mut vars = HashMap::new();
    vars.insert("BINANCE_LIVE_API_KEY".to_string(), "key".to_string());
    vars.insert("BINANCE_LIVE_API_SECRET".to_string(), "secret".to_string());
    let statuses = credential_status(&vars);
    let binance = statuses.iter().find(|s| s.venue == "binance").expect("binance present");
    assert!(binance.live);
    assert!(!binance.sim);
    assert!(!binance.demo);
}

#[test]
fn venue_list_order_is_stable() {
    let vars = HashMap::new();
    let statuses = credential_status(&vars);
    let names: Vec<&str> = statuses.iter().map(|s| s.venue.as_str()).collect();
    assert_eq!(names, VENUES.to_vec());
}

#[test]
fn never_panics_on_arbitrary_vars() {
    let mut vars = HashMap::new();
    vars.insert("SOME_RANDOM_KEY".to_string(), "".to_string());
    vars.insert("".to_string(), "".to_string());
    let _ = credential_status(&vars);
}
