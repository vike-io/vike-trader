use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_polymarket::discovery::{FetchSpec, GammaSource};
use vike_polymarket::gamma::GammaMarket;

use super::*;

/// A scripted Gamma directory: the markets it lists, a switch that takes it down, and a count of the
/// reads it was asked for.
struct Fixture {
    markets: Vec<GammaMarket>,
    down: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
}

impl Fixture {
    fn answer<T>(&self, ok: impl FnOnce() -> T) -> Result<T, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.down.load(Ordering::SeqCst) {
            Err("gamma: connection reset".to_string())
        } else {
            Ok(ok())
        }
    }
}

impl GammaSource for Fixture {
    fn list(&self, _: bool, _: usize, _: usize) -> Result<Vec<GammaMarket>, String> {
        self.answer(|| self.markets.clone())
    }

    fn by_slug(&self, slug: &str, _: FetchSpec) -> Result<Option<GammaMarket>, String> {
        self.answer(|| self.markets.iter().find(|m| m.slug == slug).cloned())
    }
}

fn market(slug: &str, tokens: &[&str]) -> GammaMarket {
    GammaMarket {
        slug: slug.into(),
        condition_id: format!("0x{slug}"),
        active: true,
        tick_size: 0.01,
        outcomes: vec!["Up".into(), "Down".into()],
        token_ids: tokens.iter().map(|s| (*s).to_string()).collect(),
        ..Default::default()
    }
}

/// What a suite watches: the hub, the Gamma switch and the two counters.
struct Rig {
    hub: FamilyHub,
    down: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    built: Arc<AtomicUsize>,
}

/// A hub over a scripted directory listing `markets`.
fn rig(markets: Vec<GammaMarket>) -> Rig {
    let down = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let built = Arc::new(AtomicUsize::new(0));
    let (d, r, b) = (Arc::clone(&down), Arc::clone(&reads), Arc::clone(&built));
    let hub = FamilyHub::with_factory(Box::new(move |family: &str| {
        b.fetch_add(1, Ordering::SeqCst);
        let gamma =
            Fixture { markets: markets.clone(), down: Arc::clone(&d), reads: Arc::clone(&r) };
        Ok(Box::new(RollingFamily::with_source(family, Box::new(gamma), FetchSpec::default())?)
            as Box<dyn SymbolResolver>)
    }));
    Rig { hub, down, reads, built }
}

fn two_windows(prefix: &str) -> Vec<GammaMarket> {
    vec![
        market(&format!("{prefix}-5m-300"), &["A_UP", "A_DOWN"]),
        market(&format!("{prefix}-5m-600"), &["B_UP", "B_DOWN"]),
    ]
}

/// 1970-01-01T00:07:30Z: the 00:05 window is current and the 00:10 one is the look-ahead.
const NOW: i64 = 450_000;

/// The answer is the current window's tokens plus the next one's, ascending.
#[test]
fn the_desired_set_is_the_current_plus_next_windows_tokens_ascending() {
    let rig = rig(two_windows("btc-updown"));
    let scope = ConnectionScope::new();
    assert_eq!(
        rig.hub.resolve(&scope, "btc-updown-5m", NOW),
        Ok(vec!["A_DOWN".into(), "A_UP".into(), "B_DOWN".into(), "B_UP".into()])
    );
}

/// **A directory outage is `Err` and never the empty set**: an empty answer would unsubscribe every
/// live book mid-outage. The words are the directory's own.
#[test]
fn a_gamma_outage_is_an_error_and_never_an_empty_set() {
    let rig = rig(two_windows("btc-updown"));
    let scope = ConnectionScope::new();
    rig.down.store(true, Ordering::SeqCst);
    let out = rig.hub.resolve(&scope, "btc-updown-5m", NOW);
    let text = out.expect_err("an outage is UNKNOWN, not empty");
    assert!(text.contains("gamma: connection reset"), "{text}");
}

/// The planner survives a failed pass (it plans without committing), so recovery needs no rebuild:
/// the same planner answers once the directory is back.
#[test]
fn a_failed_pass_leaves_the_planner_in_place_and_the_next_pass_recovers() {
    let rig = rig(two_windows("btc-updown"));
    let scope = ConnectionScope::new();
    rig.down.store(true, Ordering::SeqCst);
    assert!(rig.hub.resolve(&scope, "btc-updown-5m", NOW).is_err());
    rig.down.store(false, Ordering::SeqCst);
    let set = rig.hub.resolve(&scope, "btc-updown-5m", NOW).expect("the directory is back");
    assert_eq!(set.len(), 4);
    assert_eq!(rig.built.load(Ordering::SeqCst), 1, "one planner for the connection's family");
}

