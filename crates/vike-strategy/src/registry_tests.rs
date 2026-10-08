use super::*;

/// A minimal concrete [`HftBroker`] to instantiate the generic resolver at. `vike_model`'s own
/// `MockBroker` implements [`Broker`] but NOT [`HftBroker`] (the maker arms need the tagged
/// verbs), and the two REAL brokers — `SimBroker` / `LiveBroker` — both live ABOVE this crate,
/// so a local double is the only way to run these here. Instantiating at ONE broker is enough:
/// a generic function's body is type-checked at its definition, so the arms are proven for
/// every `B: HftBroker`.
///
/// It COUNTS submits (and nothing else): `the_empty_symbol_this_echo_reports_really_does_stop_
/// the_strategy` needs to tell "routed an order" from "routed nothing", which a broker that
/// swallows every call cannot answer. Counting rather than recording keeps the double a probe.
#[derive(Default)]
struct ProbeBroker {
    submits: usize,
}

impl Broker for ProbeBroker {
    fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {
        self.submits += 1;
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {
        self.submits += 1;
    }
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}

impl HftBroker for ProbeBroker {
    fn position(&self) -> f64 {
        0.0
    }
    fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {
        self.submits += 1;
    }
    fn modify_tagged(&mut self, _tag: &str, _q: Option<f64>, _p: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}

/// [`ProbeBroker`]'s RECORDING sibling — it keeps the `(symbol, side, qty)` of every submission
/// instead of counting them, because the two route tests below are about WHAT reached the broker
/// and not how often. Kept separate rather than widening `ProbeBroker`: that one is deliberately
/// a counter (its own doc says so), and the tests that use it read `submits`.
#[derive(Default)]
struct RecordingBroker {
    submitted: Vec<(String, i32, f64)>,
    price: f64,
}

impl Broker for RecordingBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.submitted.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, _price: f64) {
        self.submitted.push((symbol.to_string(), side, qty));
    }
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.price
    }
    fn equity(&self) -> f64 {
        1_000.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}

impl HftBroker for RecordingBroker {
    fn position(&self) -> f64 {
        0.0
    }
    fn submit_limit_tagged(&mut self, _tag: &str, side: i32, qty: f64, _price: f64) {
        self.submitted.push((String::new(), side, qty));
    }
    fn modify_tagged(&mut self, _tag: &str, _q: Option<f64>, _p: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}

/// A bar the RUNTIME would deliver: symbol-stamped with the MOUNT's own instrument
/// (`crates/vike-core/src/runtime/dispatch.rs`'s `BarClose` arm sets `bar.symbol = Some(key.1)`).
fn mounted_bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("MOUNTED".to_string()),
    }
}

fn resolve(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<ProbeBroker> + Send>, RegistryError> {
    strategy_by_name::<ProbeBroker>(name, params)
}

fn empty() -> Value {
    Value::Table(Default::default())
}

#[test]
fn registry_lists_every_match_arm() {
    // Every name PORTABLE_STRATEGIES advertises must actually resolve with DEFAULT params —
    // keeps the const in sync with the match by construction, not by convention.
    for name in PORTABLE_STRATEGIES {
        assert!(resolve(name, &empty()).is_ok(), "{name} should resolve");
    }
}

#[test]
fn unknown_name_is_a_registry_error_naming_the_roster() {
    match resolve("nope", &empty()) {
        Err(e @ RegistryError::Unknown(_)) => {
            let msg = e.to_string();
            assert!(msg.contains("nope"), "names the typo: {msg}");
            assert!(msg.contains("buy_hold"), "names the roster: {msg}");
        }
        other => panic!("expected Unknown, got Ok={}", other.is_ok()),
    }
}

/// The registry's whole reason for existing: ONE resolver, TWO brokers. A generic function's
/// body is type-checked at its DEFINITION, so this compiling IS the proof that every arm holds
/// for every `B: HftBroker` — including `vike_core::LiveBroker`, which this crate cannot name.
#[test]
fn the_resolver_is_generic_over_every_hft_broker() {
    fn probe<B: HftBroker + 'static>() {
        let _ = strategy_by_name::<B>("buy_hold", &Value::Table(Default::default()));
    }
    probe::<ProbeBroker>();
}

/// The returned box must satisfy `vike_core::StrategyMount::strategy`'s `+ Send` bound — the
/// live core moves the strategy onto its own thread. Asserted here rather than trusted: a
/// future arm holding an `Rc` would compile everywhere else and fail only at the live mount.
#[test]
fn every_resolved_strategy_is_send() {
    fn assert_send<T: Send>(_t: &T) {}
    for name in PORTABLE_STRATEGIES {
        let s = resolve(name, &empty()).expect("resolves");
        assert_send(&s);
    }
}

#[test]
fn live_capable_table_is_exhaustive() {
    // Every roster name has exactly one verdict row, and no row names a strategy that is not on
    // the roster. Adding an arm without classifying it fails HERE, which is the point: an
    // unclassified strategy is one nobody decided could trade.
    for name in PORTABLE_STRATEGIES {
        assert_eq!(
            LIVE_CAPABLE.iter().filter(|(n, _)| n == name).count(),
            1,
            "{name} needs exactly one LIVE_CAPABLE row"
        );
    }
    for (name, _) in LIVE_CAPABLE {
        assert!(
            PORTABLE_STRATEGIES.contains(name),
            "LIVE_CAPABLE names {name}, which is not on the roster"
        );
    }
}

#[test]
fn every_not_live_row_carries_a_nonempty_reason() {
    // A `no` with no reason is an opinion; a `no` with a named missing input is a claim
    // somebody can check and later disprove.
    for (name, verdict) in LIVE_CAPABLE {
        if let Some(reason) = verdict.blocker() {
            assert!(reason.len() > 20, "{name}'s reason is too thin to act on: {reason:?}");
        }
    }
}

