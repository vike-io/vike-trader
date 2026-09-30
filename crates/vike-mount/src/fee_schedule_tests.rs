use super::resolve_fee_schedule;
use crate::transition::LEGACY_REGISTRY;
use vike_exec::recon::ReconClient;
// Used only by the `fxcm`-gated live-intent assertions below.
#[cfg(feature = "fxcm")]
use vike_model::account_keys::AccountLabel;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

/// A `ReconClient` whose `fetch_fee_rates` returns a configurable outcome (the other report
/// methods are irrelevant here). Mirrors the `FailingClient` test-double pattern in
/// `vike_exec::recon::client`.
struct FeeClient(Result<Option<FeeSchedule>, String>);
impl ReconClient for FeeClient {
    fn fetch_order_status_reports(&self, _s: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _s: i64) -> Result<Vec<FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.0.clone()
    }
}

#[test]
fn no_recon_falls_back_to_static_default() {
    let default = vike_model::fee_schedule_for("binance");
    assert_eq!(resolve_fee_schedule("binance", None, default), default);
}

#[test]
fn live_some_is_preferred_over_static() {
    let live = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 2.0 };
    let rc = FeeClient(Ok(Some(live)));
    assert_eq!(
        resolve_fee_schedule("binance", Some(&rc), vike_model::fee_schedule_for("binance")),
        live
    );
}

#[test]
fn none_and_error_both_fall_back_to_static() {
    let none = FeeClient(Ok(None));
    let err = FeeClient(Err("boom".to_string()));
    let expect = vike_model::fee_schedule_for("okx");
    assert_eq!(resolve_fee_schedule("okx", Some(&none), expect), expect);
    assert_eq!(resolve_fee_schedule("okx", Some(&err), expect), expect);
}

/// The permissive default a paper (or failed-pre-fetch) mount MUST carry: `from_properties` was
/// NOT applied, so every grid field stays `0.0` (= unconstrained). `im_requirement` is the one
/// field `make_engine` always sets (`Some(1.0)`, the conservative 1× buying-power default), so it
/// is deliberately not asserted here. This is the "falls back to permissive on absent
/// properties/creds" half of the RiskGate-property-grid contract — asserted for EVERY roster venue
/// by `all_roster_venues_absent_creds_stay_paper_and_inert`, and for the feature-on connect-failure
/// demotion by `ibkr_present_config_without_gateway_demotes_to_paper`. `venue` is threaded in so a
/// failure names which venue's grid came back non-permissive.
fn assert_permissive_grid(venue: &str, limits: &vike_exec::RiskLimits) {
    assert_eq!(limits.tick_size, None, "{venue}: permissive grid has no tick constraint");
    assert_eq!(limits.lot_size, None, "{venue}: permissive grid has no lot constraint");
    assert_eq!(limits.min_qty, None, "{venue}: permissive grid has no min-qty constraint");
    assert_eq!(limits.min_notional, None, "{venue}: permissive grid has no notional constraint");
    // …and no PER-SYMBOL grid either. Asserted here so the whole roster carries it (this helper
    // is called from the roster-parameterized inert-default test): a mount that declares no leg
    // must leave `grid_by_symbol` empty, which is what keeps `RiskLimits::grid_for` returning
    // the scalars verbatim and keeps the serialized limits — hence
    // `vike_exec::engine_snapshot::state_hash` — byte-identical to before that map had a
    // producer. See `symbol_grid`'s module doc.
    assert!(
        limits.grid_by_symbol.is_empty(),
        "{venue}: a mount with no declared legs must carry no per-symbol grid"
    );
}

/// The three grid shapes the ctrader/alpaca/ibkr arms resolve, each run through the SAME
/// `RiskLimits::from_properties` call the arms make, tightens the gate to non-default limits —
/// the "sets non-default limits when properties are supplied" half of the contract, proven with
/// synthetic grids (the live fetch itself needs a running venue and is exercised by each bridge's
/// own parser tests: ctrader `risk_properties`, alpaca `parse_asset_properties`, ibkr
/// `contract_details_to_properties`).
#[test]
fn resolved_grids_yield_non_default_limits() {
    let default = vike_exec::RiskLimits::new();
    // ctrader `risk_properties`: 10^-digits tick + centi-unit volume grid (EURUSD demo values).
    let ctrader = vike_model::SymbolProperties {
        tick_size: 1.0 / 100_000.0,
        step_size: 1000.0,
        min_qty: 1000.0,
        ..Default::default()
    };
    let l = vike_exec::RiskLimits::from_properties(&ctrader);
    assert_eq!(l.tick_size, Some(1.0 / 100_000.0));
    assert_eq!(l.lot_size, Some(1000.0), "step_size → lot_size");
    assert_eq!(l.min_qty, Some(1000.0));
    assert_ne!(l.tick_size, default.tick_size, "tighter than the permissive default (None)");

    // alpaca `/v1/assets` equity default: penny tick, whole-share step.
    let alpaca =
        vike_model::SymbolProperties { tick_size: 0.01, step_size: 1.0, ..Default::default() };
    let l = vike_exec::RiskLimits::from_properties(&alpaca);
    assert_eq!(l.tick_size, Some(0.01));
    assert_eq!(l.lot_size, Some(1.0));

    // ibkr `contractDetails`: min_tick / size_increment / min_size.
    let ibkr = vike_model::SymbolProperties {
        tick_size: 0.01,
        step_size: 1.0,
        min_qty: 1.0,
        ..Default::default()
    };
    let l = vike_exec::RiskLimits::from_properties(&ibkr);
    assert_eq!(l.tick_size, Some(0.01));
    assert_eq!(l.lot_size, Some(1.0));
    assert_eq!(l.min_qty, Some(1.0));
    assert_ne!(l.min_qty, default.min_qty, "tighter than the permissive default (None)");
}

