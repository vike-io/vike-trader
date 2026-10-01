use std::cell::Cell;

use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};
use vike_model::{FillReport, OrderRequest, OrderStatusReport, PositionStatusReport};

/// The one key `load_ibkr_config_for_account` requires: the Gateway holds the login, so the
/// account number is the whole live gate.
const KEYS: &[(&str, &str)] = &[("IBKR_DEMO_ACCOUNT", "DU1234567")];

/// The LIVE tier's account key, assembled rather than spelled as one literal: every env-shaped
/// string literal in a `src/` file is a sighting to the settings registry's sweep
/// (`crates/vike-ops/src/scan.rs`'s `find_map_lookups`), and this fixture is not a read — the mount
/// resolves the DEMO tier only.
const LIVE_ACCOUNT: &str = concat!("IBKR", "_LIVE_ACCOUNT");

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The DEMO tier's `venue.ibkr.demo.<field>` rows — the gateway, which the mount reads from
/// `MountInputs::settings` (decision 0095, Task 7).
fn demo_gateway(rows: &[(&str, &str)]) -> vike_secrets::venue_setting::VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some("demo".to_string()),
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &rows)
}

/// A fixture holding `KEYS` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// Whether anything accepts a TCP connection on `127.0.0.1:port`. The dead-port test points a
/// config at two ports; a box where either answers cannot say what a dead port does, so the test
/// skips BEFORE mounting. Deciding the skip from the mount's own outcome instead would make the
/// outcome it asserts on unfalsifiable.
fn something_listens_on(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(250),
    )
    .is_ok()
}

/// The seam's stand-in for the cpapi reconcile client: it answers nothing, and nothing asks it.
struct NoReports;

impl ReconClient for NoReports {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(vec![])
    }
}

/// The seam's stand-in for a connected Gateway session: it accepts every call and sends nothing.
struct Inert;

impl ExecutionClient for Inert {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The rows `vike-mount`'s tables carried for ibkr. ⚠ This is now the ONLY place they are
/// pinned: `vike-mount`'s own registry carries ibkr `FeatureAbsent` and answers the generic facts
/// (`NoGrid`, `Undeterminable`, the absent clock), because that crate no longer compiles this
/// bridge. The clock reason is the text `vike-mount`'s `CLOCK_SOURCES` carried, whole.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = IbkrVenueMount.declaration();
    assert_eq!(IbkrVenueMount.venue(), "ibkr");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "IBKR",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT"],
            evm_key_suffixes: &[],
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: "feature-gated here, and both backends put the clock behind an authenticated \
                     TWS socket / local CP-Gateway session this pre-mount step does not open",
            unmeasured_risk: None,
        }
    );
}

/// The arming-probe row as it was: a DEMO config the loader accepts arms a demo-only arm; one it
/// refuses — no account, an unparseable port, an unknown backend — stays paper.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            IbkrVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(IbkrVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)), armed);
        for broken in [("port", "not-a-port"), ("backend", "grpc")] {
            let mut fx = MountFixture::new(&[KEYS[0]]);
            fx.settings = demo_gateway(&[broken]);
            assert_eq!(
                IbkrVenueMount.resolve(&fx.inputs(live)),
                Resolution::Paper(PaperCause::NoCredentials),
                "{broken:?}: a config the loader refuses is no config"
            );
        }
    }
}

/// Review Focus 2 at this venue: it resolves the DEMO tier and nothing else, so no ceiling makes
/// it resolve `Live` — a populated LIVE-tier `_ACCOUNT` beside the DEMO one reaches no mount, and
/// the answer under either ceiling is the demo-only arm.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = keyed(&AccountLabel::Default);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    for live in [false, true] {
        let resolved = IbkrVenueMount.resolve(&fx.inputs(live));
        assert!(
            !matches!(resolved, Resolution::Armed { tier: Tier::Live, .. }),
            "live={live}: resolved the LIVE tier"
        );
        assert_eq!(
            resolved,
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            "live={live}"
        );
    }
}

/// **A LIVE-tier `_ACCOUNT` the arm cannot use is a NAMED cause.** The mount resolves the DEMO tier
/// and nothing else, so a store holding only the LIVE account arms nothing under either ceiling — and
/// says WHICH tier it will not use. The LIVE account KEY is the whole credential here (the Gateway
/// holds the login), so it is detected by that key alone: a live-tier GATEWAY row nobody can parse
/// must not turn the named cause back into "the store holds nothing".
#[test]
fn a_live_account_alone_stays_paper_for_a_named_reason() {
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    for live in [false, true] {
        assert_eq!(
            IbkrVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "live={live}"
        );
    }
    let live_rows: Vec<vike_secrets::VenueSettingRow> = [("PORT", "not-a-port")]
        .into_iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some("live".to_string()),
            field: field.to_string(),
            value: value.to_string(),
        })
        .collect();
    fx.settings = vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &live_rows);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "an unparseable LIVE gateway row does not hide the live account"
    );
}

/// The cause is scoped to the ACCOUNT asking, in both directions.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_key() {
    let mut default_live = MountFixture::new(&[]);
    default_live.vars.insert(LIVE_ACCOUNT.to_string(), "U1".to_string());
    let mut alt_live = MountFixture::new(&[]);
    alt_live.vars.insert(account_key(LIVE_ACCOUNT, &alt()), "U2".to_string());
    alt_live.account = alt();
    assert_eq!(
        IbkrVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    default_live.account = alt();
    assert_eq!(
        IbkrVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds a live account key"
    );
    assert_eq!(
        IbkrVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        IbkrVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds a live account key"
    );
}

