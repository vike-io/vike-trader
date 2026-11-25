//! Resolve and the DEMO-tier start: the declaration, the arming gate, connect and demotion.

use std::assert_matches;
use std::cell::RefCell;

use super::*;
use crate::testing::something_listens_on;

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The rows `vike-tradehub-core`'s tables carried for ibkr, the book identity now spelled by FIELD
/// (decision 0156 item 6). ⚠ This is now the ONLY place they are pinned: `vike-tradehub-core`'s own
/// registry carries ibkr `FeatureAbsent` and answers the generic facts (`NoGrid`, `Undeterminable`,
/// the absent clock), because that crate no longer compiles this bridge. The clock reason is the
/// text `vike-tradehub-core`'s `CLOCK_SOURCES` carried, whole.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterIbkr.declaration();
    assert_eq!(VenueAdapterIbkr.venue(), "ibkr");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert_eq!(d.grid_source, GridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentityRule::Named { name_fields: &["ACCOUNT"], evm_key_fields: &[] }
    );
    assert_eq!(
        d.clock,
        ClockDeclaration::NotWired {
            reason: "feature-gated here, and both backends put the clock behind an authenticated \
                     TWS socket / local CP-Gateway session this pre-mount step does not open",
            unmeasured_risk: None,
        }
    );
}

/// The arming-probe row as it was, for a `demo` account: a config the loader accepts arms the
/// demo-only arm; one it refuses — no account, no address row, an unknown backend — does not
/// run. A `live` account holding nothing is the same `NoCredentials`, and a `live` account's
/// `ACCOUNT` never answers the demo arm (it is a live account).
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed = ArmingResolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
    };
    assert_eq!(
        VenueAdapterIbkr.resolve(&AdapterFixture::new(&[]).inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
    assert_eq!(
        VenueAdapterIbkr.resolve(&keyed(&AccountLabel::Default).inputs(AccountTier::Demo)),
        armed
    );
    for (what, broken) in [
        ("no address row", no_addresses()),
        ("an unknown backend", demo_gateway(&[("backend", "grpc")])),
    ] {
        let mut fx = AdapterFixture::new(KEYS);
        fx.settings = broken;
        assert_eq!(
            VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Demo)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "{what}: a config the loader refuses is no config"
        );
    }
    assert_eq!(
        VenueAdapterIbkr.resolve(&AdapterFixture::new(&[]).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
    assert_ne!(
        VenueAdapterIbkr.resolve(&keyed(&AccountLabel::Default).inputs(AccountTier::Live)),
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
    fx.settings = live_gateway(&[("backend", "socket")]);
    let resolved = VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Demo));
    assert!(
        !matches!(resolved, ArmingResolution::Armed { tier: AccountTier::Live, .. }),
        "a demo account resolved the LIVE tier"
    );
    assert_eq!(
        resolved,
        ArmingResolution::Armed {
            tier: AccountTier::Demo,
            held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
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
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    fx.settings = live_gateway(&[("backend", "grpc")]);
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired),
        "an unloadable LIVE gateway row does not hide the live account"
    );
}

/// The stay-paper half at the START: a `live` account that does not arm builds no reconcile client,
/// dials no Gateway and fetches no grid — the three network steps are panicking doubles.
#[test]
fn a_live_account_alone_starts_paper_and_dials_nothing() {
    let fx = AdapterFixture::new(&[("ACCOUNT", "U13112916")]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "AAPL", &tx);
    req.reconcile_enabled = true;
    let out = start_with(
        req,
        |_, _| panic!("a refused live account: the reconcile factory must not be called"),
        |_, _| panic!("(cpapi) a refused live account: the reconcile factory must not be called"),
        |_, _| panic!("a refused live account: nothing may be dialled"),
        |_, _| panic!("a refused live account: no grid may be fetched"),
    );
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// A labelled account arms from ITS OWN fields — the credentials it is handed — and from nothing
/// else: the deployment's NAMED values (`AdapterInputs::secrets`) are never read for an account
/// credential, so a store-wide `{VENUE}_{TIER}_ACCOUNT` name (composed here, so the settings
/// registry's literal sweep reads no variable) arms nobody.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_matches!(
        VenueAdapterIbkr.resolve(&keyed(&alt()).inputs(AccountTier::Demo)),
        ArmingResolution::Armed { tier: AccountTier::Demo, .. }
    );
    for label in [AccountLabel::Default, alt()] {
        let mut fx = AdapterFixture::new(&[]).with_vars(&[
            (concat!("IBKR", "_DEMO_ACCOUNT"), "DU1234567"),
            ("ACCOUNT", "DU1234567"),
        ]);
        fx.account = label.clone();
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert_eq!(
                VenueAdapterIbkr.resolve(&fx.inputs(tier)),
                ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
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
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "AAPL", &tx);
    // ON, so a `None` recon is the absent config's doing rather than the laziness gate's.
    req.reconcile_enabled = true;
    let out = VenueAdapterIbkr.start(req);
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());

    for tier in [AccountTier::Demo, AccountTier::Live] {
        let mut req = fx.request(tier, "AAPL", &tx);
        req.reconcile_enabled = true;
        let out = start_with(
            req,
            |_, _| panic!("no config: the reconcile factory must not be called"),
            |_, _| panic!("(cpapi) no config: the reconcile factory must not be called"),
            |_, _| panic!("no config: nothing may be dialled"),
            |_, _| panic!("no config: no grid may be fetched"),
        );
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "{tier}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "{tier}");
    }
}