/// ROSTER-PARAMETERIZED inert-default contract — the successor to the six hand-written
/// near-duplicate `*_absent_creds_stays_paper_and_inert` twins (binance/ctrader/alpaca/ig/oanda/
/// ibkr). For EVERY venue in the canonical `vike_model::VENUES` roster, mounting with an EMPTY
/// `.env` map — no `{VENUE}_DEMO_*` / agent-wallet / private-key / OAuth creds anywhere — must
/// yield the byte-identical PAPER engine: NO `ReconClient` handle (a paper venue never
/// reconciles); NOT marked live (`live_venues` stays empty); tagged with the venue's static
/// published fee schedule (a paper venue has no recon, so `resolve_fee_schedule` returns
/// `static_default` == `fee_schedule_for(venue)` — the invariant the retired
/// `make_engine_tags_paper_engine_with_resolved_schedule` pinned for binance, now generalized to
/// the whole roster); and a PERMISSIVE RiskGate grid (`from_properties` never ran → every
/// constraint field `None`).
///
/// It also touches NO network — each live arm's cred/config gate returns before any connect: the
/// crypto `(venue, Some)` arms don't match an absent cred (→ `_` paper); aster/hyperliquid/
/// ctrader/alpaca/ig/oanda/ibkr self-gate on their own absent config (→ paper before any connect,
/// e.g. hyperliquid's `config::load(..)?` and aster's `load_aster_credentials` short-circuit on
/// the empty map); fxcm/polymarket have no arm at all (dukascopy self-gates like the others since 2026-09-09) (→ `_` paper).
///
/// `recon_enabled` is deliberately `true` here, NOT `false`: this test's `recon.is_none()`
/// assertion is about ABSENT CREDENTIALS being the live gate, and passing `false` would satisfy
/// it through the new global reconcile gate instead, making it vacuous. With the gate on and an
/// empty `.env`, every arm's cred/config check still returns first, so no inline recon factory is
/// reached and the test stays offline.
///
/// Iterating the roster is the whole point: a newly-added bridge crate (its id landing in
/// `VENUES`) is AUTOMATICALLY held to this contract with ZERO new test code — the copy-drift
/// surface the six hand-written twins were is gone (the `venues.rs`/`fees.rs` completeness-gate
/// idiom, applied to the mount contract). aster is NOT special-cased despite being the one venue
/// whose live tier arms on credential PRESENCE alone: with empty vars it resolves no
/// agent-wallet creds and stays paper, so the loop is uniform.
/// IBKR is covered in BOTH build modes — default (no `("ibkr", _)` arm → `_` paper) and
/// `--features ibkr` (arm present but `load_ibkr_config_from` returns `None` before any connect) —
/// because this test compiles unconditionally and the assertions hold either way.
#[test]
fn all_roster_venues_absent_creds_stay_paper_and_inert() {
    // The mounted symbol is IRRELEVANT on the paper path — no arm parses it before falling back
    // to paper, and `PaperExecutionClient` only stores it — so ONE representative symbol covers
    // every venue (each venue's own symbol format is exercised by its bridge's parser tests).
    const SYMBOL: &str = "BTCUSDT";
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new(); // empty .env ⇒ absent creds for every venue
    // ⚠ The ARMING CEILING is opened for the WHOLE roster here, deliberately: this test's
    // subject is the OTHER gate — absent credentials — and a default (all-`paper`) policy would
    // satisfy every assertion below at the ceiling's early return, without any arm's own
    // cred/config check ever running. Arming everything is what keeps this the
    // absent-credentials contract rather than a second test of the ceiling.
    let armed = super::all_armed_policy(vike_config::VenueMode::Live);
    for &venue in vike_model::VENUES {
        let mut live = std::collections::HashSet::new();
        let (engine, recon) = super::make_engine(
            LEGACY_REGISTRY,
            venue,
            SYMBOL,
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&armed),
        )
        .unwrap_or_else(|e| panic!("{venue}: paper mount must never refuse to start: {e}"));
        assert!(recon.is_none(), "{venue}: absent creds → paper, no reconcile handle");
        assert!(live.is_empty(), "{venue}: absent creds → venue not marked live");
        assert_eq!(
            engine.fee_schedule,
            Some(vike_model::fee_schedule_for(venue)),
            "{venue}: paper mount tagged with the venue's static fee schedule"
        );
        assert_permissive_grid(venue, &engine.gate.limits);
    }
}

/// THE WIRING GATE for the fee LANE: `make_engine` must key the fee table off the LANE the
/// symbol routes to, not off the bare venue string.
///
/// The table's lane rows are worth nothing unless this call site passes them, and this is the
/// site that fills every `paper_client` fallback arm *and* supplies
/// `resolve_fee_schedule`'s fallback — so mounting `BTCUSDT.P` on binance used to charge the
/// SPOT 10/10 on a lane that costs 2/5 (5x the maker fee). Both symbol forms are asserted, so a
/// revert to `fee_schedule_for(venue)` fails on the `.P` case while an over-eager lane that ate
/// bare symbols fails on the other. The value pins live in `vike_model::money::fees`
/// (`lane_rows_are_pinned`); this only proves the lane REACHES the engine.
#[test]
fn make_engine_keys_the_fee_schedule_off_the_symbol_lane() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let vars = std::collections::HashMap::new(); // empty .env ⇒ paper everywhere, no network
    let mount = |venue: &str, symbol: &str| {
        let mut live = std::collections::HashSet::new();
        // The trailing `None`s: no recon client, no properties recorder, no operator
        // risk_profile, and no policy.toml (the 10th param arrived with Phase 6c, #1057). This
        // test is about which fee ROW the lane key selects, so every other input stays at its
        // absent default.
        super::make_engine(
            LEGACY_REGISTRY,
            venue,
            symbol,
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("{venue}/{symbol}: paper mount must start: {e}"))
        .0
        .fee_schedule
        .expect("make_engine always tags a schedule")
    };
    for venue in ["binance", "aster"] {
        assert_eq!(
            mount(venue, "BTCUSDT.P"),
            vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT.P")),
            "{venue}: a `.P` mount must be tagged with its PERP lane's schedule"
        );
        assert_eq!(
            mount(venue, "BTCUSDT"),
            vike_model::fee_schedule_for(venue),
            "{venue}: a bare mount must stay byte-identical to the pre-lane behavior"
        );
    }
    // binance is the venue whose lanes are actually priced apart — assert the engine really
    // ends up with two DIFFERENT schedules, so a lane resolution that silently collapsed
    // (`fee_lane` returning the bare id, a reverted call site) cannot pass this test.
    assert_ne!(
        mount("binance", "BTCUSDT.P"),
        mount("binance", "BTCUSDT"),
        "binance perp and spot mounts must not share a fee schedule"
    );
    // A single-exec-lane venue is unaffected by the suffix (bybit's exec is linear-perp only).
    assert_eq!(mount("bybit", "BTCUSDT.P"), mount("bybit", "BTCUSDT"));
}

