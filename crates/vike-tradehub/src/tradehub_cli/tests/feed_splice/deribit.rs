//! The deribit splice: scripted frames through the REAL arm reach the mounted strategy.

use super::*;

/// The label every scripted lane stamps — the venue's own slug, which is what the core dispatches on.
const VENUE: &str = "deribit";

/// The test's [`FeedCtors`] impl: `client` hands the arm the scripted double and `strat` (the CEX arm's
/// concrete dial) is refused — this test's plan is deribit's, which never asks for it.
struct ScriptedCtors {
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
}

impl FeedCtors for ScriptedCtors {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(format!("{venue}: this scripted constructor builds client() feeds only"))
    }

    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        assert_eq!(
            (venue, network),
            (VENUE, None),
            "the deribit arm asks for deribit, main network"
        );
        Ok(Box::new(ScriptedFeed::new(VENUE, true, sink, Arc::clone(&self.subs))))
    }
}

/// A HALT sentinel path THIS TEST owns and never creates — the same pinning
/// `crates/vike-tradehub/tests/daemon/support.rs`'s `own_sentinel` documents: a paper
/// mount is HALT-armed by design, so a test expecting fills must not inherit the operator's kill
/// switch off whatever box runs it.
fn own_sentinel() -> PathBuf {
    std::env::temp_dir()
        .join(format!("vike-tradehub-feed-splice-seam-{}", std::process::id()))
        .join("HALT")
}
/// THE SPLICE, deterministically: scripted Deribit frames through the REAL `wire_venue_feeds`
/// deribit arm reach the core labelled for the mount's dispatch key, and the mounted strategy
/// acts. The fill is the assert because this core has no other feed and no other order source —
/// so a fill in the `(deribit, wired-symbol)` paper book is EQUIVALENT to "a scripted venue
/// event crossed sink → core lane → the mount's own key → `buy_hold` → the paper book" (the
/// smoke's own argument, made deterministic). Red-proven against the planted cross-label mutant
/// (the arm's `subscribe_bars` handed a foreign literal): the scripted lane then emits under the
/// wrong key, no bar reaches the mount or its paper book, and the fill wait times out.
#[test]
fn a_scripted_deribit_frame_reaches_the_mounted_strategy_through_the_arms_own_wiring() {
    let symbol = super::wired_symbol("deribit").expect("assemble_runtime mounts deribit");
    let mut cfg = MakerConfig::crypto("deribit", symbol, 0.5, 10.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;

    // The REAL gate, over an EMPTY credential map — the keyless arm's own property.
    // The all-paper default policy is inert here — this call exercises the deribit arm, which never
    // reads it.
    let plan = venue_feed_plan(
        &cfg,
        &vike_tradehub_core::ArmingTable::default(),
        &vike_tradehub_core::AdapterPolicy::default(),
    )
    .expect("keyless deribit plans on an empty store");

    // The paper-seam mount (multi_mount_profile.rs's shape): `buy_hold` on the deribit dispatch
    // key, HALT pinned to a sentinel this test owns.
    let sentinel = own_sentinel();
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders: {}",
        sentinel.display()
    );
    let strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> =
        Box::new(vike_strategy::BuyHold::new(0.001, None));
    let mount = build_paper_multi_strategy_core_with(
        vec![StrategyMountSpec { strategy, spec: cfg.mount_spec() }],
        PaperCoreOpts { halt: PaperHalt::Pinned(sentinel), ..Default::default() },
    );

    // The REAL arm, over the scripted constructor. `wrap` is the identity — exactly
    // `live_runtime`'s no-recording default.
    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let ctors = ScriptedCtors { subs: Arc::clone(&subs) };
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    let live_venues: HashSet<String> = HashSet::new();
    // No `data_only` declaration — deribit's data plane is keyless, and the empty set is every
    // undeclared mount's value (the byte-identical default this test keeps pinning).
    let data_only: HashSet<String> = HashSet::new();
    let mut feeds = wire_venue_feeds(
        &plan,
        &[&cfg],
        &mount.handle,
        &live_venues,
        &[],
        &data_only,
        &wrap,
        &ctors,
    )
    .expect("the deribit arm subscribes a scripted feed without error");

    // THE SPLICE. Deliberately FIRST, before the label asserts below: this is the behavioural
    // half, and it must be the one that fails on a cross-labelled arm (a declaration check alone
    // would be the pin-without-a-gate shape this repo distrusts).
    let book = mount
        .fills
        .iter()
        .find(|((v, s), _)| v == "deribit" && s == symbol)
        .map(|(_, f)| Arc::clone(f))
        .expect("the paper mount built a (deribit, wired-symbol) book");
    assert!(
        wait_until(10, || !book.lock().expect("fills").is_empty()),
        "no paper fill within 10s: no scripted venue event reached the mounted strategy through \
         the arm's own wiring (subscriptions the arm made: {:?})",
        subs.lock().expect("subs")
    );

    // The LABEL half: the arm subscribed THIS mount's series — every declared verb, the mount's
    // own key, once each (the per-series dedup rule; one mount ⇒ one of each; `subscribe_depth`
    // deliberately absent, the caps refusal the arm documents).
    let got = subs.lock().expect("subs").clone();
    let expect: Vec<(&'static str, String)> = vec![
        ("bars", format!("{symbol}@1m")),
        ("quotes", symbol.to_string()),
        ("trades", symbol.to_string()),
        ("book", symbol.to_string()),
    ];
    assert_eq!(got, expect, "the arm's subscription set IS the mount's own dispatch key");

    // Teardown through the arm's own handle — the production `shutdown` dispatch.
    feeds.shutdown();
    mount.handle.shutdown_and_join();
}
