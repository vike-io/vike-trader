//! The policy the REAL live mount hands the venue backend carries the fail-closed venue list
//! (decision 0174 item 7), with or without a `data_only` venue.
//!
//! It was set INSIDE the `for venue in &data_only` loop, so a box with no data-only venue (every
//! normal box) kept the empty default and every engine ran with `deny_unadmitted_symbols = false`:
//! a lost, refused or racing admission was signed on the mounted symbol's grid. The earlier tests set
//! the flag by hand, which is why they could not see it. These drive `live_runtime_with` and read
//! what the BACKEND was handed, which is the value the engines are assembled from.

use super::oanda::{ScriptedOandaCtors, fill_sized_budget, oanda_armed_policy};
use super::*;

/// A backend constructor that records the `fail_closed_symbol_venues` of every policy it is asked to
/// build a backend over (the plan loop's, then the mount's), and otherwise is [`LocalCtor`].
struct Recording {
    seen: Mutex<Vec<Vec<String>>>,
}

impl BackendCtor for Recording {
    fn backend(
        &self,
        vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        data_only: &std::collections::HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend> {
        let mut venues = policy.fail_closed_symbol_venues.clone();
        venues.sort();
        self.seen.lock().unwrap().push(venues);
        LocalCtor.backend(vars, policy, data_only)
    }
}

/// The venues whose adapters sign by the order's symbol, spelled out here on purpose: the list the
/// mount uses is derived from the door's table, and a test that derived its expectation from the
/// same table could not fail.
const ADMITTING: [&str; 5] = ["aster", "binance", "bybit", "hyperliquid", "okx"];

fn mount_and_read(data_only: bool) -> Vec<Vec<String>> {
    let ctor = Recording { seen: Mutex::new(Vec::new()) };
    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let ctors = ScriptedOandaCtors { subs };
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let (handle, teardown, _live, _late, _not_running) = super::seam_live_runtime_over(
        vec![super::buy_hold_mount("oanda", data_only)],
        Some(fill_sized_budget()),
        &oanda_armed_policy("fake-fail-closed-token"),
        HashMap::new(),
        lock_dir.path(),
        &ctors,
        &ctor,
    )
    .expect("the live mount stands up over fake keys and a paper account");
    super::tear_down(handle, teardown);
    ctor.seen.into_inner().unwrap()
}

/// THE REGRESSION: a profile with NO `data_only` venue. Every backend the mount asked for carries
/// the admitting venues.
#[test]
fn a_mount_with_no_data_only_venue_still_fails_closed_for_the_admitting_venues() {
    let seen = mount_and_read(false);
    assert!(!seen.is_empty(), "the mount asked for a backend");
    for (i, venues) in seen.iter().enumerate() {
        assert_eq!(venues, &ADMITTING, "backend request {i} carried {venues:?}");
    }
}

/// And with one: the declared-venue loop changes nothing about it.
#[test]
fn a_data_only_mount_carries_the_same_list() {
    for venues in mount_and_read(true) {
        assert_eq!(venues, ADMITTING);
    }
}