/// Feature-on coverage of the actual `("ibkr", _)` arm body: config PRESENT but pointed at dead
/// ports (no TWS socket, no CP Gateway), so `IbkrExecutionClient::connect` fails fast
/// (connection refused) and the arm DEMOTES to PAPER — and the cpapi recon client resolves
/// `None` for the same reason. Needs NO running Gateway. SELF-SKIPS if a Gateway happens to be
/// reachable on the chosen ports (then IBKR would legitimately go live), so the assertion never
/// fires falsely on a dev box that has TWS/Gateway running.
#[cfg(feature = "ibkr")]
#[test]
fn ibkr_present_config_without_gateway_demotes_to_paper() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    // Dead ports: nothing listens on 127.0.0.1:6553{4,3}, so both the socket exec connect and the
    // cpapi recon tickle get an immediate connection-refused.
    let vars: std::collections::HashMap<String, String> = [
        ("IBKR_DEMO_ACCOUNT", "DUTEST000"),
        ("IBKR_DEMO_BACKEND", "socket"),
        ("IBKR_DEMO_PORT", "65534"),
        ("IBKR_DEMO_CPAPI_URL", "https://127.0.0.1:65533"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    // ⚠ A RISK BUDGET IS PART OF THIS FIXTURE, and the fxcm twin below deliberately has none.
    // The asymmetry is BY CONSTRUCTION — do not "harmonize" the two by deleting this.
    //
    // `make_engine_for_account` runs a PRE-CONNECT refusal (the #817 "the refusal happens
    // POST-connect" fix, Freqtrade's shape): when `account_arming_under` says this mount is
    // live-INTENT, a missing `max_notional_per_order`/`max_total_exposure` is
    // `MountError::MissingRiskBudget` BEFORE any arm dials anything. That probe is INTENT-based
    // and names this venue in its own comment: present live config is the operator's declared
    // intent, even where the arm would later demote to paper. ibkr's `arming.rs` row reads
    // `load_ibkr_config_for_account(Demo, …)`, which the vars above satisfy, and the ceiling
    // below arms it — so ibkr IS live-intent here, and with no budget the mount refuses without
    // ever reaching the connect-failure demotion this test exists to observe. (Measured: on
    // this test's first ever execution, once the `ibkr` CI lane widened to compile it, that is
    // exactly how it failed.)
    //
    // fxcm's row answers `(Paper, SdkAbsent)` on its FIRST conjunct — `sdk_available()` is false
    // on every CI runner — so its twin is not live-intent and never reaches the budget gate.
    // ibkr has no such conjunct available: its Gateway's liveness is knowable ONLY by
    // attempting a connect, which is precisely the I/O this gate exists to precede.
    //
    // The two caps below are the exact pair `require_live_risk_budget` reads, and nothing else
    // is set — a profile carrying no venue-owned field arms the operator budget and leaves the
    // venue GRID alone, so `assert_permissive_grid` below is unaffected and still asserts what
    // it always did.
    let budget = vike_exec::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..Default::default()
    };
    // The `live` set is checked BEFORE the `Result` is unwrapped: on the rare dev box where a
    // Gateway IS unexpectedly reachable this venue legitimately goes live, and every assertion
    // below would then be false for a correct reason — so that box must self-skip, exactly like
    // the "Gateway reachable" branch this comment guards. With the budget supplied the mount
    // returns `Ok` on BOTH branches, so the skip is the only thing separating them.
    // `recon_enabled: true` on purpose — the "no Gateway → recon unwired" assertion below is
    // about the cpapi handshake FAILING on a dead port, so the factory must actually be
    // reached; `false` would satisfy it through the global gate and pin nothing.
    let result = super::make_engine(
        LEGACY_REGISTRY,
        "ibkr",
        "AAPL.SMART.USD",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        Some(&budget),
        // ⚠ The ARMING CEILING must permit ibkr, or the arm below is never reached at all and
        // this test passes against the paper early return — every assertion here is also true
        // of a capped mount, so without this line it would be vacuous rather than red.
        Some(&super::armed_policy("ibkr", vike_config::VenueMode::Demo)),
    );
    if !live.is_empty() {
        eprintln!("skipping: an IBKR Gateway is unexpectedly reachable on the test ports");
        return;
    }
    let (engine, recon) = result
        .expect("budget supplied + no reachable Gateway -> the mount demotes to paper, not Err");
    assert!(recon.is_none(), "no Gateway → recon unwired");
    assert!(live.is_empty(), "connect failure → not marked live (demoted to paper)");
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("ibkr")));
    // Demoted to paper: the `contractDetails` pre-fetch is never reached (it lives inside the
    // `Ok(client)` branch), so limits stay permissive — same as every paper mount.
    assert_permissive_grid("ibkr", &engine.gate.limits);
}

/// Feature-on coverage of the `("fxcm", _)` arm body, and the ONLY branch of it any CI runner
/// can reach: credentials PRESENT, ForexConnect NOT linked ⇒ the arm REFUSES the live mount and
/// lands on paper.
///
/// This is the branch that matters most, because the behaviour it rules out is silent. A stub
/// build's `FxcmSession::login` returns `Unavailable`, the exec thread returns immediately, and
/// `FxcmExecutionClient::spawn` — which is infallible — hands back a client that accepts every
/// submit and forwards it to a thread that is not there. Without this refusal an operator with
/// FXCM credentials and an SDK-less binary would see the venue reported LIVE, place orders, and
/// get neither fills nor rejections: the no-silent-vanish contract failing at the mount.
///
/// Needs no SDK, no credentials of any real account and no network: the refusal happens before
/// `spawn`, so nothing dials FXCM. SELF-SKIPS on a box that HAS the SDK linked, where mounting
/// live is the correct outcome and these asserts would legitimately be false.
#[cfg(feature = "fxcm")]
#[test]
fn fxcm_credentials_without_a_loadable_shim_refuse_the_live_mount() {
    if vike_fxcm::sdk_available() {
        eprintln!("skipping: this binary HAS ForexConnect linked, so fxcm mounts live here");
        return;
    }
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let vars: std::collections::HashMap<String, String> =
        [("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "not-a-real-password")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    // `recon_enabled: true` deliberately — the "no SDK ⇒ recon unwired" assertion is about the
    // refusal happening BEFORE the recon factory is reached, so the gate must be open; `false`
    // would satisfy it through the global gate and pin nothing.
    // ⚠ The ARMING CEILING permits fxcm here, so the arm is genuinely reached and it is the
    // SDK refusal being pinned. A capped mount would satisfy every assertion below without the
    // arm ever running.
    let armed = super::armed_policy("fxcm", vike_config::VenueMode::Demo);
    let (engine, recon) = super::make_engine(
        LEGACY_REGISTRY,
        "fxcm",
        "EURUSD",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&armed),
    )
    .expect("a paper mount must never refuse to start");
    assert!(
        live.is_empty(),
        "a stub build must NOT mark fxcm live — its exec client would discard every order"
    );
    assert!(recon.is_none(), "the refusal precedes the recon factory, so nothing is wired");
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("fxcm")));
    assert_permissive_grid("fxcm", &engine.gate.limits);

    // The CONTROL: the same binary with the credentials REMOVED reaches the same paper outcome
    // by the ordinary unconfigured path, so the assertions above pin the SDK refusal and not
    // some unrelated fxcm breakage that would make every fxcm mount paper.
    let mut live2 = std::collections::HashSet::new();
    let (_e, recon2) = super::make_engine(
        LEGACY_REGISTRY,
        "fxcm",
        "EURUSD",
        &std::collections::HashMap::new(),
        &tx,
        &mut live2,
        true,
        None,
        None,
        None,
        Some(&armed),
    )
    .expect("a paper mount must never refuse to start");
    assert!(live2.is_empty() && recon2.is_none());
    // …and the PURE probe is what separates the two causes: with a linked SDK these same
    // credentials WOULD be live intent, while the empty map would not.
    assert!(super::fxcm_live_intent(true, &AccountLabel::Default, &vars));
    assert!(!super::fxcm_live_intent(
        true,
        &AccountLabel::Default,
        &std::collections::HashMap::new()
    ));
}

