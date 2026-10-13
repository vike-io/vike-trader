use super::*;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;

/// All three fields `load_ig_config_from` requires.
const KEYS: &[(&str, &str)] = &[("API_KEY", "k"), ("IDENTIFIER", "user"), ("PASSWORD", "pw")];

/// The epic every mount below is asked for.
const EPIC: &str = "CS.D.EURUSD.CFD.IP";

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = IgVenueMount.declaration();
    assert_eq!(IgVenueMount.venue(), "ig");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named { name_fields: &["IDENTIFIER"], evm_key_fields: &[] }
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

/// The arming-probe row: the whole trio on an account below `live` arms a demo-only arm; a `live`
/// account's whole trio is `LiveTierNotWired`; anything less stays `NoCredentials` at every tier.
#[test]
fn resolve_is_the_arms_own_gate() {
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(
            IgVenueMount.resolve(&MountFixture::new(KEYS).inputs(tier)),
            Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm)
            }
        );
    }
    assert_eq!(
        IgVenueMount.resolve(&MountFixture::new(KEYS).inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "a live account's trio configures nothing — and the cause says that, rather than that the \
         account holds nothing"
    );
    for tier in AccountTier::ALL {
        assert_eq!(
            IgVenueMount.resolve(&MountFixture::new(&[]).inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(
            IgVenueMount.resolve(&MountFixture::new(&KEYS[..2]).inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials),
            "all three fields are required ({tier})"
        );
    }
}

/// ig's arm has no live tier at all: no tier makes it resolve `Live`.
#[test]
fn no_tier_ever_resolves_live() {
    for tier in AccountTier::ALL {
        assert!(
            !matches!(
                IgVenueMount.resolve(&MountFixture::new(KEYS).inputs(tier)),
                Resolution::Armed { tier: AccountTier::Live, .. }
            ),
            "{tier}"
        );
    }
}

/// The stay-paper half at the MOUNT: through `mount_with`, a live account's trio spawns no exec
/// client and attempts no reconcile login, reconciliation on or off.
#[test]
fn a_live_accounts_trio_spawns_nothing_and_logs_in_nowhere() {
    let fx = MountFixture::new(KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    for recon_enabled in [false, true] {
        let mut req = fx.request(AccountTier::Live, EPIC, &tx);
        req.recon_enabled = recon_enabled;
        let out = mount_with(
            req,
            |_, _| panic!("a live account must not spawn an exec client"),
            |_, _| panic!("a live account must not attempt a reconcile login"),
        );
        assert!(matches!(out.exec, ExecOutcome::Paper), "recon_enabled={recon_enabled}");
        assert!(out.recon.is_none() && out.identity.is_none(), "recon_enabled={recon_enabled}");
    }
}

/// ig offers no startup credential probe — `vike-mount`'s credential leg never read an ig balance —
/// and that holds for an ARMED ig, the only case in which a probe is asked for.
#[test]
fn an_armed_ig_offers_no_credential_probe() {
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert!(IgVenueMount.credential_probe(&MountFixture::new(KEYS).inputs(tier)).is_none());
    }
}

/// Through the REAL `mount` (the real spawn and the real reconcile factory), an account without the
/// trio mounts PAPER with no reconcile handle and no identity, with reconciliation ON in the
/// request. `out.recon` being `None` cannot show by itself that no login was attempted — the
/// factory also answers `None` when a login fails — so the `mount_with` test below counts the
/// attempts.
#[test]
fn with_no_credentials_the_mount_is_paper_even_when_reconciling() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, EPIC, &tx);
    req.recon_enabled = true;
    let out = IgVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// What an ARMED ig mount hands `vike-mount`, read off `mount_with` — `mount`'s own body — with the
/// exec spawn and the reconcile login replaced by offline doubles, so it reads what THIS body sets:
/// a live client bound at the DEMO tier (the tier the demo account's trio authenticates, which
/// `vike-mount` records the account against), no grid, contract size, margin mode or leg grids (the
/// arm fetches none), and ONE reconcile login exactly when reconciliation is on, its handle carried
/// through. Without the trio neither network step is taken, reconciliation on or off.
#[test]
fn an_armed_mount_binds_demo_and_logs_in_to_reconcile_only_when_asked() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let fx = MountFixture::new(KEYS);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        for recon_enabled in [false, true] {
            let row = format!("tier={tier} recon_enabled={recon_enabled}");
            let mut req = fx.request(tier, EPIC, &tx);
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
            assert_eq!(exec.bound_tier, AccountTier::Demo, "the tier the mount BINDS ({row})");
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
        let mut req = no_trio.request(AccountTier::Demo, EPIC, &tx);
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

/// With no key in the account, the credentialed clock read fails naming the missing FIELDS and no
/// URL.
#[test]
fn a_credentialed_clock_read_reports_its_own_missing_key() {
    let fx = MountFixture::new(&[]);
    let e = IgVenueMount
        .server_time_ms(&fx.inputs(AccountTier::Demo), std::time::Duration::from_secs(3))
        .expect_err("no IG credentials in an empty account");
    assert!(e.contains("API_KEY"), "the message must name what is missing, not the URL: {e}");
    assert!(!e.contains("http"), "no URL in a report line: {e}");

    // Only what is MISSING is named.
    let half = MountFixture::new(&KEYS[..1]);
    let e = IgVenueMount
        .server_time_ms(&half.inputs(AccountTier::Demo), std::time::Duration::from_secs(3))
        .expect_err("half a trio reads no clock");
    assert!(e.contains("IDENTIFIER") && e.contains("PASSWORD"), "{e}");
    assert!(!e.contains("API_KEY"), "the API key is present: {e}");
}

// ── what the arm says about a LIVE account it will not use ──────────────────────────────────────

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The whole trio for `label`, every value [`SECRET`].
fn secret_trio(label: &str) -> MountFixture {
    let mut fx =
        MountFixture::new(&[("API_KEY", SECRET), ("IDENTIFIER", SECRET), ("PASSWORD", SECRET)]);
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// `mount_with` over offline doubles: a recording exec client, no reconcile login.
fn mount_offline(fx: &MountFixture, tier: AccountTier) -> (MountOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    captured(|| {
        mount_with(
            fx.request(tier, EPIC, &tx),
            |_, _| Box::new(vike_exec::testing::RecordingClient::default()),
            |_, _| None,
        )
    })
}

/// A demo account's trio — the ordinary case — mounts the demo gateway, bound at DEMO, announces
/// the mount at the tier it bound, and says nothing about a live tier.
#[test]
fn a_demo_account_mounts_and_says_nothing_about_a_live_tier() {
    let (out, events) = mount_offline(&secret_trio("ALONE"), AccountTier::Demo);
    let ExecOutcome::Live(exec) = out.exec else { panic!("a demo account's trio mounts") };
    assert_eq!(exec.bound_tier, AccountTier::Demo);
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// A `live` account holding the whole trio used to read as an empty store. It does NOT RUN and says
/// so once, at `error!`, naming the account and never a value.
#[test]
fn a_live_accounts_trio_is_said_once_and_the_mount_is_paper() {
    let (out, events) = mount_offline(&secret_trio("LIVEACCT"), AccountTier::Live);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ig"), Some("LIVEACCT")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// Half a trio on a `live` account is what the loader calls absent: paper, `NoCredentials`, and
/// silent.
#[test]
fn a_half_written_set_is_no_credentials_and_says_nothing() {
    let mut fx = MountFixture::new(&KEYS[..1]);
    fx.account = AccountLabel::parse("HALF").expect("a legal label");
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(AccountTier::Live, EPIC, &tx),
            |_, _| panic!("no complete trio, so no exec client"),
            |_, _| panic!("no complete trio, so no reconcile login"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert_eq!(
        IgVenueMount.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// A LABELLED account is mounted only when it armed, so a `live` one never reaches `mount` and said
/// nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm says exactly
/// what `mount` says for the default account. An empty and a demo account are silent.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let live = secret_trio("UNMNT");
    let (_, events) =
        captured(|| IgVenueMount.report_unmounted_account(&live.inputs(AccountTier::Live)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    for (quiet, tier) in [(&MountFixture::new(&[]), AccountTier::Live), (&live, AccountTier::Demo)]
    {
        let (_, events) = captured(|| IgVenueMount.report_unmounted_account(&quiet.inputs(tier)));
        assert!(events.is_empty(), "silent ({tier}): {events:?}");
    }
}
