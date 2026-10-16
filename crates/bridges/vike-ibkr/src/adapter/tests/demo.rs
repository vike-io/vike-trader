//! Resolve and the DEMO-tier start: the declaration, the arming gate, connect and demotion.

use std::assert_matches;
use std::cell::Cell;

use super::*;

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// Whether anything accepts a TCP connection on `127.0.0.1:port`. The dead-port test points a
/// config at two ports; a box where either answers cannot say what a dead port does, so the test
/// skips BEFORE starting. Deciding the skip from the start's own outcome instead would make the
/// outcome it asserts on unfalsifiable.
fn something_listens_on(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(250),
    )
    .is_ok()
}

/// The rows `vike-tradehub-core`'s tables carried for ibkr, the book identity now spelled by FIELD
/// (decision 0156 item 6). ⚠ This is now the ONLY place they are pinned: `vike-tradehub-core`'s own
/// registry carries ibkr `FeatureAbsent` and answers the generic facts (`NoGrid`, `Undeterminable`,
/// the absent clock), because that crate no longer compiles this bridge. The clock reason is the
/// text `vike-tradehub-core`'s `CLOCK_SOURCES` carried, whole.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = IbkrVenueAdapter.declaration();
    assert_eq!(IbkrVenueAdapter.venue(), "ibkr");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named { name_fields: &["ACCOUNT"], evm_key_fields: &[] }
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