/// The PERMISSIVE arm's twin, and the one this table did not have until 2026-09-20.
///
/// ⚠ The asymmetry it removes was the defect: a refusal needed 20 characters of argument, while
/// `None` — the shortest row anybody can type — meant MOUNTABLE BY THE ORDER-SIGNING DAEMON and
/// needed nothing. Five of the six permissive rows did carry an argument, in prose above them,
/// and one did not; from the data the two were identical. 80 characters, matching
/// `crates/vike-tradehub/src/hot_reload_tests.rs`'s `every_hot_row_carries_a_written_reason`, which
/// calls this "the `LIVE_CAPABLE` idiom" while having been stricter than it.
#[test]
fn every_live_row_carries_a_written_reason() {
    for (name, verdict) in LIVE_CAPABLE {
        if let Liveness::Live { why_safe } = verdict {
            assert!(
                why_safe.len() >= 80 && !why_safe.to_ascii_lowercase().contains("todo"),
                "{name} is declared live-mountable and needs a real written argument for why \
                     that is safe — what its inputs are on a LIVE mount and why its orders are \
                     correct there; got {why_safe:?}"
            );
        }
    }
}

/// Names carrying [`Liveness::LiveUnargued`] — mountable live with no written argument.
///
/// ⚠ It lives HERE, in the test module, because it is read by exactly one test and a
/// module-level copy is dead code in a plain `cargo clippy` lib build — `-D warnings` then
/// fails eight gates on a tree whose whole test suite is green, which is how this was found.
/// The tree's other ratchets sit with their tests for the same reason
/// (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`).
///
/// ⚠ A RATCHET: it may shrink, never grow. A new strategy does not join it; a new strategy
/// writes its `why_safe` or its `blocker`. This exists only to make an inherited gap visible
/// instead of retiring it silently or inventing a justification for it, which is the one
/// thing the length check must never be walked around.
///
/// `momentum` is here because the row was `("momentum", None)` with no comment of its own,
/// sitting directly below `trailing_scalper`'s long refusal — the five other permissive rows
/// each carried a prose argument and this one never did. Whether it is safe live is a
/// question for whoever knows; what is recorded is that the tree does not say.
const UNARGUED_LIVE: [&str; 1] = ["momentum"];

/// [`UNARGUED_LIVE`] is a RATCHET: it may shrink, never grow.
///
/// A new strategy does not join it. It exists so the one inherited row nobody argued is
/// VISIBLE, rather than either hidden behind a shared comment (where it was) or given an
/// invented justification to satisfy the check above (which would make the check theatre).
#[test]
fn the_unargued_live_set_does_not_grow() {
    let found: Vec<&str> = LIVE_CAPABLE
        .iter()
        .filter(|(_, v)| matches!(v, Liveness::LiveUnargued))
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(
        found, UNARGUED_LIVE,
        "the unargued-live set changed. A row may LEAVE it — write that strategy's `why_safe` \
             (or its `blocker`) and delete its name from UNARGUED_LIVE, shrinking the array's \
             declared length. A row may not JOIN it: a new strategy carries its own argument."
    );
}

#[test]
fn simulator_only_and_portable_rosters_are_disjoint() {
    for (name, _) in SIMULATOR_ONLY {
        assert!(
            !PORTABLE_STRATEGIES.contains(name),
            "{name} is claimed by BOTH rosters — one of them is wrong"
        );
    }
}

#[test]
fn capability_distinguishes_the_four_answers() {
    assert_eq!(capability("spread_maker"), Capability::Live);
    assert!(matches!(capability("funding_capture"), Capability::NotLive(_)));
    assert!(matches!(capability("rotation_top_k"), Capability::SimulatorOnly(_)));
    assert_eq!(capability("nope"), Capability::Unknown);
}

/// The SCRIPT path's NAME is a real registry arm this crate cannot resolve — not a typo.
/// Before [`SCRIPT_ONLY`] existed, a profile naming it was told it did not exist, which is the
/// one answer that is simply false and sends an operator hunting for a misspelling. Since the
/// 0024 reversal the reason must ALSO point at the spelling that DOES mount a script live —
/// the daemon's `rhai = "<path>"` — because "resolves only in the backtest" without that
/// pointer reads as the pre-reversal "scripts cannot go live", which is no longer true.
///
/// ⚠ It must NOT cite the record itself, and this test asserted the opposite until 2026-09-04.
/// This string is EXPORTED — it reaches `templates.json` and renders onto
/// `vike.io/docs/trader/strategies/simulator-only`, whose reader's only view of this workspace
/// is the public source mirror, and the mirror publishes no `docs/` at all. So the citation was
/// a dead link on a public page, sitting in the one sentence a reader would want to follow.
/// The INFORMATION the record carries survives in the `rhai = "<path>"` pointer above, which is
/// the actionable half; the reasoning stays in this module's doc comments, which are not
/// published as documentation. `crates/vike-docs/tests/docs_data_gate.rs`'s
/// `no_rendered_asset_cites_a_path_the_mirror_withholds` is the gate for the whole class.
#[test]
fn the_script_strategy_is_named_rather_than_called_a_typo() {
    match capability("rhai") {
        Capability::SimulatorOnly(why) => {
            assert!(why.contains("src"), "names the param the NAME arm needs: {why}");
            assert!(why.contains("vike-script"), "names what does not link here: {why}");
            assert!(why.contains("rhai = "), "points at the daemon's live path spelling: {why}");
            assert!(
                !why.contains("docs/"),
                "an EXPORTED string may not cite a path the public mirror withholds: {why}"
            );
        }
        other => panic!("rhai must not read as {other:?}"),
    }
}

#[test]
fn script_only_is_disjoint_from_both_rosters() {
    for (name, why) in SCRIPT_ONLY {
        assert!(!PORTABLE_STRATEGIES.contains(name), "{name} is on the portable roster");
        assert!(
            !SIMULATOR_ONLY.iter().any(|(n, _)| n == name),
            "{name} is claimed by SIMULATOR_ONLY too — one of the two rows is wrong"
        );
        assert!(why.len() > 20, "{name}'s reason is too thin to act on: {why:?}");
    }
}