/// The stay-paper half at the MOUNT: a LIVE account alone builds no reconcile client, dials no
/// Gateway and fetches no grid — the three network steps are panicking doubles.
#[test]
fn a_live_account_alone_mounts_paper_and_dials_nothing() {
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let out = mount_with(
        req,
        |_, _| panic!("no demo account: the reconcile factory must not be called"),
        |_, _| panic!("no demo account: nothing may be dialled"),
        |_, _| panic!("no demo account: no grid may be fetched"),
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// The account 2×2: a labelled account reads its OWN `_ACCOUNT` key and never the default
/// account's — borrowing it would place a second account's orders in the first account's book.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        IbkrVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's `_ACCOUNT`"
    );
    assert!(matches!(
        IbkrVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        IbkrVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// Absent config: paper, and no reconcile client even with reconciliation ON. Offline is proved
/// through `mount_with`, whose three network steps are panicking doubles in the second half: the
/// config is the gate, and without it `mount` returns before anything is built or dialled.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
    // ON, so a `None` recon is the absent config's doing rather than the laziness gate's.
    req.recon_enabled = true;
    let out = IbkrVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());

    let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let out = mount_with(
        req,
        |_, _| panic!("no config: the reconcile factory must not be called"),
        |_, _| panic!("no config: nothing may be dialled"),
        |_, _| panic!("no config: no grid may be fetched"),
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE CONNECTED BRANCH, through the seam: the session reaches the fold at the DEMO tier — what
/// `IBKR_DEMO_*` authenticates, under either ceiling — with the mounted symbol's pre-fetched grid,
/// no contract size, margin mode or leg grid, and the reconcile client built before the connect
/// KEPT.
#[test]
fn a_connected_mount_is_a_demo_session_with_its_grid_and_its_recon() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let props =
        SymbolProperties { tick_size: 0.01, step_size: 1.0, min_qty: 1.0, ..Default::default() };
    for live in [false, true] {
        let mut req = fx.request(live, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        let out = mount_with(
            req,
            |_, _| Some(Box::new(NoReports)),
            |_, _| Ok(Box::new(Inert)),
            |_, symbol| {
                assert_eq!(symbol, "AAPL.SMART.USD", "the grid is the mounted symbol's");
                Some(props)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("live={live}: a connected session must be a LIVE outcome")
        };
        assert_eq!(exec.bound_tier, Tier::Demo, "live={live}: `IBKR_DEMO_*` is a DEMO session");
        assert_eq!(exec.grid, Some(props));
        assert_eq!((exec.contract_size, exec.margin_mode), (None, None));
        assert!(exec.leg_grids.is_empty());
        assert!(out.recon.is_some(), "the reconcile client built before the connect is kept");
        assert!(out.identity.is_none());
    }
}

/// THE DEMOTION'S DROP, through the seam: a reconcile client built before a connect that FAILS is
/// dropped — a paper engine must not reconcile against the account the failed session was for —
/// and no grid is fetched for a session that never connected.
#[test]
fn a_failed_connect_drops_the_recon_it_built_first() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let built = Cell::new(false);
    let out = mount_with(
        req,
        |_, _| {
            built.set(true);
            Some(Box::new(NoReports))
        },
        |_, _| Err(IbkrError::Connect("connection refused".to_string())),
        |_, _| panic!("a session that never connected must not pre-fetch a grid"),
    );
    // The premise: without it, `out.recon` would be `None` for the wrong reason.
    assert!(built.get(), "a reconcile client was built before the connect");
    assert!(matches!(out.exec, ExecOutcome::Paper), "a failed connect demotes to paper");
    assert!(out.recon.is_none(), "the demotion drops the reconcile client it built");
    assert!(out.identity.is_none());
}

/// THE CONNECT-FAILURE DEMOTION against the real network steps: a present DEMO config pointed at
/// dead ports fails the synchronous connect, and the mount returns a PAPER outcome with no
/// reconcile client and no identity. Needs no Gateway — nothing listens on 127.0.0.1:6553{4,3}, so
/// both handshakes are refused at once. SELF-SKIPS, before mounting, on a box where either port
/// answers.
///
/// ⚠ The DROP of a reconcile client built before the failed connect is
/// `a_failed_connect_drops_the_recon_it_built_first`'s to prove: here the CP Gateway port is dead
/// too, so `recon_client` itself resolves `None` and `out.recon` would be `None` either way.
#[test]
fn a_present_config_without_a_gateway_demotes_to_paper() {
    if something_listens_on(65534) || something_listens_on(65533) {
        eprintln!("skipping: something answers on the IBKR test ports 65534/65533");
        return;
    }
    let mut fx = MountFixture::new(&[("IBKR_DEMO_ACCOUNT", "DUTEST000")]);
    fx.settings = demo_gateway(&[
        ("backend", "socket"),
        ("port", "65534"),
        ("cpapi_url", "https://127.0.0.1:65533"),
    ]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "AAPL.SMART.USD", &tx);
    // Reconciliation ON, so the cpapi factory is reached (and refused) rather than skipped by the
    // laziness gate.
    req.recon_enabled = true;
    let out = IbkrVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper), "a refused connect demotes to paper");
    assert!(out.recon.is_none(), "a demoted mount holds no reconcile client");
    assert!(out.identity.is_none());
}

/// A demo account whose gateway settings the loader REFUSES, beside a live account, is not "the live
/// tier alone": its fault is a setting, and the live account is not what keeps the venue on paper. The
/// cause stays the original one rather than blaming a tier that is not the problem.
#[test]
fn a_broken_demo_config_beside_a_live_account_is_not_the_live_tier() {
    let mut fx = keyed(&AccountLabel::Default);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    fx.settings = demo_gateway(&[("port", "not-a-port")]);
    for live in [false, true] {
        assert_eq!(
            IbkrVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "live={live}"
        );
    }
}
