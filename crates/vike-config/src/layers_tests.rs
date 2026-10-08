use super::*;

#[test]
fn a_flag_accepts_only_the_exact_strings() {
    assert!(parse_flag("VIKE_RECONCILE", "1").unwrap());
    assert!(!parse_flag("VIKE_RECONCILE", "0").unwrap());
    for bad in ["true", "yes", "on", "1 ", "TRUE"] {
        let err = parse_flag("VIKE_RECONCILE", bad).unwrap_err();
        assert!(err.to_string().contains("VIKE_RECONCILE"), "{err}");
        assert!(err.to_string().contains(bad), "{err}");
    }
}

// NB every env-shaped string literal anywhere under `crates/` is harvested by `vike-ops`'
// settings-registry gate and must already carry a `SETTINGS` row — including the ones in
// tests. Reuse declared names here rather than inventing a placeholder.
#[test]
fn a_non_numeric_worker_count_names_the_variable_and_the_value() {
    let err = parse_usize("VIKE_SWEEP_THREADS", "lots").unwrap_err();
    assert_eq!(err.to_string(), "VIKE_SWEEP_THREADS=lots: expected a non-negative whole number");
}

#[test]
fn an_empty_variable_reads_as_unset() {
    let env = HashMap::from([("A".to_string(), String::new()), ("B".to_string(), "x".into())]);
    assert_eq!(get(&env, "A"), None);
    assert_eq!(get(&env, "B"), Some("x"));
    assert_eq!(get(&env, "C"), None);
}
