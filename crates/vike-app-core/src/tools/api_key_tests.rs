//! [`ToolApiKeys`]' precedence, and the two properties the injection exists to buy: this
//! module reads NO global state, and there is no CWD-relative store left to read.
//!
//! The tier that WON used to be decided by `std::env::var` + a `./.env` read inside the fetch
//! thread, so neither could be exercised without mutating the process (unsound under threads)
//! or writing a file into whatever directory the test runner happened to start in.

use super::{FINNHUB_API_KEY, FMP_API_KEY, ToolApiKeys};
use std::collections::HashMap;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

#[test]
fn the_process_environment_outranks_the_store() {
    let keys = ToolApiKeys::resolve(
        &map(&[(FINNHUB_API_KEY, "from-env")]),
        &map(&[(FINNHUB_API_KEY, "from-store"), (FMP_API_KEY, "fmp-store")]),
    );
    assert_eq!(keys.finnhub.as_deref(), Some("from-env"));
    // ...and a key the environment does not mention still comes from the store.
    assert_eq!(keys.fmp.as_deref(), Some("fmp-store"));
}

#[test]
fn a_blank_value_falls_through_instead_of_winning() {
    // Both tiers, both directions: whitespace is not an answer at either level. An empty
    // env value must not shadow a real stored key (the pre-injection behaviour), and an empty
    // stored value must not resolve to `Some("")` and send an unauthenticated fetch.
    let keys = ToolApiKeys::resolve(
        &map(&[(FINNHUB_API_KEY, "   "), (FMP_API_KEY, "")]),
        &map(&[(FINNHUB_API_KEY, "real")]),
    );
    assert_eq!(keys.finnhub.as_deref(), Some("real"));
    assert_eq!(keys.fmp, None);
}

/// ⚠ The CWD-relative regression, stated as an assertion: **empty maps in ⇒ no keys out**,
/// for every working directory — including one holding a populated `.env`.
///
/// The old reader answered this case from `./.env`, so the result depended on where the binary
/// was launched from and no test could pin it without `set_current_dir` (process-global, and a
/// race in a threaded harness). Purity is what makes the property assertable at all.
#[test]
fn values_are_trimmed_and_absent_keys_are_none() {
    let keys = ToolApiKeys::resolve(&HashMap::new(), &map(&[(FMP_API_KEY, "  padded  ")]));
    assert_eq!(keys.fmp.as_deref(), Some("padded"));
    assert_eq!(keys.finnhub, None);
    // Two empty maps is the ordinary no-keys case, not an error: the calendar day-strip simply
    // shows no earnings/dividend counts.
    assert_eq!(ToolApiKeys::resolve(&HashMap::new(), &HashMap::new()), ToolApiKeys::default());
}

#[test]
fn debug_redacts_the_keys() {
    let keys = ToolApiKeys::resolve(
        &map(&[(FINNHUB_API_KEY, "secret-finnhub"), (FMP_API_KEY, "secret-fmp")]),
        &HashMap::new(),
    );
    let shown = format!("{keys:?}");
    assert!(!shown.contains("secret-finnhub"), "Debug leaked a key: {shown}");
    assert!(!shown.contains("secret-fmp"), "Debug leaked a key: {shown}");
    assert!(shown.contains("<set>"), "and still says which are configured: {shown}");
    assert!(format!("{:?}", ToolApiKeys::default()).contains("<unset>"));
}
