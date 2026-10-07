use super::*;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::{AccountLabel, account_key};

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
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "a LIVE-tier key set alone configures nothing — and the cause says that, rather than \
             that the store holds nothing (live={live})"
        );
    }
}

/// The LIVE names come from IG's own naming authority — and the legacy `MAINNET` tier its loader
/// still honours is a LIVE-tier key set too, so it gets the same named cause.
#[test]
fn the_legacy_mainnet_tier_is_a_live_tier_key_set_too() {
    let mut legacy = MountFixture::new(&[]);
    for suffix in ["API_KEY", "IDENTIFIER", "PASSWORD"] {
        let name =
            format!("IG_{}_{suffix}", Environment::Live.legacy_str().expect("a legacy tier"));
        legacy.vars.insert(name, "x".to_string());
    }
    for live in [false, true] {
        assert_eq!(
            IgVenueMount.resolve(&legacy.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "live={live}"
        );
    }
}

/// Only a COMPLETE LIVE-tier set is one: half of it is what `load_ig_config_for_account` calls
/// absent, so the original cause stands.
#[test]
fn half_a_live_key_set_is_still_no_credentials() {
    let (api_key, identifier, _password) = ig_env_var_names(Environment::Live);
    let mut half = MountFixture::new(&[]);
    half.vars.insert(api_key, "x".to_string());
    half.vars.insert(identifier, "x".to_string());
    for live in [false, true] {
        assert_eq!(
            IgVenueMount.resolve(&half.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "live={live}"
        );
    }
}

/// The cause is scoped to the ACCOUNT asking, in both directions.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_keys() {
    let (api_key, identifier, password) = ig_env_var_names(Environment::Live);
    let mut default_live = MountFixture::new(&[]);
    let mut alt_live = MountFixture::new(&[]);
    for k in [api_key, identifier, password] {
        default_live.vars.insert(k.clone(), "x".to_string());
        alt_live.vars.insert(account_key(&k, &alt()), "x".to_string());
    }
    alt_live.account = alt();
    assert_eq!(
        IgVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    default_live.account = alt();
    assert_eq!(
        IgVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds live keys"
    );
    assert_eq!(
        IgVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        IgVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds live keys"
    );
}

/// The stay-paper half at the MOUNT: through `mount_with`, a LIVE-tier set alone spawns no exec
/// client and attempts no reconcile login, reconciliation on or off — the cause and the loudness
/// changed, the outcome did not.
#[test]
fn a_live_key_set_alone_spawns_nothing_and_logs_in_nowhere() {
    let (api_key, identifier, password) = ig_env_var_names(Environment::Live);
    let mut fx = MountFixture::new(&[]);
    for k in [api_key, identifier, password] {
        fx.vars.insert(k, "x".to_string());
    }
    let (tx, _rx) = vike_exec::event_channel(8);
    for recon_enabled in [false, true] {
        let mut req = fx.request(true, EPIC, &tx);
        req.recon_enabled = recon_enabled;
        let out = mount_with(
            req,
            |_, _| panic!("a live-tier set alone must not spawn an exec client"),
            |_, _| panic!("a live-tier set alone must not attempt a reconcile login"),
        );
        assert!(matches!(out.exec, ExecOutcome::Paper), "recon_enabled={recon_enabled}");
        assert!(out.recon.is_none() && out.identity.is_none(), "recon_enabled={recon_enabled}");
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

// ── what the arm says about a LIVE tier it will not use ─────────────────────────────────────────
//
// Every account label below is unique to its test: the unused-beside-demo `warn!` is said once per
// process per `(venue, account)`, so two tests sharing a label would pass or fail by scheduling order.

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The live trio's names for `label`, composed by IG's own naming authority.
fn live_names(label: &AccountLabel) -> Vec<String> {
    let (api_key, identifier, password) = ig_env_var_names(Environment::Live);
    [api_key, identifier, password].iter().map(|n| account_key(n, label)).collect()
}

/// The demo trio AND the live trio, both for `label` (every live value is [`SECRET`]).
fn both_tiers(label: &AccountLabel) -> MountFixture {
    let mut fx = keyed(label);
    for name in live_names(label) {
        fx.vars.insert(name, SECRET.to_string());
    }
    fx
}

/// `mount_with` over offline doubles: a recording exec client, no reconcile login.
fn mount_offline(fx: &MountFixture) -> (MountOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    captured(|| {
        mount_with(
            fx.request(true, EPIC, &tx),
            |_, _| Box::new(vike_exec::testing::RecordingClient::default()),
            |_, _| None,
        )
    })
}

/// A live trio BESIDE the demo one used to mount the demo tier in silence. It still mounts the demo
/// tier — bound at DEMO, nothing else moves — and now says, once, that the live trio is unused.
#[test]
fn a_live_key_set_beside_the_demo_one_is_named_unused_and_the_mount_is_unchanged() {
    let beside = AccountLabel::parse("BESIDE").expect("a legal label");
    let (out, events) = mount_offline(&both_tiers(&beside));
    let ExecOutcome::Live(exec) = out.exec else { panic!("the demo trio mounts, as it did") };
    assert_eq!(exec.bound_tier, Tier::Demo, "the live trio must not move the bound tier");
    let unused = found_tier_events(&events);
    assert_eq!(unused.len(), 1, "exactly one line about the live trio: {events:?}");
    let e = unused[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ig"), Some("BESIDE")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is still said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// The demo trio alone — the ordinary case — says nothing new.
#[test]
fn a_demo_trio_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let (_, events) = mount_offline(&keyed(&alone));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A live trio with one key missing is a typo or a half-finished edit, and the loader calls it
/// absent — so it printed `NoCredentials` under the words of an empty store. The mount now names
/// the keys it lacks (label-composed, names only) and still lands on paper. The LEGACY `MAINNET`
/// spelling the loader also honours is a set of its own, named under ITS spelling.
#[test]
fn a_half_written_live_key_set_names_the_missing_keys_and_the_mount_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let live = live_names(&half);
    let mut fx = MountFixture::new(&[]);
    fx.account = half.clone();
    fx.vars.insert(live[0].clone(), SECRET.to_string());
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, EPIC, &tx),
            |_, _| panic!("no demo trio, so no exec client"),
            |_, _| panic!("no demo trio, so no reconcile login"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ig"), Some("HALF")), "{e:?}");
    for missing in &live[1..] {
        assert!(e.message.contains(missing.as_str()), "names `{missing}`: {}", e.message);
    }
    assert!(!e.message.contains(&live[0]), "names only what is MISSING: {}", e.message);
    assert!(!format!("{e:?}").contains(SECRET), "no credential VALUE: {e:?}");
    assert_eq!(
        IgVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the cause the arming screen prints is unchanged"
    );

    // The legacy spelling: one key of the `MAINNET` trio is that spelling's half-written set.
    let legacy_label = AccountLabel::parse("LEGACY").expect("a legal label");
    let legacy_tier = Environment::Live.legacy_str().expect("a legacy tier");
    let legacy_key = account_key(&format!("IG_{legacy_tier}_API_KEY"), &legacy_label);
    let legacy_password = account_key(&format!("IG_{legacy_tier}_PASSWORD"), &legacy_label);
    let mut legacy = MountFixture::new(&[]);
    legacy.account = legacy_label;
    legacy.vars.insert(legacy_key.clone(), SECRET.to_string());
    let (_, events) = captured(|| IgVenueMount.report_unmounted_account(&legacy.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert!(said[0].message.contains(&legacy_password), "{}", said[0].message);
    assert!(!said[0].message.contains(&legacy_key), "{}", said[0].message);
}

/// A LABELLED account is mounted only when it armed, so one holding only live keys never reaches
/// `mount` and said nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm
/// says exactly what `mount` says for the default account.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let mut live_only = MountFixture::new(&[]);
    live_only.account = unmounted.clone();
    for name in live_names(&unmounted) {
        live_only.vars.insert(name, SECRET.to_string());
    }
    let (_, events) = captured(|| IgVenueMount.report_unmounted_account(&live_only.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let (_, events) = captured(|| {
        IgVenueMount.report_unmounted_account(&MountFixture::new(&[]).inputs(true));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}