/// The `NotLive` rows are the ones that would SILENTLY never trade (or, for
/// `trailing_scalper`, trade HALF of what was backtested). Pin them by name: a future PR that
/// wires the missing input flips the row deliberately and updates this list, rather than a
/// rename quietly making a footgun mountable.
#[test]
fn the_not_live_set_is_exactly_the_known_gaps() {
    let not_live: Vec<&str> =
        LIVE_CAPABLE.iter().filter(|(_, v)| v.blocker().is_some()).map(|(n, _)| *n).collect();
    assert_eq!(
        not_live,
        vec!["trailing_scalper", "funding_carry", "funding_capture", "pairs_zscore"]
    );
}

#[test]
fn param_keys_table_is_exhaustive() {
    // Same construction as `live_capable_table_is_exhaustive`: adding a registry arm without
    // declaring what its params table may contain fails HERE, because an undeclared reader is
    // one whose typos a live mount cannot catch.
    for name in PORTABLE_STRATEGIES {
        assert_eq!(
            PARAM_KEYS.iter().filter(|(n, _)| n == name).count(),
            1,
            "{name} needs exactly one PARAM_KEYS row"
        );
    }
    for (name, keys) in PARAM_KEYS {
        assert!(
            PORTABLE_STRATEGIES.contains(name),
            "PARAM_KEYS names {name}, which is not on the roster"
        );
        match keys {
            ParamKeys::Declared(k) => {
                assert!(!k.is_empty(), "{name} declares an EMPTY key set — say NotEnumerated");
                let mut sorted: Vec<&str> = k.iter().map(|(n, _)| *n).collect();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(sorted.len(), k.len(), "{name} declares a key twice");
            }
            ParamKeys::NotEnumerated(why) => {
                assert!(why.len() > 20, "{name}'s NotEnumerated reason is too thin: {why:?}")
            }
        }
    }
}

/// A declared key whose NAME is route-shaped, in the vocabulary this workspace's params readers
/// actually use. The reverse-direction gate below forces a [`PARAM_ROUTES`] row for every one of
/// them, so a new strategy declaring `symbol` cannot inherit the inert-knob trap in silence.
///
/// ⚠ A NAME heuristic, deliberately, and it is a LOWER bound — a route key called `market` or
/// `instrument` would pass unseen. That is honest rather than tidy: the alternative is resolving
/// what a reader STORES a key into, which means guessing at bodies the
/// `crates/vike-strategy/tests/param_keys_gate.rs` scanner already declines to slice. The
/// heuristic can only ever over-fire (demanding a row for a key that turns out not to route),
/// and an over-fire is one written row, not a silent hole. `the_route_shape_gate_can_fail` is
/// its mutation self-test.
fn is_route_shaped(key: &str) -> bool {
    key == "symbol"
        || key.starts_with("symbol_")
        || key == "venue"
        || key == "venues"
        || key.starts_with("venue_")
}

#[test]
fn param_routes_table_is_exhaustive() {
    // Same construction as `param_keys_table_is_exhaustive` and for the harder reason: an
    // unclassified name is one whose route keys nothing refuses, and a route key nothing
    // refuses names an instrument real orders do not go to.
    for name in PORTABLE_STRATEGIES {
        assert_eq!(
            PARAM_ROUTES.iter().filter(|(n, _)| n == name).count(),
            1,
            "{name} needs exactly one PARAM_ROUTES row"
        );
    }
    for (name, routes) in PARAM_ROUTES {
        assert!(
            PORTABLE_STRATEGIES.contains(name),
            "PARAM_ROUTES names {name}, which is not on the roster"
        );
        match routes {
            ParamRoutes::SingleLeg(_) => {}
            ParamRoutes::MultiLeg(keys, why) => {
                assert!(
                    !keys.is_empty(),
                    "{name} is MultiLeg with no route key — say SingleLeg(&[])"
                );
                assert!(why.len() > 20, "{name}'s MultiLeg reason is too thin to act on: {why:?}");
            }
            ParamRoutes::NotEnumerated(why) => assert!(
                why.len() > 20,
                "{name}'s NotEnumerated reason is too thin to act on: {why:?}"
            ),
        }
    }
}

/// Direction 1: a route row may only name keys that name something.
///
/// The key must be a DECLARED [`PARAM_KEYS`] key of the same name (a route row over a key no
/// reader reads would refuse a profile for a knob that never existed), and its declared
/// [`ParamType`] must match what [`misrouted_params`] reads it through — `Str` for a
/// `Symbol`/`Venue`, `Table` for a `VenueMap`. That second half is what stops the two tables
/// drifting into a check that silently never fires: `misrouted_params` reads a `Symbol` key
/// with `as_str`, so a key declared `Number` would match nothing, forever, green.
#[test]
fn every_route_key_is_a_declared_key_of_the_right_type() {
    for (name, routes) in PARAM_ROUTES {
        let keys: &[(&str, RouteKind)] = match routes {
            ParamRoutes::SingleLeg(k) | ParamRoutes::MultiLeg(k, _) => k,
            ParamRoutes::NotEnumerated(_) => {
                // Mirrors its PARAM_KEYS row, and must: a name whose key set is unknown cannot
                // have a known route subset.
                assert!(
                    matches!(param_keys(name), Some(ParamKeys::NotEnumerated(_))),
                    "{name}'s PARAM_ROUTES row is NotEnumerated but its PARAM_KEYS row is not"
                );
                continue;
            }
        };
        let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
            panic!("{name} declares route keys but enumerates no params keys");
        };
        for (key, kind) in keys {
            let Some((_, ty)) = declared.iter().find(|(n, _)| n == key) else {
                panic!("{name}'s route key `{key}` is not a declared PARAM_KEYS key");
            };
            let want = match kind {
                RouteKind::Symbol | RouteKind::Venue => ParamType::Str,
                RouteKind::VenueMap => ParamType::Table,
            };
            assert_eq!(
                *ty, want,
                "{name}'s route key `{key}` is {kind:?}, which `misrouted_params` reads as \
                     {want:?} — but PARAM_KEYS declares it {ty:?}, so the check would never fire"
            );
        }
    }
}

