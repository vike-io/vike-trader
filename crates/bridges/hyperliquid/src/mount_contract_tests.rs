use super::*;
use std::assert_matches;
use vike_bridge_core::venue_mount_fixture::MountFixture;

/// A value for the account's `PRIVATE_KEY` field that does not parse as a secp256k1 key. Key
/// PRESENCE is the arming question; a key that does not parse is declined before any transport
/// exists, which is what lets the paper paths below run with no network.
const UNPARSEABLE_KEY: &str = "0xnotakey";

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture whose mounted account (`label`) holds a private key in its `PRIVATE_KEY` field.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[(config::PRIVATE_KEY, UNPARSEABLE_KEY)]);
    fx.account = label.clone();
    fx
}

/// The REAL `exchangeStatus` body, captured from the CI box on 2026-08-09 (moved from `vike-mount`,
/// whose `fixture()` helper read it).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/hyperliquid.json"))
        .expect("the captured fixture is valid JSON")
}

/// An exec client and a reconcile client that do nothing — `outcome_from_attempt` only moves them.
struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}
struct NoopRecon;
impl vike_exec::recon::ReconClient for NoopRecon {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(vec![])
    }
}

fn grid(tick: f64) -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: tick,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 1_000_000.0,
        min_notional: 10.0,
        ..Default::default()
    }
}

/// A live attempt as `live_mount_for_account` returns one: an isolated-only mounted asset and two
/// declared legs, inserted in declaration order.
fn live_attempt(master: Option<MasterOutcome>) -> LiveMountAttempt {
    let mut legs = IndexMap::new();
    legs.insert("ETH".to_string(), grid(0.01));
    legs.insert("SOL".to_string(), grid(0.001));
    LiveMountAttempt {
        live: Some(LiveMount {
            client: Box::new(NoopClient),
            recon: Box::new(NoopRecon),
            grid: grid(0.1),
            margin_mode: vike_model::MarginMode::Isolated,
            declared_leg_properties: legs,
        }),
        master,
    }
}

/// The rows `vike-mount`'s tables carried for hyperliquid, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = HyperliquidVenueMount.declaration();
    assert_eq!(HyperliquidVenueMount.venue(), "hyperliquid");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_recon_trigger, "the exec pump pokes the reconcile driver on every reconnect");
    assert_eq!(d.grid_source, DeclaredGridSource::InHand);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named { name_fields: &["ACCOUNT_ADDRESS"], evm_key_fields: &["PRIVATE_KEY"] }
    );
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "POST /info {\"type\":\"exchangeStatus\"} (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::NonceWindow,
        }
    );
}

/// THE MATRIX (decision 0095, decision 0156 item 6): the account's tier alone picks the network,
/// and the account's own key arms it — was `vike-mount`'s `would_mount_live` and its arming-probe
/// row. Under a `live` tier a missing key is `LiveCredentialsAbsent`; below it, `NoCredentials`. A
/// master address with no key is no key.
#[test]
fn resolve_arms_the_accounts_own_tier() {
    let no_live_key = Resolution::Paper(PaperCause::LiveCredentialsAbsent);
    let no_demo_key = Resolution::Paper(PaperCause::NoCredentials);
    let empty = MountFixture::new(&[]);
    let address_only = MountFixture::new(&[(config::ACCOUNT_ADDRESS, "0xmaster")]);
    let key = keyed(&AccountLabel::Default);
    for fx in [&empty, &address_only] {
        assert_eq!(HyperliquidVenueMount.resolve(&fx.inputs(AccountTier::Demo)), no_demo_key);
        assert_eq!(HyperliquidVenueMount.resolve(&fx.inputs(AccountTier::Live)), no_live_key);
    }
    assert_eq!(
        HyperliquidVenueMount.resolve(&key.inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    );
    assert_eq!(
        HyperliquidVenueMount.resolve(&key.inputs(AccountTier::Live)),
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None }
    );
}

