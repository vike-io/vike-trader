use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

/// Both halves `load_fxcm_config_for_account` requires: the ForexConnect login and its password.
const KEYS: &[(&str, &str)] = &[("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "p")];

/// A login `FxcmSession::login` can never hand to the SDK: its interior NUL is refused by
/// `CString::new` at the FFI boundary, before any FFI call. So a test that SPAWNS a session with it
/// opens no socket on any box — including a box whose `target/debug/deps/` holds a stale, loadable
/// `libfcshim.so`, which `crates/bridges/fxcm/tests/fxcm_login_failure.rs` (`UNREACHABLE_LOGIN`,
/// the same device) measured on a the CI box lane. That is what lets the tests below that spawn a client
/// ASSERT on such a box instead of skipping there.
const UNREACHABLE_LOGIN: &str = "not-a-real-login\0with-an-interior-nul";

/// `KEYS` with the login swapped for [`UNREACHABLE_LOGIN`].
const UNREACHABLE_KEYS: &[(&str, &str)] =
    &[("FXCM_DEMO_USER", UNREACHABLE_LOGIN), ("FXCM_DEMO_PASSWORD", "p")];

/// The one resolution that arms: a loadable shim AND a DEMO login.
const ARMED: Resolution =
    Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
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

/// A reconcile client that reports nothing — what a planted factory hands back, so a test can tell
/// "the factory's client was kept" from "no factory was asked" on a box where the real factory
/// could never log in.
struct NoReports;

impl ReconClient for NoReports {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = FxcmVenueMount.declaration();
    assert_eq!(FxcmVenueMount.venue(), "fxcm");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert!(matches!(d.book_identity, BookIdentity::Undeterminable { .. }));
    assert!(matches!(d.clock, ClockDecl::NotWired { unmeasured_risk: None, .. }));
}

/// THE ONE TEXT CHANGE this port makes on purpose (the venue mount contract spec's Finding 3): the
/// clock row said this venue had "no make_engine arm" while it had one. The reason must still be a
/// sentence an operator can act on.
#[test]
fn the_clock_reason_says_why_and_no_longer_denies_the_arm() {
    let ClockDecl::NotWired { reason, .. } = FxcmVenueMount.declaration().clock else {
        panic!("fxcm reads no clock");
    };
    assert!(reason.len() >= 60, "too short to be a reason: {reason}");
    assert!(!reason.contains("make_engine") && !reason.contains("no arm"), "stale: {reason}");
}

/// MOVED from `vike-mount` (`fxcm_live_intent`'s tests and the arming row built on them): the pure
/// decision under BOTH shim answers — the `true` one is unreachable on every CI runner, which is
/// why the shim is a parameter here.
#[test]
fn the_pure_decision_asks_the_shim_first_then_the_credentials() {
    let creds = keyed(&AccountLabel::Default);
    let empty = MountFixture::new(&[]);
    for live in [false, true] {
        assert_eq!(resolution_for(true, &creds.inputs(live)), ARMED, "shim + login arm");
        assert_eq!(
            resolution_for(false, &creds.inputs(live)),
            Resolution::Paper(PaperCause::SdkAbsent),
            "no shim: the mount refuses, whatever the store says"
        );
        assert_eq!(
            resolution_for(true, &empty.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "a shim with no login is the ordinary unconfigured state"
        );
        assert_eq!(
            resolution_for(false, &empty.inputs(live)),
            Resolution::Paper(PaperCause::SdkAbsent),
            "the shim is asked FIRST — the legacy arming row's order"
        );
        for half in [&KEYS[..1], &KEYS[1..]] {
            assert_eq!(
                resolution_for(true, &MountFixture::new(half).inputs(live)),
                Resolution::Paper(PaperCause::NoCredentials),
                "half a login is no login"
            );
        }
    }
}

/// THE WIRED DECISION, pinned to its ANSWERS rather than to the formula it is built from: on a box
/// whose shim does not load — every CI runner — `resolve` refuses a configured account `SdkAbsent`,
/// and refuses an unconfigured one for the shim first too, so no store can arm the venue there. On
/// a box whose shim loads (never a CI runner) the same two stores must resolve armed and
/// unconfigured. The shim is a process fact no test can plant, so this is the only place the
/// wiring to it is exercised; both answers of the decision itself are pinned unconditionally in
/// `the_pure_decision_asks_the_shim_first_then_the_credentials`.
#[test]
fn resolve_refuses_a_configured_account_on_a_box_without_the_shim() {
    let (creds, empty) = (keyed(&AccountLabel::Default), MountFixture::new(&[]));
    let shim_loads = crate::sdk_available();
    for live in [false, true] {
        let configured = FxcmVenueMount.resolve(&creds.inputs(live));
        let unconfigured = FxcmVenueMount.resolve(&empty.inputs(live));
        if shim_loads {
            assert_eq!(configured, ARMED, "a box with the shim arms a configured account");
            assert_eq!(unconfigured, Resolution::Paper(PaperCause::NoCredentials));
        } else {
            assert_eq!(
                configured,
                Resolution::Paper(PaperCause::SdkAbsent),
                "a login on a box without the shim must be refused, never armed"
            );
            assert_eq!(
                unconfigured,
                Resolution::Paper(PaperCause::SdkAbsent),
                "the shim is asked before the credentials, so the refusal names the shim"
            );
        }
    }
}

/// Review Focus 2: the mount resolves the DEMO tier only, so neither a ceiling nor a live-named
/// login makes it resolve `Live` — even with a loadable shim assumed.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = keyed(&AccountLabel::Default);
    // The live tier's LEGACY spelling, which `load_fxcm_config_for_account(Live, …)` still reads.
    fx.vars.insert("FXCM_MAINNET_USER".to_string(), "U".to_string());
    fx.vars.insert("FXCM_MAINNET_PASSWORD".to_string(), "P".to_string());
    for live in [false, true] {
        for sdk in [false, true] {
            assert!(!matches!(
                resolution_for(sdk, &fx.inputs(live)),
                Resolution::Armed { tier: Tier::Live, .. }
            ));
        }
    }
    // …and a store holding ONLY live-tier logins arms nothing at all, even with the shim: the mount
    // reads the DEMO login and nothing else. Both spellings the live loader accepts — the legacy
    // `MAINNET` one, and the primary one composed by the crate's own naming function.
    let (live_user, live_password, _, _) = crate::fxcm_env_var_names(Environment::Live);
    for live_only in [
        MountFixture::new(&[("FXCM_MAINNET_USER", "U"), ("FXCM_MAINNET_PASSWORD", "P")]),
        MountFixture::new(&[(live_user.as_str(), "U"), (live_password.as_str(), "P")]),
    ] {
        for live in [false, true] {
            assert_eq!(
                resolution_for(true, &live_only.inputs(live)),
                Resolution::Paper(PaperCause::NoCredentials),
                "a live-tier login alone is not the DEMO login this mount reads"
            );
        }
    }
}

/// The account 2×2 — which account is asked for × which account's keys the store holds — through
/// the pure decision with the shim assumed present, the only way the credential half is reachable
/// on a box without the SDK.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_eq!(resolution_for(true, &keyed(&AccountLabel::Default).inputs(true)), ARMED);
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        resolution_for(true, &alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's login"
    );
    assert_eq!(resolution_for(true, &keyed(&alt()).inputs(true)), ARMED);
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        resolution_for(true, &default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = FxcmVenueMount.mount(fx.request(true, "EURUSD", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE REFUSAL, at the bridge, through `mount` itself and so at THIS box's shim (the vike-mount twin
/// moves to `vike-tradehub`): credentials PRESENT, shim NOT loadable ⇒ paper. The EXEC half is what
/// this proves — a `mount` that stopped asking the shim would come back LIVE here. That nothing
/// reconciles is asserted too, but on this box it would hold whatever the ORDER of the steps, since
/// a reconcile login fails without the shim exactly as the exec one does; the order is pinned
/// through the seam, by `the_refusal_comes_before_the_reconcile_factory`.
///
/// On a box whose shim DOES load, the same call must mount LIVE, bound to the DEMO tier — so this
/// asserts on both kinds of box rather than skipping on one. The login is [`UNREACHABLE_LOGIN`], so
/// there both the exec session and the real reconcile factory fail their logins at the FFI boundary
/// and nothing reaches the network; nothing reconciles on that box either.
#[test]
fn credentials_without_a_loadable_shim_refuse_the_live_mount() {
    let fx = MountFixture::new(UNREACHABLE_KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "EURUSD", &tx);
    // Reconciliation ON, as on a live-configured box, so the outcome is the one such a box gets.
    req.recon_enabled = true;
    let out = FxcmVenueMount.mount(req);
    if crate::sdk_available() {
        assert!(
            matches!(out.exec, ExecOutcome::Live(LiveExec { bound_tier: Tier::Demo, .. })),
            "a loadable shim and a login mount LIVE, on the DEMO tier"
        );
    } else {
        assert!(matches!(out.exec, ExecOutcome::Paper), "no shim must never mount live");
    }
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE ORDER, through the seam and so on every box: with a login present and the shim refused, the
/// reconcile factory is never ASKED — reconciliation on — so a refused mount opens no second
/// ForexConnect session and logs no `fxcm reconcile:` failure beside its refusal.
#[test]
fn the_refusal_comes_before_the_reconcile_factory() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "EURUSD", &tx);
    req.recon_enabled = true;
    let mut asked = false;
    let out = mount_with(req, false, |_, _| {
        asked = true;
        Some(Box::new(NoReports))
    });
    assert!(!asked, "the refusal must be decided before any reconcile session is opened");
    assert!(matches!(out.exec, ExecOutcome::Paper), "a refused mount is paper");
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE LIVE BRANCH, through the seam with the shim assumed loadable — the branch no CI runner reaches
/// through `mount`. Its outcome is what `vike-mount` folds: a client bound to the DEMO tier, no grid,
/// no contract size, no margin mode, no leg grids and no identity; and the reconcile factory asked
/// exactly when reconciliation is on, with the resolved login and the mounted symbol, its client
/// kept. Offline on EVERY box: the spawned client's session thread fails its login — at the shim
/// lookup where there is no shim, at the FFI boundary on [`UNREACHABLE_LOGIN`] where there is one —
/// and waits in `refuse_every_command` until the client drops.
#[test]
fn a_loadable_shim_mounts_a_demo_bound_client_and_reconciles_only_when_asked() {
    let fx = MountFixture::new(UNREACHABLE_KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    for recon_enabled in [false, true] {
        let mut req = fx.request(false, "EURUSD", &tx);
        req.recon_enabled = recon_enabled;
        let mut asked = None;
        let out = mount_with(req, true, |cfg, symbol| {
            asked = Some((cfg.user.clone(), symbol.to_string()));
            Some(Box::new(NoReports))
        });
        let ExecOutcome::Live(live) = out.exec else {
            panic!("a loadable shim and a DEMO login mount LIVE");
        };
        assert_eq!(live.bound_tier, Tier::Demo, "the credentials authenticate the DEMO tier");
        assert!(live.grid.is_none(), "no symbol-properties endpoint: the permissive grid");
        assert!(live.contract_size.is_none() && live.margin_mode.is_none());
        assert!(live.leg_grids.is_empty());
        assert!(out.identity.is_none());
        if recon_enabled {
            assert_eq!(asked, Some((UNREACHABLE_LOGIN.to_string(), "EURUSD".to_string())));
            assert!(out.recon.is_some(), "the factory's reconcile client is the one kept");
        } else {
            assert_eq!(asked, None, "reconciliation off: the second login is never attempted");
            assert!(out.recon.is_none());
        }
    }
}