/// Direction 2: a route-shaped declared key may not go unclassified.
///
/// This is the direction that matters for a FUTURE strategy. `buy_hold`'s `symbol` was read,
/// well-typed and echoed for three review rounds while the mount overrode it; the only thing
/// that stops the fourth instance is a gate that fires when somebody declares the next one.
#[test]
fn every_route_shaped_declared_key_has_a_route_row() {
    for (name, keys) in PARAM_KEYS {
        let ParamKeys::Declared(declared) = keys else {
            continue;
        };
        let routed: Vec<&str> = match param_routes(name) {
            Some(ParamRoutes::SingleLeg(k)) | Some(ParamRoutes::MultiLeg(k, _)) => {
                k.iter().map(|(n, _)| *n).collect()
            }
            _ => Vec::new(),
        };
        for (key, _) in declared.iter() {
            if is_route_shaped(key) {
                assert!(
                    routed.contains(key),
                    "{name} declares the route-shaped key `{key}` with no PARAM_ROUTES row: a \
                         mount would OVERRIDE it (`resolve_intent_symbol`/`resolve_intent_venue`) \
                         and nothing would refuse the profile that set it"
                );
            }
        }
    }
}

/// The mutation self-test for the shape heuristic above — a gate nobody proved can fail is how
/// this defect class survived two rounds of review.
#[test]
fn the_route_shape_gate_can_fail() {
    assert!(is_route_shaped("symbol"), "the real key that carried the defect");
    assert!(is_route_shaped("symbol_a") && is_route_shaped("symbol_b"), "the two-leg spelling");
    assert!(is_route_shaped("venue") && is_route_shaped("venues"), "the venue half");
    // …and it must NOT swallow the knob keys, or direction 2 would demand a route row for every
    // key in the table and the distinction would carry no information.
    for knob in ["size", "qty", "step", "rungs", "tp", "sl", "cooldown_ms", "anchor_price"] {
        assert!(!is_route_shaped(knob), "`{knob}` is a knob, not a route");
    }
    // The gate's own input must be non-empty: if no declared key were route-shaped, direction 2
    // would pass vacuously forever.
    let route_shaped = PARAM_KEYS
        .iter()
        .filter_map(|(_, k)| match k {
            ParamKeys::Declared(d) => Some(d),
            ParamKeys::NotEnumerated(_) => None,
        })
        .flat_map(|d| d.iter())
        .filter(|(key, _)| is_route_shaped(key))
        .count();
    assert!(route_shaped > 0, "no declared key is route-shaped — direction 2 is vacuous");
}

/// `misrouted_params` reports exactly the values a mount would OVERRIDE, and nothing else.
///
/// Each assertion is a distinct arm of the rule rather than a restatement of it: the three
/// symbol states (absent / empty / naming another instrument), the venue half, the routing
/// table's per-row rule, and the two abstentions (multi-leg names, and a wrong TYPE, which is
/// `mistyped_params`' finding).
#[test]
fn misrouted_params_reports_only_what_the_mount_would_override() {
    let p = |src: &str| toml::from_str::<Value>(src).expect("test params parse");
    let keys = |src: &str| -> Vec<String> {
        misrouted_params("buy_hold", &p(src), "polymarket", "MOUNTED")
            .into_iter()
            .map(|m| m.key)
            .collect()
    };
    // ABSENT: the working default — the runtime stamps the mount's symbol onto the bar.
    assert!(keys("size = 1.0").is_empty());
    // EMPTY: a mount that cannot trade, which `resolved_params`' `opt_sym` already reports.
    assert!(keys("symbol = \"\"").is_empty());
    // AGREES: a no-op restatement of the mount, and legal.
    assert!(keys("symbol = \"MOUNTED\"").is_empty());
    // DISAGREES: the defect. One finding, naming BOTH instruments.
    let bad = misrouted_params("buy_hold", &p("symbol = \"OTHER\""), "polymarket", "MOUNTED");
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].key, "symbol");
    let said = bad[0].to_string();
    assert!(said.contains("OTHER") && said.contains("MOUNTED"), "names both: {said}");
    // A wrong TYPE is `mistyped_params`' finding — reporting it here too would hand the
    // operator two sentences about one slip, the second of them confusing.
    assert!(
        misrouted_params("buy_hold", &p("symbol = 7"), "polymarket", "MOUNTED").is_empty(),
        "a non-string symbol belongs to the TYPE check, not the ROUTE check"
    );

    // The VENUE half, on the one Live name that has one.
    let venue_keys = |src: &str| -> Vec<String> {
        misrouted_params("momentum", &p(src), "polymarket", "MOUNTED")
            .into_iter()
            .map(|m| m.key)
            .collect()
    };
    assert!(venue_keys("qty = 1.0").is_empty(), "an absent venue leaves the harness default");
    assert!(venue_keys("venue = \"polymarket\"").is_empty(), "agreeing is legal");
    assert_eq!(venue_keys("venue = \"binance\""), vec!["venue".to_string()]);
    // The routing TABLE, row by row: only this mount's own route is legal.
    assert!(venue_keys("[venues]\nMOUNTED = \"polymarket\"\n").is_empty());
    assert_eq!(
        venue_keys("[venues]\nMOUNTED = \"binance\"\n"),
        vec!["venues.MOUNTED".to_string()],
        "a row naming another VENUE is discarded by `resolve_intent_venue`"
    );
    assert_eq!(
        venue_keys("[venues]\nOTHER = \"polymarket\"\n"),
        vec!["venues.OTHER".to_string()],
        "a row naming another SYMBOL never matches the one series this mount receives"
    );
    assert_eq!(
        venue_keys("[venues]\nMOUNTED = 7\n"),
        vec!["venues.MOUNTED".to_string()],
        "a non-string row is dropped by `harness_venue_map`'s filter_map"
    );

    // MULTI-LEG names abstain: `symbol_a`/`symbol_b` name legs, not this mount's market.
    assert!(
        misrouted_params("pairs_zscore", &p("symbol_a = \"A\"\nsymbol_b = \"B\""), "v", "M")
            .is_empty(),
        "a two-leg name's keys are not claims about a single-leg mount"
    );
    // …and so does an unknown name, and a `NotEnumerated` one.
    assert!(misrouted_params("nope", &p("symbol = \"OTHER\""), "v", "M").is_empty());
    assert!(misrouted_params("spread_maker", &p("symbol = \"OTHER\""), "v", "M").is_empty());
}