/// A syntactically valid secp256k1 key that is NOT a real account — the arm's gates return
/// before anything is ever signed with it, which is the property these tests assert.
#[cfg(feature = "polymarket")]
const POLY_TEST_KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";

/// A polymarket test can only assert "no network call" if neither gate is exported in the REAL
/// process env (both gates read it as well as the map). Skip rather than produce a false green.
#[cfg(feature = "polymarket")]
fn poly_gates_clean(vars: &std::collections::HashMap<String, String>) -> bool {
    if vike_polymarket::poly_reconcile_enabled(vars) || vike_polymarket::poly_exec_enabled(vars) {
        eprintln!("skipping: POLY_RECONCILE / POLY_EXEC is exported in this process env");
        return false;
    }
    true
}

/// Every polymarket test below wants the `("polymarket", _)` ARM to actually run, so each one
/// arms the ceiling to `live` — the tier that venue's exec requires, since it has no testnet.
/// Without it the mount returns at the paper early return and the venue's own double gate,
/// which is what these tests exist to pin, is never consulted.
#[cfg(feature = "polymarket")]
fn poly_armed() -> super::MountPolicy {
    super::armed_policy("polymarket", vike_config::VenueMode::Live)
}

/// Feature-on coverage of the `("polymarket", _)` arm body, the OFFLINE half: BOTH gates are the
/// FIRST things read, so creds present + both unset ⇒ no reconcile handle, no exec client AND no
/// network call (the L2 `/auth/derive-api-key` round-trip lives behind them). This is the
/// byte-identical-to-a-default-build case the double gate exists to guarantee.
///
/// ⚠ **Since S2 this is also the SECOND gate's assertion, and the wording matters because the
/// first draft of it was wrong.** `vike_tradehub::reconcile_config::reconcile_gate` turns the master
/// gate ON for every mount that arms a live venue account. That default DOES reach Polymarket —
/// it is the same driver, mounted over the same `recon_clients` vector — so it is NOT true that
/// this venue "did not change": what changed is that its OUTER gate is now supplied by the box
/// rather than typed by a person, leaving `flags.poly_reconcile` as the one remaining act. It is
/// still one act more than any other venue needs, and it is still what this test pins: the
/// `recon_enabled: true` below passes the master gate in its DEFAULT-ON state and the test
/// demands `recon.is_none()` anyway, so a PR that dropped the venue gate as redundant reddens
/// here. Why the venue gate must survive the default:
/// `crates/bridges/polymarket/CLAUDE.md` (reconciling a live Polymarket account against a paper
/// engine, and the venue's absence of any testnet), and
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` for what the owner is
/// being asked to ratify.
#[cfg(feature = "polymarket")]
#[test]
fn polymarket_without_the_gates_is_inert_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let vars: std::collections::HashMap<String, String> =
        [("POLY_PRIVATE_KEY".to_string(), POLY_TEST_KEY.to_string())].into_iter().collect();
    if !poly_gates_clean(&vars) {
        return;
    }
    // `recon_enabled: true` on purpose: this arm reads BOTH gates
    // (`poly_recon_wanted`), and passing the master one in its default-on state is what makes
    // the assertion below about the VENUE gate rather than about a mount that was off anyway.
    let (engine, recon) = super::make_engine(
        LEGACY_REGISTRY,
        "polymarket",
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&poly_armed()),
    )
    .expect("stays paper (no exec gate) -> the refusal check must not fire");
    assert!(
        recon.is_none(),
        "flags.poly_reconcile off → no reconcile handle (and no network call)"
    );
    assert!(live.is_empty(), "flags.poly_exec off → exec stays PAPER, never marked live");
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("polymarket")));
    assert_permissive_grid("polymarket", &engine.gate.limits);
}

/// Recon gate ON but creds ABSENT ⇒ still no handle, still no network call (the factory's own
/// absent-credentials-is-the-live-gate return precedes its `ensure_l2` round-trip). Proves the
/// two gates compose in both orders, offline.
#[cfg(feature = "polymarket")]
#[test]
fn polymarket_with_the_recon_gate_but_no_creds_stays_inert() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let vars: std::collections::HashMap<String, String> =
        [(vike_polymarket::POLY_RECONCILE_ENV.to_string(), "1".to_string())].into_iter().collect();
    let (_engine, recon) = super::make_engine(
        LEGACY_REGISTRY,
        "polymarket",
        "0",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&poly_armed()),
    )
    .expect("stays paper (no exec gate) -> the refusal check must not fire");
    assert!(recon.is_none(), "gate on but no POLY_PRIVATE_KEY → reconcile-inert");
    assert!(live.is_empty());
}

/// The EXEC gate's twin: `flags.poly_exec` on with NO `POLY_PRIVATE_KEY` ⇒ no live client, venue stays
/// paper, and — the part that matters for CI — no network call, because
/// `live_mount_from_vars` returns on absent credentials before its `ensure_l2` round-trip.
#[cfg(feature = "polymarket")]
#[test]
fn polymarket_with_the_exec_gate_but_no_creds_stays_paper_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let vars: std::collections::HashMap<String, String> =
        [(vike_polymarket::POLY_EXEC_ENV.to_string(), "1".to_string())].into_iter().collect();
    let (engine, recon) = super::make_engine(
        LEGACY_REGISTRY,
        "polymarket",
        "0",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&poly_armed()),
    )
    .expect("no key -> stays paper -> the refusal check must not fire");
    assert!(live.is_empty(), "flags.poly_exec on but no key → paper, never marked live");
    assert!(recon.is_none());
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("polymarket")));
}

/// …and with an UNUSABLE key PRESENT: CONTRACT CHANGED by the pre-connect refusal (#817
/// residual). Key material the operator wrote + the explicit `flags.poly_exec` flag IS declared
/// live intent (`would_mount_live`'s polymarket row, same stance as hyperliquid's), so with
/// no risk budget the mount now REFUSES — before the factory would even try (and fail) the
/// EOA derivation — instead of silently falling back to paper as it did before. Still
/// entirely offline: the refusal precedes any network. The no-key case above keeps the paper
/// fallback (a flag with no key can never mount live, so it is not intent).
#[cfg(feature = "polymarket")]
#[test]
fn polymarket_exec_gate_with_a_bad_key_refuses_preconnect_without_budget() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let vars: std::collections::HashMap<String, String> = [
        (vike_polymarket::POLY_EXEC_ENV.to_string(), "1".to_string()),
        ("POLY_PRIVATE_KEY".to_string(), "not-a-key".to_string()),
    ]
    .into_iter()
    .collect();
    let err = match super::make_engine(
        LEGACY_REGISTRY,
        "polymarket",
        "0",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&poly_armed()),
    ) {
        Err(e) => e,
        Ok(_) => panic!("key present + flags.poly_exec + no budget must refuse pre-connect"),
    };
    let msg = format!("{err}");
    assert!(msg.contains("polymarket"), "must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order") && msg.contains("max_total_exposure"));
    assert!(live.is_empty(), "the refusal precedes the arm — nothing recorded live");

    // …and the ARMING CEILING is what decides whether any of that is reached at all. Under
    // `demo` the SAME configuration is not live intent: polymarket has no testnet, so `demo`
    // names a tier that does not exist and its exec arm refuses rather than arming the only
    // tier there is (REAL money on Polygon mainnet). Paper, offline, and no refusal to start.
    let mut demoted = std::collections::HashSet::new();
    let (_engine, recon) = super::make_engine(
        LEGACY_REGISTRY,
        "polymarket",
        "0",
        &vars,
        &tx,
        &mut demoted,
        true,
        None,
        None,
        None,
        Some(&super::armed_policy("polymarket", vike_config::VenueMode::Demo)),
    )
    .expect("a demo-capped polymarket is PAPER, and paper never refuses to start");
    assert!(demoted.is_empty(), "a `demo` ceiling must not arm a mainnet-only venue");
    assert!(recon.is_none(), "POLY_RECONCILE is unset here, so nothing reconciles either");
}

/// The go-live credential-selection matrix (completes #771). Pure — no process env, no network.
/// The four combinations of a `live` ceiling × LIVE-cred presence, each pinned to the tier the
/// mount uses. The two safety-critical rows are the bottom two: a `live` ceiling with live creds
/// is the ONLY path to real money, and a `live` ceiling WITHOUT live creds degrades to PAPER —
/// never demo-on-mainnet.
#[test]
fn cex_cred_choice_matrix() {
    use super::CexCredChoice::*;
    // Ceiling below `live` ⇒ DEMO regardless of whether live creds happen to exist (byte-identical
    // to before decision 0095 — that path never even looks at the live tier).
    assert_eq!(super::cex_cred_choice(false, false), Demo);
    assert_eq!(super::cex_cred_choice(false, true), Demo);
    // `live` ceiling + LIVE creds present ⇒ the ONLY real-money path.
    assert_eq!(super::cex_cred_choice(true, true), LiveMainnet);
    // `live` ceiling + NO live creds ⇒ PAPER, never demo-on-mainnet (absent-creds-is-the-live-gate).
    assert_eq!(super::cex_cred_choice(true, false), MainnetNoCreds);
}

/// Decision 0095: exactly binance, bybit and okx take their network — and so their credential tier
/// — from the ceiling in the shared CEX arm. Deribit shares the arm and is testnet-only.
#[test]
fn only_the_three_cex_venues_take_their_network_from_the_ceiling() {
    let chosen: Vec<&str> =
        vike_model::VENUES.iter().copied().filter(|v| super::ceiling_selects_mainnet(v)).collect();
    assert_eq!(chosen, ["binance", "bybit", "okx"]);
}

// ---- api-key permissions, STEP-2 (the binance live arm's pre-arm gate) --------------------

/// The pure verdict core: a KNOWN withdraw-capable key REFUSES (so the live arm's guard fails
/// and binance falls through to paper), the operator override forces `Allow`, and a trade-only
/// key arms.
#[test]
fn a_known_withdraw_capable_key_refuses_unless_overridden() {
    use vike_bridge_core::key_permissions::{KeyPermissions, WithdrawGate};
    let withdraw = KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN };
    assert_eq!(super::binance_withdraw_verdict(Ok(withdraw), false), WithdrawGate::Refuse);
    assert_eq!(super::binance_withdraw_verdict(Ok(withdraw), true), WithdrawGate::Allow);
    let trade_only = KeyPermissions {
        can_withdraw: Some(false),
        can_trade: Some(true),
        ip_restricted: Some(true),
    };
    assert_eq!(super::binance_withdraw_verdict(Ok(trade_only), false), WithdrawGate::Allow);
}

/// FAIL-OPEN on introspection: a fetch error (and an all-Unknown body) is NOT evidence of a
/// withdraw-capable key, so it must never refuse — the same permissive shape as the
/// `RiskLimits::from_properties` pre-fetch fallback next to the call site.
#[test]
fn an_unknown_or_failed_key_probe_never_refuses() {
    use vike_bridge_core::key_permissions::{KeyPermissions, WithdrawGate};
    let err = Err("venue error -2015: Invalid API-key".to_string());
    assert_eq!(super::binance_withdraw_verdict(err, false), WithdrawGate::Allow);
    assert_eq!(
        super::binance_withdraw_verdict(Ok(KeyPermissions::UNKNOWN), false),
        WithdrawGate::Allow
    );
}

/// The OFF/DEFAULT path: a DEMO mount short-circuits to `Allow` with NO network call — `sapi`
/// is mainnet-only, so there is nothing to introspect on the testnet host. This is why a
/// credential-free CI run (and every demo mount) is byte-identical to before this gate existed;
/// the test would hang or fail on a sandboxed runner if the demo path probed anything.
#[test]
fn a_demo_mount_never_probes_key_permissions() {
    use vike_bridge_core::key_permissions::WithdrawGate;
    let creds = vike_bridge_core::Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: None,
    };
    assert_eq!(
        super::binance_withdraw_gate(false, &creds, &std::collections::HashMap::new()),
        WithdrawGate::Allow
    );
}

/// A venue that reports no contract size yields NO grid — the literal `None` this site passed
/// before the wiring existed, so `Account::multiplier_of` falls through to its 1.0 scalar and
/// every non-deribit venue mounts byte-identically.
#[test]
fn absent_contract_size_yields_no_grid() {
    assert!(super::multiplier_grid("BTCUSDT", 0.0).is_none());
    // an explicit 1.0 is arithmetically the same as no grid — collapsed, not carried
    assert!(super::multiplier_grid("BTC-8JUL26-62000-C", 1.0).is_none());
}

/// A real contract size lands in the grid under the mounted symbol, which is exactly what
/// `Account::multiplier_of` (and therefore `validate_with_multiplier` + the snapshot the GUI
/// reads) looks up. This is the assertion that the root bug is closed.
#[test]
fn real_contract_size_lands_in_the_grid() {
    let grid = super::multiplier_grid("BTC-PERPETUAL", 10.0).expect("non-1.0 → a grid");
    assert_eq!(grid.get("BTC-PERPETUAL"), Some(&10.0));
    assert_eq!(grid.len(), 1, "only the mounted symbol");
}

/// A degenerate venue value must not produce a 0.0/negative multiplier grid — it folds to the
/// absent case, since a 0.0 multiplier would make every order measure as zero notional.
#[test]
fn degenerate_contract_size_yields_no_grid() {
    for bad in [-5.0, f64::NAN, f64::INFINITY] {
        assert!(super::multiplier_grid("X", bad).is_none(), "{bad} must not build a grid");
    }
}

/// End-to-end through the type the engine actually consults: a grid built from a contract size
/// makes `Account::multiplier_of` return it, while an unlisted symbol stays 1.0.
#[test]
fn grid_drives_account_multiplier_of() {
    let acct = vike_exec::Account::new(
        1.0,
        "deribit",
        super::multiplier_grid("BTC-PERPETUAL", 10.0),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(acct.multiplier_of("BTC-PERPETUAL"), 10.0, "the mounted symbol's contract size");
    assert_eq!(acct.multiplier_of("ETH-PERPETUAL"), 1.0, "unlisted → the scalar default");

    // and the no-contract-size venue is byte-identical to the pre-wiring `None`
    let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
    let wired = vike_exec::Account::new(
        1.0,
        "binance",
        super::multiplier_grid("BTCUSDT", 0.0),
        vike_exec::BalanceMode::Delta,
    );
    assert_eq!(plain.multiplier_of("BTCUSDT"), wired.multiplier_of("BTCUSDT"));
}

/// [`super::margin_mode_grid`]'s collapse rule, the margin-axis twin of the three
/// `multiplier_grid` tests above: `Cross` — the resolved mode on 13 of the 14 roster venues and
/// on every ordinary hyperliquid asset — yields NO grid, so `Account` keeps the empty-map
/// short-circuit and mounts byte-identically. A non-`Cross` mode lands under the mounted symbol,
/// which is exactly the key `Account::default_margin_mode_of` looks up on open-from-flat.
#[test]
fn margin_mode_grid_collapses_cross_and_carries_the_rest() {
    use vike_model::MarginMode;
    assert!(super::margin_mode_grid("BTC", MarginMode::Cross).is_none(), "Cross ⇒ no grid");

    for mode in [MarginMode::Isolated, MarginMode::Cash] {
        let grid = super::margin_mode_grid("CASHCAT", mode).expect("non-Cross ⇒ a grid");
        assert_eq!(grid.get("CASHCAT"), Some(&mode));
        assert_eq!(grid.len(), 1, "only the mounted symbol");
    }
}

/// End-to-end through the type the fold actually consults, mirroring
/// `grid_drives_account_multiplier_of` directly above: the grid drives
/// `Account::default_margin_mode_of`, an unlisted symbol stays `Cross`, and the collapsed-`Cross`
/// mount is indistinguishable from the `None` every other venue passes.
#[test]
fn margin_mode_grid_drives_account_default_margin_mode_of() {
    use vike_model::MarginMode;
    let acct = vike_exec::Account::new(1.0, "hyperliquid", None, vike_exec::BalanceMode::Delta)
        .with_default_margin_modes(super::margin_mode_grid("CASHCAT", MarginMode::Isolated));
    assert_eq!(acct.default_margin_mode_of("CASHCAT"), MarginMode::Isolated);
    assert_eq!(acct.default_margin_mode_of("BTC"), MarginMode::Cross, "unlisted → Cross");

    let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
    let wired = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta)
        .with_default_margin_modes(super::margin_mode_grid("BTCUSDT", MarginMode::Cross));
    assert_eq!(plain.default_margin_mode_of("BTCUSDT"), wired.default_margin_mode_of("BTCUSDT"));
}

// ---------------------------------------------------------------------------------------
// merge_operator_budget — the ACTUAL merge site `make_engine` calls, pinned DIRECTLY (unlike
// a test that only drives `ProfileRisk::apply_to` by hand and never touches this function —
// see this fn's own doc for why that distinction matters). The base below is shaped like a
// REAL `RiskLimits::from_properties` fetch (populated instrument grid), so flipping the
// hardcoded `GridSource::VenueFetched` inside `merge_operator_budget` to `NoGridFetched` would
// make `venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback` below observe
// the profile's illegal `tick_size` silently WIN instead of being dropped — failing first.
// ---------------------------------------------------------------------------------------

fn venue_fetched_limits() -> vike_exec::RiskLimits {
    vike_exec::RiskLimits {
        tick_size: Some(0.5),
        lot_size: Some(0.01),
        min_qty: Some(0.01),
        min_notional: Some(10.0),
        ..vike_exec::RiskLimits::new()
    }
}

#[test]
fn merge_operator_budget_none_leaves_limits_untouched() {
    let base = venue_fetched_limits();
    let got = super::merge_operator_budget("binance", base.clone(), None);
    assert_eq!(got, base, "no profile threaded in must be a byte-identical no-op");
}

/// THE point of pinning this at the `merge_operator_budget` call site rather than only at
/// `ProfileRisk::apply_to`: a clean profile (no venue-owned fields) must both arm the operator
/// budget AND leave the REAL fetched venue grid alone — proving this function actually routes
/// through `VenueFetched`, not some other source.
#[test]
fn merge_operator_budget_arms_operator_fields_and_keeps_the_venue_grid() {
    let base = venue_fetched_limits();
    let profile = vike_exec::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..vike_exec::ProfileRisk::default()
    };
    let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
    assert_eq!(got.tick_size, base.tick_size, "venue grid must stay the REAL fetched value");
    assert_eq!(got.lot_size, base.lot_size);
    assert_eq!(got.min_qty, base.min_qty);
    assert_eq!(got.min_notional, base.min_notional);
    assert_eq!(got.max_notional_per_order, Some(100.0));
    assert_eq!(got.max_total_exposure, Some(500.0));
}

/// BLOCKING-2(b) regression: a profile that ALSO (illegally) sets a venue-owned field over a
/// REAL fetched grid must not zero the operator's whole budget — only the offending venue
/// field is dropped (kept as the real fetched value), while every operator-owned field the
/// profile set still arms.
#[test]
fn venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback() {
    let base = venue_fetched_limits();
    let profile = vike_exec::ProfileRisk {
        tick_size: Some(999.0), // illegal under a real venue fetch
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        max_orders_per_window: Some(5),
        window_ms: 2000,
        ..vike_exec::ProfileRisk::default()
    };
    let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
    assert_eq!(
        got.tick_size, base.tick_size,
        "the illegal venue-owned override must be dropped, not honored"
    );
    assert_eq!(
        got.max_notional_per_order,
        Some(100.0),
        "the operator budget must still arm despite the unrelated venue-field conflict"
    );
    assert_eq!(got.max_total_exposure, Some(500.0));
    assert_eq!(got.max_orders_per_window, Some(5));
    assert_eq!(got.window_ms, 2000);
}

// ---------------------------------------------------------------------------------------
// Task 6 (armed-risk-defaults) — `arm_universal_defaults` / `require_live_risk_budget`,
// pinned DIRECTLY (the same reasoning as `merge_operator_budget`'s own tests above: a
// network-free CI test cannot drive a genuinely LIVE `make_engine` arm end to end, so the
// pure functions the live call site actually invokes are the CI-safe proof). Every test below
// was broken (by commenting out the fix under test) and confirmed to fail, then restored,
// before being trusted — see the task report for the per-test confirmation.
// ---------------------------------------------------------------------------------------

fn market_order(symbol: &str, qty: f64) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: "t".into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

#[test]
fn arm_universal_defaults_arms_the_throttle_when_absent() {
    let armed = super::arm_universal_defaults(vike_exec::RiskLimits::new());
    assert_eq!(armed.max_orders_per_window, Some(super::ARMED_MAX_ORDERS_PER_WINDOW));
    assert_eq!(armed.window_ms, 1000, "untouched -> RiskLimits::new()'s own 1000ms default");
    // Issue #822: `max_leverage` is NOT armed here any more (#817's `Some(1.0)` enforced
    // nothing and duplicated the `im_requirement` rescue). It stays whatever reached this fn.
    assert_eq!(armed.max_leverage, None);
    // required_free_bp_pct needs no rescue in this fn: already 0.0 from RiskLimits::new().
    assert_eq!(armed.required_free_bp_pct, 0.0);
}

/// The compose-not-fight property (required test 3): an explicit value already present
/// (mirroring what `merge_operator_budget` would have left after a profile set it) must NOT
/// be clobbered by the default — `.or(..)` only fills a `None`.
#[test]
fn arm_universal_defaults_never_overrides_an_explicit_value() {
    let explicit = vike_exec::RiskLimits {
        max_orders_per_window: Some(7),
        window_ms: 500,
        max_leverage: Some(3.0),
        ..vike_exec::RiskLimits::new()
    };
    let armed = super::arm_universal_defaults(explicit);
    assert_eq!(armed.max_orders_per_window, Some(7), "an explicit value must win");
    assert_eq!(armed.window_ms, 500);
    assert_eq!(armed.max_leverage, Some(3.0), "carried through untouched, never clobbered");
}

/// THE HEADLINE test (required test 1, first half): "the field is populated" is not "the
/// check runs" — build a REAL `RiskGate` straight from the armed defaults (no profile
/// involved at all) and prove it actually DENIES a rate violation once `ARMED_MAX_ORDERS_PER_WINDOW`
/// orders have already landed in the window.
#[test]
fn armed_defaults_gate_actually_denies_a_rate_violation() {
    let armed = super::arm_universal_defaults(vike_exec::RiskLimits::new());
    let mut gate = vike_exec::RiskGate::new(armed);
    let req = market_order("BTCUSDT", 0.001);
    let ctx = vike_exec::RiskContext {
        mark_price: 100.0,
        equity: 1_000_000.0,
        ..vike_exec::RiskContext::default()
    };
    for i in 0..super::ARMED_MAX_ORDERS_PER_WINDOW {
        let v = gate.check(&req, &ctx);
        assert!(v.ok, "order {i} within the armed per-window cap must pass: {v:?}");
    }
    let v = gate.check(&req, &ctx);
    assert!(!v.ok, "the order beyond the armed per-window cap must be DENIED");
    assert_eq!(v.reason, "rate-limited");
}

/// THE HEADLINE test (required test 1, second half) — the `max_leverage` field's honest
/// story, now with issue #822's resolution folded in. `RiskGate::check` still never evaluates
/// `RiskLimits::max_leverage` (no production caller of `clamp_leverage` exists either —
/// verified: `Command::SetMargin` writes `im_by_symbol` directly), so this test does NOT claim
/// that field denies anything. What enforces "no leverage unless asked" is `im_requirement`,
/// armed at 1.0 by the rescue one line below this fn's call site in `make_engine`
/// (`limits.im_requirement = limits.im_requirement.or(Some(1.0))`, pre-existing since PR #816)
/// — and, since #822, ALSO the destination an operator's `[risk] max_leverage` converts into.
/// This test proves THAT mechanism actually denies an over-leveraged order, mirroring exactly
/// what `make_engine` builds. #817's duplicate `max_leverage = 1.0` arming is gone, so the
/// field is `None` here: an inert knob is no longer populated to look like protection.
#[test]
fn armed_leverage_is_enforced_via_im_requirement_not_max_leverage() {
    let mut limits = super::arm_universal_defaults(vike_exec::RiskLimits::new());
    assert_eq!(limits.max_leverage, None, "the inert knob is no longer armed");
    limits.im_requirement = limits.im_requirement.or(Some(1.0)); // make_engine's own rescue
    let mut gate = vike_exec::RiskGate::new(limits);
    // 20 units at $100 = $2,000 notional against $1,000 equity at 1x buying power -> denied.
    let req = market_order("BTCUSDT", 20.0);
    let ctx = vike_exec::RiskContext {
        mark_price: 100.0,
        equity: 1_000.0,
        ..vike_exec::RiskContext::default()
    };
    let v = gate.check(&req, &ctx);
    assert!(!v.ok, "an order needing more than 1x buying power must be denied: {v:?}");
    assert_eq!(v.reason, "insufficient-margin");
}

/// Required test 4: over-arming, not under-arming, is the real risk of this change — a
/// perfectly normal order, comfortably inside every armed default, must still pass.
#[test]
fn armed_defaults_gate_still_passes_a_normal_order() {
    let mut limits = super::arm_universal_defaults(vike_exec::RiskLimits::new());
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    let mut gate = vike_exec::RiskGate::new(limits);
    let req = market_order("BTCUSDT", 1.0);
    let ctx = vike_exec::RiskContext {
        mark_price: 100.0,
        equity: 1_000_000.0,
        ..vike_exec::RiskContext::default()
    };
    let v = gate.check(&req, &ctx);
    assert!(v.ok, "a normal order well within every armed default must pass: {v:?}");
}

#[test]
fn require_live_risk_budget_ok_when_both_set() {
    let limits = vike_exec::RiskLimits {
        max_notional_per_order: Some(1.0),
        max_total_exposure: Some(1.0),
        ..vike_exec::RiskLimits::new()
    };
    assert!(super::require_live_risk_budget("binance", &limits, true).is_ok());
}

/// Required test 2: a live mount with NEITHER account-dependent cap set must refuse to
/// start, naming BOTH missing keys in one message (not just the first).
#[test]
fn require_live_risk_budget_names_both_missing_keys() {
    let limits = vike_exec::RiskLimits::new(); // neither cap set
    // `false` = no profile reached the mount, the shape that produces the whole-file variant
    // of the diagnostic — which is also the only way BOTH caps go missing in practice.
    let err = super::require_live_risk_budget("okx", &limits, false)
        .expect_err("neither account-dependent cap set -> must refuse to start");
    let msg = format!("{err}");
    assert!(msg.contains("okx"), "error must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order"), "must name the 1st missing key: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the 2nd missing key: {msg}");
}

/// Only the field that is ACTUALLY missing is named — a supplied cap must not be blamed.
#[test]
fn require_live_risk_budget_names_only_the_actually_missing_key() {
    let limits = vike_exec::RiskLimits {
        max_notional_per_order: Some(1.0), // supplied
        ..vike_exec::RiskLimits::new()     // max_total_exposure still None
    };
    // `true` = a profile DID reach the mount — the only way one cap can be set while the other
    // is not — so the diagnostic renders the add-these-lines variant, whose `[risk]` fragment
    // lists exactly the missing keys. (Under `false` the message deliberately prints a COMPLETE
    // minimal profile, both caps included, because there is no file to add a line to.)
    let err = super::require_live_risk_budget("bybit", &limits, true)
        .expect_err("one missing cap is still a refusal");
    let msg = format!("{err}");
    assert!(!msg.contains("max_notional_per_order"), "must not blame the supplied key: {msg}");
    assert!(msg.contains("max_total_exposure"), "must name the actually-missing key: {msg}");
}

/// The STARTUP DIAGNOSTIC contract, for the no-profile case — the wall every new user of a
/// live-mounting binary hits first. The old message named neither the knob nor the fix, so an
/// operator could only get past it by reading `run_profile.rs`'s `#[cfg(test)]` `LIVE_TOML`
/// fixture. Each assertion below is one thing that had to be discoverable from source before.
#[test]
fn no_profile_diagnostic_names_the_knob_the_keys_and_a_working_profile() {
    let err = super::require_live_risk_budget("binance", &vike_exec::RiskLimits::new(), false)
        .expect_err("no budget from any source -> refusal");
    let msg = format!("{err}");

    // 1. WHAT is wrong, in words, not a Debug dump.
    assert!(msg.contains("no risk budget"), "states the problem plainly: {msg}");
    assert!(!msg.contains("MissingRiskBudget"), "must not leak the Debug shape: {msg}");
    // 2. WHICH resolver to use. Both, because `resolve_profile`'s precedence is
    //    explicit-flag-beats-env and only some binaries pass an explicit path. Asserted on the
    //    ACTIONABLE spelling (`Set VIKE_RUN_PROFILE=`) rather than the bare name — a stronger
    //    check, and it keeps this library file free of a standalone env-shaped string literal,
    //    which `vike_ops::scan::find_map_lookups` would read as an injected-map env read here.
    assert!(msg.contains("Set VIKE_RUN_PROFILE=<run.toml>"), "names the env var: {msg}");
    assert!(msg.contains("--profile <run.toml>"), "names the flag: {msg}");
    // 3. WHICH keys — the error already knew them; it just never printed them usefully.
    assert!(msg.contains("max_notional_per_order"), "names the 1st key: {msg}");
    assert!(msg.contains("max_total_exposure"), "names the 2nd key: {msg}");
    // 4. The message IS the example: `mode` plus a `[risk]` table, which is now the WHOLE of a
    //    minimal live profile.
    //
    //    ⚠ This step used to also demand `[event_source]` and `[broker]`, because they were
    //    schema-required and a `[risk]`-only file earned a parse error. Both tables are
    //    DELETED from `RunProfile` and a profile carrying either is refused BY NAME, so the
    //    two assertions became the opposite of what they were for: they would have pinned this
    //    message to printing a paste-ready file that fails startup. They are asserted ABSENT
    //    below instead, which is the same intent — the printed example must be one an operator
    //    can actually paste.
    assert!(msg.contains("[risk]"), "shows a [risk] table: {msg}");
    assert!(msg.contains("mode = \"live\""), "shows the mode the live mount demands: {msg}");
    assert!(
        !msg.contains("[event_source]") && !msg.contains("[broker]"),
        "the example must not print a table `RunProfile::validate` now refuses by name — an \
             operator pasting this would earn a refusal from the message telling them how to fix \
             a refusal: {msg}"
    );
    // 5. WHERE the fuller template lives, so the message is not the only copy.
    assert!(msg.contains(super::EXAMPLE_PROFILE_PATH), "names the shipped template: {msg}");
}