/// The arming-probe row as it was, for a `demo` account: a config the loader accepts arms the
/// demo-only arm; one it refuses — no account, an unparseable port, an unknown backend — stays
/// paper. A `live` account holding nothing is the same `NoCredentials`, and a `live` account's
/// `ACCOUNT` never answers the demo arm (it is a live account).
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed = Resolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(HeldBelowLive::DemoOnlyArm),
    };
    assert_eq!(
        IbkrVenueAdapter.resolve(&AdapterFixture::new(&[]).inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert_eq!(
        IbkrVenueAdapter.resolve(&keyed(&AccountLabel::Default).inputs(AccountTier::Demo)),
        armed
    );
    for broken in [("port", "not-a-port"), ("backend", "grpc")] {
        let mut fx = AdapterFixture::new(KEYS);
        fx.settings = demo_gateway(&[broken]);
        assert_eq!(
            IbkrVenueAdapter.resolve(&fx.inputs(AccountTier::Demo)),
            Resolution::Paper(PaperCause::NoCredentials),
            "{broken:?}: a config the loader refuses is no config"
        );
    }
    assert_eq!(
        IbkrVenueAdapter.resolve(&AdapterFixture::new(&[]).inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert_ne!(
        IbkrVenueAdapter.resolve(&keyed(&AccountLabel::Default).inputs(AccountTier::Live)),
        armed,
        "a `live` account's `ACCOUNT` is never mounted on the demo tier"
    );
}

/// **A tier below `live` never resolves live.** A `demo` account's `ACCOUNT` arms the demo-only arm
/// even when the `account` table holds an ACTIVE ibkr/live row for the same label and the live
/// gateway rows load: the account's tier is the one thing that picks the arm, and the table cannot
/// lift a demo account to real money.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = live_store(&AccountLabel::Default, vec![account_row(VENUE, "live", None, true)]);
    fx.settings = live_gateway(&[("port", "4101"), ("backend", "socket")]);
    let resolved = IbkrVenueAdapter.resolve(&fx.inputs(AccountTier::Demo));
    assert!(
        !matches!(resolved, Resolution::Armed { tier: AccountTier::Live, .. }),
        "a demo account resolved the LIVE tier"
    );
    assert_eq!(
        resolved,
        Resolution::Armed {
            tier: AccountTier::Demo,
            held_below_live: Some(HeldBelowLive::DemoOnlyArm),
        }
    );
}

/// **A `live` account's `ACCOUNT` the arm cannot use is a NAMED cause.** With no account table
/// read, a `live` account holding its `ACCOUNT` arms nothing — and says WHICH tier it will not use.
/// The `ACCOUNT` field is the whole credential here (the Gateway holds the login), so it is
/// detected by that field alone: a live-tier GATEWAY row nobody can parse must not turn the named
/// cause back into "the store holds nothing".
#[test]
fn a_live_account_alone_stays_paper_for_a_named_reason() {
    let mut fx = AdapterFixture::new(&[("ACCOUNT", "U13112916")]);
    assert_eq!(
        IbkrVenueAdapter.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    fx.settings = live_gateway(&[("PORT", "not-a-port")]);
    assert_eq!(
        IbkrVenueAdapter.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "an unparseable LIVE gateway row does not hide the live account"
    );
}

/// The stay-paper half at the START: a `live` account that does not arm builds no reconcile client,
/// dials no Gateway and fetches no grid — the three network steps are panicking doubles.
#[test]
fn a_live_account_alone_starts_paper_and_dials_nothing() {
    let fx = AdapterFixture::new(&[("ACCOUNT", "U13112916")]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let out = start_with(
        req,
        |_, _| panic!("a refused live account: the reconcile factory must not be called"),
        |_, _| panic!("(cpapi) a refused live account: the reconcile factory must not be called"),
        |_, _| panic!("a refused live account: nothing may be dialled"),
        |_, _| panic!("a refused live account: no grid may be fetched"),
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// A labelled account arms from ITS OWN fields — the credentials it is handed — and from nothing
/// else: the deployment's NAMED values (`AdapterInputs::secrets`) are never read for an account
/// credential, so a store-wide `{VENUE}_{TIER}_ACCOUNT` name (composed here, so the settings
/// registry's literal sweep reads no variable) arms nobody.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_matches!(
        IbkrVenueAdapter.resolve(&keyed(&alt()).inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, .. }
    );
    for label in [AccountLabel::Default, alt()] {
        let mut fx = AdapterFixture::new(&[]).with_vars(&[
            (concat!("IBKR", "_DEMO_ACCOUNT"), "DU1234567"),
            ("ACCOUNT", "DU1234567"),
        ]);
        fx.account = label.clone();
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert_eq!(
                IbkrVenueAdapter.resolve(&fx.inputs(tier)),
                Resolution::Paper(PaperCause::NoCredentials),
                "{label} at {tier}: a deployment name is no account's credential"
            );
        }
    }
}

/// Absent config: paper, and no reconcile client even with reconciliation ON. Offline is proved
/// through `start_with`, whose three network steps are panicking doubles in the second half: the
/// config is the gate, and without it `start` returns before anything is built or dialled.
#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "AAPL.SMART.USD", &tx);
    // ON, so a `None` recon is the absent config's doing rather than the laziness gate's.
    req.recon_enabled = true;
    let out = IbkrVenueAdapter.start(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());

    for tier in [AccountTier::Demo, AccountTier::Live] {
        let mut req = fx.request(tier, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        let out = start_with(
            req,
            |_, _| panic!("no config: the reconcile factory must not be called"),
            |_, _| panic!("(cpapi) no config: the reconcile factory must not be called"),
            |_, _| panic!("no config: nothing may be dialled"),
            |_, _| panic!("no config: no grid may be fetched"),
        );
        assert!(matches!(out.exec, ExecOutcome::Paper), "{tier}");
        assert!(out.recon.is_none() && out.identity.is_none(), "{tier}");
    }
}

/// THE CONNECTED BRANCH, through the seam: a `demo` account's session reaches the fold at the DEMO
/// tier, on the config loaded at that tier, with the engine symbol's pre-fetched grid, no contract
/// size, margin mode or leg grid, and the reconcile client built before the connect KEPT.
#[test]
fn a_connected_start_is_a_demo_session_with_its_grid_and_its_recon() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let props =
        SymbolProperties { tick_size: 0.01, step_size: 1.0, min_qty: 1.0, ..Default::default() };
    let mut req = fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let out = start_with(
        req,
        |_, _| Some(Box::new(NoReports)),
        |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
        |cfg, _| {
            assert_eq!(cfg.tier, AccountTier::Demo, "connects on the DEMO tier's config");
            assert_eq!(cfg.account, KEYS[0].1);
            Ok(Box::new(Inert))
        },
        |_, symbol| {
            assert_eq!(symbol, "AAPL.SMART.USD", "the grid is the mounted symbol's");
            Some(props)
        },
    );
    let ExecOutcome::Live(exec) = out.exec else {
        panic!("a connected session must be a LIVE outcome")
    };
    assert_eq!(exec.bound_tier, AccountTier::Demo, "a demo account is a DEMO session");
    assert_eq!(exec.grid, Some(props));
    assert_eq!(
        (exec.contract_value, exec.margin_mode),
        (Some(vike_model::ContractValue::linear(props.multiplier())), None),
        "the contract size is the grid's own"
    );
    assert!(exec.leg_grids.is_empty());
    assert!(out.recon.is_some(), "the reconcile client built before the connect is kept");
    assert!(out.identity.is_none());
}

/// THE DEMOTION'S DROP, through the seam: a reconcile client built before a connect that FAILS is
/// dropped — a paper engine must not reconcile against the account the failed session was for —
/// and no grid is fetched for a session that never connected.
#[test]
fn a_failed_connect_drops_the_recon_it_built_first() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let built = Cell::new(false);
    let out = start_with(
        req,
        |_, _| {
            built.set(true);
            Some(Box::new(NoReports))
        },
        |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
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
/// dead ports fails the synchronous connect, and the start returns a PAPER outcome with no
/// reconcile client and no identity. Needs no Gateway — nothing listens on 127.0.0.1:6553{4,3}, so
/// both handshakes are refused at once. SELF-SKIPS, before starting, on a box where either port
/// answers.
///
/// ⚠ The DROP of a reconcile client built before the failed connect is
/// `a_failed_connect_drops_the_recon_it_built_first`'s to prove: here the gateway port is dead too,
/// so the socket reconcile factory itself resolves `None` and `out.recon` would be `None` either way.
#[test]
fn a_present_config_without_a_gateway_demotes_to_paper() {
    if something_listens_on(65534) || something_listens_on(65533) {
        eprintln!("skipping: something answers on the IBKR test ports 65534/65533");
        return;
    }
    let mut fx = AdapterFixture::new(&[("ACCOUNT", "DUTEST000")]);
    fx.settings = demo_gateway(&[
        ("backend", "socket"),
        ("port", "65534"),
        ("cpapi_url", "https://127.0.0.1:65533"),
    ]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx);
    // Reconciliation ON, so the socket reconcile factory is reached (and refused) rather than
    // skipped by the laziness gate.
    req.recon_enabled = true;
    let out = IbkrVenueAdapter.start(req);
    assert!(matches!(out.exec, ExecOutcome::Paper), "a refused connect demotes to paper");
    assert!(out.recon.is_none(), "a demoted mount holds no reconcile client");
    assert!(out.identity.is_none());
}

/// The account's tier picks the gateway rows: a `demo` account whose DEMO gateway rows the loader
/// refuses does not run with the original cause, even when the LIVE tier's rows would load — they
/// are another tier's gateway, and its `ACCOUNT` is no live account.
#[test]
fn a_broken_demo_gateway_is_not_rescued_by_the_live_rows() {
    let mut fx = keyed(&AccountLabel::Default);
    let rows: Vec<vike_secrets::VenueSettingRow> =
        [(AccountTier::Demo, "PORT", "not-a-port"), (AccountTier::Live, "PORT", "4101")]
            .into_iter()
            .map(|(tier, field, value)| vike_secrets::VenueSettingRow {
                venue: VENUE.to_string(),
                tier: Some(tier),
                field: field.to_string(),
                value: value.to_string(),
            })
            .collect();
    fx.settings = vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &rows);
    assert_eq!(
        IbkrVenueAdapter.resolve(&fx.inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}