/// The CLAIM behind the refusal, proven rather than asserted: with a params `symbol` that
/// disagrees with the dispatching series, the strategy really does submit under the name it was
/// handed — which is what the mount then overrides. If `BuyHold` ever started ignoring its own
/// `symbol` field, the refusal would be guarding nothing and this goes red.
#[test]
fn a_disagreeing_params_symbol_really_is_what_the_strategy_submits() {
    let params: Value = toml::from_str("size = 3.0\nsymbol = \"OTHER\"\n").unwrap();
    let mut s = BuyHold::from_params(&params);
    let mut broker = RecordingBroker::default();
    // The bar the RUNTIME delivers carries the MOUNT's symbol; the params key overrides it here,
    // and the mount then overrides it back — which is the whole defect.
    s.on_bar(&mut broker, &mounted_bar(1, 1.0));
    assert_eq!(
        broker.submitted.iter().map(|(s, _, _)| s.as_str()).collect::<Vec<_>>(),
        vec!["OTHER"],
        "the params symbol must be what reaches the broker — otherwise the mount's override \
             (and the refusal that now prevents it) would be guarding nothing"
    );
}

/// The DECLARED RESIDUAL on the `momentum` route row, proven rather than asserted: the harness's
/// `venue` is a LABEL, not an order destination.
///
/// This is what licenses [`resolved_params`] to keep echoing `venue=sim` for a mount that has no
/// `venue` key (`the_echo_reports_the_resolution_and_not_the_input` in
/// `crates/vike-tradehub/src/config.rs` pins that deliberately) instead of that being a fourth
/// instance of the "a diagnostic claims something false" class. Two harnesses that differ ONLY in
/// their venue tag, driven over identical bars, must produce IDENTICAL submissions — because
/// [`vike_model::Broker`]'s submit verbs take no venue at all, so the tag has nowhere to go.
///
/// MUTATION: make the two params tables agree (`venue = "one"` on both) and the equality below
/// holds for the trivial reason instead of the real one — hence the non-vacuity assert.
#[test]
fn the_harness_venue_is_a_label_and_not_an_order_destination() {
    let drive = |venue: &str| -> Vec<(String, i32, f64)> {
        let params: Value = toml::from_str(&format!("qty = 2.0\nvenue = \"{venue}\"\n")).unwrap();
        // NOT the `resolve` helper above — that one is pinned to `ProbeBroker`, which counts
        // rather than records. Same registry call, instantiated at the recording double.
        let mut s =
            strategy_by_name::<RecordingBroker>("momentum", &params).expect("momentum resolves");
        let mut broker = RecordingBroker::default();
        // Two bars with a rising price: the controller declines its first invitation (no
        // reference yet) and opens on the second, at the default `threshold = 0`.
        for (i, px) in [1.0_f64, 2.0].into_iter().enumerate() {
            broker.price = px;
            s.on_bar(&mut broker, &mounted_bar(60_000 * (i as i64 + 1), px));
        }
        broker.submitted
    };
    let one = drive("A_VENUE");
    let other = drive("ANOTHER_VENUE");
    assert!(
        !one.is_empty(),
        "the harness submitted nothing, so the equality below would hold vacuously"
    );
    assert_eq!(
        one, other,
        "two harnesses differing ONLY in their venue tag produced DIFFERENT orders — the tag \
             would then be an order destination and echoing `venue=sim` really would be a false \
             claim about where orders go"
    );
    // ...and the orders carry the MOUNT's symbol either way — the harness routes off the bar.
    assert!(one.iter().all(|(sym, _, _)| sym == "MOUNTED"), "{one:?}");
}

/// [`PARAM_GATES`]' structural direction: a row may only name keys that exist, on a name that
/// exists, once. Same construction as `every_route_key_is_a_declared_key_of_the_right_type` and
/// for the same reason — a gate over a key no reader reads would exempt a knob that never
/// existed from the class-closer, and a gate READING a key [`resolved_params`] does not carry
/// would render `(absent)` forever, exempting its key unconditionally.
#[test]
fn every_gate_names_a_declared_key_of_the_same_strategy() {
    for (name, key, gate) in PARAM_GATES {
        let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
            panic!("PARAM_GATES names {name}, which declares no params keys")
        };
        assert!(
            declared.iter().any(|(n, _)| n == key),
            "{name}'s gate is over `{key}`, which is not a declared PARAM_KEYS key"
        );
        for read in gate.keys() {
            assert!(
                declared.iter().any(|(n, _)| *n == read),
                "{name}'s gate on `{key}` reads `{read}`, which {name} does not declare — \
                     `resolved_params` would never carry it and the gate would read `(absent)` \
                     forever"
            );
            assert_ne!(
                read, *key,
                "{name}'s gate on `{key}` reads `{key}` — a key whose OWN value picks a branch \
                     is CONSUMED in both branches, which is the opposite of this table's claim"
            );
        }
        assert_eq!(
            PARAM_GATES.iter().filter(|(n, k, _)| n == name && k == key).count(),
            1,
            "{name}'s `{key}` has more than one gate row — say All(&[..])"
        );
    }
}