/// Review Focus 2 at this venue: an account below `live` never resolves `Live`, whatever it holds.
#[test]
fn a_tier_below_live_never_resolves_live() {
    let key = keyed(&AccountLabel::Default);
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        assert!(!matches!(
            HyperliquidVenueMount.resolve(&key.inputs(tier)),
            Resolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// The account's FIELDS are the only credential the mount reads: a labelled account arms on its
/// own key, and a deployment value spelled like the field word arms nothing — so no account ever
/// signs with a key that is not its own.
#[test]
fn only_the_accounts_own_fields_arm() {
    assert_matches!(
        HyperliquidVenueMount.resolve(&keyed(&alt()).inputs(AccountTier::Demo)),
        Resolution::Armed { .. }
    );
    let deployment_only =
        MountFixture::new(&[]).with_vars(&[(config::PRIVATE_KEY, UNPARSEABLE_KEY)]);
    assert_eq!(
        HyperliquidVenueMount.resolve(&deployment_only.inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials),
        "a deployment's named values are never an account's credential"
    );
}

/// No key: paper, nothing to reconcile, nothing recorded — a master address alone is no key at
/// all. (Every case here stops before a transport exists, which is why it runs with no network.)
#[test]
fn without_the_accounts_key_the_mount_is_paper() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let empty = MountFixture::new(&[]);
    let address_only = MountFixture::new(&[(config::ACCOUNT_ADDRESS, "0xmaster")]);
    for (fx, tier) in [
        (&empty, AccountTier::Demo),
        (&empty, AccountTier::Live),
        (&address_only, AccountTier::Live),
    ] {
        let out = HyperliquidVenueMount.mount(fx.request(tier, "BTC", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper));
        assert!(out.recon.is_none() && out.identity.is_none());
    }
}

/// INTENT at `resolve`, outcome at `mount` — the stance the old probe row took on purpose: a key
/// the operator wrote arms (so `vike-mount`'s budget refusal fires before any session), and
/// `mount` then declines a key that does not parse — into paper, before any transport exists.
#[test]
fn an_unparseable_key_is_intent_at_resolve_and_paper_at_mount() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let fx = keyed(&AccountLabel::Default);
    assert_eq!(
        HyperliquidVenueMount.resolve(&fx.inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    );
    let out = HyperliquidVenueMount.mount(fx.request(AccountTier::Demo, "BTC", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE OUT-PARAMETERS, AS OUTCOME FIELDS: every venue fact the old arm took through `&mut`
/// (limits, margin mode) or folded itself (leg grids, recon) reaches `LiveExec`, in order — over a
/// hand-built `LiveMount`. The `MountFacts` tests below fill one from a real instruments load, and
/// `mount_tests.rs`'s `an_isolated_only_asset_resolves_isolated_and_an_ordinary_one_stays_cross`
/// names every link of the margin-mode wiring gate.
#[test]
fn a_live_attempt_folds_every_venue_fact_into_the_outcome() {
    let out = outcome_from_attempt(live_attempt(None), config::Network::Testnet);
    assert!(out.recon.is_some(), "HL's recon is always built on a live mount");
    assert!(out.identity.is_none(), "no probe answer, nothing to record");
    let ExecOutcome::Live(live) = out.exec else { panic!("a live attempt is a live outcome") };
    assert_eq!(live.bound_tier, AccountTier::Demo, "testnet is the DEMO tier");
    assert_eq!(live.grid, Some(grid(0.1)), "the mounted symbol's grid");
    assert_eq!(live.margin_mode, Some(vike_model::MarginMode::Isolated));
    assert_eq!(live.contract_size, None, "HL sizes in the base asset");
    assert_eq!(
        live.leg_grids,
        vec![("ETH".to_string(), grid(0.01)), ("SOL".to_string(), grid(0.001))],
        "the declared legs, in declaration order"
    );
}

/// The `userRole` probe runs BEFORE `meta`, so a mount can fail AFTER the venue confirmed the
/// account — and the old arm recorded that confirmation anyway. So does the outcome: identity on a
/// PAPER exec, with no recon.
#[test]
fn a_confirmed_master_is_reported_even_when_the_mount_fell_to_paper() {
    let master = MasterOutcome { address: "0xmaster".to_string(), confirmed: true };
    let out = outcome_from_attempt(
        LiveMountAttempt { live: None, master: Some(master) },
        config::Network::Mainnet,
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none());
    let id = out.identity.expect("a confirmed answer is worth recording");
    assert_eq!(
        (id.book.as_str(), id.evidence, id.tier),
        ("0xmaster", "`userRole`", AccountTier::Live)
    );
}

/// An unanswered or contradicted probe (`confirmed: false`) records nothing — on a live outcome
/// too.
#[test]
fn an_unconfirmed_master_records_nothing() {
    let master = MasterOutcome { address: "0xsigner".to_string(), confirmed: false };
    assert!(
        outcome_from_attempt(live_attempt(Some(master)), config::Network::Testnet)
            .identity
            .is_none()
    );
}

/// The recorded tier and the bound tier are the NETWORK the handshake ran on — never a ceiling
/// in scope (the trap `vike-mount`'s `confirmation_for_account` doc describes).
#[test]
fn the_identity_and_the_bound_tier_follow_the_network() {
    for (network, tier) in [
        (config::Network::Testnet, AccountTier::Demo),
        (config::Network::Mainnet, AccountTier::Live),
    ] {
        let master = MasterOutcome { address: "0xm".to_string(), confirmed: true };
        let out = outcome_from_attempt(live_attempt(Some(master)), network);
        assert_eq!(out.identity.expect("confirmed").tier, tier);
        let ExecOutcome::Live(live) = out.exec else { panic!("live") };
        assert_eq!(live.bound_tier, tier);
    }
}

// ---- the facts ONE instruments load hands a live mount (`MountFacts`) -------------------------

/// A universe as the venue's `meta`/`spotMeta` answer it: BTC and ETH perps and the PURR/USDC spot
/// pair as `instruments_tests.rs` spells them, plus CASHCAT, the isolated-only perp
/// `mount_tests.rs` derives from.
fn universe() -> crate::feeds::instruments::HyperliquidInstruments {
    let meta = serde_json::json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
        {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true}
    ]});
    let spot = serde_json::json!({
        "tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "PURR", "szDecimals": 0, "index": 1}
        ],
        "universe": [
            {"name": "PURR/USDC", "tokens": [1, 0], "index": 0, "isCanonical": true}
        ]
    });
    crate::feeds::instruments::HyperliquidInstruments::build(&meta, &spot, None)
}

/// What `live_mount_for_account` assembles once its two clients exist — the facts `symbol` reads
/// off `instruments` — as the contract's `LiveExec`.
fn mounted(
    instruments: &crate::feeds::instruments::HyperliquidInstruments,
    symbol: &str,
    legs: &[String],
) -> LiveExec {
    let live = MountFacts::of(instruments, symbol, legs)
        .into_live_mount(Box::new(NoopClient), Box::new(NoopRecon));
    let attempt = LiveMountAttempt { live: Some(live), master: None };
    let ExecOutcome::Live(exec) = outcome_from_attempt(attempt, config::Network::Testnet).exec
    else {
        panic!("a live mount is a live outcome")
    };
    exec
}

/// The mounted symbol's grid is the VENUE'S own, off the one instruments load — and every declared
/// leg the venue lists rides beside it in declaration order, the unlisted one with no row.
#[test]
fn the_mounted_grid_and_the_legs_grids_are_the_venues_own() {
    let instruments = universe();
    let venue_grid = *instruments.properties("CASHCAT").expect("CASHCAT is listed");
    assert_ne!(
        venue_grid,
        fallback_properties(),
        "precondition: the venue's grid is not the fallback"
    );
    let legs = ["ETH".to_string(), "NOT-LISTED".to_string(), "PURR/USDC".to_string()];
    let exec = mounted(&instruments, "CASHCAT", &legs);
    assert_eq!(exec.grid, Some(venue_grid), "a listed symbol mounts the venue's grid");
    let listed = |leg: &str| (leg.to_string(), *instruments.properties(leg).expect("listed"));
    assert_eq!(
        exec.leg_grids,
        vec![listed("ETH"), listed("PURR/USDC")],
        "the legs the venue lists, in declaration order; the one it does not list gets no row"
    );
}

/// …and so does its RULING margin mode: an isolated-only asset reaches `LiveExec::margin_mode` as
/// `Isolated` — the construction link between `mount_tests.rs`'s derivation and `vike-mount`'s fold.
#[test]
fn the_mounted_symbols_ruling_margin_mode_reaches_the_outcome() {
    let venue_default = vike_model::caps_for(crate::base::consts::VENUE).default_margin_mode;
    assert_ne!(
        venue_default,
        vike_model::MarginMode::Isolated,
        "precondition: the per-venue default is not the mode an isolated-only asset rules in"
    );
    let exec = mounted(&universe(), "CASHCAT", &[]);
    assert_eq!(exec.margin_mode, Some(vike_model::MarginMode::Isolated));
}

/// A symbol `meta` does not list mounts on the fallback grid and the per-VENUE margin mode, and the
/// reconcile product follows the mounted symbol — perp when it is not listed.
#[test]
fn an_unlisted_symbol_mounts_the_fallback_grid_and_the_venues_default_mode() {
    let instruments = universe();
    let exec = mounted(&instruments, "NOT-LISTED", &[]);
    assert_eq!(exec.grid, Some(fallback_properties()));
    assert_eq!(
        exec.margin_mode,
        Some(vike_model::caps_for(crate::base::consts::VENUE).default_margin_mode)
    );
    let product = |symbol: &str| MountFacts::of(&instruments, symbol, &[]).product;
    assert_eq!(product("NOT-LISTED"), config::Product::Perp);
    assert_eq!(product("CASHCAT"), config::Product::Perp);
    assert_eq!(product("PURR/USDC"), config::Product::Spot);
}

// ---- the clock PARSE, against the real captured body (moved from vike-mount) -------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&clock_body()), Ok(1_786_242_384_206));
}

#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("time")));
}