/// The OTHER half of the two-case split: a profile exists but omits a cap. The fix is an edit,
/// not a new file, so the message must NOT print a whole profile (which an operator would
/// reasonably paste over the file they already have, losing the rest of their config).
#[test]
fn supplied_profile_diagnostic_asks_for_an_edit_not_a_new_file() {
    let limits =
        vike_exec::RiskLimits { max_notional_per_order: Some(1.0), ..vike_exec::RiskLimits::new() };
    let msg = format!(
        "{}",
        super::require_live_risk_budget("okx", &limits, true).expect_err("still a refusal")
    );
    assert!(msg.contains("[risk]"), "still shows the table to edit: {msg}");
    assert!(msg.contains("max_total_exposure = 25000.0"), "shows a usable value: {msg}");
    assert!(!msg.contains("[broker]"), "must not print a whole replacement profile: {msg}");
    assert!(!msg.contains("mode = \"live\""), "must not print a whole replacement profile: {msg}");
    assert!(msg.contains("VIKE_RUN_PROFILE / --profile"), "says which file to edit: {msg}");
}

/// [`super::BUDGET_EXAMPLES`] must cover every key [`super::require_live_risk_budget`] can
/// report. A third account-dependent cap added there without a row here would silently vanish
/// from the inline `[risk]` example — the message would name a key in its `missing:` line and
/// then fail to show how to set it, which is exactly the discoverability hole this work closed.
#[test]
fn every_missing_key_has_an_example() {
    let err = super::require_live_risk_budget("binance", &vike_exec::RiskLimits::new(), false)
        .expect_err("no budget -> refusal naming every reportable key");
    let missing = match err {
        super::MountError::MissingRiskBudget { missing, .. } => missing,
    };
    for key in &missing {
        assert!(
            super::BUDGET_EXAMPLES.iter().any(|(k, _, _)| k == key),
            "`{key}` is reportable but has no BUDGET_EXAMPLES row, so the diagnostic cannot \
                 show how to set it"
        );
    }
}