/// The gate's own mutation self-test — [`Gate::unmet`] is pure, so prove it says NOTHING for a
/// met gate and names the OFFENDING key for an unmet one, in every variant. Without this the
/// class-closer's exemptions could be vacuously green: a gate that never reports an unmet
/// conjunct exempts its key from direction 2 while direction 1 has nothing to measure.
#[test]
fn the_gate_predicate_can_actually_fail() {
    let rows = |pairs: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
        pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
    };
    let fixed = rows(&[("anchor", "fixed")]);
    let first = rows(&[("anchor", "first")]);
    assert!(Gate::Is("anchor", &["fixed"]).unmet(&fixed).is_empty());
    assert_eq!(Gate::Is("anchor", &["fixed"]).unmet(&first), vec!["anchor=first".to_string()]);
    // Positive is a NUMERIC test, so `0` and a negative both close it and a non-number does too.
    assert!(Gate::Positive("rungs").unmet(&rows(&[("rungs", "3")])).is_empty());
    assert_eq!(Gate::Positive("rungs").unmet(&rows(&[("rungs", "0")])), vec!["rungs=0"]);
    assert_eq!(Gate::Positive("size").unmet(&rows(&[("size", "-2")])), vec!["size=-2"]);
    assert_eq!(Gate::Positive("size").unmet(&rows(&[("size", "n/a")])), vec!["size=n/a"]);
    // `All` reports EVERY failing conjunct, so the operator sees all the reasons at once.
    let both = Gate::All(&[Gate::Positive("rungs"), Gate::Positive("size")]);
    assert!(both.unmet(&rows(&[("rungs", "3"), ("size", "1")])).is_empty());
    assert_eq!(
        both.unmet(&rows(&[("rungs", "0"), ("size", "0")])),
        vec!["rungs=0".to_string(), "size=0".to_string()]
    );
    // A key the echo does not carry is LOUD rather than silently "consumed".
    assert_eq!(Gate::Positive("nope").unmet(&fixed), vec!["nope=(absent)".to_string()]);
    // ...and `keys` reaches through `All`, which is what the structural gate walks.
    assert_eq!(both.keys(), vec!["rungs", "size"]);
}

#[test]
fn unknown_params_names_the_typo_and_passes_the_real_key() {
    let params: Value = toml::from_str("size = 2.0\nsizee = 3.0\nzzz = 1\n").unwrap();
    assert_eq!(unknown_params("buy_hold", &params), vec!["sizee".to_string(), "zzz".to_string()]);
    // A fully-recognised table is clean...
    let ok: Value = toml::from_str("size = 2.0\nsymbol = \"BTC\"\n").unwrap();
    assert!(unknown_params("buy_hold", &ok).is_empty());
    // ...a NotEnumerated row reports nothing (its consumer owes the stricter rule)...
    assert!(unknown_params("spread_maker", &params).is_empty());
    // ...and so does a name this registry does not resolve.
    assert!(unknown_params("nope", &params).is_empty());
}

/// A declared key carrying a value of the WRONG TYPE is reported, naming both types. This is
/// blocker 2's remaining half: `size = "2"` is a key the reader knows and a value it cannot
/// take, so `and_then(as_f64)` yields `None` and the knob mounts at its compiled default with
/// the profile stating otherwise.
#[test]
fn mistyped_params_names_the_key_and_both_types() {
    // The reviewer's own four examples, verbatim.
    let quoted: Value = toml::from_str("size = \"2\"\n").unwrap();
    let e = mistyped_params("grid", &quoted);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].key, "size");
    assert_eq!(e[0].got, "string");
    assert!(e[0].expected.contains("number"), "{:?}", e[0]);
    // ...and it really would have mounted the compiled default.
    assert_eq!(Grid::from_params(&quoted).size, Grid::default().size);

    let float_count: Value = toml::from_str("rungs = 4.0\n").unwrap();
    let e = mistyped_params("grid", &float_count);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].key.as_str(), e[0].got, e[0].expected), ("rungs", "float", "an integer"));
    assert_eq!(Grid::from_params(&float_count).rungs, Grid::default().rungs);

    let boolean: Value = toml::from_str("band = true\n").unwrap();
    let e = mistyped_params("grid", &boolean);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].key.as_str(), e[0].got), ("band", "boolean"));
    assert_eq!(Grid::from_params(&boolean).band, Grid::default().band);

    let e = mistyped_params("buy_hold", &toml::from_str("size = \"3\"\n").unwrap());
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].key.as_str(), e[0].got), ("size", "string"));
}

/// The abstentions, each for its own reason — the same three [`unknown_params`] has, plus the
/// one that matters most: a key at a type the reader DOES take is not an error, because a rule
/// refusing `qty = 1` where the reader happily takes `1.0` would break working profiles.
#[test]
fn mistyped_params_accepts_every_spelling_its_reader_accepts() {
    // The lenient numeric convention: BOTH spellings are legal for an `as_f64` key.
    for src in ["size = 2", "size = 2.0"] {
        assert!(
            mistyped_params("grid", &toml::from_str(src).unwrap()).is_empty(),
            "`{src}` must be accepted — `as_f64` takes either"
        );
    }
    // `read_side` genuinely takes EITHER, so both must pass.
    for src in ["side = \"short\"", "side = -1"] {
        assert!(
            mistyped_params("dca_accumulate", &toml::from_str(src).unwrap()).is_empty(),
            "`{src}` must be accepted — `read_side` takes either"
        );
    }
    // ...and a third type on that same key is still refused.
    assert_eq!(
        mistyped_params("dca_accumulate", &toml::from_str("side = 1.0").unwrap()).len(),
        1,
        "a float `side` reads as nothing in either arm of `read_side`"
    );
    // An UNKNOWN key is `unknown_params`' business, not this one's.
    assert!(mistyped_params("grid", &toml::from_str("sizee = \"2\"").unwrap()).is_empty());
    // A NotEnumerated row, an unknown name and a non-table all abstain.
    let bad: Value = toml::from_str("qty = \"2\"").unwrap();
    assert!(mistyped_params("spread_maker", &bad).is_empty());
    assert!(mistyped_params("nope", &bad).is_empty());
    assert!(mistyped_params("grid", &Value::Integer(3)).is_empty());
}