/// THE CONNECTED BRANCH, through the seam: a `demo` account's session reaches the fold at the DEMO
/// tier, on the config loaded at that tier, with the engine symbol's pre-fetched grid, no contract
/// size, margin mode or leg grid, and the reconcile client (built after the connect) KEPT.
#[test]
fn a_connected_start_is_a_demo_session_with_its_grid_and_its_reconcile() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_execution::event_channel(8);
    let properties =
        SymbolProperties { tick_size: 0.01, step_size: 1.0, min_qty: 1.0, ..Default::default() };
    let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
    req.reconcile_enabled = true;
    let out = start_with(
        req,
        |_, _| Some(Box::new(ReconcileClientFake::default())),
        |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
        |config, _| {
            assert_eq!(config.tier, AccountTier::Demo, "connects on the DEMO tier's config");
            assert_eq!(config.account, KEYS[0].1);
            session(config, ExecutionClientInert)
        },
        |_, symbol| {
            assert_eq!(symbol, "AAPL", "the grid is the mounted symbol's");
            Some(properties)
        },
    );
    let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
        panic!("a connected session must be a LIVE outcome")
    };
    assert_eq!(exec.bound_tier, AccountTier::Demo, "a demo account is a DEMO session");
    assert_eq!(exec.grid, Some(properties));
    assert_eq!(
        (exec.contract_value, exec.margin_mode),
        (Some(vike_model::ContractValue::linear(properties.multiplier())), None),
        "the contract size is the grid's own"
    );
    assert!(exec.leg_grids.is_empty());
    assert!(out.reconcile.is_some(), "the reconcile client built after the connect is kept");
    assert!(out.identity.is_none());
}

/// THE DEMOTION, through the seam: a connect that FAILS (or that the link watchdog's rebuild schedule
/// refuses) dials nothing else — no reconcile client and no grid for a session that never connected: one
/// connection attempt at a time, exec first, and a failed one ends the start.
#[test]
fn a_failed_connect_dials_neither_the_grid_nor_the_reconcile() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
    req.reconcile_enabled = true;
    let out = start_with(
        req,
        |_, _| panic!("a connect that failed must not dial a reconcile client"),
        |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
        |_, _| Err(IbkrError::Connect("connection refused".to_string())),
        |_, _| panic!("a session that never connected must not pre-fetch a grid"),
    );
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::Unreachable),
        "a failed connect is the venue unreachable (transient), never a refusal"
    );
    assert!(out.reconcile.is_none(), "a demoted mount holds no reconcile client");
    assert!(out.identity.is_none());
}