/// A legitimately empty set is `Ok(empty)` — a statement, which DOES unsubscribe — and is told apart
/// from the failure above by being `Ok`.
#[test]
fn a_directory_that_lists_nothing_is_an_empty_set_not_an_error() {
    let rig = rig(Vec::new());
    let scope = ConnectionScope::new();
    assert_eq!(rig.hub.resolve(&scope, "btc-updown-5m", NOW), Ok(Vec::new()));
}

/// **The grammar check runs first**: a family with no trailing `-<N>m` is refused with the words the
/// bridge returns, before a planner is built or the directory is asked.
#[test]
fn the_grammar_check_runs_first_and_builds_nothing() {
    let rig = rig(two_windows("btc-updown"));
    let scope = ConnectionScope::new();
    for refused in ["btc-updown-1h", "btc-updown-0m", "some-event-slug", ""] {
        let text = rig.hub.resolve(&scope, refused, NOW).expect_err("not a rolling family");
        assert_eq!(
            text,
            vike_catalog::polymarket_family_minutes(refused).expect_err("the catalog refuses it")
        );
    }
    assert_eq!(rig.built.load(Ordering::SeqCst), 0);
    assert_eq!(rig.reads.load(Ordering::SeqCst), 0);
    assert!(scope.is_empty(), "a refused family must not even create the connection's slot");
}

/// **One planner per `(connection, family)`**: a second ask on the same connection reuses the
/// planner (and its window cache: zero further directory reads inside the window), a second FAMILY
/// builds a second planner, and a second CONNECTION starts from nothing.
#[test]
fn the_planner_is_per_connection_and_family() {
    let mut markets = two_windows("btc-updown");
    markets.extend(two_windows("eth-updown"));
    let rig = rig(markets);

    let first = ConnectionScope::new();
    rig.hub.resolve(&first, "btc-updown-5m", NOW).expect("btc");
    let reads_after_first = rig.reads.load(Ordering::SeqCst);
    assert!(reads_after_first > 0, "guard: the first resolve read the directory");
    rig.hub.resolve(&first, "btc-updown-5m", NOW).expect("btc again");
    assert_eq!(
        rig.reads.load(Ordering::SeqCst),
        reads_after_first,
        "a re-tick inside the window must be answered from the planner's cache"
    );
    assert_eq!(rig.built.load(Ordering::SeqCst), 1);

    rig.hub.resolve(&first, "eth-updown-5m", NOW).expect("eth");
    assert_eq!(rig.built.load(Ordering::SeqCst), 2, "a second family is a second planner");

    let second = ConnectionScope::new();
    rig.hub.resolve(&second, "btc-updown-5m", NOW).expect("btc on another connection");
    assert_eq!(rig.built.load(Ordering::SeqCst), 3, "a second connection starts from nothing");
    assert!(rig.reads.load(Ordering::SeqCst) > reads_after_first);
}

/// A connection holds a bounded number of planners; the one past the cap is refused, not evicted.
#[test]
fn a_connection_holds_a_bounded_number_of_families() {
    let rig = rig(Vec::new());
    let scope = ConnectionScope::new();
    for minutes in 1..=MAX_FAMILIES_PER_CONNECTION {
        rig.hub
            .resolve(&scope, &format!("btc-updown-{minutes}m"), NOW)
            .unwrap_or_else(|e| panic!("family {minutes}: {e}"));
    }
    let past = MAX_FAMILIES_PER_CONNECTION + 1;
    let text = rig.hub.resolve(&scope, &format!("btc-updown-{past}m"), NOW).expect_err("capped");
    assert!(text.contains("is refused") && text.contains("nothing was asked"), "{text}");
    assert_eq!(rig.built.load(Ordering::SeqCst), MAX_FAMILIES_PER_CONNECTION);
    // A family already held still answers.
    rig.hub.resolve(&scope, "btc-updown-1m", NOW).expect("a held family still answers");
}