/// The FOURTH reader, and the case the three above all pass: every key spelled right, typed
/// right and routed right, and the ladder they describe between them is EMPTY.
///
/// The two rows that motivated it are the measured ones from
/// `crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger. The rows BELOW them matter as
/// much: this must be a rule about an empty ladder and never about a suspicious VALUE, or it
/// becomes the over-refusal it exists to avoid.
#[test]
fn unarmable_params_names_the_empty_ladder_and_its_resolution() {
    let at = |name: &str, src: &str| {
        unarmable_params(name, &toml::from_str::<Value>(src).expect("test TOML"))
    };
    // A FIXED anchor left at its compiled default price.
    let why = at("dca_accumulate", "anchor = \"fixed\"").expect("refused");
    assert!(why.contains("dca_accumulate"), "{why}");
    assert!(why.contains("NO rung"), "names what is wrong: {why}");
    assert!(why.contains("anchor=fixed"), "carries the resolution: {why}");
    assert!(why.contains("anchor_price=0"), "...including the knob left at its default: {why}");
    // A 0..1 grid at the compiled `step = 1.0`.
    let why = at("grid", "bounded01 = true").expect("refused");
    assert!(why.contains("bounded01=true") && why.contains("step=1"), "{why}");
    // ...and the degenerate ladder, which is the same defect: no rungs, no size, no spacing.
    for src in ["rungs = 0", "rungs = -5", "size = 0.0", "step = 0.0"] {
        assert!(at("grid", src).is_some(), "grid `{src}`");
        assert!(at("dca_accumulate", src).is_some(), "dca `{src}`");
    }
    // ⚠ The near-misses, which a rule about VALUES rather than ladders would wrongly refuse: a
    // SHORT ladder anchored at zero rests `step`, `2·step`, … and a bounded grid whose step
    // fits inside the 0..1 walls rests rungs.
    assert!(at("dca_accumulate", "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05").is_none());
    assert!(at("grid", "bounded01 = true\nstep = 0.05").is_none());
    // The ordinary tables both names ship with are armable, or every row above is trivial.
    for name in ["grid", "dca_accumulate"] {
        assert!(at(name, "").is_none(), "{name}'s own defaults must load");
        assert!(at(name, "rungs = 4\nstep = 0.5\nsize = 2.0").is_none(), "{name}");
    }
    // Every other name abstains — the question is not asked of a strategy whose order flow is a
    // function of the market rather than of the table.
    for name in ["buy_hold", "momentum", "trailing_scalper", "spread_maker", "nope"] {
        assert!(unarmable_params(name, &empty()).is_none(), "{name} must abstain");
    }
}

/// The echo's key set is the TABLE's key set, in order — so a knob added to a reader (and
/// therefore to [`PARAM_KEYS`], which its own gate enforces) cannot be left out of the mount log.
#[test]
fn resolved_params_reports_exactly_the_declared_keys_in_order() {
    for (name, keys) in PARAM_KEYS {
        match keys {
            ParamKeys::Declared(declared) => {
                let got = resolved_params(name, &empty())
                    .unwrap_or_else(|| panic!("{name} declares keys but reports none"));
                let got_keys: Vec<&str> = got.iter().map(|(k, _)| *k).collect();
                let want: Vec<&str> = declared.iter().map(|(k, _)| *k).collect();
                assert_eq!(got_keys, want, "{name}'s echo and its PARAM_KEYS row disagree");
                assert!(
                    got.iter().all(|(_, v)| !v.is_empty()),
                    "{name} echoes an EMPTY value — say what it resolved to"
                );
            }
            ParamKeys::NotEnumerated(_) => assert!(
                resolved_params(name, &empty()).is_none(),
                "{name} enumerates nothing, so it can report nothing"
            ),
        }
    }
    assert!(resolved_params("nope", &empty()).is_none());
}

/// The echo reports the RESOLVED value, not the typed one — which is the whole point, because
/// the divergences a type check cannot catch are exactly the ones nobody can see otherwise.
#[test]
fn resolved_params_reports_the_coercion_not_the_input() {
    let get = |name: &str, src: &str, key: &str| -> String {
        let p: Value = toml::from_str(src).unwrap();
        resolved_params(name, &p)
            .unwrap()
            .into_iter()
            .find(|(k, _)| *k == key)
            .unwrap_or_else(|| panic!("{name} reports no {key}"))
            .1
    };
    // `read_rungs` CLAMPS a negative count to zero — a grid that rests nothing.
    assert_eq!(get("grid", "rungs = -5", "rungs"), "0");
    // `PairsZScore` rounds and floors its window at 2.
    assert_eq!(get("pairs_zscore", "period = 1", "period"), "2");
    assert_eq!(get("pairs_zscore", "period = 2.6", "period"), "3");
    // An unrecognised `anchor` silently means `first` — so the echo says `first`.
    assert_eq!(get("grid", "anchor = \"fixd\"\nanchor_price = 42.0", "anchor"), "first");
    // ...as an unrecognised `side` silently means LONG.
    assert_eq!(get("dca_accumulate", "side = \"shrot\"", "side"), "long");
    // A default the profile never mentions is still reported: `venue` is the literal "sim".
    assert_eq!(get("momentum", "qty = 2.0", "venue"), "sim");
    // A `venues` row whose value is not a string is dropped by the reader — the echo shows the
    // map that survived, not the table that was typed.
    assert_eq!(get("momentum", "[venues]\nBTC = 7", "venues"), "(none)");
    assert_eq!(get("momentum", "[venues]\nBTC = \"okx\"", "venues"), "BTC:okx");
    // An un-armed barrier leg says so rather than printing a fake zero.
    assert_eq!(get("momentum", "qty = 1.0", "tp"), "(unarmed)");
    assert_eq!(get("momentum", "tp = 5", "tp"), "5");
    // A REQUIRED symbol left unset is a mount that cannot route — the echo must not render it
    // as an empty string, which reads like a configured value.
    assert!(get("pairs_zscore", "entry_z = 2.0", "symbol_a").contains("unset"));
    assert_eq!(get("pairs_zscore", "symbol_a = \"BTC\"", "symbol_a"), "BTC");
    // An OPTIONAL symbol has THREE states, and the echo must keep them apart. ABSENT is the
    // legal default (take the symbol off the feed); EMPTY is a value the operator STATED that
    // stops the strategy trading outright — proven, not asserted, by
    // `the_empty_symbol_this_echo_reports_really_does_stop_the_strategy` below. Rendering the
    // empty one as `""` (or as the absent one) is the `req_sym` defect wearing its sibling's
    // name, which is exactly how it survived that fix.
    for name in ["buy_hold", "grid", "dca_accumulate", "funding_capture"] {
        assert_eq!(
            get(name, "", "symbol"),
            "(from the feed)",
            "{name}: an ABSENT optional symbol is the working default"
        );
        let empty_sym = get(name, "symbol = \"\"", "symbol");
        assert!(
            empty_sym.contains("cannot trade"),
            "{name}: an EMPTY symbol stops the strategy and the echo must say so, got \
                 {empty_sym:?}"
        );
        assert_ne!(
            empty_sym,
            get(name, "", "symbol"),
            "{name}: empty and absent are different mounts and must not render alike"
        );
        assert_eq!(get(name, "symbol = \"BTCUSDT\"", "symbol"), "BTCUSDT", "{name}");
    }
    // ⚠ The THIRD renderer — `funding_carry`'s inline arm — is deliberately NOT changed, and
    // this row is why: there an empty `symbol` is a REAL, working mode (TWO-LEG delta-neutral;
    // `crates/vike-strategy/src/strategies/funding_carry.rs`'s `evaluate` gates on
    // `!self.symbol.is_empty()`), so "cannot trade" would be false about it. Same spelling,
    // opposite meaning — which is the reason the fix is per-renderer rather than a sweep.
    assert_eq!(get("funding_carry", "qty = 1.0", "symbol"), "(both legs)");
}