/// **Only a failed CONNECT is transient** (owner 2026-11-15): a session that connected and then failed (a protocol
/// refusal) or a transport this build cannot open is the venue's or the build's refusal, `NotRunning`
/// (`ArmingBlock::AdapterFailed` at the host), which the tradehub does not ask again by itself.
#[test]
fn a_failure_after_the_connect_is_a_refusal_not_an_unreachable_venue() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_execution::event_channel(8);
    for refusal in [IbkrError::Protocol("login refused".to_string()), IbkrError::Unavailable] {
        let what = refusal.to_string();
        let out = start_with(
            fx.request(AccountTier::Demo, "AAPL", &tx),
            |_, _| panic!("a refused start must not dial a reconcile client"),
            |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
            move |_, _| Err(refusal.clone()),
            |_, _| panic!("a refused start must not pre-fetch a grid"),
        );
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "{what}");
    }
}

/// ONE CONNECTION AT A TIME, EXEC FIRST: the exec connect, then the grid pre-fetch, then the reconcile
/// client — the order the gateway is dialled in (2026-10-17: it misbehaves under rapid or simultaneous
/// reconnects, and `GatewayGate::dial` spaces them).
#[test]
fn the_start_dials_execution_then_the_grid_then_the_reconcile() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
    req.reconcile_enabled = true;
    let calls = RefCell::new(Vec::new());
    let out = start_with(
        req,
        |_, _| {
            calls.borrow_mut().push("recon");
            Some(Box::new(ReconcileClientFake::default()))
        },
        |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
        |config, _| {
            calls.borrow_mut().push("exec");
            session(config, ExecutionClientInert)
        },
        |_, _| {
            calls.borrow_mut().push("grid");
            None
        },
    );
    assert_eq!(*calls.borrow(), ["exec", "grid", "recon"]);
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)));
    assert!(out.reconcile.is_some());
}

/// THE CONNECT-FAILURE DEMOTION against the real network steps: a present DEMO config pointed at
/// dead ports fails the synchronous connect, and the start returns a PAPER outcome with no
/// reconcile client and no identity. Needs no Gateway — nothing listens on 127.0.0.1:6553{4,3}, so
/// both handshakes are refused at once. SELF-SKIPS, before starting, on a box where either port
/// answers.
#[test]
fn a_present_config_without_a_gateway_demotes_to_paper() {
    if something_listens_on(65534) || something_listens_on(65533) {
        eprintln!("skipping: something answers on the IBKR test ports 65534/65533");
        return;
    }
    let mut fx = AdapterFixture::new(&[("ACCOUNT", "DUTEST000")]);
    fx.settings = demo_gateway(&[("backend", "socket")]).with_urls(addresses(
        UrlType::Demo,
        "127.0.0.1:65534",
        "https://127.0.0.1:65533",
    ));
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
    // Reconciliation ON, so the demotion is shown to leave NO reconcile client (the refused exec connect ends
    // the start before the reconcile factory is reached).
    req.reconcile_enabled = true;
    let out = VenueAdapterIbkr.start(req);
    assert!(
        matches!(
            out.exec,
            VenueAdapterExecutionOutcome::Unreachable | VenueAdapterExecutionOutcome::NotRunning
        ),
        "a refused connect does not run: unreachable over the socket, a refusal where the socket is not compiled"
    );
    assert!(out.reconcile.is_none(), "a demoted mount holds no reconcile client");
    assert!(out.identity.is_none());
}

/// The account's tier picks the gateway rows: a `demo` account whose DEMO gateway rows the loader
/// refuses does not run with the original cause, even when the LIVE tier's rows would load — they
/// are another tier's gateway, and its `ACCOUNT` is no live account.
#[test]
fn a_broken_demo_gateway_is_not_rescued_by_the_live_rows() {
    let mut fx = keyed(&AccountLabel::Default);
    fx.settings = VenueSettings::default().with_urls(addresses(
        UrlType::Demo,
        "127.0.0.1:not-a-port",
        "https://127.0.0.1:5000",
    ));
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
}

