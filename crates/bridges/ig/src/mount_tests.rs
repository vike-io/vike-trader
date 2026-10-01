use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};

/// All three secrets `load_ig_config_for_account` requires.
const KEYS: &[(&str, &str)] =
    &[("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "user"), ("IG_DEMO_PASSWORD", "pw")];

/// The epic every mount below is asked for.
const EPIC: &str = "CS.D.EURUSD.CFD.IP";

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = IgVenueMount.declaration();
    assert_eq!(IgVenueMount.venue(), "ig");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "IG",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["IDENTIFIER"],
            evm_key_suffixes: &[],
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /session/encryptionKey (X-IG-API-KEY only)",
            auth: ClockAuth::Credentialed,
            risk: ClockRisk::NoTimestamp,
        }
    );
}

#[test]
fn resolve_is_the_arms_own_gate() {
    for live in [false, true] {
        assert_eq!(
            IgVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(
            IgVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)),
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm)
            }
        );
        assert_eq!(
            IgVenueMount.resolve(&MountFixture::new(&KEYS[..2]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "all three secrets are required"
        );
    }
}

/// ig's arm has no live tier at all. A LIVE-tier key set beside the demo trio never resolves `Live`
/// under either ceiling, and a LIVE-tier key set ALONE configures nothing — paper, the trap
/// `crates/bridges/ig/CLAUDE.md` warns an operator about.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    // The LIVE names come from IG's own naming authority, not from literals.
    let (api_key, identifier, password) = ig_env_var_names(Environment::Live);
    let mut beside_demo = keyed(&AccountLabel::Default);
    let mut live_only = MountFixture::new(&[]);
    for k in [api_key, identifier, password] {
        beside_demo.vars.insert(k.clone(), "x".to_string());
        live_only.vars.insert(k, "x".to_string());
    }
    for live in [false, true] {
        assert!(!matches!(
            IgVenueMount.resolve(&beside_demo.inputs(live)),
            Resolution::Armed { tier: Tier::Live, .. }
        ));
        assert_eq!(
            IgVenueMount.resolve(&live_only.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "a LIVE-tier key set alone configures nothing (live={live})"
        );
    }
}

#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        IgVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert!(matches!(IgVenueMount.resolve(&keyed(&alt()).inputs(true)), Resolution::Armed { .. }));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        IgVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// ig offers no startup credential probe — `vike-mount`'s credential leg never read an ig balance —
/// and that holds for an ARMED ig under either ceiling, the only case in which a probe is asked for.
#[test]
fn an_armed_ig_offers_no_credential_probe() {
    for live in [false, true] {
        assert!(
            IgVenueMount.credential_probe(&keyed(&AccountLabel::Default).inputs(live)).is_none(),
            "live={live}"
        );
    }
}

/// Through the REAL `mount` (the real spawn and the real reconcile factory), a store without the
/// trio mounts PAPER with no reconcile handle and no identity, with reconciliation ON in the
/// request. `out.recon` being `None` cannot show by itself that no login was attempted — the
/// factory also answers `None` when a login fails — so the `mount_with` test below counts the
/// attempts.
#[test]
fn with_no_credentials_the_mount_is_paper_even_when_reconciling() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, EPIC, &tx);
    req.recon_enabled = true;
    let out = IgVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// What an ARMED ig mount hands `vike-mount`, read off `mount_with` — `mount`'s own body — with the
/// exec spawn and the reconcile login replaced by offline doubles, so it reads what THIS body sets:
/// a live client bound at the DEMO tier (the tier the `IG_DEMO_*` trio authenticates, which
/// `vike-mount` records the account against), no grid, contract size, margin mode or leg grids (the
/// arm fetches none), and ONE reconcile login exactly when reconciliation is on, its handle carried
/// through. Without the trio neither network step is taken, reconciliation on or off.
#[test]
fn an_armed_mount_binds_demo_and_logs_in_to_reconcile_only_when_asked() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let fx = keyed(&AccountLabel::Default);
    for live in [false, true] {
        for recon_enabled in [false, true] {
            let row = format!("live={live} recon_enabled={recon_enabled}");
            let mut req = fx.request(live, EPIC, &tx);
            req.recon_enabled = recon_enabled;
            let (mut spawns, mut logins) = (0usize, 0usize);
            let out = mount_with(
                req,
                |cfg, _events| {
                    spawns += 1;
                    assert_eq!(cfg.identifier, "user", "the spawn is handed the resolved trio");
                    Box::new(vike_exec::testing::RecordingClient::default())
                },
                |cfg, symbol| {
                    logins += 1;
                    assert_eq!((cfg.identifier.as_str(), symbol), ("user", EPIC));
                    let handle: Box<dyn ReconClient> =
                        Box::new(vike_exec::recon::FakeReconClient::default());
                    Some(handle)
                },
            );
            let ExecOutcome::Live(exec) = out.exec else {
                panic!("the trio is present, so the mount goes live ({row})");
            };
            assert_eq!(exec.bound_tier, Tier::Demo, "the tier the mount BINDS ({row})");
            assert!(exec.grid.is_none() && exec.contract_size.is_none(), "{row}");
            assert!(exec.margin_mode.is_none() && exec.leg_grids.is_empty(), "{row}");
            assert!(out.identity.is_none(), "{row}");
            assert_eq!(spawns, 1, "{row}");
            assert_eq!(logins, usize::from(recon_enabled), "the reconcile login is LAZY ({row})");
            assert_eq!(out.recon.is_some(), recon_enabled, "{row}");
        }
    }
    let no_trio = MountFixture::new(&[]);
    for recon_enabled in [false, true] {
        let mut req = no_trio.request(true, EPIC, &tx);
        req.recon_enabled = recon_enabled;
        let out = mount_with(
            req,
            |_, _| panic!("no trio, so no exec client may be spawned"),
            |_, _| panic!("no trio, so no reconcile login may be attempted"),
        );
        assert!(matches!(out.exec, ExecOutcome::Paper), "recon_enabled={recon_enabled}");
        assert!(out.recon.is_none() && out.identity.is_none(), "recon_enabled={recon_enabled}");
    }
}

/// With no key in the store, the credentialed clock read fails naming the missing variable and no
/// URL.
#[test]
fn a_credentialed_clock_read_reports_its_own_missing_key() {
    let fx = MountFixture::new(&[]);
    let e = IgVenueMount
        .server_time_ms(&fx.inputs(false), std::time::Duration::from_secs(3))
        .expect_err("no IG credentials in an empty map");
    let (api_key_var, _, _) = ig_env_var_names(Environment::Demo);
    assert!(e.contains(&api_key_var), "the message must name what is missing, not the URL: {e}");
    assert!(!e.contains("http"), "no URL in a report line: {e}");
}