/// The claim `resolved_params`' `opt_sym` now makes about an EMPTY symbol — that the strategy
/// cannot trade — proven for every strategy it renders.
///
/// ⚠ **This is the half the `req_sym` fix never had, and its absence is why the same defect
/// survived in the sibling helper.** An echo checked only against its own wording drifts the
/// moment a reader changes its guard. Delete `symbol.is_empty()` from `BuyHold::buy`, from
/// either `crates/vike-strategy/src/strategies/grid_dca.rs` `drive`, or from
/// `crates/vike-strategy/src/strategies/funding_capture.rs`'s `on_bar`, and this goes red — which is the
/// only thing keeping the sentence in an operator's mount log true.
///
/// The ABSENT case is the control, and it is load-bearing: without it "empty submits nothing"
/// would also pass for a bar that trades nothing at all, and the test would prove neither half.
#[test]
fn the_empty_symbol_this_echo_reports_really_does_stop_the_strategy() {
    // Carries BOTH a symbol and a funding rate, so one bar drives all four strategies: the grid
    // pair arm their ladders off `close`, and `funding_capture` acts only on a funding bar.
    fn a_bar() -> Bar {
        Bar {
            ts: 1,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 0.0,
            funding: Some(0.01),
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".to_string()),
        }
    }
    fn submits(name: &str, src: &str) -> usize {
        let params: Value = toml::from_str(src).unwrap();
        let mut s = resolve(name, &params).unwrap_or_else(|e| panic!("{name} resolves: {e}"));
        let mut broker = ProbeBroker::default();
        s.on_bar(&mut broker, &a_bar());
        broker.submits
    }
    for name in ["buy_hold", "grid", "dca_accumulate", "funding_capture"] {
        assert_eq!(
            submits(name, "symbol = \"\""),
            0,
            "{name} routed an order on an EMPTY symbol — the echo's \"cannot trade\" would be \
                 a lie"
        );
        assert!(
            submits(name, "") > 0,
            "{name} routed nothing even with the symbol taken off the bar, so the empty-symbol \
                 assertion above proves nothing about the symbol"
        );
    }
}

/// Every declared key must be one the reader ACCEPTS — proven behaviourally for the two arms
/// whose fields are readable from this crate, so the table is not only text-checked by
/// `tests/param_keys_gate.rs` but observed to move something.
#[test]
fn a_declared_key_actually_moves_the_strategy() {
    let params: Value = toml::from_str("size = 7.5\n").unwrap();
    assert_eq!(BuyHold::from_params(&params).size, 7.5);
    assert!(unknown_params("buy_hold", &params).is_empty());

    let g: Value = toml::from_str("band = 9.0\n").unwrap();
    assert_eq!(Grid::from_params(&g).band, 9.0);
    assert!(unknown_params("grid", &g).is_empty());
}

#[test]
fn buy_hold_from_params_reads_size_and_symbol() {
    let params: Value = toml::from_str("size = 2.5\nsymbol = \"ETHUSDT\"\n").unwrap();
    let strat = BuyHold::from_params(&params);
    assert_eq!(strat.size, 2.5);
    assert_eq!(strat.symbol.as_deref(), Some("ETHUSDT"));
}

#[test]
fn buy_hold_from_params_defaults_size_to_one_and_symbol_to_none() {
    let strat = BuyHold::from_params(&empty());
    assert_eq!(strat.size, 1.0);
    assert_eq!(strat.symbol, None);
}

#[test]
fn buy_hold_from_params_accepts_integer_size() {
    let params: Value = toml::from_str("size = 3\n").unwrap();
    assert_eq!(BuyHold::from_params(&params).size, 3.0);
}

#[test]
fn params_reach_the_resolved_strategy() {
    // The concern the per-arm resolve tests in vike-backtest exist for: a typo'd knob silently
    // falling back to a default. Proven here for the arms whose reader is in THIS crate.
    let g: Value = toml::from_str("step = 0.5\nrungs = 4\nsize = 2.0\nband = 3.0\n").unwrap();
    assert!(resolve("grid", &g).is_ok());
    let grid = Grid::from_params(&g);
    assert_eq!(grid.step, 0.5);
    assert_eq!(grid.rungs, 4);

    let m: Value = toml::from_str("qty = 5.0\ntick_size = 0.01\ngamma = 0.2\n").unwrap();
    assert!(resolve("spread_maker", &m).is_ok());
    assert_eq!(SpreadMaker::from_params(&m).unwrap().params().qty, 5.0);
}