/// **A demo account with no address row does not run, and its start NAMES the row** (decision
/// 0265): the arming cause stays the loader's `NoCredentials`, nothing is dialled, and one `error!`
/// carries the refusal's words.
#[test]
fn a_demo_account_with_no_address_row_names_it_and_dials_nothing() {
    let mut fx = keyed(&AccountLabel::Default);
    fx.settings = no_addresses();
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        let mut req = fx.request(AccountTier::Demo, "AAPL", &tx);
        req.reconcile_enabled = true;
        start_with(
            req,
            |_, _| panic!("no address: the reconcile factory must not be called"),
            |_, _| panic!("(cpapi) no address: the reconcile factory must not be called"),
            |_, _| panic!("no address: nothing may be dialled"),
            |_, _| panic!("no address: no grid may be fetched"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "one line names the row: {events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR);
    says(&said[0].message, "ibkr: no address row for demo trade socket product=gateway");
}

/// **A demo account whose gateway socket row is not `host:port` does not run, and its start NAMES
/// the row** (drift U-D2), as a missing row is named: never the silence of "no credential".
#[test]
fn a_demo_account_with_a_malformed_socket_row_names_it_and_dials_nothing() {
    let mut fx = keyed(&AccountLabel::Default);
    let urls = addresses(UrlType::Demo, "127.0.0.1", "https://127.0.0.1:5000");
    fx.settings = VenueSettings::default().with_urls(urls);
    assert_eq!(
        VenueAdapterIbkr.resolve(&fx.inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "AAPL", &tx),
            |_, _| panic!("a malformed row: the reconcile factory must not be called"),
            |_, _| panic!("(cpapi) a malformed row: the reconcile factory must not be called"),
            |_, _| panic!("a malformed row: nothing may be dialled"),
            |_, _| panic!("a malformed row: no grid may be fetched"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "one line names the row: {events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR);
    says(&said[0].message, "ibkr: the demo trade socket gateway row `127.0.0.1` is not host:port");
}

/// **The adapter declares the gateway the gatewayhub supervises** (decision 0164 verdict 7(b), amended 2026-11-21: the
/// daemon asks the bridge, never a venue name): the socket backend's port from the tier's own rows, with the bridge's
/// cadence and its parked words; `cpapi` and `oauth` supervise nothing, and a backend row that cannot be read supervises
/// nothing and says why.
#[test]
fn the_socket_gateway_is_the_declared_link_probe_and_cpapi_and_oauth_declare_none() {
    let mut fx = keyed(&AccountLabel::Default);
    let probe = VenueAdapterIbkr
        .link_probe(&fx.inputs(AccountTier::Demo), "ibkr")
        .expect("no backend row is the socket: supervised");
    assert_eq!(probe.probe_every(), crate::gateway_watch::PROBE_EVERY);
    assert_eq!(probe.max_wait(), crate::gateway_watch::MAX_WAIT);
    assert_eq!(probe.not_ready_words(), Some(crate::gateway_watch::GATEWAY_NOT_LOGGED_IN));
    assert_eq!(probe.lost(), None, "the bridge reports its own death; the probe is the port");
    for word in ["cpapi", "oauth"] {
        fx.settings = demo_gateway(&[("backend", word)]);
        let probe = VenueAdapterIbkr.link_probe(&fx.inputs(AccountTier::Demo), "ibkr");
        assert!(probe.is_none(), "{word}");
    }
    fx.settings = demo_gateway(&[("backend", "websocket")]);
    let (probe, events) =
        captured(|| VenueAdapterIbkr.link_probe(&fx.inputs(AccountTier::Demo), "ibkr"));
    assert!(probe.is_none(), "an unreadable backend row supervises nothing");
    assert!(
        events.iter().any(|e| e.message.contains("TRADE-LINK: the ibkr gateway is not supervised")),
        "{events:?}"
    );
}