/// Required test 5 (the make_engine-level proof): a PAPER mount — the arm every venue in
/// `all_roster_venues_absent_creds_stay_paper_and_inert` takes with an empty `.env` — succeeds
/// with `Ok` even though NEITHER account-dependent cap was ever supplied (no risk_profile at
/// all). `require_live_risk_budget` is gated on `live_venues.contains(venue)` at the
/// `make_engine` call site, which stays empty on the paper path, so the refusal never fires.
#[test]
fn paper_mount_starts_with_no_account_dependent_budget_at_all() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let (engine, _recon) = super::make_engine(
        LEGACY_REGISTRY,
        "binance",
        "BTCUSDT",
        &std::collections::HashMap::new(), // no creds -> paper
        &tx,
        &mut live,
        false, // reconcile off — this test is about the risk budget, not recon
        None,
        None,
        None, // no operator risk_profile either
        // ⚠ binance is ARMED by the ceiling here on purpose: this test's subject is that
        // ABSENT CREDENTIALS keep the budget refusal from firing, and a `paper` ceiling would
        // reach the same `Ok` one step earlier, without the refusal's own gate being consulted.
        Some(&super::armed_policy("binance", vike_config::VenueMode::Demo)),
    )
    .expect("a paper mount must never refuse to start over an unset account-dependent cap");
    assert_eq!(engine.gate.limits.max_notional_per_order, None);
    assert_eq!(engine.gate.limits.max_total_exposure, None);
}
